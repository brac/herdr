#!/usr/bin/env bash
#
# quickstart — build herdr and launch it in a tmux session over a parent dir.
#
# Usage:
#   ./quickstart.sh [PARENT_DIR]
#
#   PARENT_DIR  The directory of repos herdr scans (one project per subdir).
#               Defaults to this repo's parent dir (the common "terminal in the
#               parent of many repos" setup — CLAUDE.md §2).
#
# Env overrides:
#   HERDR_SESSION   tmux session name (default: "work")
#   HERDR_DIR       fallback PARENT_DIR when no arg is given
#
# herdr orchestrates tmux panes (launch/stage/approve), so tmux is required.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SESSION="${HERDR_SESSION:-work}"
PARENT_DIR="${1:-${HERDR_DIR:-$(dirname "$REPO_DIR")}}"

if ! command -v tmux >/dev/null 2>&1; then
  echo "error: tmux is required (herdr launches/stages agents into tmux panes)." >&2
  exit 1
fi

# Always rebuild so we never attach to a stale binary (a recurring footgun —
# see HANDOFF.md). `cargo build --release` is a no-op when already up to date.
echo "Building herdr (release)…"
( cd "$REPO_DIR" && cargo build --release )
BIN="$REPO_DIR/target/release/herdr"

# Launch herdr in tmux: open a window running it, creating the session if needed.
# PARENT_DIR is single-quoted into the command string so paths with spaces (e.g.
# ".../Ben Bracamonte/Work") survive tmux's `sh -c`.
if tmux has-session -t "$SESSION" 2>/dev/null; then
  tmux new-window -t "$SESSION" -n herdr -c "$PARENT_DIR" "'$BIN' '$PARENT_DIR'"
else
  tmux new-session -d -s "$SESSION" -n herdr -c "$PARENT_DIR" "'$BIN' '$PARENT_DIR'"
fi

# Attach (or switch, if we're already inside tmux).
if [ -n "${TMUX:-}" ]; then
  tmux switch-client -t "$SESSION"
else
  exec tmux attach -t "$SESSION"
fi
