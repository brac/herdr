# docs/COMPARABLES.md — competitive/technique research

Research pass over seven adjacent tools that solve overlapping problems (monitoring Claude
Code agents, costing token usage, operating multiple agents). Goal: note what each does well
and what herdr should **vendor** (lift code) vs **whiteroom** (re-derive the idea fresh),
graded against herdr's invariants (§3/§8 of `CLAUDE.md`: synchronous, no tokio, shell-out
don't vendor, lean deps, defensive parsing, compose tmux never reimplement it).

All repos cloned adjacent to herdr under `../`:

| Repo | Path | Lang | Lineage / surface |
|---|---|---|---|
| claudectl | `../claudectl` | Rust | `mercurialsolo/claudectl` — herdr's actual upstream base (already vendored into `crates/`) |
| agent-deck | `../agent-deck` | Go | `slima4` fork — multi-agent tmux session manager (closest **surface** comp) |
| claude-tui | `../claude-tui` | Python | `slima4` — statusline + live monitor + analytics + **hooks** (closest sibling philosophy) |
| ccusage | `../ccusage` | Rust | `ccusage/ccusage` — now a **Rust** cost engine (closest **cost** comp) |
| Claude-Code-Usage-Monitor | `../Claude-Code-Usage-Monitor` | Python | `Maciek-roboblog` — real-time usage w/ predictions + warnings |
| tokscale | `../tokscale` | TS+**Rust** | `junhoyeo` (MIT) — 40-tool usage analytics + contributions heatmap |
| vscode-claude-status | `../vscode-claude-status` | TS | `long-910` — VSCode status-bar usage meter |

> Naming note: the prompt's "slima4 / claudectl" conflates two authors. **claudectl** is
> `mercurialsolo`'s (the Rust base herdr forked). **slima4** authors `claude-tui` and the
> `agent-deck` fork. `slima4/claudectl` does not exist.

---

## 0. herdr's current state (verified against source, so recommendations aren't redundant)

- **Pricing** (`models.rs`): tiny hardcoded table — only `opus`/`opus-4.6`, `sonnet`/`sonnet-4.6`,
  `haiku` + an override map + a fallback. `shorten_model` is a 3-way substring match on
  `opus`/`sonnet`/`haiku` with a single special-case for `4-6`.
- **Cost mode**: herdr **always recomputes from tokens** (`estimate_cost_components`,
  `monitor.rs:680`). It never trusts a `costUSD` field from the transcript.
- **Dedup**: **none.** Reads incrementally by byte offset (`jsonl_offset` + `SeekFrom::Start`,
  `monitor.rs:660+`) so it sees each *physical* line once, but does **not** dedup by
  `(message.id, requestId)`.
- **Burn rate**: real-elapsed `cost_usd / elapsed_hrs` (`health.rs:207`). No 5h-window / projection.
- **Parsing**: `serde_json::Value` + `.get().and_then()`, degrades to `Unknown` — defensive,
  matches doctrine. Incremental byte-offset tail is **better** than every comparable here (they
  all full-reparse).
- **Status inference** (`monitor.rs`): multi-signal (CPU > stop_reason > tool_use > age) +
  invisible-permission-prompt heuristic. **Ahead of every comparable.**
- **Hooks** (`hooks.rs`): herdr **fires outbound** hooks on state change (runs user commands). It
  does **not** install **inbound** Claude Code hooks to receive push signals.
- **Subagents**: already rolls up subagent tokens (`SubagentRollup`).

### ⚠️ Two concrete bugs this research surfaced

1. **Opus 4.8 (and 4.5/4.7) is over-priced ~3×.** `shorten_model("…opus-4-8…")` → `"opus"` →
   the `opus-4.6` profile = **$15/$75 per M**. Real Opus 4.8 ≈ **$5/$25 per M** (per ccusage's
   table, `pricing.rs:615`). Every non-{opus,sonnet,haiku} model also falls back to Opus pricing.
   herdr is currently the binary you run *on Opus 4.8*, costing itself wrong. **Fix: adopt
   ccusage's per-model Claude table.**
