# HANDOFF — pick up here

Latest state of herdr for the next session. Read `CLAUDE.md` (contract; note the new **§0.1**
tmux-orchestration amendment), then `docs/CLAUDECTL_MAP.md` (as-built record of every phase).
Two skills exist: **`herdr-check`** (verify gate + WSL/tmux gotchas) and **`herdr-map`** (where code
lives). Run `herdr-check` before committing.

## Done and on `origin/main`

- **Phases 0–3, 2.5, 4a, 4b, 4c** complete. Plus a run of live-workflow fixes driven by real use.
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

## Phase 4c — as built

Roster-level **approval inspector** (decided over the chat-view placement: the staged pane already
lets you approve in-pane, so the value is approving *without switching panes*).
- New key **`A`** on the selected agent opens a centered modal (`ui/approval.rs`). Instant **`y`**
  approve was kept unchanged — `A` is the additive inspect-then-act path.
- The modal scrapes the agent's pane via **`tmux capture-pane -p`** (`terminals::capture_pane`,
  tmux-only read-only scrape — §0.1/§8) and shows the tail (the dialog sits at the pane bottom).
- In-modal keys: **`y`** approve (Enter→pane), **`n`** deny, **`i`** interrupt (both Esc→pane via
  `terminals::deny_session`/`interrupt_session`), **`r`** re-capture, **`Esc`** cancel. A failed act
  keeps the modal open so you can retry.
- Backends added in `terminals/`: `capture_pane`, `deny_session`, `interrupt_session` (shared
  `send_escape`), and `tmux::send_key` (named-key send-keys, distinct from literal-text `send_input`).
- Guards: non-agent / remote / no-tmux all no-op with a hint. No new deps; binary ~1.72 MB.
- **Known latent quirk (pre-existing, not introduced here):** an agent with an *empty* tty matches
  the first tmux pane (`pane_tty.contains("")` is always true) — same in `send_input`. Real agents
  have a tty, so it doesn't bite in practice; worth tightening if it ever does.

## Phase 5 — as built (fleet trend strip, v1)

The roster already covers per-agent *now* (Context bar + Activity sparkline columns),
so charts that re-display per-row values are redundant. Phase 5 v1 adds the one thing
a row can't: the **fleet over time + cross-session history**.
- One-line **fleet strip** beneath the roster (`ui/fleet.rs`), toggle **`G`** (default on):
  status counts (needs/proc/wait/idle), live total **burn $/hr + a trend sparkline**
  (`App.fleet_burn_history`, sampled in `tick()` at ~2s spacing, cap 40), and today/week
  cost from the existing `history::weekly_summary`.
- `App::fleet_counts() -> FleetCounts` rolls the whole fleet up (ignores triage filters).
- `stage_top_rows()` reserves +1 when the strip is shown (auto-height).
- Not yet built: per-metric chart picker, a dedicated full graphs screen, daily cost
  BarChart (would need bucketing `history::load_history` by day — scalars only today).

## Not done / next candidates

1. **Phase 5 cont.** — daily cost BarChart, or a dedicated graphs overlay, if the strip earns it.
2. **Dynamic stage auto-height** — currently re-fits only on stage/launch (so it never fights a manual
   `Ctrl-b` resize). User was offered a "re-fit on refresh with change-detection" variant; not built.
3. **Excise dormant residuals** — the inert `MockRuntime`/`rules`/`Orchestrator` bits (see map's
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
