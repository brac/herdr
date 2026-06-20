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

## Feature: Better colors
The hightlight row color is too bright and hard to read. The colors are overall could use some work. 
Build a theme system were we can slot in different colors for the UI. The default color should resemble that
of Dracula Dark

## Bug: Slow response on Status and Activity - DONE (Status); Activity open
Both the Status and Activity readouts in herdr seem to lag behidn what is actually happening with the agent. Often times
staying on processing while the task has compelted. Activity often lags behind or does not repreesnt the token
consumotion or what is going on. 
> Status lag resolved: instantaneous CPU (`ps time=` deltas, not lifetime `%cpu`) + the `user`/tool_result
> decay branch, so a finished agent leaves Processing within ~1–2 ticks (`fix:` 1e54cd7, `perf:` 8b191cd).
> Still open: the **Activity sparkline** (`activity_history`) accuracy vs. real token throughput.

## Bug: The $/hr is too small
I often see values of 6000/h or 4000/h. It seems to me that those should be the red values
> Open. Cosmetic: threshold-color high burn (red). Pairs with "Bug: $/h is wild" below.

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

## Bug: $/h is wild
The nuimber is up and down and all over the palce. I feel like it should be the average of an hour? or er min?
> Open. `burn_rate_per_hr` is an instantaneous per-tick delta (decays ×0.5) — volatile by construction.
> A rolling average over a window would steady it. Documented in the `herdr-status` skill.
