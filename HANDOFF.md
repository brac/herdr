# HANDOFF — pick up here

Latest state of herdr for the next session. Read `CLAUDE.md` (contract; note the new **§0.1**
tmux-orchestration amendment), then `docs/CLAUDECTL_MAP.md` (as-built record of every phase).
Two skills exist: **`herdr-check`** (verify gate + WSL/tmux gotchas) and **`herdr-map`** (where code
lives). Run `herdr-check` before committing.

## Done and on `origin/main`

- **Phases 0–3, 2.5, 4a, 4b** complete. Plus a run of live-workflow fixes driven by real use.
- **Discovery slug fix** — `cwd_to_slug` hyphenates *all* non-alphanumerics (was `/`-only; broke on
  the `Ben Bracamonte` space path).
- **Phase 2.5 event-driven refresh** — `notify` watcher thread → mpsc; 2s safety-net tick.
- **Phase 3 git light path** — `core/git.rs` porcelain parser, header glance, push/pull (`P`/`L`),
  bare-repo guard, actionability sort. Then two big corrections:
  - git status runs on a **background worker thread** (it froze the UI on `/mnt/c`), and
  - it's **fully event-driven, zero periodic polling** (first-seen / row-select / push-pull / `r` /
    agent JSONL activity).
- **Phase 4a** — oatmeal chat (`C`), conversation reconstructed from JSONL into a 300-msg ring.
- **Phase 4b** — send prompts from the chat via real `terminals::send_input` (+ proactive
  "read-only — needs tmux" hint when injection is unavailable).
- **Live-workflow features:**
  - `n` = instant launch (no wizard); `N` = wizard. Launch opens the agent in a tmux **split pane**.
  - **Single-agent "stage"** (`o`): one Claude visible below herdr at a time, swappable as you
    navigate, via `join-pane`/`break-pane`. Launch replaces the stage (no stacking).
  - **Auto-height**: herdr resizes its own pane to fit the roster when staging (`resize-pane`).
  - **Real kill**: `d`,`d` = SIGTERM; if ignored, `d`,`d` again = **SIGKILL** (was a no-op via the
    dormant MockRuntime).
  - Chat embedded as a split panel in the overview (not full-screen).

Tests ~170, clippy `-D warnings` clean, release ~1.6 MB, deps minimal (`notify` is the only Phase-2.5+
addition; `tempfile` is dev-only).

## Not done / next candidates

1. **Phase 4c** — the remaining Phase 4 piece: one-key **approve / deny / interrupt** (`y`/`n`/`Esc`
   → `terminals::approve_session` + decline/interrupt keystrokes) and **`tmux capture-pane`** to render
   the *actual* permission dialog (invisible in JSONL). Plan in `PHASE4_PLAN.md` Part B. **Note:** with
   the real Claude pane now embedded (stage), the user often approves directly in that pane — 4c is
   most valuable as a roster-level "approve without switching panes" affordance. Confirm it's still
   wanted before building.
2. **Phase 5 — graphs** (sparklines/gauges) once we know which numbers earn a permanent spot.
3. **Dynamic stage auto-height** — currently re-fits only on stage/launch (so it never fights a manual
   `Ctrl-b` resize). User was offered a "re-fit on refresh with change-detection" variant; not built.
4. **Excise dormant residuals** — the inert `MockRuntime`/`rules`/`Orchestrator` bits (see map's
   orchestrator-strip audit). Cleaner to remove now that real backends exist for input/kill.

## Gotchas (these have each cost a session)

- **Dormant `MockRuntime`**: `runtime.actions.*` are no-ops returning Ok. Real effects go through
  `core/terminals` and `core/process`. (Bit us on chat input *and* kill.)
- **Stale binary**: the user runs a prebuilt `target/release/herdr`; tell them to `cargo run --release`
  or rebuild+restart after every change. Check the binary mtime when "it didn't work."
- **`.git/index.lock` race**: herdr (run on its own parent) holds the lock via its git worker; safe to
  `rm -f .git/index.lock` and retry a commit when the only live git is a `… status`.
- **tmux required** for input/launch/stage; `/mnt/c` is slow (git) and inotify-unreliable (handled by
  the worker + safety-net tick). Builds only on WSL/Linux + macOS.

## Working agreement with this user

Decisive, iterates live in tmux, wants action over surveys. Commit + push at each green step (they say
when). Push uses `gh` creds. They pushed back productively on design (the tmux-orchestration departure
→ §0.1) — surface tensions honestly rather than silently crossing invariants.
