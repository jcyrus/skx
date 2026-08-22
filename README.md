# `skx` (Skill eXchange)

> The universal AI skill manager, cross-agent compiler, and TUI cockpit.

`skx` provides a single source of truth for all your AI agent instructions, context rules, and toolsets. Define your skills once in a canonical format, and `skx` handles compilation, syncing, symlinking, and drift detection across **Google Antigravity**, **Claude Code**, **GitHub Copilot**, **Cursor**, and local **MCP runtimes** (VS Code, Goose).

A working Rust implementation — four crates, 90+ tests, zero clippy warnings. See "Architecture Overview" below for the crate layout and "Development" at the end to build it yourself.

---

## ⚡ Top-Level Features

- **Spec-compliant frontmatter:** Implements the [Agent Skills spec](https://agentskills.io/specification) — `name`, `description`, `license`, `compatibility`, `allowed-tools` and `metadata` — alongside `skx`'s own `triggers`, `targets` and `mcp_dependencies`. Any key `skx` doesn't model is **preserved verbatim** through a round-trip rather than silently dropped, which matters because `skx` symlinks its rendered copy over the original.
- **Universal Skill Engine:** Write standard canonical markdown (`SKILL.md`) with rich frontmatter. `skx` translates triggers and globs into each target agent's own dialect — a Cursor `glob` override wins when present, otherwise canonical `triggers` are translated for you.
- **Sync, Symlinks & Static Export:** Zero-copy symlinks for targets whose format *is* the canonical one (Antigravity, Claude Code) — instant updates, no double-editing. `skx export` compiles everything into standalone static files instead, for committing to a team repo or feeding to CI, where a symlink back into `~/.config/skx` wouldn't survive the trip.
- **Interactive TUI Cockpit:** Keyboard-driven dashboard (`k9s`/`btop`/`lazygit` style) — a workspace drift-and-token meter across the top, a full-width skill table (author, version, **approximate context cost**, and a per-target status column) above a **markdown-rendered** preview and a live agent matrix. `/` filters, `j`/`k` navigate, `Tab` switches pane, `Space` toggles a target on/off, `s` syncs, `?` shows the key map.
- **MCP Tool Bundling:** Pair markdown instructions with the Model Context Protocol servers they depend on — `skx` merges each one into the target runtime's `mcp.json` at its own key, never clobbering servers other skills or the user configured directly.
- **Drift Detection:** `skx audit` fingerprints every artifact it writes and tells apart five states — in sync, never synced, user-modified (edited by hand — never silently overwritten), stale (the skill changed upstream and needs a re-sync), and orphaned (recorded but no longer produced by any installed skill).
- **Ecosystem Adapters:** Native compilation targets for Antigravity (`agy`), Claude Code, Cursor (`.mdc` rules with translated globs), GitHub Copilot (a marked region inside `copilot-instructions.md`, safe to share with hand-written content), and MCP-capable runtimes (VS Code, Goose).

---

## 📁 Architecture Overview

```
                    [ Central Cache (global) ]   [ Workspace Cache (local) ]
               ~/.config/skx/skills/<name>/SKILL.md   .skx/skills/<name>/SKILL.md
                                               │
                   ┌──────────────────────────────────────────────────────┐
                   │     skx_core / skx_adapters / skx_cli / skx_tui      │
                   └──────────────────────────────────────────────────────┘
                                               │
         ▼                  ▼                  ▼                  ▼                  ▼
  [ Antigravity ]    [ Claude Code ]       [ Cursor ]     [ GitHub Copilot ]  [ MCP runtime ]
  .agents/skills/    .claude/skills/     .cursor/rules/    .github/copilot-   .vscode/mcp.json
  <name>/SKILL.md    <name>/SKILL.md       <name>.mdc      instructions.md   or goose/mcp.json
     (symlink)          (symlink)          (compiled)      (marked region)     (merged JSON)
```

---

## 🚀 Quick Start

### Installation

Not yet published to crates.io — build from source for now (see "Development" at the end of this file):

```bash
cargo build --release
./target/release/skx --help
```

### 1. Initialize `skx` in a project

```bash
# Detects existing .claude/, .cursor/, .github/, .agents/, .vscode/mcp.json
# folders in the current directory and creates skx.toml
skx init
```

### 2. Install a skill from a local path

```bash
# Install into this workspace (.skx/skills/)
skx add ../my-skills/rust-expert

# Install into the global cache (~/.config/skx/skills/) instead
skx add ../my-skills/rust-expert --global
```

A skill's own `targets:` block in its frontmatter decides which agents it compiles to — see the spec below. (Installing from a Git URL or registry isn't implemented yet; `skx add` only accepts a local path or a directory containing a `SKILL.md`.)

### 3. Compile it into your agent configs

```bash
skx sync    # write/symlink into every target the skill declares
skx audit   # check for drift: never synced, hand-edited, or stale
```

### 4. Launch the TUI Cockpit

