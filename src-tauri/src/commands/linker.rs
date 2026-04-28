use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use super::scanner::parse_skill_md;
use crate::db::{self, DbPool, SkillInstallation};
use crate::AppState;

// ─── Types ────────────────────────────────────────────────────────────────────

/// Result of a single skill install operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallResult {
    pub symlink_path: String,
}

/// Result of a batch install across multiple agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchInstallResult {
    pub succeeded: Vec<String>,
    pub failed: Vec<FailedInstall>,
}

/// Describes a single failed install within a batch operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedInstall {
    pub agent_id: String,
    pub error: String,
}

/// Result of migrating one agent-local skill into the central repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrateAgentSkillResult {
    pub skill_id: String,
    pub agent_id: String,
    pub central_path: String,
    pub installed_path: String,
    pub link_type: String,
}

/// Describes a single failed or skipped migration in a batch operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedMigration {
    pub skill_id: String,
    pub error: String,
}

/// Result of migrating all eligible local skills for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchMigrateAgentSkillsResult {
    pub succeeded: Vec<MigrateAgentSkillResult>,
    pub skipped: Vec<FailedMigration>,
    pub failed: Vec<FailedMigration>,
}

// ─── Path Utilities ───────────────────────────────────────────────────────────

/// Compute a relative path from `from_dir` to `to_path`.
///
/// Both paths must be absolute. The resulting path can be used as a symlink
/// target placed inside `from_dir`.
///
/// Examples:
/// - `make_relative_path("/a/b/c", "/a/d/e/f")` -> `"../../d/e/f"`
/// - `make_relative_path("/home/user/.claude/skills", "/home/user/.agents/skills/my-skill")`
///   -> `"../../.agents/skills/my-skill"`
pub fn make_relative_path(from_dir: &Path, to_path: &Path) -> PathBuf {
    let from_components: Vec<_> = from_dir.components().collect();
    let to_components: Vec<_> = to_path.components().collect();

    // Find the length of the common path prefix.
    let common_len = from_components
        .iter()
        .zip(to_components.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Number of ".." hops needed to climb out of `from_dir`.
    let up_count = from_components.len() - common_len;

    let mut result = PathBuf::new();
    for _ in 0..up_count {
        result.push("..");
    }
    for component in &to_components[common_len..] {
        result.push(component.as_os_str());
    }

    if result.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        result
    }
}

// ─── Platform-specific symlink creation ──────────────────────────────────────

#[cfg(unix)]
pub fn create_symlink(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| format!("Failed to create symlink: {}", e))
}

#[cfg(windows)]
pub fn create_symlink(target: &Path, link: &Path) -> Result<(), String> {
    std::os::windows::fs::symlink_dir(target, link)
        .map_err(|e| format!("Failed to create symlink: {}", e))
}

#[cfg(not(any(unix, windows)))]
pub fn create_symlink(_target: &Path, _link: &Path) -> Result<(), String> {
    Err("Symlink creation is only supported on Unix systems".to_string())
}

pub fn symlink_target_path(from_dir: &Path, to_path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let from_prefix = from_dir.components().next();
        let to_prefix = to_path.components().next();
        if from_prefix != to_prefix {
            return to_path.to_path_buf();
        }
    }

    make_relative_path(from_dir, to_path)
}

// ─── Recursive Directory Copy ─────────────────────────────────────────────────

/// Recursively copy a directory tree from `src` to `dst`.
///
/// `dst` must not exist prior to the call (or may be an empty dir).
/// The behaviour mirrors `cp -r src dst` on Unix.
pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| {
        format!(
            "Failed to create destination directory '{}': {}",
            dst.display(),
            e
        )
    })?;

    for entry in std::fs::read_dir(src)
        .map_err(|e| format!("Failed to read source directory '{}': {}", src.display(), e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        let file_type = std::fs::symlink_metadata(&src_path)
            .map_err(|e| format!("Failed to determine file type: {}", e))?
            .file_type();

        if file_type.is_symlink() {
            return Err(format!(
                "Refusing to copy symlink inside skill directory: '{}'",
                src_path.display()
            ));
        }

        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                format!(
                    "Failed to copy '{}' -> '{}': {}",
                    src_path.display(),
                    dst_path.display(),
                    e
                )
            })?;
        }
    }

    Ok(())
}

// ─── Auto-centralize ─────────────────────────────────────────────────────────

/// Ensure the skill exists in the central directory. If it doesn't, copy it
/// from its actual location (looked up in the database) and update the DB
/// record to mark it as central.
///
/// This enables installing platform-specific skills to other platforms:
/// the skill is first adopted into the central directory, then distributed
/// via symlink/copy as usual.
async fn ensure_centralized(
    pool: &DbPool,
    skill_id: &str,
    canonical_dir: &Path,
) -> Result<(), String> {
    if canonical_dir.join("SKILL.md").exists() {
        return Ok(());
    }

    // Look up the skill's actual file location from the database.
    let skill = db::get_skill_by_id(pool, skill_id)
        .await?
        .ok_or_else(|| format!("Skill '{}' not found in database", skill_id))?;

    // Derive the source directory (parent of file_path).
    let source_file = PathBuf::from(&skill.file_path);
    let source_dir = source_file
        .parent()
        .ok_or_else(|| format!("Invalid file_path for skill '{}'", skill_id))?;

    if !source_file.exists() {
        return Err(format!(
            "Skill source not found at '{}'",
            source_file.display()
        ));
    }

    // Copy to central directory.
    copy_dir_all(source_dir, canonical_dir)?;

    // Update the DB record to reflect centralization.
    let mut updated = skill;
    updated.canonical_path = Some(canonical_dir.to_string_lossy().into_owned());
    updated.is_central = true;
    updated.file_path = canonical_dir
        .join("SKILL.md")
        .to_string_lossy()
        .into_owned();
    db::upsert_skill(pool, &updated).await?;

    Ok(())
}

// ─── Core Logic ───────────────────────────────────────────────────────────────

/// Core install logic, separated from the Tauri layer for testability.
///
/// Creates a relative symlink at `agent.global_skills_dir/<skill_id>` that
/// points to the canonical skill directory `central.global_skills_dir/<skill_id>`.
///
/// Returns an error if:
/// - The agent or central agent is not found in the database.
/// - The canonical skill does not exist (no SKILL.md).
/// - A real (non-symlink) directory already exists at the target path.
/// - `agent_id` is "central" (would create a self-referencing symlink).
pub async fn install_skill_to_agent_impl(
    pool: &DbPool,
    skill_id: &str,
    agent_id: &str,
) -> Result<InstallResult, String> {
    ensure_plain_skill_id(skill_id)?;

    // Guard: cannot install to the central agent itself.
    if agent_id == "central" {
        return Err("Cannot install a skill to the central agent itself".to_string());
    }

    // 1. Look up the target agent.
    let agent = db::get_agent_by_id(pool, agent_id)
        .await?
        .ok_or_else(|| format!("Agent '{}' not found", agent_id))?;

    // 2. Look up the central agent to determine the canonical root.
    let central = db::get_agent_by_id(pool, "central")
        .await?
        .ok_or_else(|| "Central agent not found in database".to_string())?;

    let canonical_dir = PathBuf::from(&central.global_skills_dir).join(skill_id);

    // 3. Ensure the skill exists in central (auto-centralize if needed).
    ensure_centralized(pool, skill_id, &canonical_dir).await?;

    // 4. Compute symlink location.
    let agent_dir = PathBuf::from(&agent.global_skills_dir);
    let symlink_path = agent_dir.join(skill_id);

    // 5. Ensure the agent's skills directory exists.
    std::fs::create_dir_all(&agent_dir)
        .map_err(|e| format!("Failed to create agent skills directory: {}", e))?;

    // 6. Handle any existing entry at the symlink path.
    match std::fs::symlink_metadata(&symlink_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Remove stale symlink so we can replace it.
            std::fs::remove_file(&symlink_path)
                .map_err(|e| format!("Failed to remove existing symlink: {}", e))?;
        }
        Ok(meta) if meta.is_dir() => {
            return Err(format!(
                "A real directory already exists at '{}'. Refusing to overwrite.",
                symlink_path.display()
            ));
        }
        Ok(_) => {
            return Err(format!(
                "A file already exists at '{}'. Refusing to overwrite.",
                symlink_path.display()
            ));
        }
        Err(_) => {} // Path does not exist — proceed normally.
    }

    // 7. Compute the relative path from the agent directory to the canonical dir.
    let relative_target = symlink_target_path(&agent_dir, &canonical_dir);

    // 8. Create the symlink.
    create_symlink(&relative_target, &symlink_path)?;

    // 9. Persist the installation record.
    let installation = SkillInstallation {
        skill_id: skill_id.to_string(),
        agent_id: agent_id.to_string(),
        installed_path: symlink_path.to_string_lossy().into_owned(),
        link_type: "symlink".to_string(),
        symlink_target: Some(canonical_dir.to_string_lossy().into_owned()),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    db::upsert_skill_installation(pool, &installation).await?;

    Ok(InstallResult {
        symlink_path: symlink_path.to_string_lossy().into_owned(),
    })
}

