---
name: herdr-map
description: Orientation map of the herdr codebase — where discovery, parsing, status, render, the event loop, git, chat, and tmux-staging live, plus the load-bearing gotchas. Use when starting work on herdr or asked where something is.
---

# herdr-map — where things live

herdr is a project-first TUI for Claude Code agents, forked from `claudectl` (ratatui + crossterm,
synchronous, no tokio). Read `CLAUDE.md` for the contract and `docs/CLAUDECTL_MAP.md` for the
as-built record (every phase logged there). Workspace: `crates/claudectl-core` (data) +
`crates/claudectl-tui` (App + render) + thin `src/main.rs` (event loop).

## The load-bearing systems

- **Discovery** — `core/discovery.rs`. `scan_sessions()`; `cwd_to_slug()` (replaces *every*
  non-ASCII-alphanumeric with `-` — was a real bug when it only did `/`).
- **JSONL parser** — `core/transcript.rs` (full content: Text/ToolUse/ToolResult) +
  `core/monitor.rs::update_tokens` (incremental seek; also fills `session.conversation` for the chat,
  and detects per-session activity to event-refresh git).
- **Status / CPU / cost inference** — `core/monitor.rs::infer_status` + `core/process.rs` (CPU is
  *instantaneous*, derived from `ps time=` deltas — NOT `%cpu`). The bug-prone heart of herdr; read the
  **`herdr-status`** skill before touching it, and debug live with `HERDR_LOG=/path`.
- **Token pricing / cost / dedup** — `core/models.rs` (family-prefix pricing table + matcher),
  `core/monitor.rs::merge_usage` (streaming-duplicate dedup, keyed `message.id:requestId`),
  `core/history.rs` (CSV + daily activity heatmap for `tui/ui/fleet.rs`). Read the **`herdr-cost`** skill.
- **Inbound hooks (opt-in)** — `core/hookstate.rs` + `src/hookcmd.rs` (`herdr hook install/notify`).
  A Notification/Stop hook writes `~/.claude/herdr/<session>.json`; the watcher + `monitor::apply_hook_override`
  make NeedsInput a fact. Read the **`herdr-hooks`** skill. Distinct from the OUTBOUND `core/hooks.rs`.
- **Roster render** — `tui/ui/table.rs` (project headers + agents + git glance, plus per-row Context
  bar and Activity sparkline); chat in `tui/ui/chat.rs`; the Phase 4c approval inspector in
  `tui/ui/approval.rs`; the Phase 5 fleet trend strip in `tui/ui/fleet.rs`; `App` owns all state in
  `tui/app.rs`.
- **Event loop** — `src/main.rs`. Event-driven: a `notify` watcher thread (`src/watcher.rs`) feeds an
  mpsc channel; `CHANNEL_POLL`=200ms drain + `TICK_RATE`=2s safety net. No tokio.
- **Git light path** — `core/git.rs` (porcelain v2 parser). Fetched on a **background worker thread**
  (`App::GitStatusService`), **event-driven only** (first-seen / row-selection / push-pull / `r` /
  agent activity) — no periodic polling. `git_cache: HashMap<PathBuf, Option<GitStatus>>`.
- **tmux orchestration** — `core/terminals/tmux.rs` + `terminals/mod.rs`. `launch` (split-window -P),
  `stage_pane`/`unstage_pane` (join/break-pane), `resize_stage_top`, `send_input`, `approve_session`,
  and (Phase 4c) `capture_pane` (read-only scrape of the permission dialog), `deny_session`/
  `interrupt_session` (Escape), `tmux::send_key` (named keys vs literal text).

## Gotchas that have already burned a session

- **The dormant `MockRuntime` trap.** `App.runtime` is an inert `MockRuntime` — `runtime.actions.*`
  (inject_text, terminate_session, …) are **no-ops that return Ok**. Anything that must affect the real
  world goes through the **real backends**: `core/terminals` (input/approve), `core/process::terminate`
  + `force_kill` (kill). If an action "says it worked but didn't," suspect a `runtime.actions` call.
- **Input/staging need tmux.** `send_input`/`stage` work only when herdr runs inside tmux and agents
  are in tmux panes (`detect_terminal()` reads herdr's own env). `App.input_supported` gates the chat;
  the chat shows "read-only — needs tmux" otherwise.
- **Slug/space paths**: project paths with spaces/dots now resolve correctly (the slug fix).
- **No `tui-textarea`/`textwrap`/`throbber-widgets` deps** — hand-rolled. Keep the dep tree minimal.

## Where the plans are

`CLAUDE.md` (contract, §0.1 = the tmux-orchestration amendment), `EVENT_LOOP_PLAN.md`,
`PHASE3_PLAN.md`, `PHASE4_PLAN.md`, and `HANDOFF.md` (latest state + what's next).
