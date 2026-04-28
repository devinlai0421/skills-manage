import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import {
  AgentWithStatus,
  CentralRepositoryConfig,
  CentralRepositoryOperationResult,
  CentralRepositoryStatus,
  CustomAgentConfig,
  ScanDirectory,
  UpdateCustomAgentConfig,
} from "@/types";

// ─── State ────────────────────────────────────────────────────────────────────

interface SettingsState {
  scanDirectories: ScanDirectory[];
  isLoadingScanDirs: boolean;
  error: string | null;
  githubPat: string;
  isLoadingGitHubPat: boolean;
  isSavingGitHubPat: boolean;
  centralRepositoryConfig: CentralRepositoryConfig | null;
  centralRepositoryStatus: CentralRepositoryStatus | null;
  isLoadingCentralRepository: boolean;
  isSavingCentralRepository: boolean;
  isRunningCentralRepositoryGit: boolean;

  // Actions — scan directories
  loadScanDirectories: () => Promise<void>;
  addScanDirectory: (path: string, label?: string) => Promise<ScanDirectory>;
  removeScanDirectory: (path: string) => Promise<void>;
  toggleScanDirectory: (path: string, active: boolean) => Promise<void>;

  // Actions — GitHub PAT
  loadGitHubPat: () => Promise<void>;
  saveGitHubPat: (value: string) => Promise<void>;
  clearGitHubPat: () => Promise<void>;

  // Actions — central repository
  loadCentralRepositoryConfig: () => Promise<void>;
  refreshCentralRepositoryStatus: () => Promise<void>;
  saveCentralRepositoryConfig: (localPath: string, remoteUrl: string) => Promise<CentralRepositoryConfig>;
  initializeCentralRepository: (localPath: string, remoteUrl: string) => Promise<CentralRepositoryOperationResult>;
  pullCentralRepository: () => Promise<CentralRepositoryOperationResult>;
  pushCentralRepository: () => Promise<CentralRepositoryOperationResult>;

  // Actions — custom agents
  addCustomAgent: (config: CustomAgentConfig) => Promise<AgentWithStatus>;
  updateCustomAgent: (agentId: string, config: UpdateCustomAgentConfig) => Promise<AgentWithStatus>;
  removeCustomAgent: (agentId: string) => Promise<void>;

  clearError: () => void;
}

// ─── Store ────────────────────────────────────────────────────────────────────

