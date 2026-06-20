# BACKLOG

_Reconciled 2026-06-19. Status markers: **DONE** / **PARTIAL** / open (no marker).
CLAUDE.md §7 build phases (0–5) are all complete; everything here is post-phase work._

## Bug: Auto Height - DONE
IF the parent project has a lot of repos, then the herder bar can extend too far when trying to start a chat window.
The auto heigh feature should not count unactive repos, or repos without any active agents in them. The auto hieght features
should only make room for showing repos with active agents, leeaving the rest of the screen space to the change window
> Resolved: `stage_top_rows()` counts only agent-bearing project rows (test `stage_top_rows_ignores_idle_zero_agent_projects`).

## Feature: Tabbed Agents
When an agent is active in a project, I want it indnetd so I can clearly see that the agent is under that project.
If an agent opens sub agents under it's perview, if possible I want those indended in that agent which is indented 
under the project. 

## Bug: Status and Context is sometimes incorrect - PARTIAL
I select a row in herdr, I press n to start an agent, it sarts and in herdr is report 15% context used up and processing already?
I haven't even asked a question yet
> "Processing already" half resolved: instantaneous CPU now seeds idle on a fresh agent's first sample
> instead of inheriting `ps %cpu`'s lifetime average (`fix:` 1e54cd7). The "15% context before any
> prompt" half is **not** verified — needs a fresh repro; likely the first API call's system-prompt
> tokens. See the `herdr-status` skill.

## Feature: Better colors - DONE
The hightlight row color is too bright and hard to read. The colors are overall could use some work. 
Build a theme system were we can slot in different colors for the UI. The default color should resemble that
of Dracula Dark
> Resolved: the Dark theme is now built from the Dracula palette (`DRAC_*` consts in `core/theme.rs`),
> and the selected row uses a muted Dracula "current line" band (`selection_bg`/`selection_fg`) instead
> of full-brightness reverse-video — the "too bright" complaint. The theme system was already slottable
> (Dark/Light/None via `ThemeMode`); `None` keeps reverse-video for NO_COLOR. See `theme.rs` + `ui/table.rs`.

## Bug: Slow response on Status and Activity - DONE (Status); Activity open
Both the Status and Activity readouts in herdr seem to lag behidn what is actually happening with the agent. Often times
staying on processing while the task has compelted. Activity often lags behind or does not repreesnt the token
consumotion or what is going on. 
> Status lag resolved: instantaneous CPU (`ps time=` deltas, not lifetime `%cpu`) + the `user`/tool_result
> decay branch, so a finished agent leaves Processing within ~1–2 ticks (`fix:` 1e54cd7, `perf:` 8b191cd).
> Still open: the **Activity sparkline** (`activity_history`) accuracy vs. real token throughput.

## Bug: The $/hr is too small - DONE
I often see values of 6000/h or 4000/h. It seems to me that those should be the red values
> Resolved with "$/h is wild" below: those 6000/h readings were the bug, not real rates — a cost delta
> divided by a hardcoded 2s tick during a sub-second event-loop burst. Now the rate is Δcost ÷ real
> wall-clock, so magnitudes are sane and the existing `>10 → red` threshold in `ui/table.rs` colors a
> genuinely hot agent red. Threshold coloring (low/mid/high) was already wired to the theme.

## Bug: Context should reset on /clear - DONE
using /clear on an agent window should reflect in herdr that the context has been cleared
> Resolved before this session: `fix:` e048a41 (attribute transcripts by session id; reset context
> on /clear) + test `merge_resets_transcript_state_when_session_id_changes`.

## Feature: Claude window border
Can tmux add a border to the claude window just as herdr has it's own window title and border ie 
-herdr-----

## Feature: Persistent cost - PARTIAL
A value that reflects the cost over multiple (all) sessions. For the repo collection as a whole and individual projects. 
> Partial: the Phase 5 fleet strip shows aggregate **today / week** cost (`history::weekly_summary`).
> Still open: all-time totals and **per-project** persistent cost.

