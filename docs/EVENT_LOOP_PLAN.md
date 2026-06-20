# Phase 2.5 plan — event-driven refresh (drop the polling lag)

> Gate (CLAUDE.md §7): *an agent's JSONL activity refreshes the roster without waiting for the next poll.*
> Invariant (CLAUDE.md §3, amended): synchronous render loop, **event-driven via OS threads — no tokio,
> no `async`/`await`.** A `notify` watcher thread feeds an `mpsc` channel; the poll timeout becomes a
> slow safety-net tick. One new dependency (`notify`), justified by removing the up-to-2s lag.

This lands **before Phase 3's refresh changes** because Phase 3 (and Phase 4) build on the new substrate.

---

## 0. Why threads, not async

The user wants off fixed polling. Full tokio fights §3 (≈2–3MB + many deps + a large refactor of the
inherited synchronous layer) and buys nothing — herdr does no concurrent network I/O in its core loop.
A **blocking `notify` watcher on its own thread + an `mpsc::channel`** gives event-driven freshness while
the render loop stays plain synchronous Rust. This is precisely how agent-deck drives hook updates.

---

## 1. What gets watched (and what does NOT)

- **Watch:** `~/.claude/projects/` **recursively** (covers every discovered project's `<slug>/*.jsonl`;
  one recursive watch is simpler and cheaper than N per-slug watches and auto-covers new sessions).
- **Watch:** the **parent dir** herdr scans (depth 1) so a newly created/cloned project appears without
  waiting for the tick.
- **Do NOT watch:** `.git` dirs (Phase 3 git status stays on the throttled timer — watching ~53 `.git`
  trees would burn inotify watches for little gain) or process/CPU (`ps` enrichment is timer-driven).

If the watcher fails to initialize (permissions, platform), **degrade to timed polling** — log and carry
on, never crash (§3 defensive).

---

## 2. Wiring — `src/main.rs` event loop + `App`

Today (`main.rs`): `crossterm::event::poll(2s)` → `App::tick()` (calls `refresh()`) → `draw`.

New shape:
```
let (tx, rx) = std::sync::mpsc::channel();
let _watcher = spawn_watcher(tx);          // notify::recommended_watcher on its own thread
loop {
    // 1. input — short timeout so we stay responsive and the tick still fires
    if crossterm::event::poll(SAFETY_NET_TICK)? { handle_key(event::read()?); }

    // 2. drain + coalesce fs events into a single refresh (writes are bursty)
    let mut dirty = false;
    while rx.try_recv().is_ok() { dirty = true; }

    // 3. refresh on either a fs event OR the safety-net tick elapsing
    if dirty || tick_elapsed() { app.tick(); }   // tick() still owns refresh()/enrichment

    terminal.draw(...)?;
}
```

- **Coalesce/debounce:** draining the channel to a single `dirty` bool per iteration collapses a burst of
  JSONL writes into one refresh — no extra dep, no thrash. (Skip `notify-debouncer-*`; hand-rolled is leaner.)
- **Safety-net tick** (`SAFETY_NET_TICK`, e.g. 2s, in the tunables module): still fires `refresh()` so
  timer-driven work (git-status throttle, `ps` CPU/MEM enrichment, elapsed clocks) keeps updating even
  with zero fs events. The watcher only *accelerates* agent-row freshness; it doesn't replace the tick.
- `App::tick()`/`refresh()` are unchanged in behavior — we change *when* they're called, not what they do.

---

## 3. The watcher thread

- `notify::recommended_watcher` (inotify on Linux/WSL, FSEvents on macOS — both our targets) in a thread
  that owns the `Watcher` and forwards each `Event` as a unit `()` (we don't need the payload — any change
  means "refresh") to `tx`. Keep the `Watcher` alive for the program's life (drop = stop).
- Filter cheaply in the thread: only forward events whose path ends in `.jsonl` or is a directory
  create/remove (new project). Drops editor temp-file noise before it reaches the loop.

---

## 4. Dependency & size

- **New dep: `notify`** (mature, widely used). Pulls a small transitive set (inotify/fsevent bindings).
  Justified per §3 (removes the polling lag). Expect a modest binary bump from ~1.4MB — acceptable per the
  explicit decision; record the before/after in the PR.

---

## 5. Tests & gate

- Unit-test the **coalescing/dirty-flag** logic in isolation (feed N synthetic events → one refresh).
- Watcher itself is integration-level: a `#[cfg]`-gated test that writes a temp `.jsonl` and asserts an
  event arrives within a timeout (gated so CI without inotify doesn't flake).
- **Gate:** start an agent in a watched project, type into it; the roster's status/tokens update in well
  under the old 2s, driven by the fs event, not the tick. Kill the watcher (simulate failure) → falls back
  to timed polling, no crash.

## 6. Scope guard

- Threads + channels only; **no tokio, no async/await** (§3). The watcher thread is blocking I/O.
- Don't watch `.git` or spawn a watcher per session — one recursive watch + the parent dir.
- Watcher failure degrades to polling; never a hard error.
