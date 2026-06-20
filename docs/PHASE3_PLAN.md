# Phase 3 plan — the git light path

> Gate (CLAUDE.md §7): *branch / dirty / ahead-behind visible per project; push/pull non-blocking.*
> Scope boundary (CLAUDE.md §4, §8): light path only. **No `git2`/libgit2, no diff/staging/rebase UI.**
> We shell out to `git`, parse stdout, and degrade to `Unknown` — never crash, never block the render loop.

This plan targets the existing seams; it is a data + render addition on the inherited layer, **no new dependencies**.

---

## Prerequisite — DONE: cwd→slug bug fixed

Comparing against `agent-deck` surfaced a live discovery bug: `discovery.rs::cwd_to_slug` replaced only
`/`, but Claude Code replaces **every non-ASCII-alphanumeric char** with `-` (verified against real
`~/.claude/projects/` dirs — `burnRat` keeps its capital R; the space in `Ben Bracamonte` → `-`). On a
path with a space or `.`, herdr silently failed to match any transcript. **Fixed** (preserve
`[A-Za-z0-9]`, hyphenate the rest) with regression tests for spaces/dots/case. This was a true
prerequisite: per-project git status is pointless if agent discovery under those projects is broken.

## Lessons folded in from agent-deck (mature Go/Bubble Tea peer)

- **Git light-path validated.** agent-deck — far larger scope — *also* shells out to the `git` binary
  with **no vendored library**. Independent confirmation of §4. (It uses git mainly for worktree
  isolation; our emphasis is status glance.)
- **Bare-repo / worktree robustness (steal).** Its `isBareRepoSelf()` avoids the false positive where
  `git rev-parse --is-bare-repository` walks *up* the tree. Folded into §1 below so bare repos and
  linked worktrees don't report garbage status.
- **Actionability sort (adopt).** It orders by `error → waiting → running → idle → stopped`, then
  recency — "needs input" beats "expensive." Folded into §3 as a roster-ordering change.
- **Pane-content status signal (noted, Phase 4-adjacent).** Its most reliable status signal is
  scraping the tmux pane for `"esc to interrupt"`, spinner glyphs, and permission dialogs
  (`"Yes, allow once"`) — more direct than our CPU heuristic and a synchronous `tmux capture-pane`.
  Not Phase 3 work, but it pairs naturally with the Phase 4 approve/interrupt control.

## Interaction with Phase 2.5 (event-driven refresh)

Phase 2.5 makes **agent/JSONL** freshness event-driven (`notify` watcher → channel). **Git status is
deliberately NOT file-watched** — watching `.git` across ~53 repos would burn inotify watches for
little gain. Git status keeps its **throttled timer** (§2 below), recomputed on the slow safety-net
tick, not on every file event. So §2's cache and §4's `try_wait()` ride the safety-net tick; only the
roster's agent rows refresh on watcher events.

---

## 0. Design decisions (resolved up front)

| Question | Decision | Why |
|---|---|---|
| One command for branch + dirty + ahead/behind? | `git -C {path} status --porcelain=v2 --branch` | One spawn yields all three (`# branch.head`, `# branch.ab +N -M`, file lines). Answers CLAUDE.md §4 open-Q #3. |
| 53 projects × every 2s poll = 53 spawns/2s? | **Throttled cache.** Recompute a project's status at most every ~10s (and on demand when selected). | A full porcelain sweep is ~5ms × 53 ≈ 265ms — too much jank at 2s cadence. Status is a *glance*; 10s staleness is fine. Local, so no thread needed — just a TTL. |
| Push/pull blocking? | **Fire-and-forget** `Command::spawn` (null stdio), track the `Child`, `try_wait()` each poll. | CLAUDE.md §3/§4: the one network seam. `try_wait()` is non-blocking, stays in the synchronous model. |
| New roster column? | **No.** Render git status inline in the project **header row** only. | A real 12th column touches every agent row + the widths array. Header-only is lean and matches the existing render (`table.rs:213` blanks cells 2..11). The header comment at `table.rs:194` already anticipates this. |
| Threads? | **None.** | Throttled-sync status + `try_wait()` push/pull keeps §3's "no async, synchronous poll loop" intact. |

