import { create } from "zustand";
import { invoke, isTauriRuntime } from "@/lib/tauri";
import { BatchMigrateAgentSkillsResult, MigrateAgentSkillResult, ScannedSkill } from "@/types";

const BROWSER_FIXTURE_SKILLS_BY_AGENT: Record<string, ScannedSkill[]> = {
  "claude-code": [
    {
      id: "fixture-central-skill",
      name: "fixture-central-skill",
      description: "Installed browser validation fixture for platform drawer flows.",
      file_path: "~/.claude/skills/fixture-central-skill/SKILL.md",
      dir_path: "~/.claude/skills/fixture-central-skill",
      link_type: "symlink",
      symlink_target: "~/.agents/skills/fixture-central-skill",
      is_central: true,
    },
  ],
  cursor: [
    {
      id: "fixture-central-skill",
      name: "fixture-central-skill",
      description: "Installed browser validation fixture for platform drawer flows.",
      file_path: "~/.cursor/skills/fixture-central-skill/SKILL.md",
      dir_path: "~/.cursor/skills/fixture-central-skill",
      link_type: "symlink",
      symlink_target: "~/.agents/skills/fixture-central-skill",
      is_central: true,
    },
  ],
};

// ─── State ────────────────────────────────────────────────────────────────────

interface SkillState {
  skillsByAgent: Record<string, ScannedSkill[]>;
  loadingByAgent: Record<string, boolean>;
  pendingSkillActionKeys: Record<string, boolean>;
  pendingMigrationKeys: Record<string, boolean>;
  isBatchMigrating: boolean;
  error: string | null;

  // Actions
  getSkillsByAgent: (agentId: string) => Promise<void>;
  uninstallSkillFromAgent: (skillId: string, agentId: string) => Promise<void>;
  migrateSkillToCentral: (
    agentId: string,
    skillId: string,
    rowId?: string
  ) => Promise<MigrateAgentSkillResult>;
  batchMigrateAgentSkillsToCentral: (
    agentId: string
  ) => Promise<BatchMigrateAgentSkillsResult>;
}

// ─── Store ────────────────────────────────────────────────────────────────────

function skillActionKey(agentId: string, skillId: string) {
  return `${agentId}::${skillId}`;
}

export const useSkillStore = create<SkillState>((set) => ({
  skillsByAgent: {},
  loadingByAgent: {},
  pendingSkillActionKeys: {},
  pendingMigrationKeys: {},
  isBatchMigrating: false,
  error: null,

  /**
   * Fetch skills for a specific agent by invoking the Tauri backend command.
   * Results are cached per agentId in skillsByAgent.
   */
  getSkillsByAgent: async (agentId: string) => {
    set((state) => ({
      loadingByAgent: { ...state.loadingByAgent, [agentId]: true },
      error: null,
    }));
    if (!isTauriRuntime()) {
      set((state) => ({
        skillsByAgent: {
          ...state.skillsByAgent,
          [agentId]: BROWSER_FIXTURE_SKILLS_BY_AGENT[agentId] ?? [],
        },
        loadingByAgent: { ...state.loadingByAgent, [agentId]: false },
      }));
      return;
    }
    try {
      const skills = await invoke<ScannedSkill[]>("get_skills_by_agent", {
        agentId,
      });
      set((state) => ({
        skillsByAgent: { ...state.skillsByAgent, [agentId]: skills },
        loadingByAgent: { ...state.loadingByAgent, [agentId]: false },
      }));
    } catch (err) {
      set((state) => ({
        error: String(err),
        loadingByAgent: { ...state.loadingByAgent, [agentId]: false },
      }));
    }
  },

  uninstallSkillFromAgent: async (skillId: string, agentId: string) => {
    const actionKey = skillActionKey(agentId, skillId);
    set((state) => ({
      pendingSkillActionKeys: {
        ...state.pendingSkillActionKeys,
        [actionKey]: true,
      },
      error: null,
    }));

    if (!isTauriRuntime()) {
      set((state) => {
        const next = { ...state.pendingSkillActionKeys };
        delete next[actionKey];
        return {
          pendingSkillActionKeys: next,
          error: "Uninstalling skills requires the Tauri desktop runtime.",
        };
      });
      return;
    }

    try {
      await invoke("uninstall_skill_from_agent", { skillId, agentId });
      const skills = await invoke<ScannedSkill[]>("get_skills_by_agent", {
        agentId,
      });

      set((state) => {
        const next = { ...state.pendingSkillActionKeys };
        delete next[actionKey];
        return {
          skillsByAgent: { ...state.skillsByAgent, [agentId]: skills },
          pendingSkillActionKeys: next,
        };
      });
    } catch (err) {
      set((state) => {
        const next = { ...state.pendingSkillActionKeys };
        delete next[actionKey];
        return {
          error: String(err),
          pendingSkillActionKeys: next,
        };
      });
      throw err;
    }
  },

  migrateSkillToCentral: async (agentId: string, skillId: string, rowId?: string) => {
    const actionKey = skillActionKey(agentId, skillId);
    set((state) => ({
      pendingMigrationKeys: {
        ...state.pendingMigrationKeys,
        [actionKey]: true,
      },
      error: null,
    }));

    if (!isTauriRuntime()) {
      set((state) => {
        const next = { ...state.pendingMigrationKeys };
        delete next[actionKey];
        return {
          pendingMigrationKeys: next,
          error: "Migrating skills requires the Tauri desktop runtime.",
        };
      });
      throw new Error("Migrating skills requires the Tauri desktop runtime.");
    }

    try {
      const result = await invoke<MigrateAgentSkillResult>("migrate_agent_skill_to_central", {
        agentId,
        skillId,
        rowId,
      });
      const skills = await invoke<ScannedSkill[]>("get_skills_by_agent", {
        agentId,
      });

      set((state) => {
        const next = { ...state.pendingMigrationKeys };
        delete next[actionKey];
        return {
          skillsByAgent: { ...state.skillsByAgent, [agentId]: skills },
          pendingMigrationKeys: next,
        };
      });

      return result;
    } catch (err) {
      set((state) => {
        const next = { ...state.pendingMigrationKeys };
        delete next[actionKey];
        return {
          error: String(err),
          pendingMigrationKeys: next,
        };
      });
      throw err;
    }
  },

  batchMigrateAgentSkillsToCentral: async (agentId: string) => {
    set({ isBatchMigrating: true, error: null });

    if (!isTauriRuntime()) {
      const error = "Migrating skills requires the Tauri desktop runtime.";
      set({ isBatchMigrating: false, error });
      throw new Error(error);
    }

    try {
      const result = await invoke<BatchMigrateAgentSkillsResult>(
        "batch_migrate_agent_skills_to_central",
        { agentId }
      );
      const skills = await invoke<ScannedSkill[]>("get_skills_by_agent", {
        agentId,
      });
      set((state) => ({
        skillsByAgent: { ...state.skillsByAgent, [agentId]: skills },
        isBatchMigrating: false,
      }));
      return result;
    } catch (err) {
      set({ error: String(err), isBatchMigrating: false });
      throw err;
    }
  },
}));
