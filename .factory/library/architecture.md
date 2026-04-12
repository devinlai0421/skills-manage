# Architecture — skills-manage

## Overview

skills-manage is a Tauri v2 desktop application with a Rust backend and React frontend. It manages AI Agent Skills (SKILL.md files) across multiple coding platforms by maintaining a central canonical directory and creating symlinks to platform-specific directories.

## Components

### Rust Backend (src-tauri/)

**Scanner** — On startup and manual refresh, walks configured directories to discover SKILL.md files, parses YAML frontmatter (name, description), detects whether entries are symlinks/copies/native, and persists results to SQLite.

**Agent Registry** — Hardcoded list of known platforms (Claude Code, Codex, Cursor, Gemini CLI, Trae, Factory Droid, OpenClaw) with their global skills directory paths. Also supports user-defined custom agents stored in SQLite.

**Linker** — Creates/removes relative symlinks from platform skill directories to the canonical `~/.agents/skills/<name>` directory. On Windows, uses junctions. Validates paths to prevent traversal attacks.

**Database** — SQLite via `tauri-plugin-sql`. Tables: skills, skill_installations, agents, collections, collection_skills, scan_directories, settings. WAL mode for responsive UI.

### React Frontend (src/)

**Layout** — Fixed sidebar (240px) + scrollable main content area. Sidebar has 3 sections: By Tool (platform list), Central Skills, Collections.

**State** — Zustand stores: skillStore (scanned skills), platformStore (agents + installations), collectionStore (user collections). Stores call Tauri `invoke()` commands.

**Routing** — React Router with routes: `/platform/:agentId`, `/central`, `/skill/:skillId`, `/collection/:collectionId`, `/settings`.

**UI Components** — shadcn/ui primitives with Tailwind CSS 4. Key components: SkillCard, SkillList, InstallDialog, CollectionEditor.

## Data Flow

```
User clicks "Install to..." on a Central Skill
  → React calls invoke("install_skill_to_agent", { skillId, agentId, method: "symlink" })
  → Rust: reads canonical path from ~/.agents/skills/<name>
  → Rust: computes relative path from target agent dir
  → Rust: creates symlink (or junction on Windows)
  → Rust: updates skill_installations table
  → Returns success to React
  → React: invalidates platform store → UI refreshes
```

## Key Directories

| Directory | Purpose |
|-----------|---------|
| `~/.agents/skills/` | Central Skills (canonical source) |
| `~/.claude/skills/` | Claude Code global skills |
| `~/.cursor/skills/` | Cursor global skills |
| `~/.agents/skills/` | Codex global skills (same as Central) |
| `~/.gemini/skills/` | Gemini CLI global skills |
| `~/.trae/skills/` | Trae global skills |
| `~/.factory/skills/` | Factory Droid global skills |
| `~/.openclaw/skills/` | OpenClaw global skills |
| `~/.junie/skills/` | Junie global skills |
| `~/.qwen/skills/` | Qwen global skills |
| `~/.trae-cn/skills/` | Trae CN global skills |
| `~/.windsurf/skills/` | Windsurf global skills |
| `~/.qoder/skills/` | Qoder global skills |
| `~/.augment/skills/` | Augment global skills |
| `~/.opencode/skills/` | OpenCode global skills |
| `~/.kilocode/skills/` | KiloCode global skills |
| `~/.ob1/skills/` | OB1 global skills |
| `~/.amp/skills/` | Amp global skills |
| `~/.kiro/skills/` | Kiro global skills |
| `~/.codebuddy/skills/` | CodeBuddy global skills |
| `~/.hermes/skills/` | Hermes global skills |
| `~/.copilot/skills/` | Copilot global skills |
| `~/.aider/skills/` | Aider global skills |
| `~/.qclaw/skills/` | QClaw global skills |
| `~/.easyclaw/skills/` | EasyClaw global skills |
| `~/.easyclaw-20260322-01/skills/` | EasyClaw V2 global skills |
| `~/.openclaw-autoclaw/skills/` | AutoClaw global skills |
| `~/.workbuddy/skills-marketplace/skills/` | WorkBuddy global skills |
| `~/.skillsmanage/` | App data (SQLite db) |

## Theming

**Catppuccin 4-flavor palette** — The app uses Catppuccin's color system with 4 selectable flavors:
- **Mocha** — Darkest, cozy feeling (default dark). Base #1e1e2e.
- **Macchiato** — Medium contrast dark. Base #24273a.
- **Frappé** — Muted dark. Base #303446.
- **Latte** — Light mode (default light). Base #eff1f5.

Each flavor maps 26 Catppuccin colors (Rosewater, Flamingo, Pink, Mauve, Red, Maroon, Peach, Yellow, Green, Teal, Sky, Sapphire, Blue, Lavender, Text, Subtext1, Subtext0, Overlay2, Overlay1, Overlay0, Surface2, Surface1, Surface0, Base, Mantle, Crust) to the shadcn/ui semantic tokens (background, foreground, card, primary, secondary, muted, accent, border, etc.).

**Implementation** — CSS custom properties per `[data-theme="mocha"]`, `[data-theme="frappe"]`, `[data-theme="macchiato"]`, `[data-theme="latte"]` selector. `themeStore` (Zustand) manages current flavor, persisted to localStorage. Setting `document.documentElement.dataset.theme` triggers instant color swap. Accent color is Green for all flavors (Catppuccin's Green: Mocha #a6e3a1, Latte #40a02b, etc.).

## Invariants

1. A symlink in a platform dir MUST point to a real directory inside `~/.agents/skills/`.
2. The app NEVER modifies SKILL.md content in canonical directories — read only.
3. The app NEVER deletes non-symlink directories in platform paths.
4. Each skill in SQLite has a unique `id` derived from its directory name (lowercase, hyphens).
5. Collections are stored only in SQLite — they don't create filesystem artifacts.