2. **Likely token over-count from streaming-duplicate lines.** Both ccusage (`mod.rs:292`) and
   tokscale (`claudecode.rs:498`) document that Claude Code writes the **same** assistant
   message's `usage` on multiple physical JSONL lines during streaming. herdr accumulates every
   usage line → double-counts. **Fix: dedup/merge by `(message.id, requestId)`.**

---

## 1. ccusage — the cost engine (Rust, sync, lift-ready) ⭐ highest-value

ccusage is **now a Rust binary** (`../ccusage/rust/crates/ccusage/`), synchronous + `std::thread`,
lean deps (`serde`, `memchr`, `rustc-hash`, `smallvec`, `compact_str`). Architecturally a
parallel-universe herdr; its cost layer is directly portable.

**VENDOR (lift algorithm, adapt to herdr modules):**
- **Per-model Claude pricing table** — `pricing.rs:564+` (`put_builtin_pricing`). Already includes
  `claude-opus-4-8` at $5/$25, cache 6.25/0.5. Copy near-verbatim → fixes bug #1. *(Re-verify
  numbers against current Anthropic docs before shipping — don't trust any single source's table.)*
- **Tiered per-token cost** — `cost.rs:84-148`. Adds the **>200k-context tier** (1M Sonnet/Opus)
  and the **5m vs 1h cache-creation split** herdr lacks. ~60 LOC, no deps.
- **CostMode::Auto** — `cost.rs:23`. Trust transcript `costUSD` when present, else compute. herdr
  always computes; Auto is more accurate when Claude writes the field.
- **`(message.id, request_id)` dedup hash** — `mod.rs:292` (FxHasher). Fixes bug #2.
- **Boundary/version-aware model-name matcher** — `pricing.rs:363-455,1085-1166`. So
  `claude-opus-4` doesn't collide with `claude-opus-4-5`, but date suffixes (`-20250514`) collapse
  to base. Replaces herdr's brittle `shorten_model`.
- **Burn-rate + projection-to-window-end + token-limit status** — `blocks.rs:535-569`. A ready
  model for a "projected 5h cost / will you hit the cap" panel (Phase 5).
- **Defensive `lenient_*` serde deserializers** — `adapter/jsonl.rs`. Reinforce "degrade not crash."