pub async fn install_skill_to_agent_auto_impl(
    pool: &DbPool,
    skill_id: &str,
    agent_id: &str,
) -> Result<InstallResult, String> {
    match install_skill_to_agent_impl(pool, skill_id, agent_id).await {
        Ok(result) => Ok(result),
        Err(error) if should_fallback_to_copy(&error) => {
            install_skill_to_agent_copy_impl(pool, skill_id, agent_id).await
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn should_fallback_to_copy(error: &str) -> bool {
    error.contains("Failed to create symlink")
}

#[cfg(not(windows))]
fn should_fallback_to_copy(_error: &str) -> bool {
    false
}

/// Core copy-install logic — copies the skill directory instead of symlinking.
///
/// Copies `central.global_skills_dir/<skill_id>` recursively into
/// `agent.global_skills_dir/<skill_id>`. Existing symlinks at the target are
/// replaced; existing real directories cause an error.
pub async fn install_skill_to_agent_copy_impl(
    pool: &DbPool,
    skill_id: &str,
    agent_id: &str,
) -> Result<InstallResult, String> {
    ensure_plain_skill_id(skill_id)?;

    // Guard: cannot install to the central agent itself.
    if agent_id == "central" {
        return Err("Cannot install a skill to the central agent itself".to_string());
    }

    // 1. Look up the target agent.
    let agent = db::get_agent_by_id(pool, agent_id)
        .await?
        .ok_or_else(|| format!("Agent '{}' not found", agent_id))?;

    // 2. Look up the central agent to determine the canonical root.
    let central = db::get_agent_by_id(pool, "central")
        .await?
        .ok_or_else(|| "Central agent not found in database".to_string())?;

    let canonical_dir = PathBuf::from(&central.global_skills_dir).join(skill_id);

    // 3. Ensure the skill exists in central (auto-centralize if needed).
    ensure_centralized(pool, skill_id, &canonical_dir).await?;

    // 4. Compute target location.
    let agent_dir = PathBuf::from(&agent.global_skills_dir);
    let target_path = agent_dir.join(skill_id);

    // 5. Ensure the agent's skills directory exists.
    std::fs::create_dir_all(&agent_dir)
        .map_err(|e| format!("Failed to create agent skills directory: {}", e))?;

    // 6. Handle any existing entry at the target path.
    match std::fs::symlink_metadata(&target_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Remove stale symlink so we can replace it with a real copy.
            std::fs::remove_file(&target_path)
                .map_err(|e| format!("Failed to remove existing symlink: {}", e))?;
        }
        Ok(meta) if meta.is_dir() => {
            return Err(format!(
                "A real directory already exists at '{}'. Refusing to overwrite.",
                target_path.display()
            ));
        }
        Ok(_) => {
            return Err(format!(
                "A file already exists at '{}'. Refusing to overwrite.",
                target_path.display()
            ));
        }
        Err(_) => {} // Path does not exist — proceed normally.
    }

    // 7. Recursively copy the canonical skill directory.
    copy_dir_all(&canonical_dir, &target_path)?;

    // 8. Persist the installation record.
    let installation = SkillInstallation {
        skill_id: skill_id.to_string(),
        agent_id: agent_id.to_string(),
        installed_path: target_path.to_string_lossy().into_owned(),
        link_type: "copy".to_string(),
        symlink_target: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    db::upsert_skill_installation(pool, &installation).await?;

    Ok(InstallResult {
        symlink_path: target_path.to_string_lossy().into_owned(),
    })
}

fn ensure_plain_skill_id(skill_id: &str) -> Result<(), String> {
    let mut components = Path::new(skill_id).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) if !skill_id.is_empty() => Ok(()),
        _ => Err(format!("Invalid skill id '{}'", skill_id)),
    }
}

fn unique_migration_path(parent: &Path, skill_id: &str, suffix: &str) -> PathBuf {
    parent.join(format!(
        ".{}.{}.{}",
        skill_id,
        suffix,
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn restore_backup(backup_path: &Path, source_dir: &Path) -> Result<(), String> {
    if source_dir.exists() || std::fs::symlink_metadata(source_dir).is_ok() {
        return Err(format!(
            "Cannot restore backup because '{}' already exists; backup kept at '{}'",
            source_dir.display(),
            backup_path.display()
        ));
    }
    std::fs::rename(backup_path, source_dir).map_err(|e| {
        format!(
            "Migration failed and restoring '{}' from backup '{}' also failed: {}",
            source_dir.display(),
            backup_path.display(),
            e
        )
    })
}

fn source_dir_for_migration(
    pool_skills: &[db::SkillForAgent],
    agent_dir: &Path,
    skill_id: &str,
    row_id: Option<&str>,
) -> Result<PathBuf, String> {
    if let Some(row_id) = row_id {
        let skill = pool_skills
            .iter()
            .find(|skill| skill.row_id == row_id)
            .ok_or_else(|| format!("Skill row '{}' not found for migration", row_id))?;
        if skill.id != skill_id {
            return Err(format!(
                "Skill row '{}' belongs to '{}', not '{}'",
                row_id, skill.id, skill_id
            ));
        }
        if skill.is_read_only {
            return Err("Read-only skills cannot be migrated".to_string());
        }
        if skill.is_central {
            return Err("Current central skills cannot be migrated".to_string());
        }
        if !matches!(skill.link_type.as_str(), "copy" | "native" | "symlink") {
            return Err(format!(
                "Only local skills can be migrated; found '{}'",
                skill.link_type
            ));
        }
        return Ok(PathBuf::from(&skill.dir_path));
    }

    let expected_dir = agent_dir.join(skill_id);
    if let Some(skill) = pool_skills
        .iter()
        .find(|skill| skill.id == skill_id && PathBuf::from(&skill.dir_path) == expected_dir)
    {
        if skill.is_read_only {
            return Err("Read-only skills cannot be migrated".to_string());
        }
        if skill.is_central {
            return Err("Current central skills cannot be migrated".to_string());
        }
        if !matches!(skill.link_type.as_str(), "copy" | "native" | "symlink") {
            return Err(format!(
                "Only local skills can be migrated; found '{}'",
                skill.link_type
            ));
        }
        return Ok(PathBuf::from(&skill.dir_path));
    }

    Ok(agent_dir.join(skill_id))
}

fn ensure_source_within_agent_dir(source_dir: &Path, agent_dir: &Path) -> Result<(), String> {
    let canonical_source = source_dir.canonicalize().map_err(|e| {
        format!(
            "Failed to canonicalize source '{}': {}",
            source_dir.display(),
            e
        )
    })?;
    let canonical_agent = agent_dir.canonicalize().map_err(|e| {
        format!(
            "Failed to canonicalize agent directory '{}': {}",
            agent_dir.display(),
            e
        )
    })?;

    if !canonical_source.starts_with(&canonical_agent) {
        return Err(format!(
            "Skill source '{}' is outside agent skills directory '{}'",
            source_dir.display(),
            agent_dir.display()
        ));
    }

    Ok(())
}

fn ensure_entry_within_agent_dir(entry_path: &Path, agent_dir: &Path) -> Result<(), String> {
    let entry_parent = entry_path
        .parent()
        .ok_or_else(|| format!("Invalid skill entry path '{}'", entry_path.display()))?
        .canonicalize()
        .map_err(|e| {
            format!(
                "Failed to canonicalize skill entry parent '{}': {}",
                entry_path.display(),
                e
            )
        })?;
    let canonical_agent = agent_dir.canonicalize().map_err(|e| {
        format!(
            "Failed to canonicalize agent directory '{}': {}",
            agent_dir.display(),
            e
        )
    })?;

    if !entry_parent.starts_with(&canonical_agent) {
        return Err(format!(
            "Skill entry '{}' is outside agent skills directory '{}'",
            entry_path.display(),
            agent_dir.display()
        ));
    }

    Ok(())
}

fn resolve_top_level_symlink_target(link_path: &Path) -> Result<PathBuf, String> {
    let raw_target = std::fs::read_link(link_path)
        .map_err(|e| format!("Failed to read symlink '{}': {}", link_path.display(), e))?;
    let resolved_target = if raw_target.is_absolute() {
        raw_target
    } else {
        link_path
            .parent()
            .ok_or_else(|| format!("Invalid symlink path '{}'", link_path.display()))?
            .join(raw_target)
    };
    let target_meta = std::fs::symlink_metadata(&resolved_target).map_err(|e| {
        format!(
            "Symlink target '{}' for '{}' not found: {}",
            resolved_target.display(),
            link_path.display(),
            e
        )
    })?;

    if target_meta.file_type().is_symlink() || !target_meta.is_dir() {
        return Err(format!(
            "Only symlinks to real skill directories can be migrated; '{}' is not a real directory",
            resolved_target.display()
        ));
    }

    Ok(resolved_target)
}

fn ensure_central_root_outside_source(
    source_dir: &Path,
    central_root: &Path,
) -> Result<(), String> {
    let canonical_source = source_dir.canonicalize().map_err(|e| {
        format!(
            "Failed to canonicalize source '{}': {}",
            source_dir.display(),
            e
        )
    })?;
    let intended_central = canonicalize_existing_parent_with_suffix(central_root)?;

    if intended_central.starts_with(&canonical_source) {
        return Err(format!(
            "Central skills directory '{}' cannot be inside source skill directory '{}'",
            central_root.display(),
            source_dir.display()
        ));
    }

    Ok(())
}

fn canonicalize_existing_parent_with_suffix(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| format!("No existing parent found for '{}'", path.display()))?;
    }

    let mut normalized = existing.canonicalize().map_err(|e| {
        format!(
            "Failed to canonicalize existing parent '{}': {}",
            existing.display(),
            e
        )
    })?;
    let suffix = path.strip_prefix(existing).map_err(|e| {
        format!(
            "Failed to compute path suffix for '{}' from '{}': {}",
            path.display(),
            existing.display(),
            e
        )
    })?;

    for component in suffix.components() {
        match component {
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => {
                return Err(format!(
                    "Unsupported path component in '{}'",
                    path.display()
                ));
            }
        }
    }

    Ok(normalized)
}

async fn persist_migration_records(
    pool: &DbPool,
    skill: &db::Skill,
    installation: &SkillInstallation,
    observation: Option<&db::AgentSkillObservation>,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

    sqlx::query(
        "INSERT INTO skills
         (id, name, description, file_path, canonical_path, is_central, source, content, scanned_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
           name           = excluded.name,
           description    = excluded.description,
           file_path      = excluded.file_path,
           canonical_path = COALESCE(excluded.canonical_path, skills.canonical_path),
           is_central     = MAX(skills.is_central, excluded.is_central),
           source         = excluded.source,
           content        = excluded.content,
           scanned_at     = excluded.scanned_at",
    )
    .bind(&skill.id)
    .bind(&skill.name)
    .bind(&skill.description)
    .bind(&skill.file_path)
    .bind(&skill.canonical_path)
    .bind(skill.is_central)
    .bind(&skill.source)
    .bind(&skill.content)
    .bind(&skill.scanned_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "INSERT INTO skill_installations
         (skill_id, agent_id, installed_path, link_type, symlink_target, created_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(skill_id, agent_id) DO UPDATE SET
           installed_path = excluded.installed_path,
           link_type      = excluded.link_type,
           symlink_target = excluded.symlink_target",
    )
    .bind(&installation.skill_id)
    .bind(&installation.agent_id)
    .bind(&installation.installed_path)
    .bind(&installation.link_type)
    .bind(&installation.symlink_target)
    .bind(&installation.created_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    if let Some(observation) = observation {
        sqlx::query(
            "INSERT INTO agent_skill_observations
             (row_id, agent_id, skill_id, name, description, file_path, dir_path,
              source_kind, source_root, link_type, symlink_target, is_read_only, scanned_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(row_id) DO UPDATE SET
               agent_id       = excluded.agent_id,
               skill_id       = excluded.skill_id,
               name           = excluded.name,
               description    = excluded.description,
               file_path      = excluded.file_path,
               dir_path       = excluded.dir_path,
               source_kind    = excluded.source_kind,
               source_root    = excluded.source_root,
               link_type      = excluded.link_type,
               symlink_target = excluded.symlink_target,
               is_read_only   = excluded.is_read_only,
               scanned_at     = excluded.scanned_at",
        )
        .bind(&observation.row_id)
        .bind(&observation.agent_id)
        .bind(&observation.skill_id)
        .bind(&observation.name)
        .bind(&observation.description)
        .bind(&observation.file_path)
        .bind(&observation.dir_path)
        .bind(&observation.source_kind)
        .bind(&observation.source_root)
        .bind(&observation.link_type)
        .bind(&observation.symlink_target)
        .bind(observation.is_read_only)
        .bind(&observation.scanned_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }

    tx.commit().await.map_err(|e| e.to_string())
}

fn rollback_filesystem_migration(
    source_dir: &Path,
    backup_source_dir: &Path,
    central_dir: &Path,
) -> Result<(), String> {
    match std::fs::symlink_metadata(source_dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::remove_file(source_dir)
                .map_err(|e| format!("Failed to remove migrated symlink during rollback: {}", e))?;
        }
        Ok(_) => {
            return Err(format!(
                "Cannot rollback migration because '{}' is no longer the expected symlink",
                source_dir.display()
            ));
        }
        Err(_) => {}
    }

    restore_backup(backup_source_dir, source_dir)?;
    if let Err(error) = std::fs::remove_dir_all(central_dir) {
        return Err(format!(
            "Restored source but failed to remove central copy '{}': {}",
            central_dir.display(),
            error
        ));
    }
    Ok(())
}

fn cleanup_backup_source(backup_source_dir: &Path) {
    match std::fs::symlink_metadata(backup_source_dir) {
        Ok(meta) if meta.file_type().is_symlink() || meta.is_file() => {
            let _ = std::fs::remove_file(backup_source_dir);
        }
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(backup_source_dir);
        }
        _ => {}
    }
}

/// Migrate one agent-local skill directory into central storage and replace the
/// agent entry with a symlink back to the new central copy.
pub async fn migrate_agent_skill_to_central_impl(
    pool: &DbPool,
    agent_id: &str,
    skill_id: &str,
    row_id: Option<&str>,
) -> Result<MigrateAgentSkillResult, String> {
    if agent_id == "central" {
        return Err("Cannot migrate skills from the central agent".to_string());
    }
    ensure_plain_skill_id(skill_id)?;

    let agent = db::get_agent_by_id(pool, agent_id)
        .await?
        .ok_or_else(|| format!("Agent '{}' not found", agent_id))?;
    let central = db::get_agent_by_id(pool, "central")
        .await?
        .ok_or_else(|| "Central agent not found in database".to_string())?;

    let agent_dir = PathBuf::from(&agent.global_skills_dir);
    let central_root = PathBuf::from(&central.global_skills_dir);
    let central_dir = central_root.join(skill_id);
    if std::fs::symlink_metadata(&central_dir).is_ok() {
        return Err(format!(
            "Central skill '{}' already exists at '{}'",
            skill_id,
            central_dir.display()
        ));
    }

    let agent_skills = db::get_skills_for_agent(pool, agent_id).await?;
    let source_dir = source_dir_for_migration(&agent_skills, &agent_dir, skill_id, row_id)?;
    let source_meta = std::fs::symlink_metadata(&source_dir)
        .map_err(|e| format!("Skill source '{}' not found: {}", source_dir.display(), e))?;
    let copy_source_dir = if source_meta.file_type().is_symlink() {
        ensure_entry_within_agent_dir(&source_dir, &agent_dir)?;
        resolve_top_level_symlink_target(&source_dir)?
    } else if source_meta.is_dir() {
        ensure_source_within_agent_dir(&source_dir, &agent_dir)?;
        source_dir.clone()
    } else {
        return Err(format!(
            "Only local directory or external symlink skills can be migrated; '{}' is not a directory or symlink",
            source_dir.display()
        ));
    };

    let source_skill_md = copy_source_dir.join("SKILL.md");
    let skill_info = parse_skill_md(&source_skill_md).ok_or_else(|| {
        format!(
            "Skill source '{}' does not contain a valid SKILL.md",
            copy_source_dir.display()
        )
    })?;

    ensure_central_root_outside_source(&copy_source_dir, &central_root)?;
    std::fs::create_dir_all(&central_root)
        .map_err(|e| format!("Failed to create central skills directory: {}", e))?;
    let temp_central_dir = unique_migration_path(&central_root, skill_id, "tmp");
    let backup_source_dir = unique_migration_path(
        source_dir
            .parent()
            .ok_or_else(|| format!("Invalid source directory '{}'", source_dir.display()))?,
        skill_id,
        "backup",
    );

    if let Err(error) = copy_dir_all(&copy_source_dir, &temp_central_dir) {
        let _ = std::fs::remove_dir_all(&temp_central_dir);
        return Err(error);
    }
    if parse_skill_md(&temp_central_dir.join("SKILL.md")).is_none() {
        let _ = std::fs::remove_dir_all(&temp_central_dir);
        return Err("Copied skill does not contain a valid SKILL.md".to_string());
    }

    std::fs::rename(&temp_central_dir, &central_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&temp_central_dir);
        format!(
            "Failed to move migrated skill into central directory '{}': {}",
            central_dir.display(),
            e
        )
    })?;

    std::fs::rename(&source_dir, &backup_source_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&central_dir);
        format!(
            "Failed to backup source skill directory '{}': {}",
            source_dir.display(),
            e
        )
    })?;

    let symlink_target = symlink_target_path(&agent_dir, &central_dir);
    if std::fs::symlink_metadata(&source_dir).is_ok() {
        let restore_result = restore_backup(&backup_source_dir, &source_dir);
        let _ = std::fs::remove_dir_all(&central_dir);
        return match restore_result {
            Ok(()) => Err(format!(
                "Cannot create symlink because '{}' already exists",
                source_dir.display()
            )),
            Err(restore_error) => Err(restore_error),
        };
    }
    if let Err(error) = create_symlink(&symlink_target, &source_dir) {
        let restore_result = restore_backup(&backup_source_dir, &source_dir);
        let _ = std::fs::remove_dir_all(&central_dir);
        return match restore_result {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!("{}; {}", error, restore_error)),
        };
    }

    let now = chrono::Utc::now().to_rfc3339();
    let skill = db::Skill {
        id: skill_id.to_string(),
        name: skill_info.name,
        description: skill_info.description,
        file_path: central_dir.join("SKILL.md").to_string_lossy().into_owned(),
        canonical_path: Some(central_dir.to_string_lossy().into_owned()),
        is_central: true,
        source: Some("native".to_string()),
        content: None,
        scanned_at: now.clone(),
    };

    let installation = SkillInstallation {
        skill_id: skill_id.to_string(),
        agent_id: agent_id.to_string(),
        installed_path: source_dir.to_string_lossy().into_owned(),
        link_type: "symlink".to_string(),
        symlink_target: Some(central_dir.to_string_lossy().into_owned()),
        created_at: now,
    };
    let migrated_observation = agent_skills
        .iter()
        .find(|observed| PathBuf::from(&observed.dir_path) == source_dir && !observed.is_read_only)
        .and_then(|observed| {
            Some(db::AgentSkillObservation {
                row_id: observed.row_id.clone(),
                agent_id: agent_id.to_string(),
                skill_id: skill_id.to_string(),
                name: skill.name.clone(),
                description: skill.description.clone(),
                file_path: source_dir.join("SKILL.md").to_string_lossy().into_owned(),
                dir_path: source_dir.to_string_lossy().into_owned(),
                source_kind: observed.source_kind.clone()?,
                source_root: observed.source_root.clone()?,
                link_type: "symlink".to_string(),
                symlink_target: Some(central_dir.to_string_lossy().into_owned()),
                is_read_only: false,
                scanned_at: skill.scanned_at.clone(),
            })
        });
    if let Err(error) =
        persist_migration_records(pool, &skill, &installation, migrated_observation.as_ref()).await
    {
        let rollback_result =
            rollback_filesystem_migration(&source_dir, &backup_source_dir, &central_dir);
        return match rollback_result {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(format!("{}; {}", error, rollback_error)),
        };
    }

    cleanup_backup_source(&backup_source_dir);

    Ok(MigrateAgentSkillResult {
        skill_id: skill_id.to_string(),
        agent_id: agent_id.to_string(),
        central_path: central_dir.to_string_lossy().into_owned(),
        installed_path: source_dir.to_string_lossy().into_owned(),
        link_type: "symlink".to_string(),
    })
}

