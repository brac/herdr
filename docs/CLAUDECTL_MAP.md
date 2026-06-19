# CLAUDECTL_MAP.md — the inherited data + render layer

> Phase 0 artifact (CLAUDE.md §1, §7). A map of what we forked from `claudectl`,
> what we deliberately left behind, and where the five load-bearing systems live.
> This is the reference a Phase 1 (project-first inversion) session reads first.

## Provenance

Vendored from `claudectl` (MIT, `github.com/mercurialsolo/claudectl`) at commit
`9c15a506452fc6df499c9241d2373256c2135197` (`v0.57.2-8-g9c15a506`). See `NOTICE`.

**Key recon finding:** the lean "<1MB, 6 deps, synchronous" claudectl described in
`CLAUDE.md`/`PROJECT_OVERVIEW.md` is the *writeup-era* version. Current claudectl is a
3-crate workspace whose **root binary** grew a local-LLM brain, an MCP agent bus, and a
coordinator/supervisor (`src/brain`, `src/bus`, `src/coord`) — pulling in tokio + rmcp +
rusqlite. Its own Cargo.toml admits enabling `bus` "relaxes the no-async-runtime invariant."
The workspace split (their epic #279) had already isolated the lean layer into two subcrates,
so the fork is **"vendor two subcrates + write a thin binary,"** not "fork a monolith and gut it."

## What we vendored vs. excluded

| Vendored (`crates/`) | Deps | Excluded (not copied) | Why |
|---|---|---|---|
| `claudectl-core` | serde, serde_json, libc, crossterm, ratatui | root `src/brain/` (local LLM) | async, SQLite, out of scope §1 |
| `claudectl-tui` | core + ratatui, crossterm, serde_json | root `src/bus/` (MCP agent bus) | tokio + rmcp, §3/§8 tripwire |
| `src/main.rs` (herdr, ~95 lines) | core, tui, ratatui, crossterm | root `src/coord/`, `src/orchestrator.rs` | DAG supervisor, §1 out of scope |

`claudectl-tui` is built **without** its `coord`/`relay`/`hive` features, so those UI panels
compile out. The upstream Brain Review screen (`src/brain_screen.rs`) lived in the binary and
was not vendored; herdr's draw loop falls through to the roster if that overlay is opened.

**Zero async / SQLite / MCP in the vendored tree** (verified). The one `tokio`/`async` grep hit
is a *sample-code string* inside `tui/src/demo.rs` fixtures — demo mode is never enabled by herdr.

## Crate layout

```
herdr/
├── Cargo.toml              # workspace + thin `herdr` binary; release profile §3
├── src/main.rs             # panic-safe alt-screen + 2s synchronous poll loop
├── examples/roster.rs      # non-interactive data-layer smoke test (no TTY needed)
└── crates/
    ├── claudectl-core/     # data layer (discovery, parse, status, terminals)
    └── claudectl-tui/      # App state + ratatui render (features off)
```

## The five load-bearing systems (CLAUDE.md §1)

### 1. Session discovery — `core/discovery.rs`
- `scan_sessions() -> Vec<ClaudeSession>` — reads `~/.claude/projects/<slug>/`; entry point.
- `cwd_to_slug(cwd)` (`discovery.rs:263`) — **the path hash. Inherit, never re-derive loosely (§8).**
  Trims trailing `/`, then replaces **every non-ASCII-alphanumeric char** with `-`, preserving case:
  `/Users/foo/bar` → `-Users-foo-bar`, `/.../Ben Bracamonte/...` → `-...-Ben-Bracamonte-...`. (The
  original `/`-only version was a latent bug — see the "Discovery fix" section below.) Has unit tests.
- `resolve_jsonl_paths()` — three-priority `--resume` fallback (own ID → resume UUID → newest `.jsonl`).
- `scan_subagents()`, `resolve_worktree_ids()` — subagent rollups + git-worktree identity.
- `projects_dir()` — `~/.claude/projects`.

### 2. JSONL transcript parser — `core/transcript.rs` + `core/monitor.rs`
- `transcript::parse_line(line) -> Option<TranscriptEvent>` — one JSONL line → structured event
  (role, stop_reason, per-tier `TranscriptUsage`, tool-use blocks, model). Metadata only, no content.
- `monitor::update_tokens(&mut session)` — **incremental** read: seeks from `jsonl_offset`, parses
  only new bytes, accumulates per-tier tokens. Resets accumulators when `offset > file_len`
  (truncation guard, §"Known hazards"). Calls `infer_status` at the end.
- Phase 4 note: parser keeps **only envelope metadata** today. The conversation panel will extend
  `TranscriptBlock`/`parse_line` to retain message text.

### 3. Status inference — `core/monitor.rs:310 infer_status()`
Five signals, precedence order: CPU >5% (Processing) → `waiting_for_task` (NeedsInput) →
telemetry unavailable + empty (Unknown) → assistant+`end_turn` (WaitingInput, or Idle if >10 min) →
**assistant + `tool_use` + CPU <2% + age >5s (NeedsInput — the invisible permission prompt, §"hazards")** →
else Processing. CPU is a 3-sample rolling average. `SessionStatus`: NeedsInput, Processing,
WaitingInput, Unknown, Idle, Finished (`session.rs:9`, `Display` impl at `:18`).

### 4. Roster render — `tui/src/ui/table.rs` (+ `detail.rs`, `status_bar.rs`, `help.rs`, `skills.rs`)
- `ui::table::render(frame, area, &app)` — the session table: PID, Project, Status, Context bar,
  Cost, $/hr, Elapsed, CPU%, MEM, In/Out tokens, activity sparkline + footer hotkeys.
- `App` (`tui/src/app.rs`) owns all state; `App::new()` → `refresh()` discovers live sessions and
  defaults `runtime` to `MockRuntime` (no brain/coord/bus needed).
- Render helpers on `ClaudeSession`: `display_name`, `format_cost`, `format_context_bar`,
  `context_percent`, `format_sparkline`, `format_tokens`, `format_mem`, `format_elapsed`.

### 5. Event loop — `herdr/src/main.rs` (replaces upstream `run_tui`)
`crossterm::event::poll` with a 2s tick → `App::tick()` (which calls `refresh()`) → `terminal.draw`.
Synchronous; no async runtime. Panic hook + explicit teardown restore the terminal (§3). Upstream's
`run_tui` swaps in a live brain/coord/bus runtime here — **deliberately omitted**; we keep the mock.

### Bonus: the UI↔runtime contract — `core/runtime.rs`
8 read-only view traits (`SessionSource`, `BrainView`, `CoordView`, `BusView`, `Actions`, …)
aggregated in `struct Runtime { Arc<dyn ...> × 8 }`. `App.runtime` is this aggregate;
`MockRuntime::default().into_runtime()` supplies inert impls. Live session data does **not** flow
through this — it comes straight from `discovery` in `refresh()`. The traits are dormant in herdr.

## Other core modules
`process.rs` (`fetch_and_enrich` — `ps` CPU/MEM, drops dead PIDs) · `terminals/` (capability matrix
+ keystroke injection: `detect_terminal`, `launch_session`, `switch_to_terminal`, `send_input`,
`approve_session`; backends: kitty/ghostty/iterm2/tmux/wezterm/warp/gnome/windows/apple — **Phase 2/launch**) ·
`launch.rs` (`LaunchRequest`, `prepare`, `launch` — **Phase 2**) · `theme.rs` (`Theme`, `status_color`) ·
`history.rs` (session history + `weekly_summary`) · `models.rs` (model profiles, `shorten_model`) ·
`config.rs`, `health.rs`, `hooks.rs`, `skills.rs`, `helpers.rs`, `logger.rs`.

## Orchestrator-strip audit (CLAUDE.md §1)

The §1 "rules engine + task orchestrator" (kill-on-cost, dependency-graphed multi-session launches)
**was never vendored** — it lived in the upstream binary crate (`coord/`, `orchestrator.rs`, `bus/`,
`brain/`). The strip happened cleanly at the **crate boundary**, on day one, as §1 asks.

Three inert residuals remain in core, kept intentionally (all TUI-coupled, zero runtime effect under
herdr's defaults) rather than carving up vendored `app.rs` now — they'll be excised during the Phase 1
app refactor (§9: inherit, don't rewrite):
- `runtime.rs` `Orchestrator` trait + `MockRuntime` no-op impl (mailbox/interrupt stubs).
- `rules.rs` (481 lines) — per-session auto-approve/deny/route evaluator. `App` calls `rules::evaluate`
  in `tick()` **only `if !self.rules.is_empty()`**; herdr leaves `app.rules` empty → never fires.
- `config.rs` `orchestrate` / `orchestrate_interval_secs` fields (default `false`; the loop they'd
  drive isn't vendored).
`main.rs` enables none of demo/rules/budget, so `App::new()`'s inert defaults hold.

## Phase 0 gate — PASSED

| Check | Result |
|---|---|
| Fork builds | ✅ debug 17.8s, release 30.1s, **zero warnings** |
| Binary size | ✅ **1.3 MB** release (lean; ratatui+crossterm dominate) |
| No async runtime | ✅ no `tokio`/`rusqlite`/`rmcp` in tree (§3) |
| Runs against live data | ✅ `examples/roster` discovered live session `29964 / herdr / Processing / $44 / 14% ctx` |
| TUI renders + input + clean exit | ✅ full frame via PTY; `q` quits, terminal restored (panic-safe §3) |
| Modules understood | ✅ this document |

## Phase 1 — project-first inversion (CLAUDE.md §2) — DONE

The spine is now **project-first**. New `claudectl-core/src/projects.rs` (`Project { path, name,
has_git }`, `scan(parent, include_non_git)`, `contains_cwd`) discovers project dirs by `read_dir` on
the parent herdr launches in — depth 1, `.git` filter by default (`--all` widens). `App` gained
`parent_dir` + `projects` (scanned each `refresh()`), a `with_parent()` constructor, and an inverted
`project_groups()`: it seeds one group per scanned project (zero-agent projects included) and attaches
each session by **canonical `cwd` path** match, with an `(other)` bucket for out-of-tree agents.
`ui::table::render` reuses the existing grouped layout, now with idle-project headers; title rebranded.

- **Gate met:** from `~/Work`, herdr shows **54 projects**, `herdr` active with its agent nested,
  53 idle projects below. `examples/projects` proves it TTY-free; clippy `-D warnings` clean;
  **132 tests** (121 core incl. 6 new + 11 tui); 0 async deps; 1.3 MB binary.
- **CLI:** `herdr [--all] [PARENT_DIR]` (no clap — std arg parse).
- **Deferred (not gate):** project-header collapse/expand selection (Phase 1.5); a spanning-width
  group-header row so long idle names don't truncate in the narrow Project column; excising the
  dormant `rules`/`Orchestrator` residuals (left untouched — `project_groups` no longer reads them and
  they remain inert under herdr's defaults; cleaner to remove alongside the Phase 4 app refactor).

→ Unblocks **Phase 2 (launch)**: `tmux new-window -c {project.path}` + `claude`, via the inherited
`launch.rs` / `terminals::launch_session`.
No new dependencies required; this is a pure data-model + render refactor on the inherited layer.

## Toolchain bump — ratatui 0.29 → 0.30 (crossterm 0.28 → 0.29) — DONE

Bumped ahead of Phase 2 to unblock eventually adopting `tui-overlay` (it needs the 0.30
`ratatui-core`/`ratatui-widgets` split; see the deferred-adoption note). The migration was nearly free:
**one** source change — ratatui 0.30 made `TableState: Copy`, so `table.rs` copies it out instead of
`.clone()` (clippy `clone_on_copy`). `Alignment` (now aliased to `HorizontalAlignment`) and
`highlight_symbol` still accept our usage unchanged.

- **Green:** clean debug+release builds, clippy `-D warnings` clean, **125 tests** (114 core + 11 tui).
  The 114 vs Phase 1's 121 is platform-gated `#[cfg(target_os = "macos")]` tests excluded under Linux,
  not a regression. Release binary **1.4 MB** (was 1.3). The `palette`/`csscolorparser`/`pest` color
  crates appear in `Cargo.lock` but are **locked-not-compiled** (ratatui 0.30 keeps `palette` off by
  default), so there was nothing to feature-trim.
- **Build note:** herdr only compiles under **WSL/Linux**, never native Windows MSVC — the inherited
  layer has unconditional Unix syscalls (`libc::getuid`/`kill`/`getppid`). Mac + WSL is the target.

## Phase 2 — launch into the selected project (CLAUDE.md §2, §7) — DONE

The launch plumbing was already inherited (`n` → `LaunchForm` → `launch::launch` →
`terminals::launch_session` → `tmux::launch` runs `tmux new-window -c {cwd} "claude …"`). The Phase 2
work was the missing **project-first selection** so you can launch into a project — including an idle,
zero-agent repo (the headline case):

- **`RosterRow` + `App::roster_layout()`** (`app.rs`): the new single source of truth for row order
  *and* selection — a `Header` per project group followed by its agents (grouped view) or just the
  visible agents (flat view). `table_state` now selects a **roster ordinal**, not a session ordinal,
  so project headers are navigable. `ui::table::render` consumes `roster_layout` directly (no more
  `selected_pid` → row matching), keeping order and selection in lockstep.
- **`selected_session()`** returns `None` on a header row (kill/switch/detail degrade gracefully);
  **`selected_launch_cwd()`** resolves the project path for a header, the owning project for an agent,
  or the agent's own cwd for an out-of-tree session. `next`/`previous`/`normalize_selection`/refresh
  seed all moved from `visible_session_count` → `roster_len`.
- **`enter_launch_mode()`** pre-fills `launch_form.cwd` from `selected_launch_cwd()`, so `n` launches
  into wherever the cursor sits; falls back to the CLI default `.` when nothing is selected.

- **Gate met:** from `~/Work`, all 11 sibling repos render as **navigable, launchable roster rows**
  (proven TTY-free by the extended `examples/projects`). clippy `-D warnings` clean; **130 tests**
  (114 core + 16 tui, +5 new for roster ordering, header-vs-agent selection, launch-cwd targeting,
  idle-project navigation, and the cwd prefill); no new dependencies.
- **Verified vs manual:** logic (unit tests) and the data/roster layer (live example) are verified.
  The true end-to-end "press `n`, watch an agent spawn" needs an interactive TUI inside a live `tmux`
  session — a manual step. Under WSL, herdr reads the **Linux** `~/.claude`, so agents must be started
  with `claude` *inside* WSL/tmux to appear.
- **Deferred (not gate):** a `tui-overlay`-style drawer for the launch modal (the inherited inline
  form stands in); prompt/resume UX polish; immediate post-launch refresh (the 2s poll picks up the
  new agent, per the §3 spawn-and-forget model).

## Discovery fix — cwd→slug now matches Claude Code exactly — DONE

Comparing against `agent-deck` surfaced a live bug in the inherited `discovery.rs::cwd_to_slug`: it
replaced only `/`, but Claude Code replaces **every non-ASCII-alphanumeric char** with `-` (verified
against real `~/.claude/projects/` dirs — `burnRat` keeps its capital R; the space in `Ben Bracamonte`
→ `-`). On any path with a space or `.`, herdr silently failed to match its transcript dir — true on
this dev machine. Fixed to preserve `[A-Za-z0-9]` and hyphenate the rest; +3 regression tests
(spaces, dots/underscores, case). This was a prerequisite for Phase 3: per-project git status is
pointless if agent discovery under those projects is broken.

## Phase 2.5 — event-driven refresh (CLAUDE.md §3, EVENT_LOOP_PLAN.md) — DONE

Polling lag is gone: a `notify` (fsnotify) watcher on its own thread feeds an `mpsc` channel that the
render loop drains, so an agent's JSONL write repaints the roster near-instantly instead of waiting up
to 2s. **Threads + channels only — no tokio, no async** (§3 amended to sanction this model).

- **`src/watcher.rs`** (new): `spawn(claude_projects, parent_dir) -> Option<(Receiver<()>, Watcher)>`.
  Watches `~/.claude/projects` **recursively** (every `<slug>/*.jsonl`) and the project `parent_dir`
  **non-recursively** (new/removed project dirs, without drowning in editors' file churn inside repos).
  Forwards only Create/Modify/Remove (drops Access/Other noise); init failure → `None` (degrade to
  polling, never crash). The `Watcher` guard is held for the loop's life.
- **`src/main.rs`** event loop: `event::poll(CHANNEL_POLL=200ms)` is the channel's poll granularity
  (crossterm can't select on an mpsc channel); keys still return instantly. Each iteration drains the
  channel into one coalesced `fs_dirty` flag → `app.tick()` on an fs event **or** the `TICK_RATE=2s`
  safety-net tick (which still drives `ps` enrichment, elapsed clocks, and the Phase 3 git throttle).
  Added a `needs_redraw` gate so the faster loop only repaints on change.
- **WSL caveat:** inotify is reliable on the **Linux** fs (`~/.claude` → `/home/...`), so agent
  activity is event-driven. inotify on the `/mnt/c` mount is unreliable, so a project **added/removed**
  under a `/mnt/c` parent is caught by the 2s safety-net tick instead — graceful, by design.
- **Gate met / green:** `notify` 8.2.0; release binary **1.53 MB** (was ~1.4; +~0.13 MB, within the
  agreed budget). **136 tests** (117 core + 16 tui + 2 watcher unit + 1 watcher e2e that writes a real
  file under a tempdir and asserts an event arrives — proves inotify delivers on this platform); clippy
  `-D warnings` clean. The true "watch an agent type and see the roster move" is a manual step needing a
  live agent in WSL/tmux.
- **Unblocks Phase 3:** git status rides the safety-net tick (not file-watched); see `PHASE3_PLAN.md`.

## Phase 3 — git light path (CLAUDE.md §4, PHASE3_PLAN.md) — DONE

Branch/dirty/ahead-behind per project, plus fire-and-forget push/pull. Shell out
to the `git` binary, parse stdout — no `git2`/libgit2, no diff/staging/rebase (§8).

- **3a — `core/git.rs`:** `GitStatus { branch, dirty, ahead, behind, upstream, bare }`
  + pure `parse_porcelain_v2()` + `status()` (one `git status --porcelain=v2 --branch`
  spawn; a second `rev-parse` only on failure, to render `(bare)` vs `None`). Defensive:
  failures → `None`, unknown lines ignored. Bare-repo guard from agent-deck's `isBareRepoSelf`.
- **3b — roster wiring:** `App.git_cache` (10s TTL, pruned, rides the safety-net tick — git
  is *not* file-watched) → `ProjectGroup.git`; the header renders `branch · ● dirty · ↑ahead
  ↓behind` themed (NO_COLOR respected). **Actionability sort** (agent-deck): active projects
  order by most-urgent agent (NeedsInput > Processing > … > cost), so a blocked agent floats up.
- **3c — push/pull:** `P`/`L` spawn `git -C {project} push|pull` with null stdio (never blocks the
  loop); `App.git_ops` tracks the `Child`, reaped each refresh via non-blocking `try_wait()`. On
  completion the project's cached status is evicted so the next refresh shows fresh ahead/behind.
  A hand-rolled braille throbber shows while in flight (main.rs forces repaints via `git_op_active`).
  No confirm modal (v1); no `tui-overlay`/`throbber-widgets-tui` dep (held the size line). Help
  overlay documents `P`/`L`.
- **Verified:** live `~/Work` shows real glances — `herdr [main ●]` (agents nested, slug fix
  paying off), `paperHands [main ● ↑19]`, `fallenFour [master ●]`, `agent-deck [main]` clean. Push
  is proven by a **hermetic E2E test** (local bare remote, no network): press push → a new commit
  lands on origin and the branch is no longer ahead.
- **Green:** **153 tests** (128 core incl. 11 git + 25 tui + 3 watcher); clippy `-D warnings` clean;
  release binary **1.55 MB**; **no new dependencies** (the git path is pure shell-out).
- **Deferred (not gate):** confirm-on-push modal; a `tui-overlay` drawer; richer git detail (it's a
  glance by design — full git is "open a tmux pane running gitui", §4).

## Phase 3 follow-up — git status moved off the render thread (perf fix)

Shipping 3b synchronously was a mistake on the WSL `/mnt/c` mount: `git status`
there costs 150ms–830ms+ per repo (vs ~5ms on a native SSD), so the 10s-TTL
batch recompute of all ~12 repos froze the UI for several seconds on each expiry
— "smooth, then a multi-second freeze, repeat." This violated §3 (slow work must
not block the render loop); the plan's "git is a cheap local read" held for
native fs, not drvfs.

Fix: a `GitStatusService` worker thread computes `git::status` off the render
thread; `refresh_git_cache` now only drains results from a channel and enqueues
stale paths (O(channel ops), never blocks). Spawn-and-forget, same pattern as
push/pull and the watcher — threads, no tokio (§3). Status trickles in over a
couple seconds in the background while the UI stays responsive.

Gap that let it through: tests ran `git` on one `/tmp` repo (fast); the example
did a single non-looping refresh, so the periodic freeze never surfaced.
Regression guard added: `git_status_is_fetched_off_thread_not_inline` asserts
`refresh_git_cache` enqueues rather than populating the cache inline. 154 tests.

## Phase 3 follow-up 2 — git status is fully event-driven (no polling at all)

The tiered-TTL sweep (idle 60s / focus 5s) was still periodic polling — the same
smell the watcher removed from agent status. Replaced with **purely event-driven**
git status: **no TTL, no timer ever re-fetches a repo.** `git_cache` is now
`HashMap<PathBuf, Option<GitStatus>>` (key present = fetched; `Some`/`None` =
status/failed). A project's status is (re)fetched on the background worker only on:
1. **First appearance** — `refresh_git_cache` enqueues any project with no cache
   entry (initial populate + newly cloned repos). Cached projects are never
   re-enqueued here, so an idle roster generates **zero** background `git`.
2. **Landing on its row** — `next`/`previous` call `enqueue_selected_git` ("passing
   over a row polls it"), re-fetching even if cached.
3. **In-app push/pull completion** — `poll_git_ops` re-enqueues that project.
4. **Manual `r`** — `enqueue_all_git` re-fetches every repo.

All fetches dedup via `git_inflight` and run on the worker thread (never blocks).
Guards: `cached_git_project_is_not_re_fetched_on_refresh` (no periodic polling) and
`navigating_onto_a_row_re_fetches_its_git_status`. Supersedes the 10s/60s TTL notes
above. 156 tests; clippy clean.

Note: an agent actively editing files in a **non-selected** repo won't update its
dirty flag until you land on it or press `r` — an accepted trade for zero idle churn.

### Addendum — agent-activity trigger

Added a 5th event trigger: in `refresh()`, a session whose `jsonl_offset` advanced
(real transcript activity) maps via its cwd to its owning project, which is
re-fetched (`enqueue_git_for_cwds`). So an agent editing files in a **non-selected**
repo now updates that repo's dirty flag live — closing the earlier trade-off.
Still event-driven (driven by transcript writes, not a timer); `git_inflight` dedup
means a streaming agent triggers at most one `git status` at a time for its project.
Guard: `agent_activity_re_fetches_its_project_git`. 157 tests.
