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

## Feature: Claude window border - DONE
Can tmux add a border to the claude window just as herdr has it's own window title and border ie 
-herdr-----
> Resolved by composing tmux (no rendering, §0.1): when an agent is staged, herdr sets
> `pane-border-status top` + a `#{pane_title}` border-format on its window and titles the agent pane
> "Claude — <project>" (and its own "herdr"), plus a **heavy** border line and Dracula colors
> (`pane-active-border-style` purple = focused, `pane-border-style` comment = idle). The agent's
> top-edge border is the divider under herdr, so it reads like `-herdr-`. Options unset on unstage.
> `terminals::set_stage_title`/`clear_stage_title` → `tmux::set_stage_border`.
> Note: the **left/right/bottom** edges stay unbordered — they're the terminal's outer edge, where
> tmux draws nothing, and herdr can't render a box *into* Claude's pane without becoming a terminal
> emulator (§8). A full 4-sided frame isn't reachable within the design.

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

## Feature: Add Mouse Scrolling - DONE (tmux-native)
set -g mouse on
Mouse scroll works on roster page but in agent window it scrolls the messages i eneted. Can we make the scroll wheel scroll the responses like a normal agent window
> Resolved the tmux-native way: `quickstart.sh` now runs `tmux set -g mouse on`, so wheel-scroll over
> the staged agent pane enters tmux copy-mode and scrolls its responses. herdr deliberately does **not**
> call crossterm `EnableMouseCapture` — that would fight tmux's mouse handling in herdr's pane and lose
> native text selection (§0.1, tmux owns window management). Add `set -g mouse on` to `~/.tmux.conf` to
> make it permanent.

## Feature: Mouse Focus - DONE (tmux-native)
Is this possible without betraying our design doc? I want to be able to click between the herdr window and the chat window
> Yes, without betraying the doc: it's tmux's `set -g mouse on` (now set by `quickstart.sh`) — click a
> pane to focus it. herdr stays out of mouse capture so tmux owns click-to-focus, fully aligned with
> §0.1. Pairs with "Add Mouse Scrolling" above.

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

## Feature: Model identification - DONE (detail view)
Each agetn should have a indicator about which model they are current running.
> Resolved via the existing **roster detail** (`Enter` → `ui/detail.rs` shows "Model: …", already
> shortened by `models::shorten_model`). Per user decision, deliberately **not** added as a roster
> column to keep the row uncluttered.

