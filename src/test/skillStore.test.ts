import { describe, it, expect, vi, beforeEach } from "vitest";
import { ScannedSkill } from "../types";
import * as tauriBridge from "@/lib/tauri";

// Mock Tauri core before importing the store
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { useSkillStore } from "../stores/skillStore";

// ─── Fixtures ─────────────────────────────────────────────────────────────────

const mockSkills: ScannedSkill[] = [
  {
    id: "frontend-design",
    name: "frontend-design",
    description: "Build distinctive, production-grade frontend interfaces",
    file_path: "~/.claude/skills/frontend-design/SKILL.md",
    dir_path: "~/.claude/skills/frontend-design",
    link_type: "symlink",
    symlink_target: "~/.agents/skills/frontend-design",
    is_central: true,
  },
  {
    id: "code-reviewer",
    name: "code-reviewer",
    description: "Review code changes and identify high-confidence, actionable bugs",
    file_path: "~/.claude/skills/code-reviewer/SKILL.md",
    dir_path: "~/.claude/skills/code-reviewer",
    link_type: "copy",
    is_central: false,
  },
];

// ─── Tests ────────────────────────────────────────────────────────────────────

describe("skillStore", () => {
  beforeEach(() => {
    // Reset store to initial state before each test
    useSkillStore.setState({
      skillsByAgent: {},
      loadingByAgent: {},
      pendingSkillActionKeys: {},
      pendingMigrationKeys: {},
      isBatchMigrating: false,
      error: null,
    });
    vi.clearAllMocks();
  });

  // ── Initial State ─────────────────────────────────────────────────────────

  it("has correct initial state", () => {
    const state = useSkillStore.getState();
    expect(state.skillsByAgent).toEqual({});
    expect(state.loadingByAgent).toEqual({});
    expect(state.pendingSkillActionKeys).toEqual({});
    expect(state.pendingMigrationKeys).toEqual({});
    expect(state.isBatchMigrating).toBe(false);
    expect(state.error).toBeNull();
  });

  // ── getSkillsByAgent ──────────────────────────────────────────────────────

  it("calls invoke('get_skills_by_agent') with agentId (camelCase)", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(mockSkills);

    await useSkillStore.getState().getSkillsByAgent("claude-code");

    expect(invoke).toHaveBeenCalledWith("get_skills_by_agent", {
      agentId: "claude-code",
    });
  });

  it("populates skillsByAgent after successful fetch", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(mockSkills);

    await useSkillStore.getState().getSkillsByAgent("claude-code");

    const state = useSkillStore.getState();
    expect(state.skillsByAgent["claude-code"]).toEqual(mockSkills);
    expect(state.loadingByAgent["claude-code"]).toBe(false);
    expect(state.error).toBeNull();
  });

  it("sets loading to true while fetching", async () => {
    let resolveSkills!: (value: ScannedSkill[]) => void;
    vi.mocked(invoke).mockReturnValueOnce(
      new Promise<ScannedSkill[]>((r) => (resolveSkills = r))
    );

    const fetchPromise = useSkillStore.getState().getSkillsByAgent("claude-code");

    // Loading should be true while the call is pending
    expect(useSkillStore.getState().loadingByAgent["claude-code"]).toBe(true);

    resolveSkills(mockSkills);
    await fetchPromise;

    expect(useSkillStore.getState().loadingByAgent["claude-code"]).toBe(false);
  });

  it("sets error and clears loading when fetch fails", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("Agent not found"));

    await useSkillStore.getState().getSkillsByAgent("claude-code");

    const state = useSkillStore.getState();
    expect(state.error).toContain("Agent not found");
    expect(state.loadingByAgent["claude-code"]).toBe(false);
    expect(state.skillsByAgent["claude-code"]).toBeUndefined();
  });

  it("can hold skills for multiple agents independently", async () => {
    const cursorSkills: ScannedSkill[] = [
      {
        id: "deploy",
        name: "deploy",
        description: "Deploy the application",
        file_path: "~/.cursor/skills/deploy/SKILL.md",
        dir_path: "~/.cursor/skills/deploy",
        link_type: "symlink",
        is_central: true,
      },
    ];

    vi.mocked(invoke)
      .mockResolvedValueOnce(mockSkills)
      .mockResolvedValueOnce(cursorSkills);

    await useSkillStore.getState().getSkillsByAgent("claude-code");
    await useSkillStore.getState().getSkillsByAgent("cursor");

    const state = useSkillStore.getState();
    expect(state.skillsByAgent["claude-code"]).toEqual(mockSkills);
    expect(state.skillsByAgent["cursor"]).toEqual(cursorSkills);
  });

  it("returns deterministic browser fixture skills when Tauri runtime is unavailable", async () => {
    const isTauriSpy = vi.spyOn(tauriBridge, "isTauriRuntime").mockReturnValue(false);

    await useSkillStore.getState().getSkillsByAgent("claude-code");

    expect(invoke).not.toHaveBeenCalled();
    expect(useSkillStore.getState().skillsByAgent["claude-code"]).toEqual([
      expect.objectContaining({
        id: "fixture-central-skill",
        link_type: "symlink",
        is_central: true,
      }),
    ]);

    isTauriSpy.mockRestore();
  });

  // ── uninstallSkillFromAgent ──────────────────────────────────────────────

  it("calls uninstall_skill_from_agent and refreshes the agent skill list", async () => {
    useSkillStore.setState({
      skillsByAgent: { "claude-code": mockSkills },
      loadingByAgent: {},
      pendingSkillActionKeys: {},
      error: null,
    });

    const remainingSkills = [mockSkills[1]];
    vi.mocked(invoke)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(remainingSkills);

    await useSkillStore
      .getState()
      .uninstallSkillFromAgent("frontend-design", "claude-code");

    expect(invoke).toHaveBeenNthCalledWith(1, "uninstall_skill_from_agent", {
      skillId: "frontend-design",
      agentId: "claude-code",
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "get_skills_by_agent", {
      agentId: "claude-code",
    });
    expect(useSkillStore.getState().skillsByAgent["claude-code"]).toEqual(
      remainingSkills
    );
    expect(useSkillStore.getState().pendingSkillActionKeys).toEqual({});
    expect(useSkillStore.getState().error).toBeNull();
  });

  it("tracks in-flight uninstall mutations by agent and skill", async () => {
    let resolveUninstall!: () => void;
    vi.mocked(invoke)
      .mockReturnValueOnce(
        new Promise<void>((resolve) => {
          resolveUninstall = resolve;
        })
      )
      .mockResolvedValueOnce([]);

    const uninstallPromise = useSkillStore
      .getState()
      .uninstallSkillFromAgent("frontend-design", "claude-code");

    expect(
      useSkillStore.getState().pendingSkillActionKeys["claude-code::frontend-design"]
    ).toBe(true);

    resolveUninstall();
    await uninstallPromise;

    expect(
      useSkillStore.getState().pendingSkillActionKeys["claude-code::frontend-design"]
    ).toBeUndefined();
  });

  it("sets error and clears pending uninstall state when uninstall fails", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("Permission denied"));

    await expect(
      useSkillStore
        .getState()
        .uninstallSkillFromAgent("frontend-design", "claude-code")
    ).rejects.toThrow("Permission denied");

    expect(useSkillStore.getState().error).toContain("Permission denied");
    expect(
      useSkillStore.getState().pendingSkillActionKeys["claude-code::frontend-design"]
    ).toBeUndefined();
  });

  // ── migrateSkillToCentral ─────────────────────────────────────────────────

  it("migrates a local agent skill to central and refreshes the agent skill list", async () => {
    const refreshedSkills: ScannedSkill[] = [
      {
        ...mockSkills[1],
        link_type: "symlink",
        symlink_target: "~/.agents/skills/code-reviewer",
        is_central: true,
      },
    ];
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        skill_id: "code-reviewer",
        agent_id: "claude-code",
        central_path: "~/.agents/skills/code-reviewer",
        installed_path: "~/.claude/skills/code-reviewer",
        link_type: "symlink",
      })
      .mockResolvedValueOnce(refreshedSkills);

    const result = await useSkillStore
      .getState()
      .migrateSkillToCentral("claude-code", "code-reviewer", "row-1");

    expect(invoke).toHaveBeenNthCalledWith(1, "migrate_agent_skill_to_central", {
      agentId: "claude-code",
      skillId: "code-reviewer",
      rowId: "row-1",
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "get_skills_by_agent", {
      agentId: "claude-code",
    });
    expect(result.skill_id).toBe("code-reviewer");
    expect(useSkillStore.getState().skillsByAgent["claude-code"]).toEqual(refreshedSkills);
    expect(useSkillStore.getState().pendingMigrationKeys).toEqual({});
  });

  it("tracks in-flight migrations by agent and skill", async () => {
    let resolveMigration!: (value: unknown) => void;
    vi.mocked(invoke)
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveMigration = resolve;
        })
      )
      .mockResolvedValueOnce([]);

    const migrationPromise = useSkillStore
      .getState()
      .migrateSkillToCentral("claude-code", "code-reviewer");

    expect(
      useSkillStore.getState().pendingMigrationKeys["claude-code::code-reviewer"]
    ).toBe(true);

    resolveMigration({
      skill_id: "code-reviewer",
      agent_id: "claude-code",
      central_path: "~/.agents/skills/code-reviewer",
      installed_path: "~/.claude/skills/code-reviewer",
      link_type: "symlink",
    });
    await migrationPromise;

    expect(
      useSkillStore.getState().pendingMigrationKeys["claude-code::code-reviewer"]
    ).toBeUndefined();
  });

  it("runs a batch migration and refreshes the agent skill list", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        succeeded: [
          {
            skill_id: "code-reviewer",
            agent_id: "claude-code",
            central_path: "~/.agents/skills/code-reviewer",
            installed_path: "~/.claude/skills/code-reviewer",
            link_type: "symlink",
          },
        ],
        skipped: [],
        failed: [],
      })
      .mockResolvedValueOnce([]);

    const result = await useSkillStore
      .getState()
      .batchMigrateAgentSkillsToCentral("claude-code");

    expect(invoke).toHaveBeenNthCalledWith(1, "batch_migrate_agent_skills_to_central", {
      agentId: "claude-code",
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "get_skills_by_agent", {
      agentId: "claude-code",
    });
    expect(result.succeeded).toHaveLength(1);
    expect(useSkillStore.getState().isBatchMigrating).toBe(false);
  });
});