## Feature: Quickstart
A script to quickly launch tmux and start herdr
tmux attach -t work
cargo run --release -- "/mnt/c/Users/Ben Bracamonte/Work"

## Feature: Add Mouse Scrolling
set -g mouse on
Mouse scroll works on roster page but in agent window it scrolls the messages i eneted. Can we make the scroll wheel scroll the responses like a normal agent window

## Feature: Mouse Focus
Is this possible without betraying our design doc? I want to be able to click between the herdr window and the chat window

## Features: Charts - DONE (v1)
What kind of charts would be informative that we could display along side ond beneath the herdr nav bar?
> v1 shipped: the Phase 5 **fleet trend strip** beneath the roster (`G` toggles) — status counts, live
> burn $/hr + a trend sparkline, today/week cost (`feat:` bb8ec94, `ui/fleet.rs`). Per-agent values stay
> in the roster (Context bar + Activity sparkline columns). Optional next: daily-cost BarChart or a
> dedicated graphs overlay if the strip earns the space.

## Feature: wk token tracker - PARTIAL
We need a way to track token usage over multiple sessions
> Partial: `history::weekly_summary` already aggregates week cost **and** tokens; the fleet strip surfaces
> week *cost*. Still open: a token-specific multi-session readout (the strip shows $, not tokens).

## Bug: Asking for permission in roster doesnt work - PARTIAL
when waiting for an approavl to run a tool, there is no indicator in herdr roster view that it needs attention or can be arrpoved. Sometimes I see a 
purple Need Input notice, but then it kept going. maybe becaues I was on auto mode?
> Partial: Phase 4c added the **approval inspector** (`A`) — `tmux capture-pane` shows the real permission
> dialog (invisible in JSONL) with approve/deny/interrupt from the roster (`feat:` 581c86d). NeedsInput
> detection also tightened via the status work. Still open: a persistent at-a-glance roster indicator,
> and the auto-mode "kept going" interaction the note describes.

## Bug: $/h is wild - DONE
The nuimber is up and down and all over the palce. I feel like it should be the average of an hour? or er min?
> Resolved: burn rate is now Δcost ÷ Δ(real wall-clock), EMA-smoothed (`smooth_burn`, α=0.3), sampled
> no faster than every 2s (`MIN_BURN_SAMPLE_MS`) — mirroring the CPU instantaneous-rate derivation. The
> old `delta * 1800.0` assumed a fixed 2s tick, but the Phase-2.5 event loop fires at irregular,
> sub-second intervals, so a JSONL burst spiked $/hr into the thousands and quiet ticks decayed it
> erratically. New `prev_cost_sample_ms` field on the session carries the sample baseline. Tests:
> `smooth_burn_steadies_a_spiky_signal`, `smooth_burn_decays_toward_zero_when_idle`. (`app.rs` refresh).

## Feature: Model identification
Each agetn should have a indicator about which model they are current running.

## Bug: Reszing the window changes the agent window size permenantly
When reszing the window smaller they resize properlly. When resize the window larger with both the roster and the agent window open, then the agent window stays the compressed size while herdr gets bigger. Upon getting larger the agent window should take more of the screen space when possible while still maintaiing the min-height for the roster

## Bug: Roster selection bar should stay with last repo selected when launching an agent.
Launhcing an agent in the roster view with n. The repo will sort to the top with the now active agent, leaving the roster selection on an unintended repo. The selection bar should follow the now sorted repo. 

## Bug: Reoster bar is too small with only one or two active agents
The roster bar should be a minimum for 12 rows when there are a small number of active agents. Ruight now there is one active agent and an open chat window and I see 7 rows. Header, Columns names, 2 repos, session overview and fleet bar.

## Feature: Split waiting states
If the agent is waiting for a repsonse from the API, the roster displays Waiting. The a task is complete the roster also dispalys Waiting. When a task is complete and the agent is waiting for the next task from the usr, then I want that task to be Job Done. So we can tell when we are waiting on the API and when we are waiting ont he user

## Feature: Error Display 
Somtimes we hit an API error. The Roster should display Error in the stats when this is encounted