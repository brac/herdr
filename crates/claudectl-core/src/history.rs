use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

/// A completed session record persisted to CSV.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub timestamp: String, // ISO 8601
    pub pid: u32,
    pub project: String,
    pub model: String,
    pub duration_secs: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

fn history_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".local")
        .join("share")
        .join("claudectl")
}

fn history_path() -> PathBuf {
    history_dir().join("history.csv")
}

/// Append a session record to the history CSV.
pub fn record_session(session: &crate::session::ClaudeSession) {
    let dir = history_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    let path = history_path();
    let needs_header = !path.exists();

    let file = OpenOptions::new().create(true).append(true).open(&path);

    let Ok(mut file) = file else { return };

    if needs_header {
        let _ = writeln!(
            file,
            "timestamp,pid,project,model,duration_secs,input_tokens,output_tokens,cost_usd"
        );
    }

    let ts = crate::logger::timestamp_now();
    let project = session.display_name().replace(',', ";");
    let model = session.model.replace(',', ";");

    let _ = writeln!(
        file,
        "{},{},{},{},{},{},{},{:.4}",
        ts,
        session.pid,
        project,
        model,
        session.elapsed.as_secs(),
        session.total_input_tokens,
        session.total_output_tokens,
        session.cost_usd,
    );
}

/// Load all history records, optionally filtered by a time window.
pub fn load_history(since_secs: Option<u64>) -> Vec<SessionRecord> {
    let path = history_path();
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        let Ok(line) = line else { continue };
        if i == 0 && line.starts_with("timestamp") {
            continue; // skip header
        }

        let fields: Vec<&str> = line.splitn(8, ',').collect();
        if fields.len() < 8 {
            continue;
        }

        let record = SessionRecord {
            timestamp: fields[0].to_string(),
            pid: fields[1].parse().unwrap_or(0),
            project: fields[2].to_string(),
            model: fields[3].to_string(),
            duration_secs: fields[4].parse().unwrap_or(0),
            input_tokens: fields[5].parse().unwrap_or(0),
            output_tokens: fields[6].parse().unwrap_or(0),
            cost_usd: fields[7].parse().unwrap_or(0.0),
        };

        // Filter by time window if specified
        if let Some(window) = since_secs {
            if let Some(record_secs) = parse_timestamp_epoch(&record.timestamp) {
                if now_secs.saturating_sub(record_secs) > window {
                    continue;
                }
            }
        }

        records.push(record);
    }

    records
}

