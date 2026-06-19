---
name: herdr-check
description: Run herdr's canonical verify gate (build + test + clippy), handling the WSL/tmux gotchas. Use before committing any herdr change, or when asked to "verify", "check", or "run the gate".
---

# herdr-check — the verify gate

herdr's green bar is: **release build + full workspace tests + clippy `-D warnings`, all clean.**
Run these from the repo root and report the totals.

```bash
cargo build --release 2>&1 | tail -1
cargo test --workspace 2>&1 | grep -E "test result:" | awk '{sum+=$4} END {print "TOTAL: " sum}'
cargo clippy --workspace --bins --tests -- -D warnings 2>&1 | grep -E "warning:|error" | head || echo "clippy clean"
```

State the test total (it only goes up — ~170+), confirm clippy is clean, and note the release binary
size (`ls -la target/release/herdr` → MB) if a dependency changed.

## Gotchas that will bite you

- **Stale binary.** The user runs a prebuilt `./target/release/herdr`; source edits don't take
  effect until rebuilt. Always remind them to **`cargo run --release -- <parent-dir>`** (rebuilds
  first) or rebuild + restart. Many "it didn't work" reports are a stale binary — check
  `ls -la target/release/herdr` mtime vs the source you changed.
- **`.git/index.lock` race.** The user often runs herdr pointed at the *parent of herdr itself*, so
  herdr's background git-status worker runs `git status` on this repo and briefly holds
  `.git/index.lock`, colliding with commits. If `git commit` fails with "index.lock exists" and the
  only running git is a `git -C …/<some-repo> status`, it's safe to `rm -f .git/index.lock` and retry.
- **Builds only on WSL/Linux or macOS**, never native Windows MSVC (unconditional `libc` syscalls).
- **WSL `/mnt/c` is slow**: `git status` is 150–830ms/repo and inotify is unreliable on the mount —
  by design herdr fetches git off a worker thread and falls back to a 2s safety-net tick.

## Commit conventions

Conventional-commit prefix (`feat:`/`fix:`/`refactor:`/`docs:`/`chore:`), imperative subject, and end
the body with:

```
Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
```

Commit/push only when asked. The remote uses `gh` credentials (`gh auth setup-git` is configured).