```bash
skx tui
# or simply
skx
```

```
 skx  v0.1.0   ~/Projects/opensource/skx
 ██████████████ 100%  │  53 skills  ·  171k tokens  ·  55 synced
╭ Skills 53 ─────────────────────────────────────────────────────────────────────╮
│    NAME                    AUTHOR         VER      TOKENS         AG CL CU CP MC│
│● G graphify                Graphify-Labs  1.2.0     10.3k         ·  ●  ·  ·  · │
│● G ai-seo                  —              0.1.0      6.8k         ·  ●  ·  ·  · │
│▲ L my-local-thing          jcyrus         0.1.0      1.2k         ·  ▲  ●  ·  · │
╰─────────────────────────────────────────────────────────────────────────────────╯
╭ Preview  11/425 ──────────────────────────╮╭ Agent Matrix ──────────────────────╮
│graphify                                   ││   antigrav ░░░  not declared       │
│▌ Initial Assessment                       ││   claude   ███  in sync            │
╰───────────────────────────────────────────╯╰────────────────────────────────────╯
 j/k  move    tab  pane    /  filter    space  toggle    s  sync    ?  help
```

**Columns.** `AUTHOR` comes from the spec's `metadata.author`; skills that don't
declare one show `—` rather than a guess, because inferring an author from a file
path confidently misattributes vendored skills. `VER` prefers the skill's own
frontmatter over the manifest's install-time snapshot. `AG CL CU CP MC` are the
five targets — two letters each, because `claude`, `cursor` and `copilot` all
start with `C`. Columns drop by priority as the terminal narrows, so a 40-column
window still shows status and name.

**`TOKENS` is the point of the table.** Skills are spent from the context window,
so that is the resource worth watching — the same way `btop` watches CPU. The
column is an approximation (`chars / 4`) and the header totals the workspace: 53
skills is ~171k tokens, and a single large one can be 10k on its own. Counts are
coloured against fixed thresholds rather than relative to the largest skill, so
"expensive" doesn't change meaning when you uninstall something bigger.

**The health meter** is the fraction of *declared targets* in sync, so one skill
drifting on one of four targets moves it by a quarter of a skill rather than the
whole thing. Every status colour — row dot, matrix bar, header count — runs
through one shared success→danger severity ramp, so the same hue always means the
same degree of wrong.

#### Keys

One motion vocabulary at every level: `j`/`k` moves inside whatever pane holds
focus, including scrolling the preview. `Esc` backs out one level and **never
quits** — `q` and `Ctrl-C` are the only two ways out.