/// Parse an ISO 8601 timestamp to epoch seconds (simplified).
fn parse_timestamp_epoch(ts: &str) -> Option<u64> {
    // Format: 2026-04-11T14:30:00Z
    if ts.len() < 19 {
        return None;
    }
    let year: u64 = ts[0..4].parse().ok()?;
    let month: u64 = ts[5..7].parse().ok()?;
    let day: u64 = ts[8..10].parse().ok()?;
    let hour: u64 = ts[11..13].parse().ok()?;
    let min: u64 = ts[14..16].parse().ok()?;
    let sec: u64 = ts[17..19].parse().ok()?;

    // Approximate days from epoch (good enough for filtering)
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let month_days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[m as usize];
        if m == 2 && is_leap(year) {
            days += 1;
        }
    }
    days += day - 1;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_leap(y: u64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// Days the activity heatmap spans — two weeks fits the one-line fleet strip.
pub const ACTIVITY_DAYS: usize = 14;

/// Per-day total cost for the last `days` days, oldest→newest, zero-filled for days
/// with no sessions. Buckets by UTC day (`epoch/86400`) — approximate but consistent,
/// and plenty for a glance heatmap.
pub fn daily_cost_series(days: usize) -> Vec<f64> {
    if days == 0 {
        return Vec::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let today = now / 86_400;
    let mut series = vec![0.0; days];
    for r in load_history(Some(days as u64 * 86_400)) {
        let Some(secs) = parse_timestamp_epoch(&r.timestamp) else {
            continue;
        };
        let day = secs / 86_400;
        if day > today {
            continue;
        }
        let back = (today - day) as usize; // 0 = today
        if back < days {
            series[days - 1 - back] += r.cost_usd;
        }
    }
    series
}

/// Bucket a per-day series into 5 intensity levels (`0..=4`) by ratio to the busiest
/// day — the GitHub-contributions shading, vendored from tokscale's
/// `calculate_intensities`: ≥0.75→4, ≥0.5→3, ≥0.25→2, >0→1, else 0.
pub fn intensities(series: &[f64]) -> Vec<u8> {
    let max = series.iter().cloned().fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return vec![0; series.len()];
    }
    series
        .iter()
        .map(|&v| {
            let r = v / max;
            if r >= 0.75 {
                4
            } else if r >= 0.5 {
                3
            } else if r >= 0.25 {
                2
            } else if r > 0.0 {
                1
            } else {
                0
            }
        })
        .collect()
}

/// Intensity levels for the last `days` days of cost — the cached input to the fleet
/// strip's activity heatmap.
pub fn daily_activity(days: usize) -> Vec<u8> {
    intensities(&daily_cost_series(days))
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    #[test]
    fn intensities_bucket_by_ratio_to_busiest_day() {
        // max = 10: 0→0, 1/10=.1→1, 5/10=.5→3, 10/10=1→4.
        assert_eq!(intensities(&[0.0, 1.0, 5.0, 10.0]), vec![0, 1, 3, 4]);
    }

    #[test]
    fn intensities_all_zero_is_all_level_zero() {
        assert_eq!(intensities(&[0.0, 0.0, 0.0]), vec![0, 0, 0]);
        assert!(intensities(&[]).is_empty());
    }

    #[test]
    fn daily_cost_series_length_matches_window() {
        assert_eq!(daily_cost_series(14).len(), 14);
        assert!(daily_cost_series(0).is_empty());
    }
}

/// Print a tabular history view.
pub fn print_history(since: &str) {
    let since_secs = parse_duration(since);
    let records = load_history(since_secs);

    if records.is_empty() {
        println!("No session history found.");
        if since_secs.is_some() {
            println!("  (filtered to last {since})");
        }
        return;
    }

    println!(
        "{:<22} {:<7} {:<20} {:<12} {:>10} {:>12} {:>12} {:>10}",
        "Timestamp", "PID", "Project", "Model", "Duration", "Input", "Output", "Cost"
    );
    println!("{}", "-".repeat(110));

    let mut total_cost = 0.0;
    let mut total_duration = 0u64;
    let mut total_input = 0u64;
    let mut total_output = 0u64;

    for r in &records {
        let dur = format_duration(r.duration_secs);
        let cost = if r.cost_usd < 1.0 {
            format!("${:.2}", r.cost_usd)
        } else {
            format!("${:.1}", r.cost_usd)
        };

        println!(
            "{:<22} {:<7} {:<20} {:<12} {:>10} {:>12} {:>12} {:>10}",
            &r.timestamp[..19.min(r.timestamp.len())],
            r.pid,
            truncate(&r.project, 20),
            truncate(&r.model, 12),
            dur,
            format_count(r.input_tokens),
            format_count(r.output_tokens),
            cost,
        );

        total_cost += r.cost_usd;
        total_duration += r.duration_secs;
        total_input += r.input_tokens;
        total_output += r.output_tokens;
    }

    println!("{}", "-".repeat(110));
    let total_cost_str = if total_cost < 1.0 {
        format!("${:.2}", total_cost)
    } else {
        format!("${:.1}", total_cost)
    };
    println!(
        "{:<22} {:<7} {:<20} {:<12} {:>10} {:>12} {:>12} {:>10}",
        format!("{} sessions", records.len()),
        "",
        "",
        "",
        format_duration(total_duration),
        format_count(total_input),
        format_count(total_output),
        total_cost_str,
    );
}

/// Print aggregate statistics.
pub fn print_stats(since: &str) {
    let since_secs = parse_duration(since);
    let records = load_history(since_secs);

    if records.is_empty() {
        println!("No session history found.");
        return;
    }

    let total_cost: f64 = records.iter().map(|r| r.cost_usd).sum();
    let total_duration: u64 = records.iter().map(|r| r.duration_secs).sum();
    let total_input: u64 = records.iter().map(|r| r.input_tokens).sum();
    let total_output: u64 = records.iter().map(|r| r.output_tokens).sum();
    let avg_cost = total_cost / records.len() as f64;
    let avg_duration = total_duration / records.len() as u64;

    println!("Session Statistics (last {since})");
    println!("{}", "=".repeat(45));
    println!("  Sessions:         {}", records.len());
    println!("  Total cost:       ${:.2}", total_cost);
    println!("  Avg cost/session: ${:.2}", avg_cost);
    println!("  Total duration:   {}", format_duration(total_duration));
    println!("  Avg duration:     {}", format_duration(avg_duration));
    println!(
        "  Total tokens:     {} in / {} out",
        format_count(total_input),
        format_count(total_output)
    );
    println!();

    // Per-project breakdown
    let mut projects: std::collections::HashMap<String, (f64, u64, usize)> =
        std::collections::HashMap::new();
    for r in &records {
        let entry = projects.entry(r.project.clone()).or_default();
        entry.0 += r.cost_usd;
        entry.1 += r.duration_secs;
        entry.2 += 1;
    }

    let mut project_list: Vec<_> = projects.into_iter().collect();
    project_list.sort_by(|a, b| b.1.0.partial_cmp(&a.1.0).unwrap());

    println!("  Per-project breakdown:");
    println!(
        "  {:<25} {:>8} {:>10} {:>10}",
        "Project", "Sessions", "Duration", "Cost"
    );
    println!("  {}", "-".repeat(55));
    for (name, (cost, dur, count)) in &project_list {
        let cost_str = if *cost < 1.0 {
            format!("${:.2}", cost)
        } else {
            format!("${:.1}", cost)
        };
        println!(
            "  {:<25} {:>8} {:>10} {:>10}",
            truncate(name, 25),
            count,
            format_duration(*dur),
            cost_str,
        );
    }

    // Per-model breakdown
    let mut models: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    for r in &records {
        let model = if r.model.is_empty() {
            "unknown".to_string()
        } else {
            r.model.clone()
        };
        let entry = models.entry(model).or_default();
        entry.0 += r.cost_usd;
        entry.1 += 1;
    }

    let mut model_list: Vec<_> = models.into_iter().collect();
    model_list.sort_by(|a, b| b.1.0.partial_cmp(&a.1.0).unwrap());

    println!();
    println!("  Per-model breakdown:");
    println!("  {:<20} {:>8} {:>10}", "Model", "Sessions", "Cost");
    println!("  {}", "-".repeat(40));
    for (name, (cost, count)) in &model_list {
        let cost_str = if *cost < 1.0 {
            format!("${:.2}", cost)
        } else {
            format!("${:.1}", cost)
        };
        println!("  {:<20} {:>8} {:>10}", name, count, cost_str);
    }
}

/// Weekly usage summary for the TUI title bar.
#[derive(Debug, Clone, Default)]
pub struct WeeklySummary {
    pub cost_usd: f64,
    pub total_tokens: u64,
    #[allow(dead_code)]
    pub session_count: usize,
    pub today_cost_usd: f64,
}

/// Compute weekly and daily cost/token summary from history.
pub fn weekly_summary() -> WeeklySummary {
    let week_secs = 7 * 86400;
    let day_secs = 86400;
    let week_records = load_history(Some(week_secs));
    let day_records = load_history(Some(day_secs));

    WeeklySummary {
        cost_usd: week_records.iter().map(|r| r.cost_usd).sum(),
        total_tokens: week_records
            .iter()
            .map(|r| r.input_tokens + r.output_tokens)
            .sum(),
        session_count: week_records.len(),
        today_cost_usd: day_records.iter().map(|r| r.cost_usd).sum(),
    }
}

/// Per-project rollup within the all-time summary.
#[derive(Debug, Clone, Default)]
pub struct ProjectTotal {
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub session_count: usize,
}

/// All-time usage across every recorded session (BACKLOG: "Persistent cost" —
/// the repo collection as a whole *and* per project). Derived by scanning the
/// full history CSV; cheap relative to the TUI's ~30s summary refresh.
#[derive(Debug, Clone, Default)]
pub struct AllTimeSummary {
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub session_count: usize,
    /// Keyed by the record's `project` field (a session's `display_name()`), so
    /// it matches a project's directory name unless a custom session name was set.
    pub per_project: std::collections::HashMap<String, ProjectTotal>,
}

impl AllTimeSummary {
    /// All-time total for a project by its history name. `None` when that project
    /// has no recorded sessions yet.
    pub fn project(&self, name: &str) -> Option<&ProjectTotal> {
        self.per_project.get(name)
    }
}

/// Compute the all-time cost/token summary (collection-wide + per project).
pub fn all_time_summary() -> AllTimeSummary {
    aggregate_all_time(&load_history(None))
}

/// Pure rollup of records into an [`AllTimeSummary`] (split out so it's testable
/// without touching the on-disk history file).
fn aggregate_all_time(records: &[SessionRecord]) -> AllTimeSummary {
    let mut summary = AllTimeSummary::default();
    for r in records {
        let tokens = r.input_tokens + r.output_tokens;
        summary.cost_usd += r.cost_usd;
        summary.total_tokens += tokens;
        summary.session_count += 1;
        let entry = summary.per_project.entry(r.project.clone()).or_default();
        entry.cost_usd += r.cost_usd;
        entry.total_tokens += tokens;
        entry.session_count += 1;
    }
    summary
}

// ─── Persistent running tally (the cost ledger) ──────────────────────────────
//
// The fleet strip's day/week/all-time totals and the activity heatmap must
// **survive restarts** — a running tally, not a per-launch one. The legacy
// `history.csv` only ever captured a session at the instant it reached
// `Finished` (rare for interactive agents), so those counters effectively reset
// every launch. The ledger fixes that: it stores the latest-known cost for
// **every** session keyed by Claude Code's stable `session_id`, upserted on
// every refresh (live sessions included). Re-observing a session — next tick or
// after a herdr restart — overwrites its entry instead of appending, so the
// tally is never double-counted and a crash loses at most one save interval.
// `history.csv` stays put as the CLI `history`/`stats` archive (§ `print_*`).

use std::collections::HashMap;

/// One session's latest-known usage, as persisted in the ledger. Serialized as
/// the value of a `session_id → entry` JSON map.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LedgerEntry {
    /// Session start, epoch **milliseconds** (matches `ClaudeSession::started_at`).
    /// The session's whole cost is attributed to this day in the heatmap/day
    /// buckets — consistent with how `history.csv` bucketed by record time.
    pub started_at_ms: u64,
    pub project: String,
    pub model: String,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The persistent cost tally. Entries are keyed by `session_id`; summaries are
/// derived on demand (cheap in-memory folds). Disk writes are throttled by the
/// caller (the TUI saves every ~30s and on quit).
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    entries: HashMap<String, LedgerEntry>,
    /// On-disk location, or `None` for an ephemeral ledger (tests / demo) that
    /// never touches disk — so unit tests can't clobber the real tally.
    path: Option<PathBuf>,
    /// Set on `upsert`/seed; cleared on a successful `save`. Skips no-op writes.
    dirty: bool,
}

