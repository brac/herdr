# herdr

A project-first terminal cockpit for operating [Claude Code](https://docs.claude.com/en/docs/claude-code) agents across a whole tree of repos.

Launch it from the folder that holds your projects. herdr finds each repo, shows what Claude Code agents are doing inside them, lets you launch and steer those agents, and reports light git status, all in one keyboard-driven terminal UI. tmux handles the window management; herdr supplies the smart panels on top.

It is a single self-contained Rust binary, synchronous (no async runtime), and fast. It reads Claude Code's own local files to learn what each agent is up to, and it shells out to `git`, `ps`, and `tmux` rather than reimplementing them.

## Why

If you run several Claude Code agents at once, across several repos, you lose track fast. Which one is waiting on you? Which one is burning money? What did that agent in the other pane just do? herdr answers all of that from one screen, so you can supervise a fleet of agents the way you would supervise a build dashboard.

## What it does

- **Project-first roster.** One row per project under your parent directory, with each project's live agents nested beneath it.
- **Real status.** Tells you when an agent is processing, finished a turn, waiting on the API, or blocked on a permission prompt, inferred from CPU, the transcript, and message age.
- **Cost and tokens.** Per-agent input/output/cache token counts, a steady dollars-per-hour burn rate, and persistent all-time and weekly totals.
- **Launch and steer.** Start a new agent into a tmux pane, approve a permission prompt, send a prompt, read the conversation, or interrupt, all without leaving herdr.
- **Single-agent stage.** Keep exactly one agent's live pane visible below the roster and swap which one with a keystroke.
- **Light git status.** Branch, dirty flag, and ahead/behind counts per project, plus fire-and-forget push and pull.
- **At-a-glance graphs.** A fleet trend strip with a live burn sparkline, daily activity heatmap, and status counts.

## Requirements

- **Rust** 1.88 or newer (edition 2024), to build the binary.
- **tmux**, which herdr uses to launch, stage, and interact with agent panes.
- **Claude Code** installed and used normally. herdr reads the files Claude Code writes under `~/.claude`.

Built and tested on Linux and WSL.

## Quick start

The fastest path is the bundled script, which builds a release binary and opens herdr in tmux over your projects directory:

```sh
./quickstart.sh /path/to/your/projects
```

With no argument it defaults to this repo's parent directory (the common "a terminal in the folder that holds many repos" setup). You can also set `HERDR_SESSION` to name the tmux session.

To build and run by hand:

```sh
cargo build --release
./target/release/herdr /path/to/your/projects
```

herdr scans the given directory for subfolders that are git repos and lists each as a project. Pass `--all` to include non-git subfolders too.

## Keys

Press `?` inside herdr for the full list. The everyday ones:

| Key | Action |
| --- | --- |
| `j` / `k` or arrows | Move through the roster |
| `Enter` | Toggle the detail panel for the selected agent |
| `o` | Stage the selected agent's live pane below the roster (press again to hide) |
| `Tab` | Jump into the selected agent's terminal |
| `n` / `N` | Launch a new agent (quick / guided) |
| `y` | Approve a permission prompt |
| `A` | Approval inspector: see the captured prompt, then approve, deny, or interrupt |
| `i` | Send text to the selected agent |
| `C` | Open the agent's conversation |
| `d`, `d` | Stop the agent (a second `d`,`d` forces it) |
| `f` and `!` `@` `#` `$` | Filter by status (cycle, or jump straight to needs-input / processing / waiting / idle) |
| `/` | Search; `z` clears all filters |
| `g` / `G` | Toggle grouped-by-project view / the fleet trend strip |
| `P` / `L` | git push / pull the selected project |
| `q` | Quit |

## Instant permission detection (optional)

herdr already infers when an agent is blocked on a permission prompt. For instant, exact detection you can install a small opt-in hook that lets Claude Code tell herdr the moment an agent needs you:

```sh
./target/release/herdr hook install
```

This merges a `Notification` and a `Stop` hook into your `~/.claude/settings.json` (your other settings are preserved). Restart your Claude Code sessions for the hooks to take effect. To undo it:

```sh
./target/release/herdr hook uninstall
```

## How it works

- **tmux owns the windows; herdr composes it.** herdr never parses or renders a terminal itself. It drives tmux (`split-window`, `join-pane`, `break-pane`, `resize-pane`, `send-keys`) to place agents into panes and keep one staged below the roster. It does not own or name tmux sessions.
- **It reads Claude Code's local files.** Agent discovery, transcripts, and per-agent token usage all come from `~/.claude`. herdr parses these defensively and degrades to an unknown state rather than crashing if the format shifts.
- **Slow work never blocks the screen.** git push/pull and agent launches are fire-and-forget; the next refresh picks up the result.
- **Event-driven, not polling.** A filesystem watcher repaints the roster the moment an agent writes activity, with a short safety-net tick as a backstop.

## Project layout

```
src/                     the binary: event loop, tmux launch, the hook CLI
crates/claudectl-core/   discovery, JSONL parsing, status inference, cost, git
crates/claudectl-tui/    the ratatui roster, detail panel, chat, fleet strip
docs/                    architecture notes and the comparables research
```

## Provenance and license

herdr began as a fork of [claudectl](https://github.com/mercurialsolo/claudectl), whose data layer (the path-hash that maps a working directory to Claude Code's transcript folder, the incremental JSONL parser, and the multi-signal status inference) it builds on. See `NOTICE` and `docs/REFERENCES.md`.

MIT licensed. Contributions are welcome; please keep the dependency tree small and the render loop synchronous (see `CLAUDE.md` for the project's working agreement).
