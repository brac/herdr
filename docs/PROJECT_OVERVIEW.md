# PROJECT_OVERVIEW.md — cockpit

A narrative companion to `CLAUDE.md`. This file gives a planning session the *why* and the *map of
the territory*; `CLAUDE.md` gives the *rules*. Read both before producing a plan.

---

## The idea in one paragraph

I run a single terminal from the **parent directory of all my projects**. From there I want to see
every project, see what Claude Code agents are doing inside each, launch new agents into them, check
light git status (branch, dirty, ahead/behind), and fire off push/pull — plus some terminal graphs
TBD. **tmux owns the window management** (splits, navigation, pane lifecycle); this tool is the set
of *smart panels, modals, and drawers* that live inside those panes. It is built as a fork of
`claudectl`, inverted from session-first to **project-first**.

---

## How we got here (decisions already made — do not relitigate)

1. **tmux is the window manager.** We are not building splits or navigation. The tool supplies smart
   content inside panes the user has already split. This is the central bet; everything follows.
2. **Fork claudectl, don't start clean.** claudectl already solves the hard data layer (session
   discovery, the `cwd→slug` hash, incremental JSONL parsing, multi-signal status inference,
   per-tier cost accounting, terminal keystroke-injection). That writeup is effectively our
   architecture doc. Rebuilding it would be weeks of reverse-engineering undocumented internals.
3. **Project-first, not session-first.** The one structural refactor. Top-level entity is a project
   directory; agents/sessions hang off it. (See `CLAUDE.md` §2.)
4. **Light path on git — shell out, never vendor.** `git status --porcelain`, fire-and-forget
   push/pull. No `git2`, no diff/staging UI. gitui is a *feel* reference, not a dependency.
5. **No reimplementing git. No owning session lifecycle.** Those are the two ways this project
   bloats into something it shouldn't be. Both are tripwires.
6. **Lean, synchronous, small.** No async runtime. <1MB mindset. Boring code wins. Inherited from
   claudectl's philosophy and kept deliberately.

## Why each "is not" matters

- **Not a window manager:** tmux is better at it than we'd ever be, and composing existing tools
  (drop into a pane running gitui/lazygit) beats reimplementing them. This is the same
  "let the platform do its job" discipline used across my other projects.
- **Not a git client:** full git TUIs already exist (gitui, lazygit). Our value is the *agent +
  project overview*, with git as a light status glance — not a replacement for those tools.
- **Not a lifecycle owner:** `claude-dashboard` already does k9s-style tmux session management. If I
  wanted that, I'd use it. I want the inverse: tmux owns lifecycle, we supply intelligence.

---

## What "done for v1" looks like

From the parent-dir terminal I can, without leaving the cockpit:
- See all projects and, per project, which agents are running and their status/cost/context%.
- Launch a new agent into any project (`tmux new-window -c {cwd}` + send `claude`).
- See light git status per project and trigger non-blocking push/pull.
- Read an agent's conversation in an oatmeal-style chat panel.
- Glance at a couple of graphs for whichever numbers prove worth watching.

Everything beyond that (rules engine, orchestrated multi-agent dependency graphs, full git) is
explicitly deferred or out of scope.

---

## Reference material to examine during planning

### Primary — read these first
- **claudectl** — the fork base. Source + the author's architecture writeup. This *is* the data
  layer. Map: discovery module, JSONL parser, status inference, roster render, event loop, terminal
  capability matrix. Repo: `github.com/mercurialsolo/claudectl`. Writeup URL is in `/docs/REFERENCES.md`.
- **ratatui** + **crossterm** — the rendering + terminal-backend stack claudectl uses and we keep.
  Native widgets we'll use directly: `Sparkline`, `BarChart`, `Gauge`, `List`, `Table`, `Paragraph`.

### Panel-specific references (study the relevant one when building that panel)
- **oatmeal** — terminal LLM chat with left/right chat bubbles + slash commands. Reference for the
  **conversation panel** layout (assistant left, me right) and message wrapping feel.
- **gitui** — fast multi-panel git TUI. Reference for **hotkey scheme and navigation feel ONLY**.
  Do not lift its git layer (`git2`-based, heavier than we want).
- **joshuto** — ranger-like file manager. Reference for the **file-browser panel** tree/preview
  composition.
- **claude-dashboard** (seunggabi) — k9s-style Claude Code session manager (Go, tmux lifecycle).
  Reference for the **roster/navigation feel**; it's the inverse of our lifecycle stance, so lift
  ideas, not architecture.

### Widget crates to evaluate (pull rather than rebuild)
- **tui-overlay** — drawers, modals, popovers, toasts from one primitive, with dim backdrop. For the
  command palette (Ctrl+P), per-agent action menus, and confirm dialogs.
- **tui-skeleton** — pulse/sweep/shimmer placeholders. **Only** for network-bound panels
  (git push/pull; any future brac.dev / platform / session-backend sources). Not for local reads.
- **throbber-widgets-tui** — spinners/throbbers for in-progress actions.
- **tui-textarea** — multiline input widget, if/when we drive agent prompts from inside the cockpit.
- **textwrap** — text wrapping for chat bubbles.
- **ratatui-image** — *optional/stretch*: Kitty/iTerm2/sixel inline images, with graceful fallback.
  Only relevant if we want a burnRat-style sprite or rich file previews in a pane later. Adds weight;
  gate it behind a clear want, not "because we can."

### Tooling / conventions to adopt
- **gfargo/tui-design-skill** — a Claude Code skill encoding seven canonical TUI layouts, monospace
  visual hierarchy, keybinding conventions, and the four non-negotiables (alt screen, panic-safe
  restore, SIGWINCH, SIGTSTP). Worth dropping into the repo so coding agents respect TUI conventions.

### Ecosystem indexes (for breadth, if a gap appears)
- **ratatui/awesome-ratatui** — curated ratatui apps + widgets.
- **rothgar/awesome-tuis** — cross-language TUI index (Go Bubble Tea/Lipgloss, .NET Spectre.Console)
  if we ever want non-Rust inspiration. We are staying in Rust.

---

## Known hazards (flagged in the claudectl writeup — expect these)

- The `cwd → slug` mapping must match Claude Code's algorithm exactly. Inherit it; don't re-derive.
- `--resume` sessions write to a *different* JSONL than their own ID — three-priority resolver.
- JSONL truncation (new session reusing a path) must reset token accumulators or counts freeze.
- Permission prompts are **invisible** in the transcript; inferred from low CPU + stale `tool_use`.
- `ps`/JSONL are undocumented internals — parse defensively, degrade to `Unknown`, never crash.
- Push/pull are network-bound — must be spawn-and-forget or they stall the synchronous render loop.

---

## Open questions for the planning session to resolve

1. Project discovery filter: every subdir, or only those containing `.git`? (Default: `.git` dirs,
   with a flag to widen.)
2. How deep does the parent scan go — one level, or recursive with a depth cap?
3. Exact porcelain command for ahead/behind that's cheapest and most portable across the repos.
4. Where the orchestrator/rules-engine removal lands cleanly in the fork (Phase 0 vs 1).
5. Whether the conversation panel reads the *focused* project's newest session, or is selectable.