fn ledger_path() -> PathBuf {
    history_dir().join("ledger.json")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// One-time migration: turn the legacy `history.csv` archive into ledger entries
/// so an existing user keeps their accrued tally. Synthetic keys (`csv:<n>`) —
/// those rows have no `session_id` — and they never collide with live sessions
/// (a CSV row is a *finished* session whose process is long gone), so seeding is
/// safe. Runs only when `ledger.json` is absent; thereafter the ledger owns it.
fn seed_from_csv() -> HashMap<String, LedgerEntry> {
    load_history(None)
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            (
                format!("csv:{i}"),
                LedgerEntry {
                    started_at_ms: parse_timestamp_epoch(&r.timestamp).unwrap_or(0) * 1000,
                    project: r.project,
                    model: r.model,
                    cost_usd: r.cost_usd,
                    input_tokens: r.input_tokens,
                    output_tokens: r.output_tokens,
                },
            )
        })
        .collect()
}

impl Ledger {
    /// Load the persistent ledger from `~/.local/share/claudectl/ledger.json`.
    /// On first run (file absent) seed from the legacy CSV; a corrupt/partial
    /// file degrades to empty rather than crashing (CLAUDE.md: defensive parsing).
    pub fn load() -> Self {
        let path = ledger_path();
        match fs::read_to_string(&path) {
            Ok(s) => Ledger {
                entries: serde_json::from_str(&s).unwrap_or_default(),
                path: Some(path),
                dirty: false,
            },
            Err(_) => Ledger {
                entries: seed_from_csv(),
                path: Some(path),
                dirty: true, // persist the one-time CSV migration on the next save
            },
        }
    }

