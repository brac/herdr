---
name: herdr-hooks
description: herdr's opt-in INBOUND Claude Code hook channel — `herdr hook install/uninstall/notify/status`, the per-session status file, and how it turns the invisible-permission-prompt NeedsInput heuristic into a fact. Use for hook install/debug, "permission prompt not detected", or ~/.claude/settings.json work. Not to be confused with the OUTBOUND `core/hooks.rs` (which fires user commands on state change).
---

# herdr-hooks — the inbound Notification/Stop hook channel

herdr normally *infers* when an agent is blocked on a permission prompt (the `<2% CPU + stale
tool_use` heuristic in `infer_status`, see herdr-status). This opt-in channel lets Claude Code tell
herdr exactly when an agent needs the user, making NeedsInput a fact instead of a guess.

## The flow

1. `herdr hook install` merges a `Notification` and a `Stop` hook into `~/.claude/settings.json`,
   each running `<abs path>/herdr hook notify`.
2. When Claude Code fires the event, it pipes a JSON payload to `herdr hook notify` on **stdin**,
   which writes `~/.claude/herdr/<session_id>.json` atomically (`Notification` -> NeedsInput,
   `Stop`/`SubagentStop` -> JobDone).
3. herdr's `notify` watcher (which also watches `~/.claude/herdr/`) wakes the render loop.
4. `monitor::apply_hook_override` reads the status file and overrides the heuristic when it is fresh.

## Where it lives

- **`core/hookstate.rs`** — the whole channel: status-file format + `read` + `write_from_stdin`; the
  settings.json merge (`install` / `uninstall` / `settings_path`); `dir()`. serde_json only.
- **`src/hookcmd.rs`** — the thin `herdr hook <notify|install|uninstall|status>` dispatcher
  (computes `current_exe()` for the registered command, prints user-facing messages).
- **`src/main.rs`** — intercepts `herdr hook …` argv[0] BEFORE entering the TUI and exits.
- **`src/watcher.rs`** — also watches `discovery::herdr_state_dir()`, creating it so the watch
  succeeds before the first hook fires.
- **`core/monitor.rs::apply_hook_override`** — called right after `infer_status` in `finalize_usage`.
- **`core/discovery.rs::herdr_state_dir`** — `~/.claude/herdr`.

## The override rule (`apply_hook_override`)

A hook state wins ONLY when it is the most recent thing that happened:

- Fresh: `now - ts_ms <= 5 min` (else a stale leftover from a prior run).
- Not superseded: `ts_ms + 500ms >= session.last_message_ts` (the transcript mtime). Once the agent
  resumes and writes new JSONL, `last_message_ts` advances past the hook and the heuristic takes back
  over — so the override **self-clears**; nobody deletes the file.
- NeedsInput never stomps an active `Processing`/`Error`; JobDone only fills in an ambiguous
  `Idle`/`WaitingInput`/`Unknown`.

## Gotchas

- **Hooks are read at Claude Code session start.** After `install`, the user must restart their
  Claude Code sessions for the hooks to fire. Say so.
- **Opt-in, never auto-install.** Writing the user's `settings.json` is an outward-facing change;
  only do it on explicit `herdr hook install`. The merge is idempotent (re-install does not
  duplicate; matched by the `hook notify` command suffix) and preserves the user's other hooks/keys.
- **Keyed by `session_id`.** The status file name and `apply_hook_override` both use the Claude Code
  session UUID. If herdr's `session.session_id` ever diverged from the hook payload's `session_id`,
  the override would silently no-op (degrading to the heuristic — safe, but dead).
- **Do not conflate with `core/hooks.rs`.** That is the OUTBOUND registry (herdr runs user shell
  commands when *its* state changes). This channel is INBOUND (Claude Code -> herdr). Different
  direction, different file.
- The hook command is registered as the absolute path of whatever binary ran `install` (use the
  release binary, e.g. via `quickstart.sh`'s `target/release/herdr`).

## Debug

- `herdr hook status` — prints `~/.claude/herdr/` and each session's current state.
- Inspect the files directly: `cat ~/.claude/herdr/<session_id>.json`.
- Roster not flipping? Check (a) hooks installed (`grep -c "hook notify" ~/.claude/settings.json`),
  (b) the Claude session was restarted since install, (c) the watcher is live, (d) `HERDR_LOG` for
  the status decision.

## Verify

Unit tests: `hookstate::tests::state_and_settings_under_temp_home` (round-trip read + merge-safe
install/uninstall), `read_missing_is_none`. End-to-end: run `herdr hook install/notify/status`
against a throwaway `HOME` so the real `~/.claude` is untouched.