export const useSettingsStore = create<SettingsState>((set) => ({
  scanDirectories: [],
  isLoadingScanDirs: false,
  error: null,
  githubPat: "",
  isLoadingGitHubPat: false,
  isSavingGitHubPat: false,
  centralRepositoryConfig: null,
  centralRepositoryStatus: null,
  isLoadingCentralRepository: false,
  isSavingCentralRepository: false,
  isRunningCentralRepositoryGit: false,

  // ── Scan Directories ───────────────────────────────────────────────────────

  /**
   * Load all scan directories from the backend.
   */
  loadScanDirectories: async () => {
    set({ isLoadingScanDirs: true, error: null });
    try {
      const dirs = await invoke<ScanDirectory[]>("get_scan_directories");
      set({ scanDirectories: dirs, isLoadingScanDirs: false });
    } catch (err) {
      set({ error: String(err), isLoadingScanDirs: false });
    }
  },

  /**
   * Add a new custom scan directory.
   * Returns the created ScanDirectory or throws on error.
   */
  addScanDirectory: async (path: string, label?: string) => {
    const dir = await invoke<ScanDirectory>("add_scan_directory", {
      path,
      label: label || null,
    });
    // Refresh the list
    set((state) => ({
      scanDirectories: [...state.scanDirectories, dir],
    }));
    return dir;
  },

  /**
   * Remove a custom scan directory by path.
   */
  removeScanDirectory: async (path: string) => {
    await invoke<void>("remove_scan_directory", { path });
    set((state) => ({
      scanDirectories: state.scanDirectories.filter((d) => d.path !== path),
    }));
  },

  /**
   * Toggle the active state of a custom scan directory.
   * Persists the change to the backend database.
   */
  toggleScanDirectory: async (path: string, active: boolean) => {
    await invoke<void>("set_scan_directory_active", { path, isActive: active });
    set((state) => ({
      scanDirectories: state.scanDirectories.map((d) =>
        d.path === path ? { ...d, is_active: active } : d
      ),
    }));
  },

  // ── GitHub PAT ────────────────────────────────────────────────────────────

  loadGitHubPat: async () => {
    set({ isLoadingGitHubPat: true, error: null });
    try {
      const value = await invoke<string | null>("get_setting", { key: "github_pat" });
      set({
        githubPat: value ?? "",
        isLoadingGitHubPat: false,
      });
    } catch (err) {
      set({
        error: String(err),
        isLoadingGitHubPat: false,
      });
    }
  },

  saveGitHubPat: async (value: string) => {
    set({ isSavingGitHubPat: true, error: null });
    try {
      await invoke("set_setting", { key: "github_pat", value });
      set({
        githubPat: value.trim(),
        isSavingGitHubPat: false,
      });
    } catch (err) {
      set({
        error: String(err),
        isSavingGitHubPat: false,
      });
      throw err;
    }
  },

  clearGitHubPat: async () => {
    set({ isSavingGitHubPat: true, error: null });
    try {
      await invoke("set_setting", { key: "github_pat", value: "" });
      set({
        githubPat: "",
        isSavingGitHubPat: false,
      });
    } catch (err) {
      set({
        error: String(err),
        isSavingGitHubPat: false,
      });
      throw err;
    }
  },

  // ── Central Repository ─────────────────────────────────────────────────────

  loadCentralRepositoryConfig: async () => {
    set({ isLoadingCentralRepository: true, error: null });
    try {
      const config = await invoke<CentralRepositoryConfig>("get_central_repository_config");
      const status = await invoke<CentralRepositoryStatus>("get_central_repository_status");
      set({
        centralRepositoryConfig: config,
        centralRepositoryStatus: status,
        isLoadingCentralRepository: false,
      });
    } catch (err) {
      set({
        error: String(err),
        isLoadingCentralRepository: false,
      });
    }
  },

  refreshCentralRepositoryStatus: async () => {
    try {
      const status = await invoke<CentralRepositoryStatus>("get_central_repository_status");
      set({ centralRepositoryStatus: status });
    } catch (err) {
      set({ error: String(err) });
      throw err;
    }
  },

  saveCentralRepositoryConfig: async (localPath: string, remoteUrl: string) => {
    set({ isSavingCentralRepository: true, error: null });
    try {
      const config = await invoke<CentralRepositoryConfig>("set_central_repository_config", {
        localPath,
        remoteUrl,
      });
      const status = await invoke<CentralRepositoryStatus>("get_central_repository_status");
      set({
        centralRepositoryConfig: config,
        centralRepositoryStatus: status,
        isSavingCentralRepository: false,
      });
      return config;
    } catch (err) {
      set({
        error: String(err),
        isSavingCentralRepository: false,
      });
      throw err;
    }
  },

  initializeCentralRepository: async (localPath: string, remoteUrl: string) => {
    set({ isRunningCentralRepositoryGit: true, error: null });
    try {
      const result = await invoke<CentralRepositoryOperationResult>("initialize_central_repository", {
        localPath,
        remoteUrl,
      });
      set({
        centralRepositoryConfig: {
          local_path: localPath,
          remote_url: remoteUrl.trim() || null,
        },
        centralRepositoryStatus: result.status,
        isRunningCentralRepositoryGit: false,
      });
      return result;
    } catch (err) {
      set({
        error: String(err),
        isRunningCentralRepositoryGit: false,
      });
      throw err;
    }
  },

  pullCentralRepository: async () => {
    set({ isRunningCentralRepositoryGit: true, error: null });
    try {
      const result = await invoke<CentralRepositoryOperationResult>("pull_central_repository");
      set({
        centralRepositoryStatus: result.status,
        isRunningCentralRepositoryGit: false,
      });
      return result;
    } catch (err) {
      set({
        error: String(err),
        isRunningCentralRepositoryGit: false,
      });
      throw err;
    }
  },

  pushCentralRepository: async () => {
    set({ isRunningCentralRepositoryGit: true, error: null });
    try {
      const result = await invoke<CentralRepositoryOperationResult>("push_central_repository");
      set({
        centralRepositoryStatus: result.status,
        isRunningCentralRepositoryGit: false,
      });
      return result;
    } catch (err) {
      set({
        error: String(err),
        isRunningCentralRepositoryGit: false,
      });
      throw err;
    }
  },

  // ── Custom Agents ──────────────────────────────────────────────────────────

  /**
   * Register a new user-defined agent.
   * Returns the created AgentWithStatus or throws on error.
   */
  addCustomAgent: async (config: CustomAgentConfig) => {
    const agent = await invoke<AgentWithStatus>("add_custom_agent", { config });
    return agent;
  },

  /**
   * Update an existing user-defined agent.
   * Returns the updated AgentWithStatus or throws on error.
   */
  updateCustomAgent: async (agentId: string, config: UpdateCustomAgentConfig) => {
    const agent = await invoke<AgentWithStatus>("update_custom_agent", {
      agentId,
      config,
    });
    return agent;
  },

  /**
   * Remove a user-defined agent by ID.
   */
  removeCustomAgent: async (agentId: string) => {
    await invoke<void>("remove_custom_agent", { agentId });
  },

  // ── Misc ───────────────────────────────────────────────────────────────────

  clearError: () => set({ error: null }),
}));