    /// An in-memory ledger that never writes to disk (tests, demo mode).
    pub fn ephemeral() -> Self {
        Ledger::default()
    }

    /// Record a session's **current** totals, keyed by its stable session id.
    /// Idempotent: the latest call for a given id wins. Sessions without an id
    /// are skipped (nothing stable to key on).
    pub fn upsert(&mut self, s: &crate::session::ClaudeSession) {
        self.record(
            &s.session_id,
            LedgerEntry {
                started_at_ms: s.started_at,
                project: s.display_name().to_string(),
                model: s.model.clone(),
                cost_usd: s.cost_usd,
                input_tokens: s.total_input_tokens,
                output_tokens: s.total_output_tokens,
            },
        );
    }

    /// The keying core of [`upsert`], split out so it's testable without
    /// constructing a full `ClaudeSession`.
    fn record(&mut self, session_id: &str, mut entry: LedgerEntry) {
        if session_id.is_empty() {
            return;
        }
        // Keep the earliest-seen start so the heatmap's start-day bucket is stable
        // across the many re-observations of a long-running session.
        if let Some(existing) = self.entries.get(session_id) {
            if existing.started_at_ms != 0 {
                entry.started_at_ms = existing.started_at_ms;
            }
        }
        self.entries.insert(session_id.to_string(), entry);
        self.dirty = true;
    }

