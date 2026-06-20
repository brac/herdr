---
name: herdr-status
description: How herdr infers an agent's Status, CPU%, and cost/burn — the signal model, the load-bearing gotchas (ps %cpu is a lifetime average; Claude Code turns end on tool_use), and how to debug a wrong status with HERDR_LOG. Use for any "status is stuck/wrong/laggy", CPU%, or $/hr work.
---

# herdr-status — agent status, CPU, and cost inference

Status/CPU/cost is the single most bug-prone area of herdr (the backlog has several
"status wrong / $/hr wild / activity lags" items). This is the model and the traps.

## Where it lives

- **`core/process.rs::fetch_and_enrich`** — one `ps` call for all PIDs → CPU, MEM, TTY, args.
- **`core/process.rs::smooth_cpu`** — asymmetric EMA over the CPU signal.
- **`core/monitor.rs::infer_status`** — the status decision tree (consumes CPU + transcript signals).
- **`core/monitor.rs::update_tokens`** — incremental JSONL read → tokens/cost + `last_msg_type`/
  `last_stop_reason`/`is_waiting_for_task`, then calls `infer_status`.
- **`tui/app.rs::refresh`** — computes `burn_rate_per_hr` from the cost delta each tick.

## The status decision tree (`infer_status`, in order)

1. `cpu_percent > 5.0` → **Processing** (live work beats stale JSONL).
2. `is_waiting_for_task` (JSONL `waiting_for_task`) → **NeedsInput**.
3. no telemetry + no msg type → **Unknown**.
4. `assistant` + `end_turn` → **Waiting** (→ **Idle** after `IDLE_AFTER_MINS` = 10).
5. `assistant` + `tool_use` → low CPU + pending tool ⇒ **NeedsInput**; low CPU + age>5s ⇒ NeedsInput;
   else **Processing** (permission prompts are invisible in JSONL — this heuristic is how we spot them).
6. `user`/tool_result → CPU>1% **or** fresh (< `USER_LINE_PROCESSING_GRACE_SECS` = 10) ⇒ **Processing**;
   else **Waiting** (→ Idle after 10 min).
7. fallback → **Idle**.

## Gotchas that have each cost a session

- **`ps %cpu` is a LIFETIME AVERAGE, not instantaneous.** It's CPU-time ÷ elapsed, so a long-lived
  agent that did real work stays >5% for tens of minutes after going idle — which pins it to
  **Processing forever** via branch 1 and short-circuits everything below. **Fix already in place:**
  herdr reads `ps -o time=` (cumulative CPU seconds) and diffs it over wall-clock between ticks to get
  *real* instantaneous CPU% (`prev_cpu_secs`/`prev_cpu_sample_ms` on the session; `parse_cputime`
  handles `MM:SS`/`HH:MM:SS`/`DD-HH:MM:SS`). **Never go back to `%cpu`.** Sub-750ms windows are skipped
  (`MIN_CPU_SAMPLE_MS`) because `ps time=` has 1s resolution.
- **Claude Code agents almost always end a turn on a tool call.** Empirically ~135 `tool_use` vs ~1
  `end_turn` per session — so the transcript is usually parked at `assistant(tool_use) → user(tool_result)`
  while the agent waits for the next human prompt. Branch 6 (`user`) therefore MUST be CPU+age aware;
  it used to return Processing unconditionally → finished agents read "Processing at $0/hr." If you
  touch branch 6, keep it from re-introducing that.
- **CPU is low during API generation.** The local `claude` process is I/O-bound while the model streams,
  so "low CPU" ≠ "idle." Branches 5/6 lean on this with short grace windows; don't tighten them to the
  point a mid-generation pause flaps the status.
- **First sample seeds idle.** A freshly discovered agent shows 0% for one tick (no prior counter to
  diff) — a brand-new busy agent reads idle for ~2s before catching up. Expected, don't "fix" it by
  seeding from `%cpu`.
- **Tunables are named consts**, not scattered literals: `CPU_EMA_ALPHA_RISE/FALL`, `MIN_CPU_SAMPLE_MS`
  (process.rs); `IDLE_AFTER_MINS`, `USER_LINE_PROCESSING_GRACE_SECS` (monitor.rs). Tune there.

## Cost / burn

`burn_rate_per_hr` = (cost delta since last tick) scaled to an hour, computed in `App::refresh`; it
**decays ×0.5** when no new cost arrives so it falls back toward 0 instead of freezing. Known-open
backlog: it's still volatile ("$/hr is wild" / "too small") — a rolling average over a window would
steady it. The fleet strip (`ui/fleet.rs`) sums `burn_rate_per_hr` across agents for the trend.

## Debugging a wrong status

Opt-in file log (it was never wired before; `logger::log` is a silent no-op until init):

```bash
HERDR_LOG=/tmp/herdr.log cargo run --release -- "<parent-dir>"
tail -f /tmp/herdr.log   # in another pane
```

Emits one line per status decision with exactly what each branch saw:

```
status pid=844 -> WaitingInput | cpu=0.4% msg_type=user stop=- waiting_for_task=false
```

Read `cpu`, `msg_type`, `stop`, `waiting_for_task` straight off the line and walk the decision tree
above — no guessing. If `cpu` looks stuck high on an idle agent, suspect the `%cpu`/`time=` path first.
