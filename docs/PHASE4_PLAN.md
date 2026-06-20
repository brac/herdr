# Phase 4 plan — conversation + control (oatmeal chat with individual agents)

> Gate (CLAUDE.md §7, amended): *read an agent's exchange **and** drive it — send a prompt, approve a
> permission prompt — without leaving the cockpit.*
> Stance (CLAUDE.md §0): herdr does **not** run or own the agent. Input is **injected into the agent's
> existing tmux pane** via the inherited keystroke backends. tmux owns the process; we are a remote control.

Built on **Phase 2.5** (event-driven refresh): after you send a prompt, the agent's reply lands in the
JSONL and the watcher repaints the chat near-instantly — the loop feels live, not polled.

---

## Part A — Conversation view (read)

**Parser extension (`core/transcript.rs` + `core/monitor.rs`).** Today `parse_line` keeps only envelope
metadata (the §1 design). Extend `TranscriptBlock`/`parse_line` to **retain message text** (assistant
text blocks + user text), and have `monitor::update_tokens`'s incremental seek append parsed messages to
a per-session **bounded ring buffer** (cap ~200 messages, in the tunables module) so memory stays flat on
long sessions. Defensive: unknown block types are skipped, never panic (§3).

**Render (`tui/src/ui/`, new `chat.rs`; reference: oatmeal).** A focused chat view for the selected agent:
assistant bubbles **left**, your messages **right**, `textwrap` for wrapping to pane width. Reachable from
a roster row (e.g. `Enter` on an agent → chat; `Esc` back). Tool-use blocks render as compact one-liners,
not walls of JSON. Auto-scroll to newest; `j/k` / `PgUp/PgDn` to scroll history.

---

## Part B — Control (send prompts + approve / deny / interrupt)

All input rides the **inherited `terminals/` backends** (`send_input`, `approve_session`,
`switch_to_terminal`) — the keystroke-injection matrix herdr forked (tmux `send-keys`, kitten `@send-text`,
iTerm AppleScript). The target is the selected agent's pane/session; no new launch, no lifecycle ownership.

1. **Prompt input** — a `tui-textarea` box at the bottom of the chat view. On submit: `send_input(session,
   text)` → keystrokes into the agent's pane, then a newline to dispatch. Clear the box; the reply arrives
   via the Phase 2.5 watcher. Multi-line supported (Shift+Enter newline, Enter sends).
2. **Approve / deny / interrupt** — when the selected agent is `SessionStatus::NeedsInput` (herdr's
   invisible-permission-prompt heuristic, `monitor.rs`), surface one-key actions: `y` approve / `n` deny
   (→ `approve_session` / inject the decline keystroke), `Esc` interrupt (send Esc to the pane).
3. **Show *what* is being approved (steal from agent-deck).** The permission prompt is invisible in the
   JSONL (§hazard), so to render the actual `"Do you want to … / Yes, allow once"` dialog, **scrape the
   tmux pane** via `tmux capture-pane -p` and show the captured prompt above the y/n buttons. Synchronous
   shell-out, fits §3. This also lets us tighten the NeedsInput signal (pane shows a dialog vs. just idle).

---

## Dependencies

- **`tui-textarea`** — multiline input (pre-approved, PROJECT_OVERVIEW). 
- **`textwrap`** — bubble wrapping (pre-approved).
- Both gated behind this phase; record size delta in the PR (§3, §8).

---

## Sub-steps & gates

| Step | Deliverable | Gate |
|---|---|---|
| **4a** | Parser keeps message text + bounded ring buffer; oatmeal `chat.rs` read view | open an agent, read its exchange as left/right bubbles; long sessions don't grow memory |
| **4b** | `tui-textarea` input → `send_input` into the pane | type a prompt in herdr → it appears in the agent's pane and runs; reply repaints via the watcher |
| **4c** | `y/n/Esc` approve/deny/interrupt + `capture-pane` prompt preview | agent hits a permission prompt → herdr shows the dialog text and `y` approves it without switching panes |

Each step: build + `cargo test --workspace` + `clippy -D warnings` green. Manual verify needs a live agent
in a tmux pane inside WSL (input injection can't be unit-tested end-to-end).

---

## Scope guards (CLAUDE.md §0, §8)

- **We do not spawn or own the agent** — input goes to the *existing* pane via send-keys. tmux owns lifecycle.
- No reimplementing the terminal or a PTY; reuse the inherited `terminals/` backends only.
- `capture-pane` is read-only scraping for prompt text, not a terminal emulator.
- Keep parser changes defensive — a schema change degrades to "no content," never a crash.