    /// Persist to disk if backed by a real path and changed since the last save.
    /// Atomic (tmp + rename) so a crash mid-write can't corrupt the tally.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = self.path.clone() else { return };
        let Some(dir) = path.parent() else { return };
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        let Ok(json) = serde_json::to_string(&self.entries) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, json.as_bytes()).is_ok() && fs::rename(&tmp, &path).is_ok() {
            self.dirty = false;
        }
    }

    /// Daily + weekly totals, derived from the ledger (windowed on each session's
    /// start time). Mirrors the legacy [`weekly_summary`] shape.
    pub fn weekly_summary(&self) -> WeeklySummary {
        let now = now_secs();
        let mut s = WeeklySummary::default();
        for e in self.entries.values() {
            let age = now.saturating_sub(e.started_at_ms / 1000);
            if age <= 7 * 86400 {
                s.cost_usd += e.cost_usd;
                s.total_tokens += e.input_tokens + e.output_tokens;
                s.session_count += 1;
            }
            if age <= 86400 {
                s.today_cost_usd += e.cost_usd;
            }
        }
        s
    }

    /// All-time totals (collection-wide + per project) across every ledger entry.
    pub fn all_time_summary(&self) -> AllTimeSummary {
        let mut s = AllTimeSummary::default();
        for e in self.entries.values() {
            let tokens = e.input_tokens + e.output_tokens;
            s.cost_usd += e.cost_usd;
            s.total_tokens += tokens;
            s.session_count += 1;
            let p = s.per_project.entry(e.project.clone()).or_default();
            p.cost_usd += e.cost_usd;
            p.total_tokens += tokens;
            p.session_count += 1;
        }
        s
    }

    /// Per-day cost for the last `days` days, oldest→newest, zero-filled — the
    /// ledger-backed twin of [`daily_cost_series`].
    fn daily_cost_series(&self, days: usize) -> Vec<f64> {
        if days == 0 {
            return Vec::new();
        }
        let today = now_secs() / 86_400;
        let mut series = vec![0.0; days];
        for e in self.entries.values() {
            let day = (e.started_at_ms / 1000) / 86_400;
            if day > today {
                continue;
            }
            let back = (today - day) as usize;
            if back < days {
                series[days - 1 - back] += e.cost_usd;
            }
        }
        series
    }

    /// Activity-heatmap intensity levels for the last `days` days.
    pub fn daily_activity(&self, days: usize) -> Vec<u8> {
        intensities(&self.daily_cost_series(days))
    }
}

/// Parse a duration string like "24h", "30m", "7d" into seconds.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: u64 = num_str.parse().ok()?;
    match unit {
        "s" => Some(num),
        "m" => Some(num * 60),
        "h" => Some(num * 3600),
        "d" => Some(num * 86400),
        "w" => Some(num * 604800),
        _ => None,
    }
}

fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m{s:02}s")
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("24h"), Some(86400));
        assert_eq!(parse_duration("30m"), Some(1800));
        assert_eq!(parse_duration("7d"), Some(604800));
        assert_eq!(parse_duration("1w"), Some(604800));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(3661), "1h01m");
        assert_eq!(format_duration(125), "2m05s");
        assert_eq!(format_duration(0), "0m00s");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world!", 8), "hello...");
    }

    #[test]
    fn test_parse_timestamp_epoch() {
        // 2026-01-01T00:00:00Z
        let ts = parse_timestamp_epoch("2026-01-01T00:00:00Z").unwrap();
        // Should be reasonable (after 2025)
        assert!(ts > 1735689600); // 2025-01-01
        assert!(ts < 1798761600); // 2027-01-01
    }

    #[test]
    fn test_is_leap() {
        assert!(is_leap(2000));
        assert!(is_leap(2024));
        assert!(!is_leap(1900));
        assert!(!is_leap(2023));
    }

    #[test]
    fn aggregate_all_time_rolls_up_collection_and_per_project() {
        let rec = |project: &str, input: u64, output: u64, cost: f64| SessionRecord {
            timestamp: "2026-01-01T00:00:00Z".into(),
            pid: 1,
            project: project.into(),
            model: "sonnet".into(),
            duration_secs: 60,
            input_tokens: input,
            output_tokens: output,
            cost_usd: cost,
        };
        let records = vec![
            rec("alpha", 100, 50, 1.0),
            rec("alpha", 200, 100, 2.0),
            rec("beta", 10, 5, 0.5),
        ];
        let s = aggregate_all_time(&records);
        assert_eq!(s.session_count, 3);
        assert!((s.cost_usd - 3.5).abs() < 1e-9);
        assert_eq!(s.total_tokens, 465);

        let alpha = s.project("alpha").expect("alpha present");
        assert_eq!(alpha.session_count, 2);
        assert!((alpha.cost_usd - 3.0).abs() < 1e-9);
        assert_eq!(alpha.total_tokens, 450);
        assert_eq!(s.project("beta").unwrap().total_tokens, 15);
        assert!(s.project("missing").is_none());
    }
}

#[cfg(test)]
mod ledger_tests {
    use super::*;

    impl Ledger {
        fn from_entries(entries: HashMap<String, LedgerEntry>) -> Self {
            Ledger {
                entries,
                path: None,
                dirty: false,
            }
        }
    }

    fn entry(project: &str, started_at_ms: u64, cost: f64, tokens: u64) -> LedgerEntry {
        LedgerEntry {
            started_at_ms,
            project: project.into(),
            model: "sonnet".into(),
            cost_usd: cost,
            input_tokens: tokens,
            output_tokens: 0,
        }
    }

    #[test]
    fn all_time_summary_rolls_up_every_entry_regardless_of_age() {
        // Two old, one recent — all-time counts them all.
        let entries = HashMap::from([
            ("s1".into(), entry("alpha", 0, 1.0, 100)),
            ("s2".into(), entry("alpha", 0, 2.0, 200)),
            ("s3".into(), entry("beta", 0, 0.5, 50)),
        ]);
        let s = Ledger::from_entries(entries).all_time_summary();
        assert_eq!(s.session_count, 3);
        assert!((s.cost_usd - 3.5).abs() < 1e-9);
        assert_eq!(s.total_tokens, 350);
        assert!((s.project("alpha").unwrap().cost_usd - 3.0).abs() < 1e-9);
    }

    #[test]
    fn weekly_summary_windows_today_and_week_by_start_time() {
        let now_ms = now_secs() * 1000;
        let day_ago = now_ms - 2 * 86_400 * 1000; // within week, not today
        let month_ago = now_ms - 30 * 86_400 * 1000; // outside the week
        let entries = HashMap::from([
            ("today".into(), entry("a", now_ms, 1.0, 10)),
            ("thisweek".into(), entry("a", day_ago, 2.0, 20)),
            ("old".into(), entry("a", month_ago, 4.0, 40)),
        ]);
        let s = Ledger::from_entries(entries).weekly_summary();
        assert!((s.today_cost_usd - 1.0).abs() < 1e-9, "only today's session");
        assert!((s.cost_usd - 3.0).abs() < 1e-9, "today + this-week, not month");
        assert_eq!(s.total_tokens, 30);
    }

    #[test]
    fn upsert_is_idempotent_by_session_id() {
        // Re-observing the same session (rising cost) overwrites, never doubles.
        let mut ledger = Ledger::ephemeral();
        ledger.record("abc", entry("proj", 0, 1.0, 100));
        ledger.record("abc", entry("proj", 0, 2.5, 250)); // same id, more spend
        let summary = ledger.all_time_summary();
        assert_eq!(summary.session_count, 1, "one session, not two");
        assert!((summary.cost_usd - 2.5).abs() < 1e-9, "latest cost wins");
        assert_eq!(summary.total_tokens, 250);

        // A blank session id is unkeyable — skipped.
        ledger.record("", entry("proj", 0, 9.0, 0));
        assert_eq!(ledger.all_time_summary().session_count, 1);
    }

    #[test]
    fn ephemeral_ledger_never_writes() {
        let mut ledger = Ledger::ephemeral();
        ledger.record("x", entry("proj", 0, 1.0, 10));
        assert!(ledger.dirty);
        ledger.save(); // no path → no-op, stays dirty
        assert!(ledger.dirty, "ephemeral save is a no-op");
    }
}
