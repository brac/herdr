# BACKLOG

_Reconciled 2026-06-19. Status markers: **DONE** / **PARTIAL** / open (no marker).
CLAUDE.md §7 build phases (0–5) are all complete; everything here is post-phase work._

## Bug: Auto Height - DONE
IF the parent project has a lot of repos, then the herder bar can extend too far when trying to start a chat window.
The auto heigh feature should not count unactive repos, or repos without any active agents in them. The auto hieght features
should only make room for showing repos with active agents, leeaving the rest of the screen space to the change window
> Resolved: `stage_top_rows()` counts only agent-bearing project rows (test `stage_top_rows_ignores_idle_zero_agent_projects`).

## Feature: Tabbed Agents - DONE
When an agent is active in a project, I want it indnetd so I can clearly see that the agent is under that project.
If an agent opens sub agents under it's perview, if possible I want those indended in that agent which is indented 
under the project. 
> Resolved: agent rows are now indented (`AGENT_INDENT`) under their project header in grouped view,
> and sub-agents nest one level deeper still (their `├─`/`└─` tree glyphs were already there), so the
> roster reads project → agent → sub-agent. Indent is suppressed in flat view (no header to nest under).
> `ui/table.rs` (`session_row`/`subagent_row`).

## Bug: Status and Context is sometimes incorrect - DONE
I select a row in herdr, I press n to start an agent, it sarts and in herdr is report 15% context used up and processing already?
I haven't even asked a question yet
> "Processing already" half resolved: instantaneous CPU now seeds idle on a fresh agent's first sample
> instead of inheriting `ps %cpu`'s lifetime average (`fix:` 1e54cd7). The "15% context" half is
> **not a bug**: that's real consumed context — the first API call already includes the system prompt,
> tool definitions, CLAUDE.md, and env, which on a 200k model lands ~15% before you type anything
> (`monitor.rs:146` tracks the last call's input+cache tokens). Left as-is rather than hiding accurate
> data. See the `herdr-status` skill.

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

## Feature: Persistent cost - DONE
A value that reflects the cost over multiple (all) sessions. For the repo collection as a whole and individual projects. 
> Resolved: `history::all_time_summary()` rolls the full history CSV into collection-wide totals **and**
> a per-project map (`ProjectTotal`). The fleet strip shows **all $X (tokens)** for the collection;
> each project header shows its all-time spend as **Σ$X** (matched by name; shown even for idle projects
> with past sessions). Cached on `App.all_time_summary`, refreshed on the weekly-summary cadence (~30s).
> Limitation: per-project match is by `display_name`, so a custom session name won't fold into the dir.

## Feature: Quickstart - DONE
A script to quickly launch tmux and start herdr
tmux attach -t work
cargo run --release -- "/mnt/c/Users/Ben Bracamonte/Work"
> Resolved: `./quickstart.sh [PARENT_DIR]` — rebuilds release (never a stale binary), then creates or
> reuses a tmux session (default `work`, override `HERDR_SESSION`) with herdr running over PARENT_DIR
> (defaults to the repo's parent dir; handles paths with spaces). Switches client if already in tmux.

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

## Feature: wk token tracker - DONE
We need a way to track token usage over multiple sessions
> Resolved: the fleet strip now surfaces **wk tokens** alongside wk cost, plus **all-time tokens** in the
> new all-time segment (`ui/fleet.rs`, `fmt_tokens`). Multi-session token totals come from
> `history::weekly_summary.total_tokens` and `history::all_time_summary().total_tokens`.

## Bug: Asking for permission in roster doesnt work - PARTIAL
when waiting for an approavl to run a tool, there is no indicator in herdr roster view that it needs attention or can be arrpoved. Sometimes I see a 
purple Need Input notice, but then it kept going. maybe becaues I was on auto mode?
> Partial: Phase 4c added the **approval inspector** (`A`) — `tmux capture-pane` shows the real permission
> dialog (invisible in JSONL) with approve/deny/interrupt from the roster (`feat:` 581c86d). NeedsInput
> detection also tightened via the status work. **Persistent indicator now added** (`ui/table.rs`): a
> bold `⚠N` badge on any project header hosting N agents awaiting approval, plus a leading `⚠` on the
> agent's Status cell (visible in NO_COLOR too). Still open: the auto-mode "kept going" interaction.

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

## Feature: Split waiting states - DONE
If the agent is waiting for a repsonse from the API, the roster displays Waiting. The a task is complete the roster also dispalys Waiting. When a task is complete and the agent is waiting for the next task from the usr, then I want that task to be Job Done. So we can tell when we are waiting on the API and when we are waiting ont he user
> Resolved: new `SessionStatus::JobDone` ("Job Done", cyan). `infer_status` now splits the two:
> assistant `end_turn` (turn complete, your move) → **Job Done**; a `user`/tool_result line where Claude
> still owes output (request in flight) → **Waiting**. (`monitor.rs`; tests
> `assistant_end_turn_reads_as_job_done_not_waiting` + the existing user-branch tests.)

## Feature: Error Display - DONE
Somtimes we hit an API error. The Roster should display Error in the stats when this is encounted
> Resolved: new `SessionStatus::Error` (red, sorts just below Needs Input). Claude Code writes API
> errors as a `<synthetic>` `isApiErrorMessage` line ("API Error: 529 Overloaded…"); `transcript.rs`
> emits a `TranscriptEvent::ApiError`, `monitor.rs` keeps it sticky (`last_was_api_error`) until a
> newer message supersedes it, and `infer_status` reports Error when CPU is low (an active retry stays
> Processing). Tests `api_error_flag_reads_as_error_when_idle`, `active_retry_after_error_reads_as_processing`.

## Bug: Sparklines
The per agent sparklin looks like it only has two states, on and off
The fleet sparkline has all sorts of varitaion. Make the roster sparkline more granula like the fleet sparkline