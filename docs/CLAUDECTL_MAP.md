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
- `cwd_to_slug(cwd)` (`discovery.rs:263`) — **the path hash. Inherit, never re-derive (§8).**
  Trims trailing `/`, replaces `/` → `-`: `/Users/foo/bar` → `-Users-foo-bar`. Has unit tests.
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

## Phase 1 hand-off — the project-first inversion seam (CLAUDE.md §2)

Today the spine is **session-first**: `App.sessions: Vec<ClaudeSession>`, rendered flat by
`ui::table::render`. Phase 1 inverts to **project-first**:
1. New top-level entity = a project dir from `read_dir` on the parent the tool launches in
   (default filter: dirs containing `.git` — observed **53 of 77** dirs under `~/Work`; resolves
   open-question #1). Depth: one level (resolves #2).
2. Group existing `ClaudeSession`s under their project by matching `session.cwd` → project path
   (reuse `cwd_to_slug`'s inverse intuition; the cwd is already on each session).
3. Rewrite `ui::table::render` into a project roster, each row expandable to its 0..N agents.
4. While rewriting `app.rs`, excise the three dormant residuals above.
No new dependencies required; this is a pure data-model + render refactor on the inherited layer.