---

## 1. New module — `crates/claudectl-core/src/git.rs`

Pure shell-out + parse, claudectl-style. Defensive: any failure → `None`/`Unknown`, never panics (§3).

```rust
pub struct GitStatus {
    pub branch: String,        // "main", or "(detached)" / "(unknown)"
    pub dirty: bool,           // any staged/unstaged/untracked change
    pub ahead: u32,
    pub behind: u32,
    pub upstream: bool,        // false → ahead/behind meaningless, hide them
}

/// Run `git -C {path} status --porcelain=v2 --branch`; parse stdout.
/// Returns None if the dir isn't a repo or git is unavailable.
pub fn status(path: &Path) -> Option<GitStatus>;

/// Pure parser over captured stdout — the unit-testable core.
fn parse_porcelain_v2(stdout: &str) -> GitStatus;
```

**Parser rules (porcelain=v2 --branch):**
- `# branch.head <name>` → `branch` (value `(detached)` when head is detached).
- `# branch.ab +<N> -<M>` → `ahead`/`behind`; **line absent ⇒ `upstream = false`**.
- Any line starting `1 `, `2 ` (changed/renamed) or `? ` (untracked) or `u ` (unmerged) ⇒ `dirty = true`.
- Unknown lines ignored (forward-compatible per §3).

**Bare-repo / worktree robustness (from agent-deck):** `--porcelain=v2 --branch` handles linked
worktrees fine, but guard the repo check so a project that is a *bare* repo (or sits under one)
doesn't report a confusing branch. Borrow agent-deck's `isBareRepoSelf` idea: if
`git -C {path} rev-parse --is-bare-repository` is true, render `(bare)` and skip dirty/ahead-behind
rather than emit noise. Cheap, and prevents a class of false positives across a mixed `~/Work`.

**Tests (pure, no git binary needed):** clean+upstream, dirty (each line type), ahead/behind, detached HEAD,
no-upstream (ab line absent), bare repo, empty/garbage stdout → sane defaults. Mirror `discovery.rs` test
style. Optionally one `#[cfg]`-gated integration test that `git init`s a tempdir, gated on git availability.

Wire into `crates/claudectl-core/src/lib.rs` (`pub mod git;`).

---

## 2. Throttled cache in `App` — `crates/claudectl-tui/src/app.rs`