**WHITEROOM / SKIP:**
- Full-file `fs::read` re-parse — herdr's incremental tail is better; don't regress. (Do borrow the
  `memmem "usage":{` prefilter + `from_slice`-into-typed-struct micro-opts *inside* herdr's reader.)
- Runtime `ureq` pricing fetch, `sqlite` dep, `jiff` TZ, the codex/gemini/copilot adapter tree.
- ccusage punts on `cwd→slug` (treats slug as opaque) — herdr already owns the real hash.

**Build pattern worth copying:** `build.rs` fetches a *pinned* LiteLLM price JSON at build time,
compacts keys, `include_str!`s it → always-offline price table, zero runtime network. Optional.

---

## 2. claude-tui — hooks as a push signal (Python) ⭐ highest-value idea

Monorepo of stdlib-only single-file tools. Same author lineage, same sync/defensive DNA, but
**session-first** and — critically — its status model is **clock-only** (no CPU, no stop_reason,
**no permission-prompt detection**). herdr's status model is strictly ahead; do **not** regress.

The gold here is **hooks as a *push* channel**, which herdr does not yet consume:

- claude-tui installs `SessionStart`/`PreToolUse`/`PostToolUse` hooks that read the stdin payload
  (`{cwd, tool_input.file_path, …}`) and inject context (`claude-code-hooks/*.py`,
  `install.sh:254-312`). It uses hooks only to *inject*, never to feed its own monitor — **that gap
  is herdr's opportunity.**
- **Claude Code's `Notification` hook fires exactly when an agent needs permission/input** — the
  canonical signal herdr currently *infers* from `<2% CPU + stale tool_use`. **A herdr-installed
  `Notification`/`Stop` hook that writes a tiny `~/.claude/herdr/<session>.json` would be picked up
  by herdr's existing `notify` watcher** → promotes the permission heuristic from *guess* to *fact*,
  zero new deps, zero polling, fully inside the sync/lean invariants.
- Install mechanics to copy: merge-not-clobber into `settings.json`, reference a stable
  `herdr hook X` command (not absolute paths, survives upgrades), clean up stale entries
  (`install.sh:275-312`). Tripwire: writing to the user's `settings.json` must be **opt-in**.

**Also VENDOR (small, defensive, hard-won):**
- Compaction-detection predicate `type=="summary" || (type=="system" && subtype=="compact_boundary")`
  (`lib.py:97`).
- **Wasted-context / efficiency metric** + **EMA turns-until-compaction** vs a fixed 33k buffer
  (`lib.py:362-376`, `monitor.py:139-171`) — strong Phase-5 graph candidates.
- `waiting_for_response` boolean (dangling user turn, JSONL-only) — a clean **fallback** status
  signal + per-turn response timer when `ps` is unavailable (`lib.py:160-175`).
- Subagent/skill lifecycle via `tool_use_id → tool_result` pairing (`lib.py:274-297`) — relevant to
  herdr's tabbed-agents.
- `gitBranch` is recorded **per-entry in the JSONL** (`session-stats.py:170`) — free fallback if
  shelling out to git is slow/absent.

**WHITEROOM/SKIP:** mtime polling (herdr's notify is better), the clock-only status model, hand-rolled
ANSI rendering, the MITM-proxy "sniffer," session-first discovery + lossy slug-reverse.

---

## 3. agent-deck — the surface comp (Go, tmux-composed)

Closest **surface** match (one TUI operating many agents) and reassuringly **draws the same line
herdr does**: tmux owns the agent PTYs; agent-deck shells out to tmux for launch/keys/status/capture
and never builds a VT/ANSI renderer. But it is **session-first** and a tmux **session-lifecycle
owner** (herdr's §8 tripwire), and goes heavy on git worktrees (beyond herdr's light path).

**WHITEROOM (different language → vendor nothing):**
- **`ToolDef` multi-tool adapter table** (`internal/session/userconfig.go:1656` +
  `tmux/patterns.go`): per-tool `{command, wrapper, resume_flag, icon, busy/prompt/spinner regex}`.
  The clean seam herdr would need to ever add Gemini/Codex, *and* a viable **fallback** status
  signal (regex on `StripANSI`'d `capture-pane` text) alongside herdr's CPU/JSONL heuristics.
- **`tmux -C` control mode, two PTY-free tricks** — persistent `send-keys` streaming for
  low-latency input (`keysender.go`, <1ms/keystroke, Phase 4) and **`%output` event-driven refresh**
  (`controlpipe.go`) — a tmux-native complement to the `notify` watcher (Phase 2.5). Both are "ask
  tmux," fully inside "compose tmux."
- **Roster UX**: group rows with aggregated child status (`▾ proj (3) ● 2 ◐ 1` — perfect for
  herdr's collapsed *project* rows), status filter quick-keys (`!`=running `@`=waiting `#`=idle
  `$`=error), responsive list/preview breakpoints (50/80 cols), badge composition
  (`[branch][worktree][Nm ago]`).
- **Orphan recovery**: on startup, discover live `agentdeck_*` tmux sessions and reattach state —
  herdr's project scan should likewise reconcile pre-existing Claude panes.
- **`acknowledged`-downgrades-waiting**: once you've attended an agent, stop nagging "needs input"
  (`sessionstatus.go:116`).

**Do NOT copy (crosses herdr's line or wrong UX):**
- Attach-via-PTY (`pty.go` + `tea.Exec`) **suspends the whole TUI** to run `tmux attach`. herdr's
  composed single-stage pane (join/break-pane, agent stays live below the roster) is the better UX.
- The cached `capture-pane` "preview" is a **dead text snapshot**; herdr's live pane is strictly
  better. (capture-pane is fine for non-interactive glances only.)
- tmux **session** ownership (naming/socket/reaping) — §8 tripwire. herdr owns panes, not sessions.
- Hook-install machinery (heavier than claude-tui's), worktree/mergeback/jujutsu git, MCP pool.

---

## 4. Claude-Code-Usage-Monitor — predictions & warnings (Python)

Background **thread** fetches+analyzes on a 10s interval; `rich.Live` renders at 0.75Hz — a sync
threads+callbacks model close to herdr's. Core entity is a **5-hour reset block** (quota-first).

**VENDOR (real subtlety):**
- **P90 personalized-limit calculator** (`core/p90_calculator.py:17-49`): filter history to blocks
  that hit ≥95% of a known cap, take the **90th percentile** of their token totals, progressive
  fallback. Derives *your* ceiling from *your* history instead of a hardcoded guess. Best feature to
  lift. (Rust has no `statistics.quantiles`; replicate the `n=10`/index-8 inclusive method exactly.)
- **JSONL limit-notice regexes** (`core/analyzer.py:219-385`): reverse-engineered patterns for
  Claude's actual "limit reached / wait N minutes / <reset epoch>" notices — reads **ground-truth**
  reset times instead of guessing. Copy verbatim as patterns.

**WHITEROOM (trivial, constants are heuristic → put in herdr's `data/` tunables):**
- Time-to-limit ETA: `minutes = remaining / rate; eta = now + minutes`, clamp to reset when rate≤0
  (`display_controller.py:660-669`). ~5 lines. **Pick cost *or* tokens and label honestly** — this
  tool labels it "tokens will run out" but actually computes it on **cost**.
- Plan-limit table (pro/max5/max20, `core/plans.py:47`) — community guesses that drift; treat as
  overridable config, not gospel.
- 24h-cooldown-persisted-to-JSON warning gate (`notifications.py:78`) so a warning doesn't re-fire
  every tick.
- Rolling-1h-window burn rate with boundary proration (`calculations.py:94-187`) — only if herdr
  wants a "recent" rate *alongside* its lifetime $/hr.

**SKIP:** full re-parse every tick, **numpy** dep, the three inconsistent burn rates it ships
(herdr's single real-elapsed $/hr is cleaner — keep it), letting the 5h block become the top entity.

---

## 5. tokscale — battle-tested Claude parser + heatmap (Rust, MIT) ⭐ Rust, liftable

MIT-licensed Rust workspace (`../tokscale/crates/tokscale-core`). Hot path (scan→parse→aggregate) is
**fully sync + rayon**; `tokio` appears in **4 files, all `pricing/`** (HTTP fetch only) — so the
parser/aggregator lift **without** dragging in async.

**VENDOR (MIT, sync, lean):**
- **Claude dedup + max-merge** (`sessions/claudecode.rs:498-543`): composite key
  `"{message.id}:{requestId}"`, and on a repeat it **merges per-field max** rather than dropping —
  captures the most-complete token counts across streaming-duplicate lines. *More correct than a
  naive seen-set drop.* Directly fixes herdr bug #2; the most battle-hardened Claude JSONL parser
  reviewed.
- **Contributions-heatmap bucketer** (`aggregator.rs:14` + `:704` `calculate_intensities`): group by
  date, sum, then 5-level intensity by ratio to the max day. ~50 LOC, zero deps, drop-in for a herdr
  activity heatmap/sparkline (Phase 5). Note: it keys on **cost**; herdr probably wants token-total
  or message-count (activity, not spend).
- **Normalized data model** (`sessions/mod.rs:43` `UnifiedMessage`, `lib.rs:177` `TokenBreakdown`) —
  adds a `reasoning` tier herdr lacks; good shape if herdr ever goes multi-tool.
- **Subagent attribution** (`claudecode.rs:89-170`): 3-tier resolve (sibling `.meta.json` → scan
  parent JSONL for the spawning `Agent` tool_use joining `tool_use_id→agentId` → fallback).
- Path normalization `normalize_workspace_key`/`workspace_label_from_key` (`mod.rs:325`, UNC/Windows-safe).

**WHITEROOM/SKIP:** all of `pricing/` (async + reqwest + native-tls + 5395-line fuzzy matcher — use
ccusage's smaller matcher + a static table instead), `scanner.rs` rayon multi-tool discovery (wrong
shape — herdr is event-driven on one tool), the entire `tokscale-cli` (clap+resvg+image+… dep blob),
`message_cache.rs` (bincode+zstd — herdr's local reads are <1ms, no cache needed).

---

## 6. vscode-claude-status — cheap aggregation patterns (TS, small)

Not a status monitor — a **usage/cost meter**. No process/CPU/state inference at all (its "status"
is *data freshness*). Narrow value:

- **Activity-gated network** (`dataManager.ts:147` + `wasJsonlUpdatedRecently`): only hit the API
  when a JSONL file `stat` shows a change in the last 300s. Mirrors herdr's "git status is throttled,
  not file-watched" stance — apply to any future network panel.
- **Freshness as a first-class enum** (`dataSource: api|cache|stale|…`) rendered as `[Nm ago]` +
  dimmed color — a clean way to surface "data is N seconds stale" *distinctly* from agent status.
- **cwd→project two-strategy resolve** (`projectCost.ts:56`): the hash
  (`replace(/[^a-zA-Z0-9]/g,'-')`) — **independent confirmation herdr's "hyphenate all
  non-alphanumerics" fix is correct** — plus a fallback that scans dirs matching the `cwd` field in
  the first 30 JSONL lines (robustness if the hash ever misses).
- Tiny dependency-free formatters (`formatTokens` 1.2K/3.4M, `formatDuration`, `truncateName>12→…`,
  ASCII `buildBar`) — taste references for a compact line.
- The configurable `statusBar.format` is **not** a model to copy: its whole vocabulary is 5
  cost/usage tokens, zero state/status — and a custom format short-circuits all contextual
  decoration. herdr's interesting fields (status/CPU/branch/ahead-behind/$hr) have no analog here.

---

## 7. Prioritized backlog (what to actually do)

**Now — correctness (small, high-value):**
1. Replace `models.rs` table with ccusage's per-model Claude table (`pricing.rs:564+`) + swap
   `shorten_model` for its boundary-aware matcher. **Fixes Opus-4.8 3× over-pricing (bug #1).**
2. Add `(message.id, requestId)` dedup with **max-merge** (tokscale `claudecode.rs:498`). **Fixes
   token over-count (bug #2).**
3. Add `CostMode::Auto` — trust `costUSD` when the transcript writes it (ccusage `cost.rs:23`).
4. Tiered cost: >200k-context tier + 5m/1h cache split (ccusage `cost.rs:84`).

**Next — the status-accuracy unlock:**
5. Opt-in **`Notification`/`Stop` hook installer** → writes `~/.claude/herdr/<session>.json` →
   existing `notify` watcher consumes it. Turns the permission-prompt heuristic into fact
   (claude-tui `install.sh` mechanics). Pairs with `acknowledged`-downgrades-waiting (agent-deck).

**Phase 2.5 / 4 — orchestration (inside "compose tmux"):**
6. Evaluate `tmux -C` control mode: `%output` event refresh (complement notify) + persistent
   `send-keys` for low-latency input (agent-deck `controlpipe.go`/`keysender.go`).
7. Orphan-recovery: reconcile pre-existing Claude tmux panes on the project scan.

**Phase 5 — graphs (numbers that earned a spot):**
8. Activity heatmap (tokscale `calculate_intensities`, keyed on tokens/messages not cost).
9. P90 personalized limit (CCUsage-Monitor `p90_calculator.py`) + projection-to-5h-window
   (ccusage `blocks.rs`) + JSONL limit-notice regexes for ground-truth reset times.
10. Wasted-context / turns-until-compaction (claude-tui).

**Roster UX (cheap wins):** aggregated child-status on collapsed project rows, status filter
quick-keys (`!@#$`), badge composition, freshness `[Nm ago]` (agent-deck + vscode-claude-status).