| Key | Action |
| --- | --- |
| `j` / `k`, `↓` / `↑` | Move within the focused pane |
| `g` / `G` | Jump to first / last |
| `Tab` / `⇧Tab` | Cycle focus: Table → Preview → Matrix |
| `Ctrl-d` / `Ctrl-u`, `PgDn` / `PgUp` | Half-page scroll |
| `/` | Filter — fuzzy on the name, literal substring on description or author |
| `Esc` | Back out one level (overlay → filter → focus) |
| `Space` | Toggle the selected target (focuses the Matrix if it isn't) |
| `s` | Sync the **selected** skill |
| `S` | Sync **everything** — asks first |
| `d` | Discover unmanaged skills on disk |
| `?` / `F1` | Toggle the key map |
| `q` / `Ctrl-C` | Quit |

Effort scales with consequence: `s` touches only the row under the cursor, while
`S` writes across every declared target and shows a confirmation. Syncing runs on
a worker thread with a determinate progress bar, so the interface keeps
responding instead of freezing for the duration.

### Theming & accessibility

Two themes ship, and every text token is verified at **≥4.5:1 (WCAG AA)** against
its own background, with borders at **≥3:1** per WCAG 1.4.11. A test asserts this
on every build, so the palette can't silently regress.

```bash
SKX_THEME=light skx     # or dark
```

Resolution order is `SKX_THEME` → `COLORFGBG` → dark. There is no portable way to
*ask* a terminal for its background colour — the OSC 11 query needs a raw-mode
read with a timeout and several emulators ignore it silently — so the explicit
setting stays authoritative rather than being a mere override.

Colour depth is detected from `COLORTERM`/`TERM` and the palette is quantised once
at startup. The 16-colour surfaces are hand-authored rather than derived: sixteen
colours cannot express three near-black backgrounds, so a generic mapping
collapses them onto one and destroys the distinction between a table header and
the selected row.

---

## 🛠 Command Reference

| Command             | Alias    | Description                                                                    |
| ------------------- | -------- | ------------------------------------------------------------------------------ |
| `skx add <path>`     | `skx a`  | Install a skill from a local path or directory containing a `SKILL.md`. `-g`/`--global` installs into the shared cache instead of this workspace. |
| `skx remove <name>` | `skx rm` | Unlink every artifact the skill produced, strip it from shared files, and remove it from the cache and manifest. |
| `skx list`          | `skx ls` | List installed skills, their scopes (global/local), and where they came from.  |
| `skx sync`          | `skx s`  | Compile every installed skill through every target it declares — symlink where the format allows, write/merge where it doesn't. |
| `skx audit`         | —        | Report drift per artifact: not yet synced, in sync, hand-edited, stale (skill changed upstream), or orphaned. |
| `skx export`        | `skx ex` | Compile every skill into standalone static files under `-o`/`--out` (default `skx-export/`) — always real files, never symlinks, for committing to a repo or feeding to CI. |
| `skx tui`           | —        | Open the full-screen interactive TUI dashboard (also the default when `skx` is run with no subcommand). |

Installing from a Git repo or registry URL is on the roadmap, not implemented yet — `skx add` rejects anything that looks like a URL with a clear error rather than failing silently.

---

## 📄 Canonical `SKILL.md` Specification

Skills managed by `skx` use standard YAML frontmatter to support heterogeneous agent features without vendor lock-in:

```markdown
---
name: rust-systems-expert
description: Deep systems architectural conventions, memory layout, and concurrency patterns
version: 1.0.0
triggers:
  - "*.rs"
  - "Cargo.toml"
targets:
  antigravity:
    scope: workspace
    auto_activate: true
  claude_code:
    enabled: true
  cursor:
    glob: "**/*.rs"
  copilot:
    enabled: true
mcp_dependencies:
  - name: rust-analyzer-mcp
    command: rust-analyzer-mcp
    args: ["--stdio"]
---

# Rust Systems Engineering Instructions

- Prefer zero-cost abstractions and enforce explicit lifetime annotations where ambiguity arises.
- Structure error types using `thiserror` for internal libraries and `anyhow` for application boundaries.
```

A skill only compiles to a target if its own frontmatter declares that target's block — there's no implicit "compile everywhere." Adding an empty `cursor: {}` block is enough to opt in without an override; a per-target override (like Cursor's `glob`) takes precedence over the top-level `triggers` when both are present, otherwise `triggers` is translated for you. `mcp_dependencies` is separate from `targets` — it applies to whatever MCP-capable runtime is configured, with no `targets.mcp` block needed.

---

## 🔌 Supported Agent Matrix

| Agent / Tool        | Global Config Path         | Workspace Config Path             | Generated Format                        |
| -------------------- | -------------------------- | --------------------------------- | ---------------------------------------- |
| **Antigravity**      | `~/.gemini/config/skills/` | `.agents/skills/<name>/SKILL.md`  | Canonical `SKILL.md`, symlinked          |
| **Claude Code**      | `~/.claude/skills/`        | `.claude/skills/<name>/`          | Canonical `SKILL.md`, symlinked          |
| **Cursor**           | `~/.config/cursor/rules/`  | `.cursor/rules/<name>.mdc`        | `.mdc` rule, compiled (`description`/`globs`/`alwaysApply`) |
| **GitHub Copilot**   | — (workspace only)         | `.github/copilot-instructions.md` | Marked region merged into a shared file  |
| **MCP runtime**      | `~/.config/goose/mcp.json` | `.vscode/mcp.json`                | One JSON-pointer merge per MCP dependency |

Antigravity and Claude Code get a real symlink back to the cached `SKILL.md` — edit the cache once, both stay current, and there's nothing to re-sync until the skill itself changes. Cursor, Copilot, and MCP configs are genuine format translations (or share a file with content you wrote by hand), so those are always written, never linked — `skx sync` re-runs the translation each time, and `skx audit` won't overwrite a file you edited directly.

---

## 🧑‍💻 Development

A Cargo workspace with four crates:

| Crate           | Responsibility                                                                 |
| --------------- | ------------------------------------------------------------------------------- |
| `skx_core`      | Canonical schema, YAML frontmatter parser, `skx.toml` manifest, cache path resolution, the write/symlink engine, and drift detection. |
| `skx_adapters`  | The `SkillAdapter` trait plus one implementation per target (Antigravity, Claude Code, Cursor, Copilot, MCP). |
| `skx_cli`       | The `skx` binary: argument parsing and thin orchestration over `skx_core`/`skx_adapters`. |
| `skx_tui`       | The ratatui cockpit — a pure, unit-testable `App` data model plus a separate rendering layer. |

```bash
cargo build --workspace          # build everything
cargo test --workspace           # 90+ tests: unit tests in skx_core/skx_adapters/skx_tui,
                                  # plus subprocess integration tests in skx_cli/tests/cli.rs
                                  # that drive the real binary against a temp workspace + fake HOME
cargo clippy --workspace --all-targets
cargo fmt --all
```

`skx_core`'s tests are the ones to read first if you're touching the write engine — they cover the byte-stability guarantee sync/drift detection depends on, symlink vs. compiled writes, region/JSON-pointer merging without clobbering hand-written content, and the audit states themselves.
