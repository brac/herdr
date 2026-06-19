# CLAUDE.md — cockpit

> Working name: **cockpit** (rename freely). A single-binary terminal cockpit for operating
> Claude Code agents and their git repos across a tree of projects. tmux owns the window
> management; this tool supplies the smart panels, modals, and drawers that live inside the panes.

This file is the contract for AI coding agents working on this repo. Read it fully before
proposing a plan or writing code. When in doubt, prefer the lean choice and ask.

---

## 0. What this is (and is not)

**Is:** a project-first TUI that runs from the parent directory of many repos. It discovers
projects, shows what Claude Code agents are doing inside each, launches new agents, and reports
light git status. Built on a fork of `claudectl`.

**Is NOT:**
- A window manager. **tmux does splits, navigation, and pane lifecycle.** We never reimplement that.
- A git client. We **shell out** to `git` for light status and fire-and-forget push/pull.
  We do **not** vendor `git2`/libgit2, and we do **not** reimplement staging/diffing. If the user
  wants full interactive git, the answer is "open a tmux pane running gitui," not "build gitui here."
- A session-lifecycle owner like `claude-dashboard`. We do not create/name/attach tmux sessions
  *for* the user as our core job. We launch agents into panes and observe; tmux owns the rest.
- An async application. No tokio. See §3.

---

## 1. Origin and provenance

This project starts as a **fork of `claudectl`** (MIT, ratatui + crossterm, <1MB, 6 runtime deps,
synchronous, no async runtime). claudectl already solves the painful data layer:

- Session discovery from `~/.claude/sessions/*.json`
- The `cwd → slug` path-hash that maps a working dir to Claude Code's project transcript dir
  (reverse-engineering this is expensive — **do not redo it; inherit it**)
- `--resume` JSONL path resolution (three-priority fallback)
- Incremental JSONL parsing via byte-offset seeking (+ file-truncation reset edge case)
- Multi-signal status inference (CPU% > stop_reason > message age > tool_use), including the
  **invisible-permission-prompt heuristic** (`<2% CPU + tool_use stop_reason >5s old` = NeedsInput)
- Per-tier token/cost accounting (input/output/cache_read/cache_creation priced separately)
- Terminal capability matrix (Kitty/Ghostty/tmux/iTerm2/WezTerm…): Launch/Switch/Input/Approve
  and the keystroke-injection backends (`tmux send-keys`, `kitten @send-text`, iTerm AppleScript)

**First task for any planning session:** read the claudectl source alongside its author writeup
(architecture doc, in `/docs/REFERENCES.md`) and produce a map of: discovery module, JSONL parser,
status inference, the roster render, and the event loop. We refactor *these*, we do not rewrite them.

claudectl also ships a **rules engine + task orchestrator** (auto-approve/deny, kill-on-cost,
dependency-graphed multi-session launches). **This is out of scope for v1.** Plan to rip it out to
stay lean — but do it cleanly on day one, not month two. If we ever want it back, it's in git history.

---

## 2. The one structural change: project-first

claudectl is **session-first** ("a session exists; describe it"). We are **project-first**
("a project folder exists; it may have 0..N agents, a git state, a build state").

This inversion is the load-bearing refactor. Everything else hangs off it.

- Top-level entity = a **project directory** under the parent dir the tool is launched from.
  Discover via `read_dir` on the parent, filtering for dirs (optionally those containing `.git`).
- Sessions hang **off** projects, not at the root. Reuse claudectl's per-session attach to populate
  each project's agent list by matching session `cwd` to the project path.
- The parent-dir scan and the project list are the same thing. The terminal-in-parent-dir setup is
  a feature, not a constraint — lean into it.

Do this refactor (Phase 1) before any new panel work.

---

## 3. Architecture invariants (do not violate without explicit signoff)

- **No async runtime.** Poll loop: `crossterm::event::poll` with timeout → synchronous data refresh
  → render. Local filesystem reads are <1ms; async buys nothing and costs 2–3MB + complexity.
- **Slow/network work is spawn-and-forget.** `git push`/`pull` and agent launches use
  `Command::spawn()` (stdin/stdout/stderr null), never block the render loop. The *next* poll picks
  up the new state. This is claudectl's existing hook pattern — reuse it.
- **Shell out, don't vendor.** `git status --porcelain`, `git rev-list --count`, `ps`, etc. Parse
  stdout. This is why the binary stays small (claudectl uses `ps` over `sysinfo` for exactly this).
- **Keep the dependency tree minimal.** Every dep is a tax on binary size, compile time, and supply
  chain. New deps require justification in the PR description. Current ceiling mindset: <1MB binary.
- **Defensive parsing.** Claude Code's JSONL and `~/.claude` layout are **undocumented internals**,
  not a stable API. Every field can change. Degrade to an `Unknown` state; never crash on a schema
  change. Preserve claudectl's `TelemetryStatus`-style graceful degradation.
- **Panic-safe terminal restore.** Alt screen on entry, guaranteed restore on exit/panic. A crashed
  TUI must never leave the user's terminal corrupted. (`panic = "abort"` is fine and intentional.)
- **Boring code wins.** `Vec::remove(0)` on a 3-element CPU-smoothing buffer is fine. Don't
  over-engineer. Match claudectl's taste.