## Bug: Reszing the window changes the agent window size permenantly - DONE
When reszing the window smaller they resize properlly. When resize the window larger with both the roster and the agent window open, then the agent window stays the compressed size while herdr gets bigger. Upon getting larger the agent window should take more of the screen space when possible while still maintaiing the min-height for the roster
> Resolved: `Event::Resize` only repainted — `resize_stage_top` was never re-issued, so tmux kept the
> old height and a grown window left the agent pane compressed. The event loop now calls
> `App::refit_stage()` on resize, which re-fits herdr to its roster height (capped to preserve the
> agent's min) when an agent is staged. Gated on "staged" so it never fights a manual `Ctrl-b` resize.
> (`src/main.rs`, `app.rs`.)

## Bug: Roster selection bar should stay with last repo selected when launching an agent. - DONE
Launhcing an agent in the roster view with n. The repo will sort to the top with the now active agent, leaving the roster selection on an unintended repo. The selection bar should follow the now sorted repo. 
> Resolved: selection is now sticky by *stable identity*, not row index. `refresh()` captures a
> `RosterSelKey` (project path for a header, PID for an agent) before the re-sort and re-anchors the
> cursor onto it afterward (`selection_key` / `reselect_by_key`). So after `n` the cursor follows the
> launched repo to the top — and the roster no longer drifts under you on any status-driven re-sort.
> Test `reselect_by_key_follows_the_same_entity_across_a_resort`.

## Bug: Reoster bar is too small with only one or two active agents - DONE
The roster bar should be a minimum for 12 rows when there are a small number of active agents. Ruight now there is one active agent and an open chat window and I see 7 rows. Header, Columns names, 2 repos, session overview and fleet bar.
> Resolved: `stage_top_rows()` floor raised 6 → 12 rows. `terminals::cap_stage_top_rows` still trims it
> on short terminals so the staged agent pane keeps its minimum, so the floor never starves the agent.

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

## Bug: Sparklines - DONE
The per agent sparklin looks like it only has two states, on and off
The fleet sparkline has all sorts of varitaion. Make the roster sparkline more granula like the fleet sparkline
> Resolved: `activity_history` now stores a continuous **CPU% per tick** (`Vec<f32>`) instead of a
> discrete status level that only ever produced 1–2 bar heights. `format_sparkline` scales each bar
> against a 100% ceiling (a multi-core spike clamps to full, idle ticks blank), giving fleet-strip-like
> granularity. Test `sparkline_scales_cpu_to_block_heights`. (`session.rs`.)

## Bug: Opus 4.8 (and unrecognized models) over-priced ~3× - DONE
Surfaced by the comparable-tool research (`docs/COMPARABLES.md`, 2026-06-20). `models.rs::shorten_model`
collapses any `…opus-4-8…` (or 4.5/4.7) to `"opus"`, which resolves to the `opus-4.6` profile = $15/$75
per M. Real Opus 4.8 is ~$5/$25 per M. Every model that isn't opus/sonnet/haiku also lands on the Opus
fallback profile, over-counting cost. herdr is the binary you run *on* Opus 4.8 and it mis-costs itself.
> Resolved: rewrote `built_in_profile` to current Anthropic list pricing (Opus 4.5–4.8 $5/$25, Sonnet
> 4–4.6 $3/$15, Haiku 4.5 $1/$5; cache read 0.1× / 5m write 1.25× input — verified against the
> `claude-api` reference), matched by model **family prefix** so every version/date/`[1m]` suffix
> resolves. Replaced `shorten_model`'s opus-only special-case with a boundary/version-aware matcher
> (`family-major.minor`, split on non-alphanumerics so date stamps and `[1m]` don't confuse it). The
> fallback now prices at mid-tier (Sonnet) not Opus, still flagged `cost_estimate_unverified`. Tests
> `opus_48_priced_at_current_rate_not_legacy`, `shorten_model_extracts_family_and_version` (`models.rs`).

## Bug: Token over-count from streaming-duplicate JSONL lines - DONE
Surfaced by the same research. Claude Code writes the *same* assistant message's `usage` on multiple
physical JSONL lines during streaming; herdr accumulates every usage line (no dedup), so multi-line
turns inflate token + cost totals. Both ccusage (`mod.rs:292`) and tokscale (`claudecode.rs:498`)
document and handle this.
> Resolved: `transcript.rs` now captures `message.id` + top-level `requestId`; `monitor.rs` keys each
> usage line by `(message.id:requestId)` and **max-merges** per tier (`merge_usage`), adding only the
> increase over any prior emission to the running totals — on both the own-agent and subagent paths.
> Dedup state persists across incremental ticks (carried by `merge_discovered_session`), resets on
> `/clear` (fresh session) and on file truncation. Tests `merge_usage_dedups_streaming_duplicates`,
> `merge_usage_keyless_lines_add_in_full` (`monitor.rs`).
> Deferred (lower value once the table above is correct): `CostMode::Auto` (trust `costUSD` — present
> only sporadically in transcripts, and our computed cost now matches it); the 5m/1h cache-creation
> split (real but ~1.6× on a minor tier); a `>200k` long-context tier (**N/A** — current Opus/Sonnet
> 1M context is flat-priced, no premium). User-configurable price overrides need a config-file
> subsystem herdr doesn't have yet (`models::set_overrides` is wired but unused).

## Feature: Inbound permission-prompt hooks (status from fact, not guess) - DONE
herdr *infers* the invisible permission-prompt NeedsInput state (`<2% CPU + stale tool_use`). Claude
Code's `Notification` hook fires *exactly* when an agent needs permission/input — make it authoritative.
(COMPARABLES §7 item 5; whiteroomed from claude-tui's hooks-as-push-signal.)
> Resolved (Phase B): opt-in, merge-safe. `herdr hook install` merges `Notification`/`Stop` hooks into
> `~/.claude/settings.json` (idempotent; preserves the user's other hooks; `herdr hook uninstall`
> reverts). The hook runs `herdr hook notify`, which reads the payload on stdin and atomically writes
> `~/.claude/herdr/<session_id>.json` (`Notification`→NeedsInput, `Stop`→JobDone). herdr's `notify`
> watcher now also watches `~/.claude/herdr/`, and `monitor::apply_hook_override` treats a *fresh* status
> file (newer than the transcript mtime, <5min old) as authoritative — self-clearing once the agent
> writes new JSONL, so no one has to delete the file. Works without hooks installed (pure no-op).
> New `core/src/hookstate.rs` + `src/hookcmd.rs`; tests `state_and_settings_under_temp_home`,
> `read_missing_is_none`; verified end-to-end against a temp HOME. **Usage:** run `herdr hook install`
> once, then restart Claude Code sessions. Deferred: agent-deck's "acknowledged-downgrades-waiting"
> (stop nagging once you've attended an agent) — the override already self-clears on the agent's next
> JSONL line, so this is a minor polish; and stale-file cleanup for dead sessions.

## Feature: Phase C tmux orchestration — INVESTIGATED, mostly N/A for herdr
COMPARABLES §7 items 6–7 (tmux `-C` control-mode refresh + low-latency send-keys; orphan-pane
recovery) were borrowed from agent-deck. On investigation against herdr's actual architecture, both
are obviated — herdr is **not** agent-deck:
- **Orphan recovery — already covered.** `~/.claude/sessions/<pid>.json` is written by **Claude Code
  itself** (v2.1.183: `sessionId/cwd/startedAt/status/name/…`), so herdr's `discovery::scan_sessions`
  already finds *every* running agent — herdr-launched or manual — and maps each to its tmux pane via
  TTY (`ps` → `pane_for_tty`). agent-deck needs orphan recovery only because it owns named
  `agentdeck_*` tmux **sessions**; herdr owns none (§0.1/§8) and discovers via a restart-independent
  registry. No gap.
- **tmux `-C` control mode — doesn't earn its complexity.** herdr's refresh signal is JSONL writes,
  caught reliably by the `notify` watcher (transcripts live on the **Linux fs** `~/.claude/projects`,
  not the flaky `/mnt/c` mount), and herdr sends whole *prompts*, not keystrokes — so neither the
  `%output` refresh nor the persistent-`send-keys` benefit applies. A persistent `tmux -C` subprocess
  + protocol parsing would add real complexity (and edge toward §8) for ~zero gain. Skipped per the
  plan's "commit only if it earns its complexity" gate.
> Shipped the one real adjacent win the investigation surfaced: herdr parsed *past* Claude Code's
> session `name` (it read only pid/sessionId/cwd/startedAt). `discovery::scan_sessions` now lifts
> `name` into a new `ClaudeSession.cc_name`, shown read-only in the **detail view** under Project
> (e.g. "audit-model-pricing-bugs") so multiple agents in one repo are tellable apart. Kept separate
> from `display_name()`/`session_name` so it never disturbs history's per-project cost folding.
> Follow-up (UX decision, not done): promote `cc_name` to the roster agent-row label and the staged-
> pane border title.

## Feature: Phase D graphs — daily activity heatmap (D1 done; D2/D3 deferred)
COMPARABLES §7 items 8–10. Applied the plan's own gate ("each graph reflects real data and is
something we actually glance at; defer any that don't earn the space").
> Shipped D1 — **daily activity heatmap** on the fleet strip (`G`). New `history::daily_cost_series`
> + `history::intensities` (the 5-level GitHub-contributions bucketer vendored from tokscale's
> `calculate_intensities`: ≥0.75→4 … >0→1, else 0) → `history::daily_activity(ACTIVITY_DAYS=14)`.
> Cached on `App.daily_activity`, refreshed on the `weekly_summary` cadence (~30s) so it never reads
> `history.csv` on the render path. `ui/fleet.rs` renders one cell per UTC day (`· ░ ▒ ▓ █`, shaded by
> that day's spend vs. the busiest day) — the "days" time axis the live burn sparkline (~30s) can't
> show. Tests `intensities_bucket_by_ratio_to_busiest_day`, `daily_cost_series_length_matches_window`,
> `heatmap_maps_levels_to_shades`. (Populates as sessions complete — same `history.csv` the all-time/
> weekly totals already use.)
> Deferred with rationale:
> - **D2 (P90 personalized limit + 5h-block projection)** — would introduce a 5-hour "block" entity in
>   tension with the project-first model (CLAUDE.md §2); the plan-limit tables are community guesses;
>   and the JSONL limit-notice regexes can't be verified without a real "limit reached" sample. Heavy +
>   speculative for a narrow quota-prediction payoff.
> - **D3 (turns-until-compaction / wasted-context)** — value dropped sharply with 1M-context models
>   (auto-compaction is now rare), and the detail view already shows context %/bar. Marginal vs. effort.
> Follow-up if wanted: a full multi-row GitHub-style heatmap overlay (the `intensities` bucketer is
> already in place and tested, so it's mostly render plumbing).

## Feature: Phase E roster UX cheap wins (E1 + E2 done; E3 deferred)
COMPARABLES §7 tail.
> Shipped:
> - **E1 — Error badge on project headers.** The header already flags blocked agents (`⚠N`); now it
>   also flags errored ones with `✕N` (red, `t.status_error`), shown only when a project hosts an
>   agent in `SessionStatus::Error` — same tasteful, only-when-relevant pattern. (Chose this over the
>   full per-status count breakdown agent-deck shows on *collapsed* groups: herdr always shows the
>   agent rows, so a full breakdown is redundant clutter — and the user keeps the roster lean.)
>   `ui/table.rs`.
> - **E2 — status filter quick-keys.** `!`=needs-input `@`=processing `#`=waiting `$`=idle jump
>   straight to a filter (vs. `f` which cycles); pressing the same key again toggles back to All for a
>   clean on/off (`App::set_status_filter`). Documented in the `?` help overlay. `app.rs`. Test
>   `status_quick_key_sets_then_toggles_back_to_all`.
> Deferred: **E3 (freshness `[Nm ago]`)** — herdr's roster is event-driven (notify watcher + 2s
>   safety tick), so it's near-always fresh; a staleness indicator (valuable for vscode-claude-status's
>   *network* panel) would read "now" almost always here. Low value.

## Bug: If an anget process Job Done then the fleet count should be idle +1
what is the difference between idle and waitign anyway? And jobs done? I feel like I am conflating terms

## Bug: Approve / Deny not visible on roster view
I think we have the functionlailty but I don't see any notification that an agent is asking for approve deny or some other reponse from the user before proceeding. If its just approve deny then I would like to be able to anserw that from the roster view. Other questions the user needs to go to the claude window for more details

## Feature: Color the agents in roster
The agent names, which seem to be just the same as the project that they were stated on are just regualr gray. Can we use a more poppy dracula themeed color? I want those to stand out more

## Bug: Is that hooks thing installed?
I thgouth that we had a hook to tell when an agent does stuff so we could more relaiably determine when an agent needs direction or is done with the job etc. How do we install that? target/release/herd install ?

## Bug: Sub-agents never appeared in the roster + their tokens vanished - DONE
When an agent spawns sub-agents (e.g. Explore agents in a code review), I want them tabbed under the
orchestrating agent with their own cost/context, their token spend rolled into the parent's counters
(kept even after they finish and disappear), and reflected in the today/wk/all fleet counters — which
were also constantly 0.
> Root cause: the sub-agent structs (`SubagentRollup`/`subagent_breakdown`/`subagent_row`) were all
> built and correct, but fed by a **dead discovery path**. `discovery::scan_subagents` scanned
> `/tmp/claude-{uid}/{slug}/{sessionId}/tasks/*.jsonl` ("Feature #29", inherited from claudectl) — but
> Claude Code v2.1.x stores only `.output` scratch there. Real sub-agent transcripts live at
> `~/.claude/projects/{slug}/{sessionId}/subagents/**/agent-*.jsonl` (+ `agent-*.meta.json`). So
> `active_subagent_count` was always 0; no rows, no token attribution. Measured on live data: **409M
> tokens across 491 sub-agent transcripts** (2.45M of it *output*) were being silently dropped.
> Fixes:
> - `scan_subagents` now derives the dir from `jsonl_path` (resume-safe) and recursively collects
>   `agent-*.jsonl` (skips `.meta.json`), splitting **all-discovered** (`subagent_jsonl_paths`, drives
>   rollup so a sub-agent that came and went is still counted) from the **fresh-by-mtime active subset**
>   (`active_subagent_jsonl_paths`, < `SUBAGENT_ACTIVE_SECS`=25s → individual live rows; older collapse
>   to "completed (N)"). `discovery.rs`.
> - `monitor::refresh_subagent_rollups` iterates the full set; `update_subagent_rollup` resolves a
>   human label once from the `agent-*.meta.json` sidecar (`agentType · description`, e.g.
>   "Explore · Map launch→discovery"). Rollup tokens/cost already fold into `session.cost_usd`/totals
>   via `finalize_usage`, so the parent agent row + fleet live-cost already include sub-agents.
> - Fleet today/wk/all were CSV-only (`history.csv`, written only at `Finished`) → 0 for never-finished
>   agents. New `App::fleet_totals()` folds live (active-session) cost+tokens into today/wk/all (skips
>   `Finished` to avoid double-count); wired into `ui/fleet.rs` strip + `ui/table.rs` title.
> Tests: `scan_subagents_finds_agent_jsonls_and_splits_active_by_mtime`,
> `subagents_dir_derives_from_resolved_transcript_path`, `scan_subagents_clears_when_no_subagents_dir`,
> `resolve_subagent_label_reads_meta_sidecar`, `truncate_label_*`, `fleet_totals_fold_live_spend_and_skip_finished`.
> Deferred: per-sub-agent **context %** (the rollup accumulates deltas, not last-call absolute context,
> so the sub-agent rows show cost/tokens but `-` for context); a cap on simultaneous active rows if a
> wide fan-out ever floods the roster (YAGNI).