pub async fn batch_migrate_agent_skills_to_central_impl(
    pool: &DbPool,
    agent_id: &str,
) -> Result<BatchMigrateAgentSkillsResult, String> {
    if agent_id == "central" {
        return Err("Cannot migrate skills from the central agent".to_string());
    }
    let agent = db::get_agent_by_id(pool, agent_id)
        .await?
        .ok_or_else(|| format!("Agent '{}' not found", agent_id))?;
    let agent_dir = PathBuf::from(&agent.global_skills_dir);

    let mut candidates = Vec::new();
    let entries = std::fs::read_dir(&agent_dir).map_err(|e| {
        format!(
            "Failed to read agent skills directory '{}': {}",
            agent_dir.display(),
            e
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let skill_id = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        candidates.push((skill_id, path));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let mut result = BatchMigrateAgentSkillsResult {
        succeeded: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };

    for (skill_id, path) in candidates {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.file_type().is_symlink() && !meta.is_dir() => {
                result.skipped.push(FailedMigration {
                    skill_id,
                    error: "Only local directory or external symlink skills can be migrated"
                        .to_string(),
                });
            }
            Ok(_) => {
                match migrate_agent_skill_to_central_impl(pool, agent_id, &skill_id, None).await {
                    Ok(migrated) => result.succeeded.push(migrated),
                    Err(error) if error.contains("already exists") => {
                        result.skipped.push(FailedMigration { skill_id, error });
                    }
                    Err(error) => result.failed.push(FailedMigration { skill_id, error }),
                }
            }
            Err(error) => result.skipped.push(FailedMigration {
                skill_id,
                error: error.to_string(),
            }),
        }
    }

    Ok(result)
}

/// Core uninstall logic, separated from the Tauri layer for testability.
///
/// Removes the symlink at `agent.global_skills_dir/<skill_id>` and deletes the
/// corresponding `skill_installations` record.
///
/// For symlinked skills: removes the symlink.
/// For copied skills: removes the copied directory (tracked in the DB as link_type='copy').
/// Refuses to delete real directories not tracked as copies in the DB.
pub async fn uninstall_skill_from_agent_impl(
    pool: &DbPool,
    skill_id: &str,
    agent_id: &str,
) -> Result<(), String> {
    ensure_plain_skill_id(skill_id)?;

    // 1. Look up the agent.
    let agent = db::get_agent_by_id(pool, agent_id)
        .await?
        .ok_or_else(|| format!("Agent '{}' not found", agent_id))?;

    // 2. Compute the expected install location.
    let install_path = PathBuf::from(&agent.global_skills_dir).join(skill_id);

    // 3. Look up the installation record to determine how it was installed.
    let installations = db::get_skill_installations(pool, skill_id).await?;
    let record = installations.iter().find(|r| r.agent_id == agent_id);
    let link_type = record.map(|r| r.link_type.as_str()).unwrap_or("symlink");

    // 4. Inspect the entry at that path and remove it appropriately.
    match std::fs::symlink_metadata(&install_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Always safe to remove symlinks.
            std::fs::remove_file(&install_path)
                .map_err(|e| format!("Failed to remove symlink: {}", e))?;
        }
        Ok(meta) if meta.is_dir() => {
            // Only remove real directories that were explicitly installed as copies.
            if link_type == "copy" {
                std::fs::remove_dir_all(&install_path)
                    .map_err(|e| format!("Failed to remove copied skill directory: {}", e))?;
            } else {
                return Err(format!(
                    "Path '{}' exists but is not a symlink. Refusing to delete.",
                    install_path.display()
                ));
            }
        }
        Ok(_) => {
            return Err(format!(
                "Path '{}' exists but is not a symlink. Refusing to delete.",
                install_path.display()
            ));
        }
        Err(_) => {
            // Path doesn't exist — still clean up the DB record.
        }
    }

    // 5. Remove the installation record from the database.
    db::delete_skill_installation(pool, skill_id, agent_id).await?;

    Ok(())
}

// ─── Tauri Commands ───────────────────────────────────────────────────────────

/// Tauri command: install a skill to a single agent via relative symlink.
#[tauri::command]
pub async fn install_skill_to_agent(
    state: State<'_, AppState>,
    skill_id: String,
    agent_id: String,
    method: Option<String>,
) -> Result<InstallResult, String> {
    match method.as_deref().unwrap_or("auto") {
        "copy" => install_skill_to_agent_copy_impl(&state.db, &skill_id, &agent_id).await,
        "symlink" => install_skill_to_agent_impl(&state.db, &skill_id, &agent_id).await,
        _ => install_skill_to_agent_auto_impl(&state.db, &skill_id, &agent_id).await,
    }
}

/// Tauri command: remove a skill's symlink from an agent.
#[tauri::command]
pub async fn uninstall_skill_from_agent(
    state: State<'_, AppState>,
    skill_id: String,
    agent_id: String,
) -> Result<(), String> {
    uninstall_skill_from_agent_impl(&state.db, &skill_id, &agent_id).await
}

/// Tauri command: install a skill to multiple agents in one call.
///
/// `method` must be either `"symlink"` (default, creates a relative symlink) or
/// `"copy"` (copies the skill directory). Each agent install is attempted
/// independently; failures are collected in the `failed` list rather than
/// short-circuiting the entire batch.
#[tauri::command]
pub async fn batch_install_to_agents(
    state: State<'_, AppState>,
    skill_id: String,
    agent_ids: Vec<String>,
    method: Option<String>,
) -> Result<BatchInstallResult, String> {
    let method = method.as_deref().unwrap_or("auto");
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();

    for agent_id in &agent_ids {
        let install_result = match method {
            "copy" => install_skill_to_agent_copy_impl(&state.db, &skill_id, agent_id).await,
            "symlink" => install_skill_to_agent_impl(&state.db, &skill_id, agent_id).await,
            _ => install_skill_to_agent_auto_impl(&state.db, &skill_id, agent_id).await,
        };
        match install_result {
            Ok(_) => succeeded.push(agent_id.clone()),
            Err(e) => failed.push(FailedInstall {
                agent_id: agent_id.clone(),
                error: e,
            }),
        }
    }

    Ok(BatchInstallResult { succeeded, failed })
}

#[tauri::command]
pub async fn migrate_agent_skill_to_central(
    state: State<'_, AppState>,
    agent_id: String,
    skill_id: String,
    row_id: Option<String>,
) -> Result<MigrateAgentSkillResult, String> {
    migrate_agent_skill_to_central_impl(&state.db, &agent_id, &skill_id, row_id.as_deref()).await
}

#[tauri::command]
pub async fn batch_migrate_agent_skills_to_central(
    state: State<'_, AppState>,
    agent_id: String,
) -> Result<BatchMigrateAgentSkillsResult, String> {
    batch_migrate_agent_skills_to_central_impl(&state.db, &agent_id).await
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use sqlx::SqlitePool;
    use std::fs;
    use tempfile::TempDir;

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Create an in-memory SQLite pool with the full schema initialised and
    /// the central/claude-code agent directories redirected to `central_dir`
    /// and `agent_dir` respectively.
    async fn setup_db(central_dir: &Path, agent_dir: &Path) -> DbPool {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();

        sqlx::query("UPDATE agents SET global_skills_dir = ? WHERE id = 'central'")
            .bind(central_dir.to_str().unwrap())
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query("UPDATE agents SET global_skills_dir = ? WHERE id = 'claude-code'")
            .bind(agent_dir.to_str().unwrap())
            .execute(&pool)
            .await
            .unwrap();

        pool
    }

    /// Create a minimal skill directory containing a valid `SKILL.md`.
    fn create_central_skill(central_dir: &Path, skill_id: &str) -> PathBuf {
        let skill_dir = central_dir.join(skill_id);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {}\ndescription: Test skill\n---\n\n# {}\n",
                skill_id, skill_id
            ),
        )
        .unwrap();
        skill_dir
    }

    fn create_agent_skill(agent_dir: &Path, skill_id: &str) -> PathBuf {
        let skill_dir = agent_dir.join(skill_id);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {}\ndescription: Agent local skill\n---\n\n# {}\n",
                skill_id, skill_id
            ),
        )
        .unwrap();
        fs::write(skill_dir.join("notes.txt"), "local notes").unwrap();
        skill_dir
    }

    // ── make_relative_path ────────────────────────────────────────────────────

    #[test]
    fn test_make_relative_path_sibling_dirs() {
        let from = Path::new("/home/user/claude/skills");
        let to = Path::new("/home/user/.agents/skills/my-skill");
        let rel = make_relative_path(from, to);
        assert_eq!(rel, PathBuf::from("../../.agents/skills/my-skill"));
    }

    #[test]
    fn test_make_relative_path_same_parent() {
        let from = Path::new("/tmp/test/agent");
        let to = Path::new("/tmp/test/central/skill-x");
        let rel = make_relative_path(from, to);
        assert_eq!(rel, PathBuf::from("../central/skill-x"));
    }

    #[test]
    fn test_make_relative_path_deep_nesting() {
        let from = Path::new("/a/b/c/d");
        let to = Path::new("/a/x/y");
        let rel = make_relative_path(from, to);
        assert_eq!(rel, PathBuf::from("../../../x/y"));
    }

    // ── install_skill_to_agent_impl ───────────────────────────────────────────

    #[tokio::test]
    async fn test_install_creates_symlink() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;

        create_central_skill(&central_dir, "my-skill");

        let result = install_skill_to_agent_impl(&pool, "my-skill", "claude-code").await;
        assert!(result.is_ok(), "install should succeed: {:?}", result);

        let symlink_path = agent_dir.join("my-skill");
        let meta = fs::symlink_metadata(&symlink_path).unwrap();
        assert!(meta.file_type().is_symlink(), "entry should be a symlink");
    }

    #[tokio::test]
    async fn test_install_symlink_is_relative() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "rel-skill");

        install_skill_to_agent_impl(&pool, "rel-skill", "claude-code")
            .await
            .unwrap();

        let symlink_path = agent_dir.join("rel-skill");
        let link_target = fs::read_link(&symlink_path).unwrap();
        assert!(
            link_target.is_relative(),
            "symlink target should be relative, got {:?}",
            link_target
        );
    }

    #[tokio::test]
    async fn test_install_symlink_resolves_correctly() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "resolve-skill");

        install_skill_to_agent_impl(&pool, "resolve-skill", "claude-code")
            .await
            .unwrap();

        let symlink_path = agent_dir.join("resolve-skill");
        // Following the symlink should give access to SKILL.md in the central dir.
        let skill_md = symlink_path.join("SKILL.md");
        assert!(
            skill_md.exists(),
            "SKILL.md should be accessible via symlink"
        );
    }

    #[tokio::test]
    async fn test_install_creates_agent_dir_if_missing() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        // Do NOT pre-create agent_dir — install should create it.
        let agent_dir = tmp.path().join("new-agent-dir");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "dir-skill");

        let result = install_skill_to_agent_impl(&pool, "dir-skill", "claude-code").await;
        assert!(result.is_ok(), "install should create missing agent dir");
        assert!(agent_dir.exists(), "agent dir should have been created");
    }

    #[tokio::test]
    async fn test_install_updates_db_record() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "db-skill");

        install_skill_to_agent_impl(&pool, "db-skill", "claude-code")
            .await
            .unwrap();

        let installations = db::get_skill_installations(&pool, "db-skill")
            .await
            .unwrap();
        assert_eq!(installations.len(), 1);
        assert_eq!(installations[0].agent_id, "claude-code");
        assert_eq!(installations[0].link_type, "symlink");
    }

    #[tokio::test]
    async fn test_install_fails_when_canonical_missing() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        // Do NOT create the skill in central_dir.

        let result = install_skill_to_agent_impl(&pool, "nonexistent-skill", "claude-code").await;
        assert!(
            result.is_err(),
            "install should fail if canonical skill missing"
        );
    }

    #[tokio::test]
    async fn test_install_fails_for_unknown_agent() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "some-skill");

        let result = install_skill_to_agent_impl(&pool, "some-skill", "nonexistent-agent").await;
        assert!(result.is_err(), "install should fail for unknown agent");
    }

    #[tokio::test]
    async fn test_install_to_central_agent_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &tmp.path().join("claude")).await;
        create_central_skill(&central_dir, "self-skill");

        let result = install_skill_to_agent_impl(&pool, "self-skill", "central").await;
        assert!(
            result.is_err(),
            "installing to 'central' should be rejected"
        );
    }

    #[tokio::test]
    async fn test_install_rejects_path_traversal_skill_id() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        let pool = setup_db(&central_dir, &agent_dir).await;

        let result = install_skill_to_agent_impl(&pool, "../escape", "claude-code").await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid skill id"));
    }

    #[tokio::test]
    async fn test_install_replaces_existing_symlink() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "re-link-skill");

        // Install once.
        install_skill_to_agent_impl(&pool, "re-link-skill", "claude-code")
            .await
            .unwrap();

        // Install again — should replace the existing symlink without error.
        let result = install_skill_to_agent_impl(&pool, "re-link-skill", "claude-code").await;
        assert!(result.is_ok(), "re-install should succeed: {:?}", result);
    }

    #[tokio::test]
    async fn test_install_refuses_to_overwrite_real_dir() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "real-dir-skill");

        // Create a real (non-symlink) directory at the install location.
        fs::create_dir_all(agent_dir.join("real-dir-skill")).unwrap();

        let result = install_skill_to_agent_impl(&pool, "real-dir-skill", "claude-code").await;
        assert!(
            result.is_err(),
            "install should refuse to overwrite a real directory"
        );
    }

    // ── uninstall_skill_from_agent_impl ───────────────────────────────────────

    #[tokio::test]
    async fn test_uninstall_removes_symlink() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "uninstall-skill");

        install_skill_to_agent_impl(&pool, "uninstall-skill", "claude-code")
            .await
            .unwrap();

        let symlink_path = agent_dir.join("uninstall-skill");
        assert!(symlink_path.exists() || fs::symlink_metadata(&symlink_path).is_ok());

        uninstall_skill_from_agent_impl(&pool, "uninstall-skill", "claude-code")
            .await
            .unwrap();

        assert!(
            fs::symlink_metadata(&symlink_path).is_err(),
            "symlink should have been removed"
        );
    }

    #[tokio::test]
    async fn test_uninstall_removes_db_record() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "db-uninstall-skill");

        install_skill_to_agent_impl(&pool, "db-uninstall-skill", "claude-code")
            .await
            .unwrap();

        uninstall_skill_from_agent_impl(&pool, "db-uninstall-skill", "claude-code")
            .await
            .unwrap();

        let installations = db::get_skill_installations(&pool, "db-uninstall-skill")
            .await
            .unwrap();
        assert!(installations.is_empty(), "DB record should be removed");
    }

    #[tokio::test]
    async fn test_uninstall_refuses_real_dir() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;

        // Place a real directory where the symlink would be.
        fs::create_dir_all(agent_dir.join("protected-skill")).unwrap();

        let result = uninstall_skill_from_agent_impl(&pool, "protected-skill", "claude-code").await;
        assert!(
            result.is_err(),
            "uninstall should refuse to delete a real directory"
        );

        // Ensure the directory still exists.
        assert!(
            agent_dir.join("protected-skill").is_dir(),
            "real directory should NOT have been deleted"
        );
    }

    #[tokio::test]
    async fn test_uninstall_rejects_path_traversal_skill_id() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        let pool = setup_db(&central_dir, &agent_dir).await;

        let result = uninstall_skill_from_agent_impl(&pool, "../escape", "claude-code").await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid skill id"));
    }

    #[tokio::test]
    async fn test_uninstall_nonexistent_path_still_cleans_db() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;

        // Manually insert an installation record without creating the symlink.
        let installation = SkillInstallation {
            skill_id: "ghost-skill".to_string(),
            agent_id: "claude-code".to_string(),
            installed_path: agent_dir.join("ghost-skill").to_string_lossy().into_owned(),
            link_type: "symlink".to_string(),
            symlink_target: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        db::upsert_skill_installation(&pool, &installation)
            .await
            .unwrap();

        let result = uninstall_skill_from_agent_impl(&pool, "ghost-skill", "claude-code").await;
        assert!(result.is_ok(), "uninstall of missing path should succeed");

        let installations = db::get_skill_installations(&pool, "ghost-skill")
            .await
            .unwrap();
        assert!(installations.is_empty(), "DB record should be cleaned up");
    }

    // ── batch install ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_batch_install_multiple_agents() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let claude_dir = tmp.path().join("claude");
        let cursor_dir = tmp.path().join("cursor");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &claude_dir).await;

        // Override cursor's dir too.
        sqlx::query("UPDATE agents SET global_skills_dir = ? WHERE id = 'cursor'")
            .bind(cursor_dir.to_str().unwrap())
            .execute(&pool)
            .await
            .unwrap();

        create_central_skill(&central_dir, "batch-skill");

        let result = batch_install_impl(
            &pool,
            "batch-skill",
            &["claude-code".to_string(), "cursor".to_string()],
        )
        .await;

        assert_eq!(result.succeeded.len(), 2);
        assert!(result.failed.is_empty());

        assert!(fs::symlink_metadata(claude_dir.join("batch-skill")).is_ok());
        assert!(fs::symlink_metadata(cursor_dir.join("batch-skill")).is_ok());
    }

    #[tokio::test]
    async fn test_batch_install_partial_failure() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let claude_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &claude_dir).await;
        create_central_skill(&central_dir, "partial-skill");

        let result = batch_install_impl(
            &pool,
            "partial-skill",
            &[
                "claude-code".to_string(),
                "nonexistent-agent".to_string(), // will fail
            ],
        )
        .await;

        assert_eq!(result.succeeded.len(), 1);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].agent_id, "nonexistent-agent");
    }

    /// Helper that mirrors `batch_install_to_agents` but works with a raw pool
    /// (no Tauri State).
    async fn batch_install_impl(
        pool: &DbPool,
        skill_id: &str,
        agent_ids: &[String],
    ) -> BatchInstallResult {
        let mut succeeded = Vec::new();
        let mut failed = Vec::new();

        for agent_id in agent_ids {
            match install_skill_to_agent_impl(pool, skill_id, agent_id).await {
                Ok(_) => succeeded.push(agent_id.clone()),
                Err(e) => failed.push(FailedInstall {
                    agent_id: agent_id.clone(),
                    error: e,
                }),
            }
        }

        BatchInstallResult { succeeded, failed }
    }

    // ── install_skill_to_agent_copy_impl ──────────────────────────────────────

    #[tokio::test]
    async fn test_copy_install_creates_real_directory() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "copy-skill");

        let result = install_skill_to_agent_copy_impl(&pool, "copy-skill", "claude-code").await;
        assert!(result.is_ok(), "copy install should succeed: {:?}", result);

        let target = agent_dir.join("copy-skill");
        let meta = fs::symlink_metadata(&target).unwrap();
        // Must be a real directory — NOT a symlink.
        assert!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "installed path should be a real directory, not a symlink"
        );
    }

    #[tokio::test]
    async fn test_copy_install_files_are_copied() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;

        // Create skill with multiple files to verify all are copied.
        let skill_dir = central_dir.join("multi-file-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: multi-file-skill\ndescription: Test\n---\n",
        )
        .unwrap();
        fs::write(skill_dir.join("extra.txt"), "extra content").unwrap();

        install_skill_to_agent_copy_impl(&pool, "multi-file-skill", "claude-code")
            .await
            .unwrap();

        let installed_skill_dir = agent_dir.join("multi-file-skill");

        // Verify SKILL.md was copied.
        let skill_md = installed_skill_dir.join("SKILL.md");
        assert!(skill_md.exists(), "SKILL.md should be copied to agent dir");

        // Verify extra file was copied.
        let extra = installed_skill_dir.join("extra.txt");
        assert!(extra.exists(), "extra.txt should be copied to agent dir");
        assert_eq!(
            fs::read_to_string(&extra).unwrap(),
            "extra content",
            "copied file contents should match"
        );

        // Confirm that the installed path is NOT a symlink.
        let meta = fs::symlink_metadata(&installed_skill_dir).unwrap();
        assert!(
            !meta.file_type().is_symlink(),
            "installed directory must NOT be a symlink"
        );
    }

    #[tokio::test]
    async fn test_copy_install_updates_db_with_copy_type() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "db-copy-skill");

        install_skill_to_agent_copy_impl(&pool, "db-copy-skill", "claude-code")
            .await
            .unwrap();

        let installations = db::get_skill_installations(&pool, "db-copy-skill")
            .await
            .unwrap();
        assert_eq!(installations.len(), 1);
        assert_eq!(installations[0].agent_id, "claude-code");
        assert_eq!(
            installations[0].link_type, "copy",
            "DB should record link_type as 'copy'"
        );
    }

    #[tokio::test]
    async fn test_copy_install_to_central_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &tmp.path().join("claude")).await;
        create_central_skill(&central_dir, "self-copy-skill");

        let result = install_skill_to_agent_copy_impl(&pool, "self-copy-skill", "central").await;
        assert!(
            result.is_err(),
            "copy install to 'central' should be rejected"
        );
    }

    #[tokio::test]
    async fn test_copy_install_rejects_path_traversal_skill_id() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        let pool = setup_db(&central_dir, &agent_dir).await;

        let result = install_skill_to_agent_copy_impl(&pool, "../escape", "claude-code").await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid skill id"));
    }

    #[tokio::test]
    async fn test_copy_install_fails_when_canonical_missing() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        // Deliberately do NOT create the skill in central_dir.

        let result = install_skill_to_agent_copy_impl(&pool, "missing-skill", "claude-code").await;
        assert!(
            result.is_err(),
            "copy install should fail when canonical skill is missing"
        );
    }

    #[tokio::test]
    async fn test_copy_install_refuses_to_overwrite_real_dir() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "existing-dir-skill");

        // Create a real directory at the target location.
        fs::create_dir_all(agent_dir.join("existing-dir-skill")).unwrap();

        let result =
            install_skill_to_agent_copy_impl(&pool, "existing-dir-skill", "claude-code").await;
        assert!(
            result.is_err(),
            "copy install should refuse to overwrite an existing real directory"
        );
    }

    // ── migrate_agent_skill_to_central_impl ───────────────────────────────────

    #[tokio::test]
    async fn test_migrate_agent_skill_to_central_moves_local_copy_to_central_and_links_back() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_agent_skill(&agent_dir, "local-skill");

        let result = migrate_agent_skill_to_central_impl(&pool, "claude-code", "local-skill", None)
            .await
            .unwrap();

        assert_eq!(result.skill_id, "local-skill");
        assert_eq!(result.agent_id, "claude-code");
        assert_eq!(result.link_type, "symlink");

        let central_skill_dir = central_dir.join("local-skill");
        assert!(central_skill_dir.join("SKILL.md").exists());
        assert_eq!(
            fs::read_to_string(central_skill_dir.join("notes.txt")).unwrap(),
            "local notes"
        );

        let agent_skill_dir = agent_dir.join("local-skill");
        let meta = fs::symlink_metadata(&agent_skill_dir).unwrap();
        assert!(meta.file_type().is_symlink());
        assert!(agent_skill_dir.join("SKILL.md").exists());

        let skill = db::get_skill_by_id(&pool, "local-skill")
            .await
            .unwrap()
            .unwrap();
        assert!(skill.is_central);
        assert_eq!(
            skill.canonical_path.as_deref(),
            Some(central_skill_dir.to_str().unwrap())
        );

        let installations = db::get_skill_installations(&pool, "local-skill")
            .await
            .unwrap();
        let installation = installations
            .iter()
            .find(|record| record.agent_id == "claude-code")
            .unwrap();
        assert_eq!(installation.link_type, "symlink");
        assert_eq!(
            installation.symlink_target.as_deref(),
            Some(central_skill_dir.to_str().unwrap())
        );
    }

    #[tokio::test]
    async fn test_migrate_agent_skill_to_central_refuses_existing_central_skill() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "conflict-skill");
        create_agent_skill(&agent_dir, "conflict-skill");

        let result =
            migrate_agent_skill_to_central_impl(&pool, "claude-code", "conflict-skill", None).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
        let meta = fs::symlink_metadata(agent_dir.join("conflict-skill")).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
    }

    #[tokio::test]
    async fn test_migrate_agent_skill_to_central_moves_external_symlink_target_to_central() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        let target_dir = create_agent_skill(&tmp.path().join("other"), "external-linked");
        fs::write(target_dir.join("from-target.txt"), "external notes").unwrap();
        let agent_link = agent_dir.join("external-linked");
        create_symlink(&target_dir, &agent_link).unwrap();
        db::upsert_agent_skill_observation(
            &pool,
            &db::AgentSkillObservation {
                row_id: "claude-code::user::external-linked".to_string(),
                agent_id: "claude-code".to_string(),
                skill_id: "external-linked".to_string(),
                name: "external-linked".to_string(),
                description: None,
                file_path: agent_link.join("SKILL.md").to_string_lossy().into_owned(),
                dir_path: agent_link.to_string_lossy().into_owned(),
                source_kind: "user".to_string(),
                source_root: agent_dir.to_string_lossy().into_owned(),
                link_type: "symlink".to_string(),
                symlink_target: Some(target_dir.to_string_lossy().into_owned()),
                is_read_only: false,
                scanned_at: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await
        .unwrap();

        let result = migrate_agent_skill_to_central_impl(
            &pool,
            "claude-code",
            "external-linked",
            Some("claude-code::user::external-linked"),
        )
        .await
        .unwrap();

        assert_eq!(result.skill_id, "external-linked");
        let central_skill_dir = central_dir.join("external-linked");
        assert!(central_skill_dir.join("SKILL.md").exists());
        assert_eq!(
            fs::read_to_string(central_skill_dir.join("from-target.txt")).unwrap(),
            "external notes"
        );
        let meta = fs::symlink_metadata(&agent_link).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(agent_link.canonicalize().unwrap(), central_skill_dir.canonicalize().unwrap());
        assert!(target_dir.join("SKILL.md").exists());

        let agent_skills = db::get_skills_for_agent(&pool, "claude-code").await.unwrap();
        let migrated_row = agent_skills
            .iter()
            .find(|skill| skill.id == "external-linked")
            .unwrap();
        assert!(migrated_row.is_central);
    }

    #[tokio::test]
    async fn test_migrate_agent_skill_to_central_rejects_mismatched_row_id() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        let actual_dir = create_agent_skill(&agent_dir, "actual-skill");
        db::upsert_agent_skill_observation(
            &pool,
            &db::AgentSkillObservation {
                row_id: "row-actual".to_string(),
                agent_id: "claude-code".to_string(),
                skill_id: "actual-skill".to_string(),
                name: "actual-skill".to_string(),
                description: None,
                file_path: actual_dir.join("SKILL.md").to_string_lossy().into_owned(),
                dir_path: actual_dir.to_string_lossy().into_owned(),
                source_kind: "user".to_string(),
                source_root: agent_dir.to_string_lossy().into_owned(),
                link_type: "copy".to_string(),
                symlink_target: None,
                is_read_only: false,
                scanned_at: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await
        .unwrap();

        let result = migrate_agent_skill_to_central_impl(
            &pool,
            "claude-code",
            "other-skill",
            Some("row-actual"),
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not 'other-skill'"));
        assert!(actual_dir.is_dir());
        assert!(!central_dir.join("other-skill").exists());
    }

    #[tokio::test]
    async fn test_migrate_agent_skill_to_central_rejects_internal_symlink() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        let skill_dir = create_agent_skill(&agent_dir, "leaky-skill");
        let secret_file = tmp.path().join("secret.txt");
        fs::write(&secret_file, "secret").unwrap();
        create_symlink(&secret_file, &skill_dir.join("secret-link")).unwrap();

        let result =
            migrate_agent_skill_to_central_impl(&pool, "claude-code", "leaky-skill", None).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Refusing to copy symlink"));
        assert!(skill_dir.is_dir());
        assert!(!central_dir.join("leaky-skill").exists());
        let leftovers = fs::read_dir(&central_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[tokio::test]
    async fn test_migrate_agent_skill_to_central_rejects_central_root_inside_source() {
        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&agent_dir).unwrap();
        let skill_dir = create_agent_skill(&agent_dir, "nested-central");
        let central_dir = skill_dir.join("central-root");

        let pool = setup_db(&central_dir, &agent_dir).await;

        let result =
            migrate_agent_skill_to_central_impl(&pool, "claude-code", "nested-central", None).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot be inside source"));
        let meta = fs::symlink_metadata(&skill_dir).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        assert!(
            !central_dir.exists(),
            "central root inside source should not be created"
        );
    }

    #[tokio::test]
    async fn test_migrate_agent_skill_to_central_rejects_nonexistent_central_root_via_symlink_parent(
    ) {
        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&agent_dir).unwrap();
        let skill_dir = create_agent_skill(&agent_dir, "symlink-nested-central");
        let symlink_parent = tmp.path().join("central-link");
        create_symlink(&skill_dir, &symlink_parent).unwrap();
        let central_dir = symlink_parent.join("nested-central");

        let pool = setup_db(&central_dir, &agent_dir).await;

        let result = migrate_agent_skill_to_central_impl(
            &pool,
            "claude-code",
            "symlink-nested-central",
            None,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot be inside source"));
        let meta = fs::symlink_metadata(&skill_dir).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        assert!(!central_dir.exists());
    }

    #[tokio::test]
    async fn test_migrate_agent_skill_to_central_rolls_back_when_db_write_fails() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        let skill_dir = create_agent_skill(&agent_dir, "db-fail");
        sqlx::query(
            "CREATE TRIGGER fail_db_fail_skill
             BEFORE INSERT ON skills
             WHEN NEW.id = 'db-fail'
             BEGIN
               SELECT RAISE(FAIL, 'db fail');
             END",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result =
            migrate_agent_skill_to_central_impl(&pool, "claude-code", "db-fail", None).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("db fail"));
        let meta = fs::symlink_metadata(&skill_dir).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        assert!(skill_dir.join("SKILL.md").exists());
        assert!(!central_dir.join("db-fail").exists());
    }

    #[tokio::test]
    async fn test_batch_migrate_agent_skills_to_central_reports_partial_results() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_agent_skill(&agent_dir, "migrate-me");
        create_agent_skill(&agent_dir, "skip-conflict");
        create_central_skill(&central_dir, "skip-conflict");

        let result = batch_migrate_agent_skills_to_central_impl(&pool, "claude-code")
            .await
            .unwrap();

        assert_eq!(result.succeeded.len(), 1);
        assert_eq!(result.succeeded[0].skill_id, "migrate-me");
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].skill_id, "skip-conflict");
        assert!(result.failed.is_empty());
        assert!(central_dir.join("migrate-me/SKILL.md").exists());
        assert!(fs::symlink_metadata(agent_dir.join("migrate-me"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::symlink_metadata(agent_dir.join("skip-conflict"))
            .unwrap()
            .is_dir());
    }

    #[tokio::test]
    async fn test_batch_migrate_agent_skills_to_central_uses_agent_root_when_claude_has_duplicate_observations(
    ) {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        let plugin_dir = tmp.path().join("plugin");
        fs::create_dir_all(&central_dir).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();
        fs::create_dir_all(&plugin_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        let user_skill_dir = create_agent_skill(&agent_dir, "shared-skill");
        let plugin_skill_dir = create_agent_skill(&plugin_dir, "shared-skill");

        for (row_id, dir_path, source_kind, is_read_only) in [
            (
                "plugin-row",
                plugin_skill_dir.clone(),
                "plugin".to_string(),
                true,
            ),
            (
                "user-row",
                user_skill_dir.clone(),
                "user".to_string(),
                false,
            ),
        ] {
            db::upsert_agent_skill_observation(
                &pool,
                &db::AgentSkillObservation {
                    row_id: row_id.to_string(),
                    agent_id: "claude-code".to_string(),
                    skill_id: "shared-skill".to_string(),
                    name: "shared-skill".to_string(),
                    description: None,
                    file_path: dir_path.join("SKILL.md").to_string_lossy().into_owned(),
                    dir_path: dir_path.to_string_lossy().into_owned(),
                    source_kind,
                    source_root: dir_path.parent().unwrap().to_string_lossy().into_owned(),
                    link_type: "copy".to_string(),
                    symlink_target: None,
                    is_read_only,
                    scanned_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await
            .unwrap();
        }

        let result = batch_migrate_agent_skills_to_central_impl(&pool, "claude-code")
            .await
            .unwrap();

        assert_eq!(result.succeeded.len(), 1);
        assert!(result.failed.is_empty(), "{:?}", result.failed);
        assert_eq!(result.succeeded[0].skill_id, "shared-skill");
        assert!(fs::symlink_metadata(agent_dir.join("shared-skill"))
            .unwrap()
            .file_type()
            .is_symlink());
        let refreshed_rows = db::get_skills_for_agent(&pool, "claude-code")
            .await
            .unwrap();
        let user_row = refreshed_rows
            .iter()
            .find(|row| row.row_id == "user-row")
            .unwrap();
        assert_eq!(user_row.link_type, "symlink");
        assert_eq!(
            user_row.symlink_target.as_deref(),
            Some(central_dir.join("shared-skill").to_str().unwrap())
        );
        assert!(
            plugin_skill_dir.is_dir(),
            "plugin duplicate must not be migrated or modified"
        );
    }

    // ── uninstall (copy) ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_uninstall_removes_copied_directory() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "uninstall-copy-skill");

        // First, install via copy.
        install_skill_to_agent_copy_impl(&pool, "uninstall-copy-skill", "claude-code")
            .await
            .unwrap();

        let target = agent_dir.join("uninstall-copy-skill");
        assert!(
            target.is_dir(),
            "copied directory should exist before uninstall"
        );

        // Now uninstall.
        uninstall_skill_from_agent_impl(&pool, "uninstall-copy-skill", "claude-code")
            .await
            .unwrap();

        assert!(
            fs::symlink_metadata(&target).is_err(),
            "copied directory should have been removed after uninstall"
        );
    }

    #[tokio::test]
    async fn test_uninstall_copy_removes_db_record() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "db-copy-uninstall-skill");

        install_skill_to_agent_copy_impl(&pool, "db-copy-uninstall-skill", "claude-code")
            .await
            .unwrap();

        uninstall_skill_from_agent_impl(&pool, "db-copy-uninstall-skill", "claude-code")
            .await
            .unwrap();

        let installations = db::get_skill_installations(&pool, "db-copy-uninstall-skill")
            .await
            .unwrap();
        assert!(
            installations.is_empty(),
            "DB record should be removed after uninstall"
        );
    }

    #[tokio::test]
    async fn test_uninstall_refuses_real_dir_without_copy_record() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;

        // Place a real directory with NO DB record as 'copy' type.
        fs::create_dir_all(agent_dir.join("protected-skill")).unwrap();

        let result = uninstall_skill_from_agent_impl(&pool, "protected-skill", "claude-code").await;
        assert!(
            result.is_err(),
            "uninstall should refuse to delete a real directory without a copy record"
        );

        // Ensure the directory still exists.
        assert!(
            agent_dir.join("protected-skill").is_dir(),
            "real directory should NOT have been deleted"
        );
    }

    #[tokio::test]
    async fn test_batch_install_uses_copy_method() {
        let tmp = TempDir::new().unwrap();
        let central_dir = tmp.path().join("central");
        let agent_dir = tmp.path().join("claude");
        fs::create_dir_all(&central_dir).unwrap();

        let pool = setup_db(&central_dir, &agent_dir).await;
        create_central_skill(&central_dir, "batch-copy-skill");

        let mut succeeded = Vec::new();
        let mut failed = Vec::new();
        for agent_id in &["claude-code".to_string()] {
            match install_skill_to_agent_copy_impl(&pool, "batch-copy-skill", agent_id).await {
                Ok(_) => succeeded.push(agent_id.clone()),
                Err(e) => failed.push(FailedInstall {
                    agent_id: agent_id.clone(),
                    error: e,
                }),
            }
        }

        assert_eq!(succeeded.len(), 1);
        assert!(failed.is_empty());

        // The installed directory must NOT be a symlink.
        let target = agent_dir.join("batch-copy-skill");
        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(
            !meta.file_type().is_symlink(),
            "batch copy install should create a real directory"
        );
    }
}
