use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tauri::State;

use crate::db::{self, DbPool};
use crate::path_utils::{central_skills_dir, expand_home_path, path_to_string};
use crate::AppState;

const CENTRAL_REPOSITORY_PATH_KEY: &str = "central_repository_path";
const CENTRAL_REPOSITORY_REMOTE_URL_KEY: &str = "central_repository_remote_url";
const CENTRAL_AGENT_ID: &str = "central";
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CentralRepositoryConfig {
    pub local_path: String,
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CentralRepositoryStatus {
    pub is_git_repository: bool,
    pub branch: Option<String>,
    pub remote_url: Option<String>,
    pub has_changes: bool,
    pub ahead: u32,
    pub behind: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CentralRepositoryOperationResult {
    pub output: String,
    pub status: CentralRepositoryStatus,
}

fn normalize_optional_remote(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn command_output_to_string(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout,
        (true, false) => stderr,
        (false, false) => format!("{}\n{}", stdout, stderr),
    }
}

fn run_git_command(repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started_at.elapsed() >= GIT_COMMAND_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git {:?} timed out", args));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("Failed to wait for git: {}", e)),
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to read git output: {}", e))?;
    let text = command_output_to_string(&output);
    if output.status.success() {
        Ok(text)
    } else if text.is_empty() {
        Err(format!("git {:?} failed", args))
    } else {
        Err(text)
    }
}

fn try_git_command(repo_path: &Path, args: &[&str]) -> Option<String> {
    run_git_command(repo_path, args)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_ahead_behind(value: &str) -> (u32, u32) {
    let mut parts = value.split_whitespace();
    let behind = parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    let ahead = parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    (ahead, behind)
}

fn normalize_central_repository_inputs(
    local_path: &str,
    remote_url: &str,
) -> Result<CentralRepositoryConfig, String> {
    let trimmed_path = local_path.trim();
    if trimmed_path.is_empty() {
        return Err("Central repository path cannot be empty".to_string());
    }

    Ok(CentralRepositoryConfig {
        local_path: path_to_string(&expand_home_path(trimmed_path)),
        remote_url: normalize_optional_remote(remote_url),
    })
}

fn is_exact_git_repository_root(local_path: &Path) -> Result<bool, String> {
    let Some(repo_root) = try_git_command(local_path, &["rev-parse", "--show-toplevel"]) else {
        return Ok(false);
    };

    let requested = fs::canonicalize(local_path)
        .map_err(|e| format!("Failed to resolve central repository path: {}", e))?;
    let root = fs::canonicalize(repo_root)
        .map_err(|e| format!("Failed to resolve git repository root: {}", e))?;
    Ok(requested == root)
}

async fn ensure_exact_git_repository(pool: &DbPool) -> Result<PathBuf, String> {
    let config = get_central_repository_config_impl(pool).await?;
    let repo_path = PathBuf::from(&config.local_path);
    let status = get_central_repository_status_for_path(&repo_path).await?;
    if status.is_git_repository {
        Ok(repo_path)
    } else {
        Err(status.last_error.unwrap_or_else(|| {
            "Central repository is not an initialized Git repository".to_string()
        }))
    }
}

async fn sync_central_agent_path(pool: &DbPool, local_path: &str) -> Result<(), String> {
    sqlx::query("UPDATE agents SET global_skills_dir = ? WHERE id = ?")
        .bind(local_path)
        .bind(CENTRAL_AGENT_ID)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query(
        "INSERT OR IGNORE INTO scan_directories
         (path, label, is_active, is_builtin, added_at)
         VALUES (?, 'Central Skills', 1, 1, datetime('now'))",
    )
    .bind(local_path)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

pub async fn get_central_repository_config_impl(
    pool: &DbPool,
) -> Result<CentralRepositoryConfig, String> {
    let local_path = match db::get_setting(pool, CENTRAL_REPOSITORY_PATH_KEY).await? {
        Some(path) if !path.trim().is_empty() => path,
        _ => db::get_agent_by_id(pool, CENTRAL_AGENT_ID)
            .await?
            .map(|agent| agent.global_skills_dir)
            .unwrap_or_else(|| path_to_string(&central_skills_dir())),
    };

    let remote_url = db::get_setting(pool, CENTRAL_REPOSITORY_REMOTE_URL_KEY)
        .await?
        .and_then(|value| normalize_optional_remote(&value));

    Ok(CentralRepositoryConfig {
        local_path,
        remote_url,
    })
}

pub async fn set_central_repository_config_impl(
    pool: &DbPool,
    local_path: &str,
    remote_url: &str,
) -> Result<CentralRepositoryConfig, String> {
    let config = normalize_central_repository_inputs(local_path, remote_url)?;

    db::set_setting(pool, CENTRAL_REPOSITORY_PATH_KEY, &config.local_path).await?;
    db::set_setting(
        pool,
        CENTRAL_REPOSITORY_REMOTE_URL_KEY,
        config.remote_url.as_deref().unwrap_or(""),
    )
    .await?;
    sync_central_agent_path(pool, &config.local_path).await?;

    Ok(config)
}

pub async fn central_skills_dir_for_pool(pool: &DbPool) -> Result<PathBuf, String> {
    let config = get_central_repository_config_impl(pool).await?;
    Ok(PathBuf::from(config.local_path))
}

pub async fn get_central_repository_status_for_path(
    local_path: &Path,
) -> Result<CentralRepositoryStatus, String> {
    if !local_path.exists() {
        return Ok(CentralRepositoryStatus {
            is_git_repository: false,
            branch: None,
            remote_url: None,
            has_changes: false,
            ahead: 0,
            behind: 0,
            last_error: Some("Directory does not exist".to_string()),
        });
    }

    if !is_exact_git_repository_root(local_path)? {
        return Ok(CentralRepositoryStatus {
            is_git_repository: false,
            branch: None,
            remote_url: None,
            has_changes: false,
            ahead: 0,
            behind: 0,
            last_error: Some("Directory is not the root of a Git repository".to_string()),
        });
    }

    let branch = try_git_command(local_path, &["branch", "--show-current"]);
    let remote_url = try_git_command(local_path, &["config", "--get", "remote.origin.url"]);
    let has_changes = try_git_command(local_path, &["status", "--porcelain"])
        .map(|status| !status.is_empty())
        .unwrap_or(false);
    let (ahead, behind) = try_git_command(
        local_path,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    )
    .map(|counts| parse_ahead_behind(&counts))
    .unwrap_or((0, 0));

    Ok(CentralRepositoryStatus {
        is_git_repository: true,
        branch,
        remote_url,
        has_changes,
        ahead,
        behind,
        last_error: None,
    })
}

pub async fn get_central_repository_status_impl(
    pool: &DbPool,
) -> Result<CentralRepositoryStatus, String> {
    let config = get_central_repository_config_impl(pool).await?;
    get_central_repository_status_for_path(Path::new(&config.local_path)).await
}

pub async fn initialize_central_repository_impl(
    pool: &DbPool,
    local_path: &str,
    remote_url: &str,
) -> Result<CentralRepositoryOperationResult, String> {
    let config = normalize_central_repository_inputs(local_path, remote_url)?;
    let repo_path = PathBuf::from(&config.local_path);
    if repo_path.exists() && !repo_path.is_dir() {
        return Err("Central repository path must be a directory".to_string());
    }
    fs::create_dir_all(&repo_path).map_err(|e| format!("Failed to create directory: {}", e))?;

    let mut output_parts = Vec::new();
    if !repo_path.join(".git").exists() {
        match run_git_command(&repo_path, &["init", "-b", "main"]) {
            Ok(output) => output_parts.push(output),
            Err(_) => output_parts.push(run_git_command(&repo_path, &["init"])?),
        }
    }

    if let Some(remote) = config.remote_url.as_deref() {
        let remote_result =
            if try_git_command(&repo_path, &["remote", "get-url", "origin"]).is_some() {
                run_git_command(&repo_path, &["remote", "set-url", "origin", remote])
            } else {
                run_git_command(&repo_path, &["remote", "add", "origin", remote])
            }?;
        output_parts.push(remote_result);
    }

    let config = set_central_repository_config_impl(
        pool,
        &config.local_path,
        config.remote_url.as_deref().unwrap_or(""),
    )
    .await?;
    let repo_path = PathBuf::from(&config.local_path);

    Ok(CentralRepositoryOperationResult {
        output: output_parts
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        status: get_central_repository_status_for_path(&repo_path).await?,
    })
}

pub async fn pull_central_repository_impl(
    pool: &DbPool,
) -> Result<CentralRepositoryOperationResult, String> {
    let repo_path = ensure_exact_git_repository(pool).await?;
    let output = run_git_command(&repo_path, &["pull", "--ff-only"])?;
    Ok(CentralRepositoryOperationResult {
        output,
        status: get_central_repository_status_for_path(&repo_path).await?,
    })
}

pub async fn push_central_repository_impl(
    pool: &DbPool,
) -> Result<CentralRepositoryOperationResult, String> {
    let repo_path = ensure_exact_git_repository(pool).await?;
    let output = run_git_command(&repo_path, &["push"])?;
    Ok(CentralRepositoryOperationResult {
        output,
        status: get_central_repository_status_for_path(&repo_path).await?,
    })
}

#[tauri::command]
pub async fn get_central_repository_config(
    state: State<'_, AppState>,
) -> Result<CentralRepositoryConfig, String> {
    get_central_repository_config_impl(&state.db).await
}

#[tauri::command]
pub async fn set_central_repository_config(
    state: State<'_, AppState>,
    local_path: String,
    remote_url: String,
) -> Result<CentralRepositoryConfig, String> {
    set_central_repository_config_impl(&state.db, &local_path, &remote_url).await
}

#[tauri::command]
pub async fn get_central_repository_status(
    state: State<'_, AppState>,
) -> Result<CentralRepositoryStatus, String> {
    get_central_repository_status_impl(&state.db).await
}

#[tauri::command]
pub async fn initialize_central_repository(
    state: State<'_, AppState>,
    local_path: String,
    remote_url: String,
) -> Result<CentralRepositoryOperationResult, String> {
    initialize_central_repository_impl(&state.db, &local_path, &remote_url).await
}

#[tauri::command]
pub async fn pull_central_repository(
    state: State<'_, AppState>,
) -> Result<CentralRepositoryOperationResult, String> {
    pull_central_repository_impl(&state.db).await
}

#[tauri::command]
pub async fn push_central_repository(
    state: State<'_, AppState>,
) -> Result<CentralRepositoryOperationResult, String> {
    push_central_repository_impl(&state.db).await
}

#[cfg(test)]
mod tests {
    use crate::db::{self, DbPool};
    use sqlx::SqlitePool;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    async fn setup_test_db() -> DbPool {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();
        pool
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn set_config_persists_path_remote_and_updates_central_agent() {
        let pool = setup_test_db().await;
        let temp_dir = TempDir::new().unwrap();
        let local_path = temp_dir.path().join("central-skills");
        let remote_url = "https://example.com/skills.git";

        let config = super::set_central_repository_config_impl(
            &pool,
            local_path.to_str().unwrap(),
            remote_url,
        )
        .await
        .unwrap();

        assert_eq!(config.local_path, local_path.to_string_lossy());
        assert_eq!(config.remote_url.as_deref(), Some(remote_url));

        let central = db::get_agent_by_id(&pool, "central")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(central.global_skills_dir, local_path.to_string_lossy());
        assert_eq!(
            db::get_setting(&pool, "central_repository_path")
                .await
                .unwrap()
                .as_deref(),
            Some(local_path.to_str().unwrap())
        );
    }

    #[tokio::test]
    async fn status_reports_non_git_directory() {
        let temp_dir = TempDir::new().unwrap();

        let status = super::get_central_repository_status_for_path(temp_dir.path())
            .await
            .unwrap();

        assert!(!status.is_git_repository);
        assert!(status.branch.is_none());
        assert!(status.remote_url.is_none());
        assert!(!status.has_changes);
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
    }

    #[tokio::test]
    async fn initialize_creates_git_repo_and_sets_origin() {
        let pool = setup_test_db().await;
        let temp_dir = TempDir::new().unwrap();
        let local_path = temp_dir.path().join("repo");
        let remote_url = "https://example.com/skills.git";

        let result = super::initialize_central_repository_impl(
            &pool,
            local_path.to_str().unwrap(),
            remote_url,
        )
        .await
        .unwrap();

        assert!(local_path.join(".git").exists());
        assert!(result.status.is_git_repository);
        assert_eq!(result.status.remote_url.as_deref(), Some(remote_url));
    }

    #[tokio::test]
    async fn status_detects_branch_and_working_tree_changes() {
        let temp_dir = TempDir::new().unwrap();
        run_git(temp_dir.path(), &["init", "-b", "main"]);
        fs::write(temp_dir.path().join("SKILL.md"), "# Test\n").unwrap();

        let status = super::get_central_repository_status_for_path(temp_dir.path())
            .await
            .unwrap();

        assert!(status.is_git_repository);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(status.has_changes);
    }

    #[tokio::test]
    async fn status_rejects_git_repository_child_directory() {
        let temp_dir = TempDir::new().unwrap();
        run_git(temp_dir.path(), &["init", "-b", "main"]);
        let child = temp_dir.path().join("nested-central");
        fs::create_dir_all(&child).unwrap();

        let status = super::get_central_repository_status_for_path(&child)
            .await
            .unwrap();

        assert!(!status.is_git_repository);
        assert_eq!(
            status.last_error.as_deref(),
            Some("Directory is not the root of a Git repository")
        );
    }

    #[tokio::test]
    async fn initialize_does_not_persist_config_when_path_is_file() {
        let pool = setup_test_db().await;
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("not-a-directory");
        fs::write(&file_path, "not a directory").unwrap();
        let original_config = super::get_central_repository_config_impl(&pool)
            .await
            .unwrap();

        let result =
            super::initialize_central_repository_impl(&pool, file_path.to_str().unwrap(), "").await;

        assert!(result.is_err());
        let current_config = super::get_central_repository_config_impl(&pool)
            .await
            .unwrap();
        assert_eq!(current_config, original_config);
    }

    #[tokio::test]
    async fn custom_central_scan_directory_survives_database_reinit() {
        let pool = setup_test_db().await;
        let temp_dir = TempDir::new().unwrap();
        let local_path = temp_dir.path().join("central-skills");

        super::set_central_repository_config_impl(&pool, local_path.to_str().unwrap(), "")
            .await
            .unwrap();
        db::init_database(&pool).await.unwrap();

        let dirs = db::get_scan_directories(&pool).await.unwrap();
        assert!(dirs
            .iter()
            .any(|dir| dir.path == local_path.to_string_lossy()));
    }
}