### Release profile (inherit from claudectl)
```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = true
panic = "abort"
```

---

## 4. The git "light path" (non-negotiable scope boundary)

- Status per project: current branch, dirty flag, ahead/behind counts. Source via
  `git status --porcelain=v2 --branch` (or `git status --porcelain` + `git rev-list --left-right --count @{u}...HEAD`).
- Push/pull: **fire-and-forget** `Command::spawn()`. Display result on a later poll. This is the
  one place network latency breaks the synchronous model, so it's the one place a **skeleton/shimmer
  is justified** (`tui-skeleton`). Local JSONL reads are <1ms and get **no** skeleton.
- **Forbidden:** staging UI, diff viewer, interactive rebase, anything that wants `git2`/libgit2.
  gitui is a UX *reference* for hotkeys and feel only — never a code dependency.

---

## 5. Panels, mapped to sources and references

| Panel | Source | Reference to study | Notes |
|---|---|---|---|
| Project roster (top-level) | parent `read_dir` + claudectl discovery | claudectl roster render; k9s feel from `claude-dashboard` | The spine. Project-first. |
| Agent status (per project) | claudectl status inference | claudectl (inherit as-is) | CPU/stop_reason/age heuristics; permission-prompt detection |
| Conversation (chat bubbles) | **extend** claudectl JSONL parser to retain message text | `oatmeal` (left/right bubble layout) | claudectl keeps only envelope metadata today; we keep content. assistant=left, me=right. `textwrap` for wrapping; `tui-textarea` if driving input from inside. |
| File browser (scoped to project cwd) | `read_dir` on focused project | `joshuto` (tree/preview composition) | Lift composition idea, not the whole app. |
| Git pane (per project) | shell out to `git` (light path) | gitui (hotkeys/feel only) | See §4. |
| Graphs (TBD content) | claudectl token/cost data | ratatui native `Sparkline`/`BarChart`/`Gauge` | Capability is free; defer the *content* until we know which numbers we keep glancing at. |
| Command palette / action drawers | our wiring | `tui-overlay` | Ctrl+P palette, per-agent action menu (Approve/Switch/Input — claudectl has the keystroke backends), confirm-modals with dim backdrop. |
| Loading states | n/a | `tui-skeleton`, `throbber-widgets-tui` | Only on network-bound panels (git push/pull, any future brac.dev/platform sources). NOT on local reads. |

---

## 6. Hotkey / UX conventions (TUI non-negotiables)

- Alt screen; panic-safe restore; handle **SIGWINCH** (resize) and **SIGTSTP** (ctrl-z suspend).
- Conventional keys: `q` quit, `?` help overlay, `/` search, `Esc` close overlay/back, `Tab` cycle
  focus, `hjkl` motion, `Ctrl+P` command palette. Contextual keys per focused panel (gitui's scheme).
- Keyboard-first; mouse optional, never required.
- Respect `NO_COLOR`. Color is a semantic system (status = color), not decoration.

Reference skill worth loading into this repo: `gfargo/tui-design-skill` (seven canonical layouts,
monospace hierarchy, keybinding conventions, the four non-negotiables above).

---

## 7. Build phases (gated — prove each before the next)

- **Phase 0 — Recon.** Fork claudectl, build, run against live sessions. Map the modules (§1).
  Gate: the fork builds and runs locally; we understand discovery + parser + status + event loop.
- **Phase 1 — Inversion.** Refactor data model to project-first (§2). Scan parent dir; sessions hang
  off projects. Gate: roster shows projects, each listing its live agents.
- **Phase 2 — Launch.** Launch an agent into a project from an action drawer: `tmux new-window -c
  {cwd}` + send-keys `claude`. Gate: can start an agent without leaving the cockpit.
- **Phase 3 — Git light path.** Porcelain status per project; spawn-and-forget push/pull; skeleton
  while network resolves. Gate: branch/dirty/ahead-behind visible; push/pull non-blocking.
- **Phase 4 — Conversation.** Extend JSONL parser to keep message content; oatmeal-style bubbles.
  Gate: can read an agent's exchange in-cockpit. (This is what turns monitor → cockpit feel.)
- **Phase 5 — Graphs.** Add ratatui sparklines/gauges for the numbers that earned a permanent spot.
  Gate: graphs reflect real data and we actually glance at them.

Strip the claudectl orchestrator/rules engine during Phase 0/1 unless a phase explicitly needs it.

---

## 8. Scope tripwires (stop and flag if you find yourself doing these)

- Pulling in `git2`/libgit2 or building any diff/staging UI → STOP. Light path only.
- Adding tokio/async-std → STOP. Synchronous poll loop only.
- Creating/owning tmux sessions as a primary feature → STOP. tmux owns lifecycle.
- Reimplementing the `cwd → slug` hash → STOP. Inherit claudectl's.
- Designing graph content before the spine works → STOP. Defer to Phase 5.
- Any new dependency without a one-line justification in the PR → STOP.

---

## 9. Working style for the agent

- Propose a plan before large edits; respect the phase gates.
- Prefer being challenged over being agreeable — if a request fights an invariant in §3 or a
  tripwire in §8, say so plainly and propose the lean alternative.
- Keep diffs small and reviewable. Match claudectl's existing style and module boundaries.
- Magic numbers and tunables live in a `data/`-style module, not scattered inline.