- Add field: `git_cache: HashMap<PathBuf, (Instant, GitStatus)>`.
- Add a tunable `GIT_STATUS_TTL: Duration = Duration::from_secs(10)` (CLAUDE.md §9 wants tunables in a
  `data/`-style module — none exists yet, so **create `core/git.rs`'s `const`s** or a tiny `core/tunables.rs`).
- In `App::refresh()` (`app.rs:671`, after `projects::scan` at `:686`): for each project with `has_git`,
  recompute `git::status` **only if** absent or older than the TTL; store `(Instant::now(), status)`.
  Skip the `(other)` bucket and non-git dirs. Prune cache entries whose project vanished.
- Surface into the roster: add `git: Option<GitStatus>` to `ProjectGroup` (`app.rs:3059`), populated in
  `project_groups()` (`app.rs:3112`) by cloning from `git_cache` keyed by `proj.path`. `(other)` → `None`.

---

## 3. Render in the project header — `crates/claudectl-tui/src/ui/table.rs:185`

In the `RosterRow::Header` arm, append a git segment to the header line for groups with `git: Some(_)`:

- Format: `branch` + dirty marker `●` (clean: none, or dim `✓`) + `↑{ahead}` / `↓{behind}` when `upstream`.
  Example active: `herdr (2 sessions, 1 active, $44.0, ctx 14%)  main ●↑2`. Idle: `web  main ✓`.
- Semantic color via `theme.rs` (`status_color`-style): clean=green/dim, dirty=yellow, ahead/behind=cyan.
  **Respect `NO_COLOR`** — theme already gates this (CLAUDE.md §6); no raw ANSI.
- Idle projects (`session_count == 0`) currently render just the dim name (`table.rs:197`) — now name + git.
- Keep it in the header line; do **not** add a column. Update the `table.rs:194` comment to "implemented".

---

## 4. Push / pull — fire-and-forget actions

**Keys (audited — both free; `p` is taken by peers):** `P` = push, `L` = pull. Handled in `app.rs` key match.
Target dir = `selected_launch_cwd()` (`app.rs:3210`) so it works on a header or an agent row.

- Add `git_ops: HashMap<PathBuf, (GitOp, Child)>` to `App`. On `P`/`L`: `Command::new("git").args(["-C", path, "push"|"pull"]).stdin/out/err(null).spawn()`, store the `Child`. Reuse the `hooks.rs:83` spawn idiom.
- In `App::tick()` (`app.rs:1342`): `try_wait()` each in-flight op (non-blocking). On exit, drop it, **force-refresh that project's git status** (bypass TTL so the new ahead/behind shows immediately), and surface a transient toast/status-bar note (success/fail by exit code).
- While an op is in flight, show a **throbber** next to that project's git segment. This is the one place CLAUDE.md §4 sanctions a spinner/skeleton (network-bound). `throbber-widgets-tui` is pre-approved in PROJECT_OVERVIEW; or hand-roll a 3-frame spinner to avoid the dep — **recommend hand-roll** to hold the <1MB line; revisit if we want it elsewhere.
- Confirm-on-push? v1: no modal (lean); just fire. Note as a deferred polish if it bites.

---

## 4b. Roster ordering — actionability sort (from agent-deck)

`project_groups()` (`app.rs:3156`) currently sorts active-projects-first then by **cost desc**. Cost is a
weak urgency signal. Adopt agent-deck's **actionability** order for the active band: a project's rank =
its most-urgent agent's status (`NeedsInput → Processing → WaitingInput → Idle/Unknown`), then total
cost, then name. Idle (zero-agent) projects stay below, sorted by name. Net effect: a project with an
agent blocked on a permission prompt floats to the top where you'll act on it. Small, well-tested change
to the existing sort closure; no new state.

## 5. Scope tripwires actively avoided (CLAUDE.md §8)

- No `git2`/libgit2; no staging, diff, or rebase UI. Status + push/pull only.
- No threads/async; throttled-sync status + `try_wait()` keep the synchronous poll loop (§3).
- No new column; header-only render.
- No new runtime dep (hand-rolled throbber). Any dep needs a one-line PR justification (§8).

---

## 6. Sub-steps & gates

| Step | Deliverable | Gate |
|---|---|---|
| **3a** | `core/git.rs`: `GitStatus` + `parse_porcelain_v2` + `status()`, unit tests | parser tests green; `git::status(".")` returns real branch/dirty for herdr |
| **3b** | `App.git_cache` (TTL) + `ProjectGroup.git` + header render | from `~/Work`, each git project's header shows branch/dirty/ahead-behind; non-git + `(other)` show none; no perceptible poll jank |
| **3c** | `P`/`L` fire-and-forget push/pull + `try_wait` + throbber + transient result | press `P` on a project → render never stalls; ahead/behind updates on a later poll; spinner while in flight |

Each step: `cargo build --release` + `cargo test --workspace` + `cargo clippy -- -D warnings` green before the next.
Manual verify (3c) needs a real repo with a remote — do it against herdr itself inside WSL/tmux.

---

## 7. Estimated surface

- New: `core/git.rs` (~120 LOC + tests). 
- Edited: `core/lib.rs` (1 line), `tui/app.rs` (cache field, refresh hook, `ProjectGroup.git`, key handlers, tick `try_wait`), `tui/ui/table.rs` (header git segment), `theme.rs` (maybe a git color helper).
- Tests: ~6–8 new parser unit tests + roster/cache tests.
- Deps: **none added.** Binary stays ~1.4 MB.
