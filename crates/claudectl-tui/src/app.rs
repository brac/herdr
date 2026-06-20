use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use claudectl_core::discovery;
use claudectl_core::git::{self, GitStatus};
use claudectl_core::helpers::{
    create_aggregate_session, dirs_home, fire_notification, fire_webhook,
};
use claudectl_core::hooks::{HookEvent, HookRegistry};
use claudectl_core::launch::{self, LaunchRequest};
use claudectl_core::monitor;
use claudectl_core::process;
use claudectl_core::projects::{self, Project};
use claudectl_core::session::{ClaudeSession, SessionStatus};
use claudectl_core::terminals;
use claudectl_core::theme::Theme;

pub const SORT_COLUMNS: &[&str] = &["Status", "Context", "Cost", "$/hr", "Elapsed"];

/// Phase 5: how many fleet-burn samples the trend sparkline keeps. At ~2s/sample
/// that's ~80s of recent history — enough to read the fleet's spend trajectory
/// without holding unbounded data.
pub const FLEET_HISTORY_CAP: usize = 40;
/// Minimum spacing between fleet-burn samples, so event-driven refreshes don't
/// bunch up the trend's time axis (sample at roughly the safety-net tick rate).
const FLEET_SAMPLE_MIN_MS: u64 = 1_800;

/// Skip per-agent burn-rate samples taken over a window shorter than this. The
/// Phase-2.5 event loop can refresh sub-second during a JSONL burst; dividing a
/// cost delta by a near-zero interval is what sent $/hr to absurd spikes
/// ("6000/h"). Wait for a meaningful window before recomputing the rate.
const MIN_BURN_SAMPLE_MS: u64 = 2_000;
/// EMA weight for the new burn sample. Low → steady trailing average rather than
/// a per-tick reading, which is what made $/hr jump "all over the place".
const BURN_EMA_ALPHA: f64 = 0.3;

/// Gentle symmetric EMA so $/hr reads as a smoothed trailing average. Used both
/// on the way up and down, so a finished agent's rate eases to zero instead of
/// snapping (the decay also clears it once it falls below the display floor).
fn smooth_burn(prev: f64, sample: f64) -> f64 {
    prev + BURN_EMA_ALPHA * (sample - prev)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    All,
    NeedsInput,
    Processing,
    WaitingInput,
    Unknown,
    Idle,
    Finished,
}

impl StatusFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::NeedsInput,
            Self::NeedsInput => Self::Processing,
            Self::Processing => Self::WaitingInput,
            Self::WaitingInput => Self::Unknown,
            Self::Unknown => Self::Idle,
            Self::Idle => Self::Finished,
            Self::Finished => Self::All,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "needsinput" | "needs-input" => Some(Self::NeedsInput),
            "processing" => Some(Self::Processing),
            "waiting" | "waitinginput" | "waiting-input" => Some(Self::WaitingInput),
            "unknown" => Some(Self::Unknown),
            "idle" => Some(Self::Idle),
            "finished" => Some(Self::Finished),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::NeedsInput => "Needs Input",
            Self::Processing => "Processing",
            Self::WaitingInput => "Waiting",
            Self::Unknown => "Unknown",
            Self::Idle => "Idle",
            Self::Finished => "Finished",
        }
    }

    fn matches(self, status: SessionStatus) -> bool {
        match self {
            Self::All => true,
            Self::NeedsInput => status == SessionStatus::NeedsInput,
            Self::Processing => status == SessionStatus::Processing,
            Self::WaitingInput => status == SessionStatus::WaitingInput,
            Self::Unknown => status == SessionStatus::Unknown,
            Self::Idle => status == SessionStatus::Idle,
            Self::Finished => status == SessionStatus::Finished,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusFilter {
    All,
    Attention,
    OverBudget,
    HighContext,
    UnknownTelemetry,
    Conflict,
}

impl FocusFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Attention,
            Self::Attention => Self::OverBudget,
            Self::OverBudget => Self::HighContext,
            Self::HighContext => Self::UnknownTelemetry,
            Self::UnknownTelemetry => Self::Conflict,
            Self::Conflict => Self::All,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "attention" => Some(Self::Attention),
            "overbudget" | "over-budget" => Some(Self::OverBudget),
            "highcontext" | "high-context" => Some(Self::HighContext),
            "unknowntelemetry" | "unknown-telemetry" => Some(Self::UnknownTelemetry),
            "conflict" | "conflicts" => Some(Self::Conflict),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Attention => "Attention",
            Self::OverBudget => "Over Budget",
            Self::HighContext => "High Context",
            Self::UnknownTelemetry => "Unknown Telemetry",
            Self::Conflict => "Conflict",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchField {
    Cwd,
    Prompt,
    Resume,
}

impl LaunchField {
    fn next(self) -> Self {
        match self {
            Self::Cwd => Self::Prompt,
            Self::Prompt => Self::Resume,
            Self::Resume => Self::Resume,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Cwd => Self::Cwd,
            Self::Prompt => Self::Cwd,
            Self::Resume => Self::Prompt,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cwd => "cwd",
            Self::Prompt => "prompt",
            Self::Resume => "resume",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchForm {
    pub field: LaunchField,
    pub cwd: String,
    pub prompt: String,
    pub resume: String,
}

impl Default for LaunchForm {
    fn default() -> Self {
        Self {
            field: LaunchField::Cwd,
            cwd: ".".into(),
            prompt: String::new(),
            resume: String::new(),
        }
    }
}

impl LaunchForm {
    pub fn active_buffer(&self) -> &str {
        match self.field {
            LaunchField::Cwd => &self.cwd,
            LaunchField::Prompt => &self.prompt,
            LaunchField::Resume => &self.resume,
        }
    }

    fn active_buffer_mut(&mut self) -> &mut String {
        match self.field {
            LaunchField::Cwd => &mut self.cwd,
            LaunchField::Prompt => &mut self.prompt,
            LaunchField::Resume => &mut self.resume,
        }
    }

    fn advance(&mut self) {
        self.field = self.field.next();
    }

    fn retreat(&mut self) {
        self.field = self.field.prev();
    }

    fn is_last_field(&self) -> bool {
        self.field == LaunchField::Resume
    }

    pub fn status_hint(&self) -> String {
        format!(
            "New session [{}] Enter next, Tab move, Ctrl+Enter launch, Esc cancel",
            self.field.label()
        )
    }

    fn request(&self) -> Result<LaunchRequest, String> {
        launch::prepare(
            &self.cwd,
            Some(self.prompt.as_str()),
            Some(self.resume.as_str()),
        )
    }

    pub fn summary(&self) -> String {
        let cwd = compact_value(&self.cwd, ".");
        let prompt = if self.prompt.trim().is_empty() {
            "skip".to_string()
        } else {
            "set".to_string()
        };
        let resume = compact_value(&self.resume, "skip");
        format!("cwd={cwd} | prompt={prompt} | resume={resume}")
    }
}

fn compact_value(value: &str, empty_label: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return empty_label.to_string();
    }

    const MAX_LEN: usize = 24;
    if trimmed.chars().count() <= MAX_LEN {
        trimmed.to_string()
    } else {
        let prefix: String = trimmed.chars().take(MAX_LEN - 1).collect();
        format!("{prefix}…")
    }
}

/// A push or pull (Phase 3c). The verb feeds status messages and the throbber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitOpKind {
    Push,
    Pull,
}

impl GitOpKind {
    /// The `git` subcommand.
    fn arg(self) -> &'static str {
        match self {
            GitOpKind::Push => "push",
            GitOpKind::Pull => "pull",
        }
    }
    /// Human verb for status/throbber text.
    pub fn verb(self) -> &'static str {
        self.arg()
    }
}

/// An in-flight fire-and-forget git push/pull for one project (Phase 3c, §4).
/// The render loop polls `child` with `try_wait()`; `started` drives the throbber.
pub struct GitOp {
    pub kind: GitOpKind,
    child: Child,
    started: Instant,
}

/// Background git-status worker (CLAUDE.md §3/§4). `git status` on the WSL
/// `/mnt/c` mount costs 100ms–1s+ per repo; computing it for every project on
/// the render thread froze the UI on each cache-TTL expiry. So status is fetched
/// on a worker thread and the cache is updated from results — spawn-and-forget,
/// the render loop never blocks on `git`. One worker processes requests serially
/// in the background; the UI stays responsive while statuses trickle in.
struct GitStatusService {
    req_tx: Sender<PathBuf>,
    res_rx: Receiver<(PathBuf, Option<GitStatus>)>,
}

impl GitStatusService {
    fn spawn() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<(PathBuf, Option<GitStatus>)>();
        std::thread::spawn(move || {
            // Exits when `req_rx` closes (App dropped → req_tx gone).
            while let Ok(path) = req_rx.recv() {
                let status = git::status(&path);
                if res_tx.send((path, status)).is_err() {
                    break;
                }
            }
        });
        Self { req_tx, res_rx }
    }
}

/// Phase 4c approval-inspector action delivered to an agent's pane. Each maps to
/// a keystroke via the inherited `terminals/` backend (approve = Enter, deny /
/// interrupt = Escape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApprovalAct {
    Approve,
    Deny,
    Interrupt,
}

impl ApprovalAct {
    /// Verb for an error status ("Approve failed: …").
    fn verb(self) -> &'static str {
        match self {
            ApprovalAct::Approve => "Approve",
            ApprovalAct::Deny => "Deny",
            ApprovalAct::Interrupt => "Interrupt",
        }
    }

    /// Past tense for a success status ("Approved foo").
    fn past_tense(self) -> &'static str {
        match self {
            ApprovalAct::Approve => "Approved",
            ApprovalAct::Deny => "Denied",
            ApprovalAct::Interrupt => "Interrupted",
        }
    }
}

/// Phase 5 fleet roll-up for the trend strip: per-status agent counts plus the
/// summed live burn. A single glance at the whole fleet, which the per-row roster
/// can't give without scanning every line.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FleetCounts {
    pub needs_input: usize,
    pub processing: usize,
    pub waiting: usize,
    pub idle: usize,
    pub total: usize,
    pub burn_per_hr: f64,
}

pub struct App {
    pub sessions: Vec<ClaudeSession>,
    /// Parent dir herdr was launched from; the project roster scans this (§2).
    pub parent_dir: PathBuf,
    /// Project directories discovered under `parent_dir`. Project-first: these
    /// exist whether or not they currently host any agents.
    pub projects: Vec<Project>,
    /// Widen the project scan to non-git subdirectories (default false = .git only).
    pub include_non_git: bool,
    /// Per-project git status (Phase 3), keyed by project path. A present key
    /// means "fetched" (`Some` = status, `None` = no status / failed), so we
    /// never auto-refetch a project we already have. Event-driven only — no TTL,
    /// no periodic sweep (see `refresh_git_cache` / `enqueue_git`).
    pub git_cache: HashMap<PathBuf, Option<GitStatus>>,
    /// Background worker that computes `git status` off the render thread (§3).
    git_svc: GitStatusService,
    /// Projects whose status request is in flight on the worker — avoids
    /// re-enqueuing the same path while it's being computed.
    git_inflight: HashSet<PathBuf>,
    /// In-flight fire-and-forget push/pull per project (Phase 3c). Polled each
    /// refresh via `try_wait`; drives the throbber while present.
    pub git_ops: HashMap<PathBuf, GitOp>,
    pub table_state: TableState,
    pub should_quit: bool,
    pub status_msg: String,
    pub pending_kill: Option<u32>,
    /// PIDs already sent SIGTERM that are still alive — a repeat kill escalates
    /// to SIGKILL (CLAUDE.md §3 defensive: never crash if a PID is gone).
    pub sigterm_sent: HashSet<u32>,
    pub input_mode: bool,
    pub input_buffer: String,
    pub input_target_pid: Option<u32>,
    // ── Role-bind input mode (#307) ──────────────────────────────────────
    /// When true, keystrokes accumulate in `role_bind_buffer` instead of
    /// triggering normal-mode handlers. Entered via Ctrl+R on the
    /// dashboard; Esc cancels, Enter commits via `Actions::bind_bus_role`.
    pub role_bind_mode: bool,
    pub role_bind_buffer: String,
    pub role_bind_target_pid: Option<u32>,
    pub role_bind_target_cwd: Option<String>,
    pub notify: bool,
    pub prev_statuses: HashMap<u32, SessionStatus>,
    pub show_help: bool,
    pub sort_column: usize,
    pub auto_approve: HashSet<u32>,
    pub pending_auto_approve: Option<u32>,
    /// PID awaiting override reason (1=always safe, 2=one-time, 3=brain is wrong).
    pub pending_override_reason: Option<u32>,
    pub finished_at: HashMap<u32, std::time::Instant>, // When PIDs were first seen as Finished
    pub debug: bool,
    pub debug_timings: DebugTimings,
    pub grouped_view: bool,
    pub detail_panel: bool, // Show expanded detail for selected session
    /// Phase 5: show the fleet trend strip beneath the roster (toggle with `G`).
    pub show_fleet: bool,
    /// Rolling samples of total fleet burn ($/hr summed across agents), newest
    /// last. Drives the trend sparkline — the time axis no single roster row has.
    pub fleet_burn_history: Vec<f64>,
    /// Wall-clock (ms) of the last fleet-burn sample; gates the sample cadence.
    last_fleet_sample_ms: u64,
    pub webhook_url: Option<String>,
    pub webhook_filter: Option<Vec<String>>, // Only fire on these status names
    pub launch_mode: bool,                   // Capturing launch wizard fields
    pub launch_form: LaunchForm,
    pub search_mode: bool,
    pub search_buffer: String,
    pub search_query: String,
    pub status_filter: StatusFilter,
    pub focus_filter: FocusFilter,
    pub budget_usd: Option<f64>,     // Per-session budget
    pub kill_on_budget: bool,        // Auto-kill when budget exceeded
    pub budget_warned: HashSet<u32>, // PIDs that have been warned at 80%
    pub budget_killed: HashSet<u32>, // PIDs that have been killed
    pub theme: Theme,
    pub weekly_summary: claudectl_core::history::WeeklySummary,
    /// All-time persistent cost/tokens (collection-wide + per project), refreshed
    /// on the same cadence as `weekly_summary`.
    pub all_time_summary: claudectl_core::history::AllTimeSummary,
    pub weekly_summary_tick: u32, // Refresh every N ticks
    pub hooks: HookRegistry,
    pub daily_limit: Option<f64>,
    pub weekly_limit: Option<f64>,
    pub daily_alert_fired: bool, // Prevent repeated alerts per app session
    pub weekly_alert_fired: bool,
    pub context_warn_threshold: u8, // 0-100, fires on_context_high hook
    pub context_warned: HashSet<u32>, // PIDs that have been warned (reset if context drops below threshold)
    pub needs_input_since: HashMap<u32, std::time::Instant>, // When each PID entered NeedsInput
    pub conflict_pids: HashSet<u32>,  // PIDs that share a working directory with another session
    pub conflict_alerted: HashSet<String>, // cwds that have already triggered a conflict alert
    pub file_conflict_pids: HashSet<u32>, // PIDs involved in file-level conflicts
    pub file_conflicts: HashMap<String, Vec<u32>>, // file path → PIDs that modified it
    pub file_conflict_alerted: HashSet<String>, // Files already alerted
    pub file_conflicts_enabled: bool, // Config: detect file-level conflicts
    pub auto_deny_file_conflicts: bool, // Config: auto-deny conflicting writes
    pub demo_mode: bool,
    pub demo_tick: u32,
    pub demo_highlight: Option<crate::demo::DemoHighlightState>,
    pub session_recordings: HashMap<u32, String>, // pid -> output_path for active recordings
    pub rules: Vec<claudectl_core::rules::AutoRule>,
    pub auto_actions_fired: HashMap<u32, std::time::Instant>, // Debounce: pid -> last action time
    pub last_rule_action: Option<String>,                     // Last auto-action status for display
    pub health_thresholds: claudectl_core::health::HealthThresholds,
    pub brain_config: Option<claudectl_core::config::BrainConfig>,
    /// Stateful brain driver, swapped in by `main.rs` when the brain is
    /// configured. Held as `Box<dyn BrainDriver>` (not `Arc`) because every
    /// method needs `&mut`. `None` when the brain is off.
    pub brain_driver: Option<Box<dyn claudectl_core::runtime::BrainDriver>>,
    pub idle_config: claudectl_core::config::IdleConfig,
    pub last_user_interaction: std::time::Instant,
    pub idle_mode_active: bool,
    pub idle_tasks_launched: Vec<String>,
    pub idle_report: Vec<String>,
    // Coordination layer (feature-gated)
    #[cfg(feature = "coord")]
    pub coord_leases: Vec<claudectl_core::runtime::LeaseSummary>,
    #[cfg(feature = "coord")]
    pub coord_handoffs: Vec<claudectl_core::runtime::HandoffSummary>,
    #[cfg(feature = "coord")]
    pub coord_lease_sessions: HashSet<String>,
    #[cfg(feature = "coord")]
    pub coord_handoff_sessions: HashSet<String>,
    #[cfg(feature = "coord")]
    pub coord_interrupt_targets: HashSet<String>,
    #[cfg(feature = "coord")]
    pub coord_pending_interrupts: Vec<claudectl_core::runtime::InterruptSummary>,
    #[cfg(feature = "coord")]
    pub coord_tick: u32,
    // Relay peers panel (feature-gated)
    #[cfg(feature = "relay")]
    pub show_peers_panel: bool,
    // relay_peers is populated when relay serve is active and rendered by
    // ui::peers::render_peers_panel when show_peers_panel is true. Currently
    // a stub — rendering integration is wired when the relay serve loop runs
    // inside the TUI (not yet connected to the TUI render loop).
    #[cfg(feature = "relay")]
    #[allow(dead_code)]
    pub relay_peers: Vec<crate::ui::peers::PeerDisplayInfo>,
    /// Remote sessions received from connected worker peers (relay heartbeats).
    #[cfg(feature = "relay")]
    pub remote_sessions: Vec<claudectl_core::session::ClaudeSession>,

    // ── Skills & Hive overlay state ────────────────────────────────────────
    /// Whether the skills/hive overlay is open.
    pub show_skills: bool,
    /// Which tab is currently active inside the overlay.
    pub skills_tab: SkillsTab,
    /// Currently selected index into `skills`.
    pub skills_selected: usize,
    /// Discovered skills (refreshed when the overlay opens or `r` is pressed).
    pub skills: Vec<claudectl_core::skills::DiscoveredSkill>,
    /// Semantic keys (`skill:<name>`) for skills already present in the hive store.
    pub shared_skill_keys: std::collections::HashSet<String>,
    /// Transient status message shown in the overlay footer.
    pub skills_status_msg: Option<String>,
    /// True when a `claudectl relay serve` subprocess has been started from the TUI.
    pub hive_listener_running: bool,
    /// Local peer identity, populated when the Hive tab is opened.
    pub hive_identity: Option<String>,
    /// Known peers from the local relay state (peer id, optional last address).
    pub hive_known_peers: Vec<(String, Option<String>)>,
    /// Last invite generated from the TUI (held in memory only).
    pub hive_last_invite: Option<HiveInvite>,
    /// When true, the overlay captures text input for a join code.
    pub hive_join_input_mode: bool,
    /// Buffer for the join input.
    pub hive_join_buffer: String,

    // ── Brain review overlay state ─────────────────────────────────────────
    /// Whether the brain review/scorecard overlay is open.
    pub show_brain: bool,
    /// Phase 4 conversation view: whether the chat overlay is open, which agent
    /// it's pinned to (by PID, so roster reordering doesn't switch it), and how
    /// far it's scrolled back from the newest message (0 = pinned to bottom).
    pub show_chat: bool,
    pub chat_pid: Option<u32>,
    pub chat_scroll: u16,
    /// Phase 4b reply box: the prompt being composed in the chat overlay. The
    /// box is always focused for typing — arrows/PgUp/PgDn scroll, Esc closes.
    pub chat_input: String,
    /// Whether this terminal can inject keystrokes into agent panes (computed
    /// once at startup). False outside tmux/Kitty on WSL → chat is read-only.
    pub input_supported: bool,
    /// Phase 4c approval inspector: PID of the agent whose permission prompt is
    /// being inspected (modal open when `Some`). `A` opens it for the selected
    /// agent; y/n/i act, Esc closes. None = no modal.
    pub approval_pid: Option<u32>,
    /// Captured tmux pane text (the rendered permission dialog) shown in the
    /// approval inspector so you can see *what* you're approving (CLAUDE.md §0.1
    /// read-only scrape). Empty when the capture failed or isn't available.
    pub approval_preview: String,
    /// The tmux pane id of the agent currently shown in herdr's split "stage"
    /// (only one at a time). `o` swaps it; launches replace it. None = nothing staged.
    pub staged_pane: Option<String>,
    /// Which tab is currently active inside the brain overlay.
    pub brain_tab: BrainTab,
    /// Currently selected index into `brain_queue`.
    pub brain_review_selected: usize,
    /// Prioritized review candidates (refreshed when the overlay opens or `r` is pressed).
    pub brain_queue: Vec<claudectl_core::runtime::ReviewItemSummary>,
    /// All decision records loaded for the scorecard view.
    pub brain_decisions_cache: Vec<claudectl_core::runtime::DecisionSummary>,
    /// Transient status message shown in the overlay footer.
    pub brain_status_msg: Option<String>,
    /// When true, the overlay captures text input for a canonical-note.
    pub brain_note_input_mode: bool,
    /// Buffer for the in-progress note.
    pub brain_note_buffer: String,

    /// UI ↔ runtime contract (epic #279, issue #275). `App::new` starts with
    /// an in-memory `MockRuntime`; `main` swaps in the live runtime at
    /// startup. Call sites prefer `self.runtime.{view,actions,...}.method()`
    /// over `crate::brain::*` / `crate::coord::*` so that future TUI
    /// extraction is a mechanical file move.
    pub runtime: claudectl_core::runtime::Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillsTab {
    Skills,
    Hive,
}

impl SkillsTab {
    pub fn toggle(self) -> Self {
        match self {
            Self::Skills => Self::Hive,
            Self::Hive => Self::Skills,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainTab {
    Scorecard,
    Review,
}

impl BrainTab {
    pub fn toggle(self) -> Self {
        match self {
            Self::Scorecard => Self::Review,
            Self::Review => Self::Scorecard,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HiveInvite {
    pub relay_code: String,
    pub invite_link: String,
    pub word_phrase: String,
}

#[derive(Default, Clone)]
pub struct DebugTimings {
    pub scan_ms: f64,
    pub ps_ms: f64,
    pub jsonl_ms: f64,
    pub total_ms: f64,
    // Rolling averages (last 10 ticks)
    history: Vec<(f64, f64, f64, f64)>,
}

impl DebugTimings {
    pub fn record(&mut self, scan: f64, ps: f64, jsonl: f64, total: f64) {
        self.scan_ms = scan;
        self.ps_ms = ps;
        self.jsonl_ms = jsonl;
        self.total_ms = total;
        self.history.push((scan, ps, jsonl, total));
        if self.history.len() > 10 {
            self.history.remove(0);
        }
    }

    pub fn avg_total_ms(&self) -> f64 {
        if self.history.is_empty() {
            return 0.0;
        }
        self.history.iter().map(|h| h.3).sum::<f64>() / self.history.len() as f64
    }

    pub fn format(&self) -> String {
        format!(
            "tick: {:.1}ms (avg {:.1}ms) | scan: {:.1}ms | ps: {:.1}ms | jsonl: {:.1}ms",
            self.total_ms,
            self.avg_total_ms(),
            self.scan_ms,
            self.ps_ms,
            self.jsonl_ms,
        )
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Project a live `ClaudeSession` to the core `SessionSnapshot` DTO the
/// runtime traits accept. Used by `BrainDriver` call sites that have the
/// live values in memory already.
fn snapshot_from(session: &ClaudeSession) -> claudectl_core::runtime::SessionSnapshot {
    claudectl_core::runtime::SessionSnapshot {
        session_id: session.session_id.clone(),
        pid: session.pid,
        cwd: session.cwd.clone(),
        project_name: session.project_name.clone(),
        status: session.status.to_string(),
        cost_usd: session.cost_usd,
        context_tokens: session.context_tokens,
        context_max: session.context_max,
        last_message_ts: session.last_message_ts,
    }
}

/// Build a runtime `ObservationInput` from the live session + an observed-
/// action label. Centralizes the projection so call sites don't repeat the
/// field plumbing (cf. the 5 sites that used to call
/// `brain::decisions::log_observation` directly).
fn observation_from(
    session: &ClaudeSession,
    action: &str,
) -> claudectl_core::runtime::ObservationInput {
    claudectl_core::runtime::ObservationInput {
        session_pid: session.pid,
        project: session.display_name().to_string(),
        tool: session.pending_tool_name.clone(),
        command: session.pending_tool_input.clone(),
        observed_action: action.to_string(),
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_parent(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    /// Construct against a specific parent directory (the project roster root).
    /// `new()` delegates here with the process's current dir.
    pub fn with_parent(parent_dir: PathBuf) -> Self {
        let mut app = Self {
            sessions: Vec::new(),
            parent_dir,
            projects: Vec::new(),
            include_non_git: false,
            git_cache: HashMap::new(),
            git_svc: GitStatusService::spawn(),
            git_inflight: HashSet::new(),
            git_ops: HashMap::new(),
            table_state: TableState::default(),
            should_quit: false,
            status_msg: String::new(),
            pending_kill: None,
            sigterm_sent: HashSet::new(),
            input_mode: false,
            role_bind_mode: false,
            role_bind_buffer: String::new(),
            role_bind_target_pid: None,
            role_bind_target_cwd: None,
            input_buffer: String::new(),
            input_target_pid: None,
            notify: false,
            prev_statuses: HashMap::new(),
            show_help: false,
            sort_column: 0,
            auto_approve: HashSet::new(),
            pending_auto_approve: None,
            pending_override_reason: None,
            finished_at: HashMap::new(),
            debug: false,
            debug_timings: DebugTimings::default(),
            grouped_view: true,
            detail_panel: false,
            show_fleet: true,
            fleet_burn_history: Vec::new(),
            last_fleet_sample_ms: 0,
            webhook_url: None,
            webhook_filter: None,
            launch_mode: false,
            launch_form: LaunchForm::default(),
            search_mode: false,
            search_buffer: String::new(),
            search_query: String::new(),
            status_filter: StatusFilter::All,
            focus_filter: FocusFilter::All,
            budget_usd: None,
            kill_on_budget: false,
            budget_warned: HashSet::new(),
            budget_killed: HashSet::new(),
            theme: Theme::from_mode(claudectl_core::theme::ThemeMode::Dark),
            weekly_summary: claudectl_core::history::weekly_summary(),
            all_time_summary: claudectl_core::history::all_time_summary(),
            weekly_summary_tick: 0,
            hooks: HookRegistry::new(),
            daily_limit: None,
            weekly_limit: None,
            daily_alert_fired: false,
            weekly_alert_fired: false,
            context_warn_threshold: 75,
            context_warned: HashSet::new(),
            needs_input_since: HashMap::new(),
            conflict_pids: HashSet::new(),
            conflict_alerted: HashSet::new(),
            file_conflict_pids: HashSet::new(),
            file_conflicts: HashMap::new(),
            file_conflict_alerted: HashSet::new(),
            file_conflicts_enabled: true,
            auto_deny_file_conflicts: false,
            demo_mode: false,
            demo_tick: 0,
            demo_highlight: None,
            session_recordings: HashMap::new(),
            rules: Vec::new(),
            auto_actions_fired: HashMap::new(),
            last_rule_action: None,
            health_thresholds: claudectl_core::health::HealthThresholds::default(),
            brain_config: None,
            brain_driver: None,
            runtime: claudectl_core::runtime::MockRuntime::default().into_runtime(),
            idle_config: claudectl_core::config::IdleConfig::default(),
            last_user_interaction: std::time::Instant::now(),
            idle_mode_active: false,
            idle_tasks_launched: Vec::new(),
            idle_report: Vec::new(),
            #[cfg(feature = "coord")]
            coord_leases: Vec::new(),
            #[cfg(feature = "coord")]
            coord_handoffs: Vec::new(),
            #[cfg(feature = "coord")]
            coord_lease_sessions: HashSet::new(),
            #[cfg(feature = "coord")]
            coord_handoff_sessions: HashSet::new(),
            #[cfg(feature = "coord")]
            coord_interrupt_targets: HashSet::new(),
            #[cfg(feature = "coord")]
            coord_pending_interrupts: Vec::new(),
            #[cfg(feature = "coord")]
            coord_tick: 0,
            #[cfg(feature = "relay")]
            show_peers_panel: false,
            #[cfg(feature = "relay")]
            relay_peers: Vec::new(),
            #[cfg(feature = "relay")]
            remote_sessions: Vec::new(),
            show_skills: false,
            skills_tab: SkillsTab::Skills,
            skills_selected: 0,
            skills: Vec::new(),
            shared_skill_keys: std::collections::HashSet::new(),
            skills_status_msg: None,
            hive_listener_running: false,
            hive_identity: None,
            hive_known_peers: Vec::new(),
            hive_last_invite: None,
            hive_join_input_mode: false,
            hive_join_buffer: String::new(),
            show_brain: false,
            show_chat: false,
            chat_pid: None,
            chat_scroll: 0,
            chat_input: String::new(),
            input_supported: terminals::supports_input(),
            approval_pid: None,
            approval_preview: String::new(),
            staged_pane: None,
            brain_tab: BrainTab::Scorecard,
            brain_review_selected: 0,
            brain_queue: Vec::new(),
            brain_decisions_cache: Vec::new(),
            brain_status_msg: None,
            brain_note_input_mode: false,
            brain_note_buffer: String::new(),
        };
        #[cfg(feature = "coord")]
        app.coord_refresh();
        app.refresh();
        if app.roster_len() > 0 {
            app.table_state.select(Some(0));
        }
        app
    }

    pub fn refresh(&mut self) {
        let tick_start = std::time::Instant::now();

        if self.demo_mode {
            self.refresh_demo();
            if self.debug {
                let total_elapsed = tick_start.elapsed();
                self.debug_timings
                    .record(0.0, 0.0, 0.0, total_elapsed.as_secs_f64() * 1000.0);
            }
            return;
        }

        // Remember what's selected by stable identity so the cursor follows it
        // across this tick's re-sort (BACKLOG: selection follows the launched repo).
        let sel_key = self.selection_key();

        // Project-first roster: scan the parent dir for project directories.
        // Cheap local read; projects exist independent of agents (§2).
        self.projects = projects::scan(&self.parent_dir, self.include_non_git);
        self.poll_git_ops();
        self.refresh_git_cache();

        // Discover which PIDs have session files
        let scan_start = std::time::Instant::now();
        let discovered = discovery::scan_sessions();
        let scan_elapsed = scan_start.elapsed();

        // Build a map of existing sessions by PID for state preservation
        let mut existing: HashMap<u32, ClaudeSession> =
            self.sessions.drain(..).map(|s| (s.pid, s)).collect();

        // Merge: reuse existing session state (jsonl_offset, tokens, cost, cpu_history)
        // or create new from discovered
        let mut new_pids: Vec<u32> = Vec::new();
        let mut sessions: Vec<ClaudeSession> = discovered
            .into_iter()
            .map(|new| {
                if let Some(prev) = existing.remove(&new.pid) {
                    merge_discovered_session(prev, new)
                } else {
                    // Brand new session
                    new_pids.push(new.pid);
                    new
                }
            })
            .collect();

        // Enrich with ps data (CPU, MEM, TTY, command args) + filter dead PIDs
        let ps_start = std::time::Instant::now();
        process::fetch_and_enrich(&mut sessions);
        let ps_elapsed = ps_start.elapsed();

        // Resolve JSONL paths (only for sessions that don't have one yet)
        for session in &mut sessions {
            if session.jsonl_path.is_none() {
                discovery::resolve_jsonl_paths(std::slice::from_mut(session));
            }
        }

        // Scan for subagents
        discovery::scan_subagents(&mut sessions);

        // Resolve git worktree identity (for conflict detection, runs once per session)
        discovery::resolve_worktree_ids(&mut sessions);

        // Read JSONL incrementally (only new bytes since last offset). A session
        // whose offset advanced had real transcript activity this refresh — its
        // working tree likely changed, so re-fetch that project's git status
        // (event-driven; in-flight dedup keeps streaming from spamming `git`).
        let jsonl_start = std::time::Instant::now();
        let mut active_cwds: Vec<String> = Vec::new();
        for session in &mut sessions {
            let before = session.jsonl_offset;
            monitor::update_tokens(session);
            if session.jsonl_offset != before {
                active_cwds.push(session.cwd.clone());
            }
        }
        let jsonl_elapsed = jsonl_start.elapsed();
        self.enqueue_git_for_cwds(&active_cwds);

        // Burn rate = Δcost ÷ Δwall-clock, EMA-smoothed. The event-driven loop
        // (Phase 2.5) refreshes at irregular, sometimes sub-second intervals, so
        // the old `delta * 1800` (a hardcoded 2s tick) spiked $/hr into the
        // thousands on a JSONL burst and decayed it erratically on quiet ticks
        // (BACKLOG: "$/h is wild" / "$/hr too small"). Mirror the CPU derivation:
        // seed the baseline on first sight, skip windows too short to be
        // meaningful, divide by real elapsed time, and smooth the result.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        for session in &mut sessions {
            if session.prev_cost_sample_ms == 0 {
                // First sample for this session: record the baseline, no rate yet.
                session.prev_cost_usd = session.cost_usd;
                session.prev_cost_sample_ms = now_ms;
                continue;
            }
            let elapsed_ms = now_ms.saturating_sub(session.prev_cost_sample_ms);
            if elapsed_ms < MIN_BURN_SAMPLE_MS {
                // Window too short — let cost/clock accumulate for the next tick,
                // keeping the prior smoothed rate on display.
                continue;
            }
            let delta = (session.cost_usd - session.prev_cost_usd).max(0.0);
            let inst = delta / (elapsed_ms as f64 / 1000.0) * 3600.0;
            session.burn_rate_per_hr = smooth_burn(session.burn_rate_per_hr, inst);
            if session.burn_rate_per_hr < 0.01 {
                session.burn_rate_per_hr = 0.0;
            }
            session.prev_cost_usd = session.cost_usd;
            session.prev_cost_sample_ms = now_ms;
        }

        // Budget enforcement
        if let Some(budget) = self.budget_usd {
            for session in &sessions {
                let pct = session.cost_usd / budget * 100.0;

                // Warn at 80%
                if (80.0..100.0).contains(&pct) && !self.budget_warned.contains(&session.pid) {
                    self.budget_warned.insert(session.pid);
                    self.status_msg = format!(
                        "BUDGET WARNING: {} at {:.0}% (${:.2}/${:.2})",
                        session.display_name(),
                        pct,
                        session.cost_usd,
                        budget
                    );
                    fire_notification(&format!("{} budget {:.0}%", session.display_name(), pct));
                    self.hooks.fire(HookEvent::BudgetWarning, session);
                }

                // Kill at 100%
                if pct >= 100.0 && !self.budget_killed.contains(&session.pid) {
                    self.budget_killed.insert(session.pid);
                    if self.kill_on_budget {
                        let _ = self.runtime.actions.terminate_session(session.pid);
                        self.status_msg = format!(
                            "BUDGET EXCEEDED: Killed {} (${:.2}/${:.2})",
                            session.display_name(),
                            session.cost_usd,
                            budget
                        );
                    } else {
                        self.status_msg = format!(
                            "BUDGET EXCEEDED: {} at ${:.2}/{:.2} — use --kill-on-budget to auto-kill",
                            session.display_name(),
                            session.cost_usd,
                            budget
                        );
                    }
                    fire_notification(&format!("{} exceeded budget!", session.display_name()));
                    self.hooks.fire(HookEvent::BudgetExceeded, session);
                }
            }
        }

        // Context threshold warnings
        if self.context_warn_threshold > 0 {
            let threshold = self.context_warn_threshold as f64;
            for session in &sessions {
                let pct = session.context_percent();
                if pct >= threshold && !self.context_warned.contains(&session.pid) {
                    self.context_warned.insert(session.pid);
                    self.status_msg = format!(
                        "CONTEXT HIGH: {} at {:.0}% of context window",
                        session.display_name(),
                        pct
                    );
                    fire_notification(&format!(
                        "{} context at {:.0}%",
                        session.display_name(),
                        pct
                    ));
                    self.hooks.fire(HookEvent::ContextHigh, session);
                } else if pct < threshold && self.context_warned.contains(&session.pid) {
                    // Reset warning if context dropped (e.g., after /compact)
                    self.context_warned.remove(&session.pid);
                }
            }
        }

        // Record activity for sparkline and cache decay score
        for session in &mut sessions {
            session.record_activity();
            session.decay_score =
                claudectl_core::health::compute_decay_score(session, &self.health_thresholds);
        }

        // Track when sessions first appear as Finished, remove after 30s
        let now = std::time::Instant::now();
        for session in &sessions {
            if session.status == SessionStatus::Finished
                && !self.finished_at.contains_key(&session.pid)
            {
                self.finished_at.insert(session.pid, now);
                // Record to history on first Finished detection
                claudectl_core::history::record_session(session);
            }
        }
        sessions.retain(|s| {
            if s.status == SessionStatus::Finished {
                if let Some(&t) = self.finished_at.get(&s.pid) {
                    return now.duration_since(t).as_secs() < 30;
                }
            }
            true
        });
        // Clean up old finished_at entries + their session files
        let expired: Vec<u32> = self
            .finished_at
            .iter()
            .filter(|(_, t)| now.duration_since(**t).as_secs() >= 60)
            .map(|(pid, _)| *pid)
            .collect();
        for pid in &expired {
            let session_file = dirs_home()
                .join(".claude/sessions")
                .join(format!("{pid}.json"));
            let _ = std::fs::remove_file(session_file);
        }
        self.finished_at
            .retain(|_, t| now.duration_since(*t).as_secs() < 60);

        // Sort
        self.apply_sort(&mut sessions);

        // Notifications and webhooks: check for status transitions
        for session in &sessions {
            let prev = self.prev_statuses.get(&session.pid).copied();
            let changed = prev.is_some() && prev != Some(session.status);

            if !changed {
                continue;
            }

            claudectl_core::logger::log(
                "DEBUG",
                &format!(
                    "session {}: status {} -> {}",
                    session.display_name(),
                    prev.unwrap(),
                    session.status
                ),
            );

            // Desktop notification on NeedsInput
            if self.notify && session.status == SessionStatus::NeedsInput {
                fire_notification(&session.project_name);
            }

            // Webhook on status change
            if let Some(ref url) = self.webhook_url {
                let new_status = session.status.to_string();
                let should_fire = match &self.webhook_filter {
                    Some(filter) => filter.iter().any(|f| f.eq_ignore_ascii_case(&new_status)),
                    None => true,
                };
                if should_fire {
                    claudectl_core::logger::log(
                        "DEBUG",
                        &format!(
                            "webhook fired for {} -> {}",
                            session.display_name(),
                            new_status
                        ),
                    );
                    fire_webhook(
                        url,
                        session,
                        prev.map(|p| p.to_string()).unwrap_or_default(),
                    );
                }
            }

            // Event hooks
            self.hooks.fire_with_status(
                HookEvent::StatusChange,
                session,
                &prev.unwrap().to_string(),
                &session.status.to_string(),
            );

            match session.status {
                SessionStatus::NeedsInput => {
                    self.hooks.fire(HookEvent::NeedsInput, session);
                }
                SessionStatus::Finished => {
                    self.hooks.fire(HookEvent::Finished, session);
                }
                SessionStatus::Idle => {
                    self.hooks.fire(HookEvent::Idle, session);
                }
                _ => {}
            }
        }

        // Fire hooks for newly discovered sessions
        for session in sessions.iter().filter(|s| new_pids.contains(&s.pid)) {
            self.hooks.fire(HookEvent::SessionStart, session);
        }

        // Track NeedsInput wait times
        let now_instant = std::time::Instant::now();
        for session in &sessions {
            if session.status == SessionStatus::NeedsInput {
                // Record when it first entered NeedsInput
                self.needs_input_since
                    .entry(session.pid)
                    .or_insert(now_instant);
            } else {
                // Clear if no longer NeedsInput
                self.needs_input_since.remove(&session.pid);
            }
        }
        // Clean up entries for sessions that no longer exist
        let active_pids: HashSet<u32> = sessions.iter().map(|s| s.pid).collect();
        self.needs_input_since
            .retain(|pid, _| active_pids.contains(pid));

        // Conflict detection: find sessions sharing the same git worktree
        // Uses worktree_id (git show-toplevel) so different worktrees don't false-positive
        self.conflict_pids.clear();
        let mut wt_sessions: HashMap<&str, Vec<u32>> = HashMap::new();
        for session in &sessions {
            if session.status != SessionStatus::Finished {
                let key = session.worktree_id.as_deref().unwrap_or(&session.cwd);
                wt_sessions.entry(key).or_default().push(session.pid);
            }
        }
        for (wt, pids) in &wt_sessions {
            if pids.len() >= 2 {
                for &pid in pids {
                    self.conflict_pids.insert(pid);
                }
                // Fire hook once per worktree conflict (not on every tick)
                if !self.conflict_alerted.contains(*wt) {
                    self.conflict_alerted.insert(wt.to_string());
                    let project = sessions
                        .iter()
                        .find(|s| s.pid == pids[0])
                        .map(|s| s.display_name())
                        .unwrap_or("unknown");
                    self.status_msg =
                        format!("CONFLICT: {} sessions sharing {}", pids.len(), project);
                    fire_notification(&format!("{} sessions in {}", pids.len(), project));
                    if let Some(session) = sessions.iter().find(|s| s.pid == pids[0]) {
                        self.hooks.fire(HookEvent::ConflictDetected, session);
                    }
                }
            }
        }
        // Clear alerts for worktrees that no longer have conflicts
        self.conflict_alerted.retain(|wt| {
            wt_sessions
                .get(wt.as_str())
                .map(|pids| pids.len() >= 2)
                .unwrap_or(false)
        });

        // File-level conflict detection: find files edited by multiple sessions
        self.file_conflict_pids.clear();
        self.file_conflicts.clear();
        // Reset has_file_conflict on all sessions
        for session in &mut sessions {
            session.has_file_conflict = false;
        }

        if self.file_conflicts_enabled {
            // Build file → PIDs map from files_modified across active sessions
            let mut file_pids: HashMap<String, Vec<u32>> = HashMap::new();
            for session in &sessions {
                if session.status == SessionStatus::Finished {
                    continue;
                }
                for file in session.files_modified.keys() {
                    file_pids.entry(file.clone()).or_default().push(session.pid);
                }
                // Also consider pending file edits (predictive conflict)
                if let Some(ref pending) = session.pending_file_path {
                    file_pids
                        .entry(pending.clone())
                        .or_default()
                        .push(session.pid);
                }
            }

            // Deduplicate PIDs per file (a session may appear twice if it both modified and is pending)
            for pids in file_pids.values_mut() {
                pids.sort_unstable();
                pids.dedup();
            }

            // Record conflicts where 2+ sessions touch the same file
            for (file, pids) in &file_pids {
                if pids.len() >= 2 {
                    for &pid in pids {
                        self.file_conflict_pids.insert(pid);
                    }
                    self.file_conflicts.insert(file.clone(), pids.clone());

                    // Mark sessions with pending file conflicts
                    for session in &mut sessions {
                        if let Some(ref pending) = session.pending_file_path {
                            if pending == file && pids.contains(&session.pid) {
                                session.has_file_conflict = true;
                            }
                        }
                    }

                    // Fire alert once per conflicting file
                    if !self.file_conflict_alerted.contains(file) {
                        self.file_conflict_alerted.insert(file.clone());
                        let names: Vec<&str> = pids
                            .iter()
                            .filter_map(|pid| {
                                sessions
                                    .iter()
                                    .find(|s| s.pid == *pid)
                                    .map(|s| s.display_name())
                            })
                            .collect();
                        let short = file.rsplit('/').next().unwrap_or(file);
                        self.status_msg =
                            format!("FILE CONFLICT: {} edited by {}", short, names.join(", "));
                        fire_notification(&format!("File conflict: {short}"));
                        if let Some(session) = sessions.iter().find(|s| s.pid == pids[0]) {
                            self.hooks.fire(HookEvent::ConflictDetected, session);
                        }
                    }
                }
            }

            // Clear alerts for files no longer in conflict
            self.file_conflict_alerted
                .retain(|f| self.file_conflicts.contains_key(f));
        }

        // Update prev_statuses
        self.prev_statuses = sessions.iter().map(|s| (s.pid, s.status)).collect();

        // Drop SIGTERM-escalation tracking for PIDs that are gone (kill worked).
        self.sigterm_sent
            .retain(|pid| sessions.iter().any(|s| s.pid == *pid));

        self.sessions = sessions;

        // Append remote sessions from relay peers (if relay feature active)
        #[cfg(feature = "relay")]
        {
            for remote in &self.remote_sessions {
                self.sessions.push(remote.clone());
            }
        }

        self.normalize_selection();
        // Re-anchor the cursor onto the same project/agent after the re-sort.
        self.reselect_by_key(sel_key);

        // Record debug timings
        if self.debug {
            let total_elapsed = tick_start.elapsed();
            self.debug_timings.record(
                scan_elapsed.as_secs_f64() * 1000.0,
                ps_elapsed.as_secs_f64() * 1000.0,
                jsonl_elapsed.as_secs_f64() * 1000.0,
                total_elapsed.as_secs_f64() * 1000.0,
            );
        }
    }

    /// Drain the background git worker into the cache and fetch any **not-yet-
    /// fetched** project (CLAUDE.md §3/§4). This is the only place that runs every
    /// refresh, and it is **not** periodic polling: a project is enqueued here
    /// exactly once — when it first appears (startup or a newly cloned repo) and
    /// has no cache entry. Re-fetches are event-driven elsewhere: landing on a
    /// row (`next`/`previous`), an in-app push/pull, or manual `r`. O(channel ops);
    /// `git status` itself runs on the worker, never on the render thread.
    fn refresh_git_cache(&mut self) {
        let live: HashSet<PathBuf> = self
            .projects
            .iter()
            .filter(|p| p.has_git)
            .map(|p| p.path.clone())
            .collect();

        // Drain completed background results into the cache. A present key (even
        // `None`) marks the project fetched, so we don't re-request it below.
        while let Ok((path, status)) = self.git_svc.res_rx.try_recv() {
            self.git_inflight.remove(&path);
            self.git_cache.insert(path, status);
        }

        // Drop state for projects that vanished.
        self.git_cache.retain(|path, _| live.contains(path));
        self.git_inflight.retain(|path| live.contains(path));

        // One-time fetch for projects we've never fetched (initial populate +
        // newly discovered repos). Cached projects are never re-enqueued here —
        // that's what keeps idle repos from generating any periodic `git` churn.
        let unfetched: Vec<PathBuf> = live
            .into_iter()
            .filter(|p| !self.git_cache.contains_key(p) && !self.git_inflight.contains(p))
            .collect();
        for path in unfetched {
            self.enqueue_git(&path);
        }
    }

    /// Request a fresh `git status` for one project on the background worker.
    /// Bypasses the cache (used for on-demand re-fetch: selection, push/pull,
    /// manual refresh) but dedups against in-flight requests.
    fn enqueue_git(&mut self, path: &Path) {
        if self.git_inflight.contains(path) {
            return;
        }
        if self.git_svc.req_tx.send(path.to_path_buf()).is_ok() {
            self.git_inflight.insert(path.to_path_buf());
        }
    }

    /// Re-fetch git status for the project under the cursor (called on row
    /// navigation — "passing over a row polls it"). No-op when nothing's selected.
    fn enqueue_selected_git(&mut self) {
        if let Some(path) = self.selected_launch_cwd() {
            self.enqueue_git(&path);
        }
    }

    /// Re-fetch git status for each project that owns one of `cwds` (agent
    /// transcript activity → working tree likely changed). Event-driven; the
    /// `git_inflight` dedup means a streaming agent triggers at most one git
    /// status at a time for its project, never a pile-up.
    fn enqueue_git_for_cwds(&mut self, cwds: &[String]) {
        if cwds.is_empty() {
            return;
        }
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let mut targets: Vec<PathBuf> = Vec::new();
        for cwd in cwds {
            let cwd_c = canon(Path::new(cwd));
            for proj in &self.projects {
                if proj.has_git && projects::contains_cwd(&canon(&proj.path), &cwd_c) {
                    if !targets.contains(&proj.path) {
                        targets.push(proj.path.clone());
                    }
                    break; // first owning project wins
                }
            }
        }
        for path in targets {
            self.enqueue_git(&path);
        }
    }

    /// Re-fetch every git project (manual full refresh, `r`).
    fn enqueue_all_git(&mut self) {
        let paths: Vec<PathBuf> = self
            .projects
            .iter()
            .filter(|p| p.has_git)
            .map(|p| p.path.clone())
            .collect();
        for path in paths {
            self.enqueue_git(&path);
        }
    }

    /// Phase 3c (CLAUDE.md §4): fire-and-forget `git push`/`pull` into the
    /// selected project. Spawns with null stdio and returns immediately — the
    /// render loop never blocks on the network. The result lands on a later
    /// refresh via `poll_git_ops`. Re-pressing while one is in flight is a no-op.
    pub fn start_git_op(&mut self, kind: GitOpKind) {
        let Some(path) = self.selected_launch_cwd() else {
            self.status_msg = "No project selected".into();
            return;
        };
        let name = project_label(&path);

        if self.git_ops.contains_key(&path) {
            self.status_msg = format!("git {} already running in {name}", kind.verb());
            return;
        }

        match Command::new("git")
            .arg("-C")
            .arg(&path)
            .arg(kind.arg())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                self.status_msg = format!("git {} started in {name}…", kind.verb());
                self.git_ops.insert(
                    path,
                    GitOp {
                        kind,
                        child,
                        started: Instant::now(),
                    },
                );
            }
            Err(e) => self.status_msg = format!("git {} failed to start: {e}", kind.verb()),
        }
    }

    /// Reap finished push/pull children (non-blocking `try_wait`). On completion,
    /// drop the op and evict the project's cached git status so the next
    /// `refresh_git_cache` recomputes fresh ahead/behind. Errors count as done.
    fn poll_git_ops(&mut self) {
        if self.git_ops.is_empty() {
            return;
        }
        let mut done: Vec<(PathBuf, GitOpKind, bool)> = Vec::new();
        for (path, op) in self.git_ops.iter_mut() {
            match op.child.try_wait() {
                Ok(Some(status)) => done.push((path.clone(), op.kind, status.success())),
                Ok(None) => {} // still running
                Err(_) => done.push((path.clone(), op.kind, false)),
            }
        }
        for (path, kind, ok) in done {
            self.git_ops.remove(&path);
            self.enqueue_git(&path); // re-fetch status so ahead/behind updates
            let name = project_label(&path);
            self.status_msg = if ok {
                format!("git {} done: {name}", kind.verb())
            } else {
                format!("git {} failed: {name}", kind.verb())
            };
        }
    }

    /// Whether any push/pull is in flight (drives throbber repaints in main.rs).
    pub fn git_op_active(&self) -> bool {
        !self.git_ops.is_empty()
    }

    /// Throbber label for an in-flight op on `path` (e.g. "⠹ push"), or `None`.
    pub fn git_op_label(&self, path: &Path) -> Option<String> {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let op = self.git_ops.get(path)?;
        let frame = (op.started.elapsed().as_millis() / 100) as usize % FRAMES.len();
        Some(format!("{} {}", FRAMES[frame], op.kind.verb()))
    }

    fn apply_sort(&self, sessions: &mut [ClaudeSession]) {
        match self.sort_column {
            0 => sessions.sort_by(|a, b| {
                a.status.sort_key().cmp(&b.status.sort_key()).then_with(|| {
                    // Within NeedsInput, sort by longest waiting first
                    if a.status == SessionStatus::NeedsInput {
                        let a_wait = self.wait_duration(a.pid).unwrap_or_default();
                        let b_wait = self.wait_duration(b.pid).unwrap_or_default();
                        b_wait.cmp(&a_wait)
                    } else {
                        b.elapsed.cmp(&a.elapsed)
                    }
                })
            }),
            1 => sessions.sort_by(|a, b| {
                b.context_percent()
                    .partial_cmp(&a.context_percent())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            2 => sessions.sort_by(|a, b| {
                b.cost_usd
                    .partial_cmp(&a.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            3 => sessions.sort_by(|a, b| {
                b.burn_rate_per_hr
                    .partial_cmp(&a.burn_rate_per_hr)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            4 => sessions.sort_by_key(|s| std::cmp::Reverse(s.elapsed)),
            _ => {}
        }
    }

    pub fn cycle_sort(&mut self) {
        self.sort_column = (self.sort_column + 1) % SORT_COLUMNS.len();
        self.status_msg = format!("Sort: {}", SORT_COLUMNS[self.sort_column]);
        let mut sessions = std::mem::take(&mut self.sessions);
        self.apply_sort(&mut sessions);
        self.sessions = sessions;
    }

    fn refresh_demo(&mut self) {
        self.demo_tick += 1;
        let mut sessions = crate::demo::generate_sessions(self.demo_tick);

        // When the Skills & Hive view is open during a demo, scripted
        // navigation: cycle selection on the Skills tab, then flip to Hive
        // around tick 6, then back to Skills around tick 12.
        if self.show_skills {
            let phase = self.demo_tick % 14;
            match phase {
                1..=5 => {
                    self.skills_tab = SkillsTab::Skills;
                    if !self.skills.is_empty() {
                        self.skills_selected =
                            ((phase as usize - 1) % self.skills.len()).min(self.skills.len() - 1);
                    }
                }
                6 => {
                    self.skills_tab = SkillsTab::Hive;
                    self.skills_status_msg = Some("Hive: 2 peers connected".into());
                }
                7..=11 => {
                    self.skills_tab = SkillsTab::Hive;
                }
                _ => {
                    self.skills_tab = SkillsTab::Skills;
                    self.skills_status_msg = None;
                }
            }
        }

        // Track NeedsInput wait times (same as real mode)
        let now_instant = std::time::Instant::now();
        for session in &sessions {
            if session.status == SessionStatus::NeedsInput {
                self.needs_input_since
                    .entry(session.pid)
                    .or_insert(now_instant);
            } else {
                self.needs_input_since.remove(&session.pid);
            }
        }

        // Conflict detection using worktree_id
        self.conflict_pids.clear();
        let mut wt_sessions: HashMap<&str, Vec<u32>> = HashMap::new();
        for session in &sessions {
            if session.status != SessionStatus::Finished {
                let key = session.worktree_id.as_deref().unwrap_or(&session.cwd);
                wt_sessions.entry(key).or_default().push(session.pid);
            }
        }
        for pids in wt_sessions.values() {
            if pids.len() >= 2 {
                for &pid in pids {
                    self.conflict_pids.insert(pid);
                }
            }
        }

        // Scripted demo events: rules, brain, routing, health alerts
        if let Some(event) = crate::demo::demo_event(self.demo_tick) {
            self.status_msg = event.message.clone();
            match event.kind {
                crate::demo::EventKind::RuleAction => {
                    self.last_rule_action = Some(event.message);
                }
                crate::demo::EventKind::BrainSuggestion | crate::demo::EventKind::BrainOverride => {
                    // Show brain activity via status message
                }
                crate::demo::EventKind::Route | crate::demo::EventKind::HealthAlert => {}
                crate::demo::EventKind::HiveSync | crate::demo::EventKind::HiveInfluence => {}
            }
        }

        // Update demo peers panel and remote sessions
        #[cfg(feature = "relay")]
        {
            self.relay_peers = crate::demo::demo_peers(self.demo_tick);
            // Auto-show peers panel on first hive sync event
            if self.demo_tick % 32 == 14 && !self.show_peers_panel {
                self.show_peers_panel = true;
            }
            // Demo remote sessions from connected peers
            self.remote_sessions.clear();
            if self.demo_tick % 32 >= 14 {
                let remote_json = serde_json::json!({
                    "pid": 99001, "project": "backend",
                    "status": "Processing", "cost_usd": 1.4,
                    "elapsed_secs": 320, "context_pct": 42.0,
                });
                if let Some(s) = ClaudeSession::from_remote_json("ci-runner-9d1e", &remote_json) {
                    self.remote_sessions.push(s);
                }
            }
            if self.demo_tick % 32 >= 28 {
                let remote_json = serde_json::json!({
                    "pid": 99002, "project": "frontend",
                    "status": "Needs Input", "cost_usd": 0.32,
                    "elapsed_secs": 150,
                });
                if let Some(s) = ClaudeSession::from_remote_json("alice-mbp-f3a1", &remote_json) {
                    self.remote_sessions.push(s);
                }
            }
        }

        // Inject fake brain pending suggestions so the status bar shows brain activity.
        // Demo mode flows through the BrainDriver trait's set_pending escape hatch
        // rather than mutating an engine field directly — same path the real brain
        // would take.
        if let Some(ref mut driver) = self.brain_driver {
            driver.clear_pending();
            let phase = self.demo_tick % 32;
            if (9..=12).contains(&phase) {
                if let Some(s) = sessions
                    .iter()
                    .find(|s| s.status == SessionStatus::NeedsInput)
                {
                    driver.set_pending(claudectl_core::runtime::PendingSuggestion {
                        pid: s.pid,
                        action: "approve".into(),
                        message: s.pending_tool_input.clone(),
                        reasoning: "Safe build command, no side effects".into(),
                        confidence: 0.92,
                        suggested_at: 0,
                    });
                }
            }
            if (14..=16).contains(&phase) {
                if let Some(s) = sessions
                    .iter()
                    .find(|s| s.status == SessionStatus::NeedsInput)
                {
                    driver.set_pending(claudectl_core::runtime::PendingSuggestion {
                        pid: s.pid,
                        action: "deny".into(),
                        message: s.pending_tool_input.clone(),
                        reasoning: "Destructive operation, needs manual review".into(),
                        confidence: 0.87,
                        suggested_at: 0,
                    });
                }
            }
        }

        // ── Demo highlight reel support ────────────────────────────────
        // Ensure demo sessions have JSONL paths so the session recorder can attach.
        // Drip-feed scripted events for sessions that are actively being recorded.
        let highlight = self
            .demo_highlight
            .get_or_insert_with(crate::demo::DemoHighlightState::new);

        for session in &mut sessions {
            let path = highlight.ensure_jsonl(session.pid).clone();
            session.jsonl_path = Some(path);
        }

        // Feed new JSONL events only into sessions being recorded.
        // When the script is exhausted, mark the PID for auto-stop.
        let recording_pids: Vec<u32> = self.session_recordings.keys().copied().collect();
        let mut finished_pids: Vec<u32> = Vec::new();
        for pid in recording_pids {
            if !highlight.drip_feed(pid) {
                finished_pids.push(pid);
            }
        }

        // Auto-stop recordings whose scripts are done
        for pid in finished_pids {
            if let Some(path) = self.session_recordings.remove(&pid) {
                self.status_msg = format!("Recording complete → {path}");
            }
        }

        // Compute decay scores for demo sessions (same as real refresh path)
        for session in &mut sessions {
            session.decay_score =
                claudectl_core::health::compute_decay_score(session, &self.health_thresholds);
        }

        self.sessions = sessions;
        self.normalize_selection();
    }

    pub fn tick(&mut self) {
        self.status_msg.clear();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        for session in &mut self.sessions {
            let elapsed_ms = now_ms.saturating_sub(session.started_at);
            session.elapsed = std::time::Duration::from_millis(elapsed_ms);
        }

        self.refresh();

        // Phase 5: sample total fleet burn for the trend sparkline. Gated so a
        // burst of event-driven ticks doesn't compress the trend's time axis;
        // burn_rate_per_hr was just recomputed by refresh().
        if now_ms.saturating_sub(self.last_fleet_sample_ms) >= FLEET_SAMPLE_MIN_MS {
            let total: f64 = self.sessions.iter().map(|s| s.burn_rate_per_hr).sum();
            self.fleet_burn_history.push(total);
            if self.fleet_burn_history.len() > FLEET_HISTORY_CAP {
                self.fleet_burn_history.remove(0);
            }
            self.last_fleet_sample_ms = now_ms;
        }

        self.run_auto_actions();

        // Check idle mode transition
        self.check_idle_mode();

        // Refresh weekly summary every ~30s (15 ticks at 2s interval)
        self.weekly_summary_tick += 1;
        if self.weekly_summary_tick >= 15 {
            self.weekly_summary_tick = 0;
            self.weekly_summary = claudectl_core::history::weekly_summary();
            self.all_time_summary = claudectl_core::history::all_time_summary();
            self.check_aggregate_budgets();
        }

        // Refresh coordination state every ~6s (3 ticks at 2s interval)
        #[cfg(feature = "coord")]
        {
            self.coord_tick += 1;
            if self.coord_tick >= 3 {
                self.coord_tick = 0;
                self.coord_refresh();
            }
        }
    }

    /// Get how long a session has been waiting for input, if applicable.
    pub fn wait_duration(&self, pid: u32) -> Option<std::time::Duration> {
        self.needs_input_since
            .get(&pid)
            .map(|since| since.elapsed())
    }

    /// Format wait duration as a compact string (e.g., "2m 34s").
    pub fn format_wait_time(&self, pid: u32) -> Option<String> {
        let dur = self.wait_duration(pid)?;
        let secs = dur.as_secs();
        if secs < 60 {
            Some(format!("{secs}s"))
        } else {
            Some(format!("{}m {}s", secs / 60, secs % 60))
        }
    }

    /// Refresh cached coordination state from the runtime.
    ///
    /// The `expire_stale_*` calls remain direct because they're side-effects
    /// on the SQLite store (bookkeeping), not part of the read-only
    /// `CoordView` surface. The actual list queries go through the runtime
    /// trait so the binary-coord coupling stays one layer thick.
    #[cfg(feature = "coord")]
    pub fn coord_refresh(&mut self) {
        // Bookkeeping (expire stale leases + interrupts). Best-effort; the
        // orchestrator logs failures internally and never propagates them.
        self.runtime.orchestrator.expire_stale();

        self.coord_leases = self.runtime.coord.active_leases();
        self.coord_handoffs = self.runtime.coord.pending_handoffs();
        self.coord_pending_interrupts = self.runtime.coord.pending_interrupts();

        self.coord_lease_sessions = self
            .coord_leases
            .iter()
            .map(|l| l.owner_session_id.clone())
            .collect();
        self.coord_handoff_sessions = self
            .coord_handoffs
            .iter()
            .flat_map(|h| {
                let mut ids = vec![h.from_session_id.clone()];
                if let Some(ref to) = h.to_session_id {
                    ids.push(to.clone());
                }
                ids
            })
            .collect();
        self.coord_interrupt_targets = self
            .coord_pending_interrupts
            .iter()
            .map(|i| i.target_session_id.clone())
            .collect();
    }

    #[cfg(feature = "coord")]
    pub fn session_has_lease(&self, session_id: &str) -> bool {
        self.coord_lease_sessions.contains(session_id)
    }

    #[cfg(feature = "coord")]
    pub fn session_has_handoff(&self, session_id: &str) -> bool {
        self.coord_handoff_sessions.contains(session_id)
    }

    #[cfg(feature = "coord")]
    pub fn session_has_interrupt(&self, session_id: &str) -> bool {
        self.coord_interrupt_targets.contains(session_id)
    }

    /// Compute budget exhaustion ETA based on current burn rate.
    /// Returns (spent, limit, eta_string, urgency) where urgency is 0=safe, 1=warn, 2=critical.
    pub fn budget_eta(&self) -> Option<(f64, f64, String, u8)> {
        let live_cost: f64 = self.sessions.iter().map(|s| s.cost_usd).sum();
        let total_burn: f64 = self.sessions.iter().map(|s| s.burn_rate_per_hr).sum();

        // Prefer daily limit, fall back to per-session budget
        let (spent, limit) = if let Some(daily) = self.daily_limit {
            (self.weekly_summary.today_cost_usd + live_cost, daily)
        } else if let Some(budget) = self.budget_usd {
            // For per-session budget, show the session closest to limit
            if let Some(session) = self.sessions.iter().max_by(|a, b| {
                (a.cost_usd / budget)
                    .partial_cmp(&(b.cost_usd / budget))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                (session.cost_usd, budget)
            } else {
                return None;
            }
        } else {
            return None;
        };

        let remaining = limit - spent;
        if remaining <= 0.0 {
            return Some((spent, limit, "exceeded".into(), 2));
        }
        if total_burn < 0.01 {
            return Some((spent, limit, "safe".into(), 0));
        }

        let hours_left = remaining / total_burn;
        let mins_left = (hours_left * 60.0) as u64;
        let eta_str = if mins_left >= 120 {
            format!("{}h {}m", mins_left / 60, mins_left % 60)
        } else {
            format!("{}m", mins_left)
        };

        let urgency = if mins_left <= 30 {
            2
        } else if mins_left <= 120 {
            1
        } else {
            0
        };
        Some((spent, limit, eta_str, urgency))
    }

    fn check_aggregate_budgets(&mut self) {
        let ws = &self.weekly_summary;

        // Also include cost from currently live sessions (not yet in history)
        let live_cost: f64 = self.sessions.iter().map(|s| s.cost_usd).sum();

        // Daily limit check
        if let Some(daily_limit) = self.daily_limit {
            let today_total = ws.today_cost_usd + live_cost;
            let pct = today_total / daily_limit * 100.0;

            if pct >= 80.0 && !self.daily_alert_fired {
                self.daily_alert_fired = true;
                self.status_msg = format!(
                    "DAILY BUDGET: ${:.2}/${:.2} ({:.0}%)",
                    today_total, daily_limit, pct
                );
                fire_notification(&format!("Daily budget at {:.0}%", pct));

                // Fire hooks with a synthetic session containing aggregate data
                let mut dummy = create_aggregate_session(today_total, daily_limit, "daily");
                self.hooks.fire(HookEvent::BudgetWarning, &dummy);

                if pct >= 100.0 {
                    dummy.cost_usd = today_total;
                    self.hooks.fire(HookEvent::BudgetExceeded, &dummy);
                }
            }
        }

        // Weekly limit check
        if let Some(weekly_limit) = self.weekly_limit {
            let week_total = ws.cost_usd + live_cost;
            let pct = week_total / weekly_limit * 100.0;

            if pct >= 80.0 && !self.weekly_alert_fired {
                self.weekly_alert_fired = true;
                self.status_msg = format!(
                    "WEEKLY BUDGET: ${:.2}/${:.2} ({:.0}%)",
                    week_total, weekly_limit, pct
                );
                fire_notification(&format!("Weekly budget at {:.0}%", pct));

                let mut dummy = create_aggregate_session(week_total, weekly_limit, "weekly");
                self.hooks.fire(HookEvent::BudgetWarning, &dummy);

                if pct >= 100.0 {
                    dummy.cost_usd = week_total;
                    self.hooks.fire(HookEvent::BudgetExceeded, &dummy);
                }
            }
        }
    }

    fn check_idle_mode(&mut self) {
        if !self.idle_config.enabled {
            return;
        }
        let idle_threshold = std::time::Duration::from_secs(self.idle_config.after_idle_mins * 60);
        let was_idle = self.idle_mode_active;
        self.idle_mode_active = self.last_user_interaction.elapsed() > idle_threshold;

        if self.idle_mode_active && !was_idle {
            claudectl_core::logger::log("IDLE", "Entering idle mode");
        }
    }

    /// Check if currently in idle mode (used by other systems like lifecycle restart).
    #[allow(dead_code)]
    pub fn is_idle(&self) -> bool {
        self.idle_mode_active
    }

    fn run_auto_actions(&mut self) {
        // In demo mode, events are scripted in refresh_demo() — skip real execution
        if self.demo_mode {
            return;
        }

        // Legacy per-PID auto-approve (toggled with 'a' key)
        let legacy_pids: Vec<u32> = self
            .sessions
            .iter()
            .filter(|s| s.status == SessionStatus::NeedsInput && self.auto_approve.contains(&s.pid))
            .map(|s| s.pid)
            .collect();

        for pid in legacy_pids {
            if let Some(session) = self.sessions.iter().find(|s| s.pid == pid) {
                let _ = self
                    .runtime
                    .actions
                    .log_observation(observation_from(session, "user_approve"));
                match terminals::approve_session(session) {
                    Ok(()) => self.status_msg = format!("Auto-approved {}", session.display_name()),
                    Err(e) => self.status_msg = format!("Auto-approve error: {e}"),
                }
            }
        }

        // Built-in file conflict auto-deny: deny writes to files being edited by another session
        if self.auto_deny_file_conflicts {
            let conflict_candidates: Vec<(u32, String, String)> = self
                .sessions
                .iter()
                .filter(|s| {
                    s.status == SessionStatus::NeedsInput
                        && s.has_file_conflict
                        && s.pending_file_path.is_some()
                })
                .filter_map(|s| {
                    let file = s.pending_file_path.as_ref()?;
                    let other_pids = self.file_conflicts.get(file)?;
                    let other_name = other_pids
                        .iter()
                        .filter(|&&p| p != s.pid)
                        .find_map(|pid| {
                            self.sessions
                                .iter()
                                .find(|o| o.pid == *pid)
                                .map(|o| format!("{} (PID {})", o.display_name(), o.pid))
                        })
                        .unwrap_or_else(|| "another session".into());
                    Some((s.pid, file.clone(), other_name))
                })
                .collect();

            for (pid, file, other) in conflict_candidates {
                // Debounce
                if let Some(last) = self.auto_actions_fired.get(&pid) {
                    if last.elapsed().as_secs() < 5 {
                        continue;
                    }
                }
                if let Some(session) = self.sessions.iter().find(|s| s.pid == pid) {
                    // Log passive observation: conflict auto-deny
                    let _ = self
                        .runtime
                        .actions
                        .log_observation(observation_from(session, "conflict_deny"));
                    let short = file.rsplit('/').next().unwrap_or(&file);
                    let msg = format!("File {short} is being edited by {other}");
                    match self.runtime.actions.inject_text(&session.session_id, &msg) {
                        Ok(()) => {
                            let status = format!(
                                "File conflict: denied {} edit to {short}",
                                session.display_name()
                            );
                            claudectl_core::logger::log("CONFLICT", &status);
                            self.status_msg = status;
                        }
                        Err(e) => {
                            self.status_msg = format!("File conflict deny error: {e}");
                        }
                    }
                    self.auto_actions_fired
                        .insert(pid, std::time::Instant::now());
                }
            }
        }

        // Rule-based auto-actions
        if !self.rules.is_empty() {
            let candidates: Vec<u32> = self
                .sessions
                .iter()
                .filter(|s| {
                    matches!(
                        s.status,
                        SessionStatus::NeedsInput | SessionStatus::WaitingInput
                    )
                })
                .filter(|s| !self.auto_approve.contains(&s.pid)) // Legacy takes priority
                .map(|s| s.pid)
                .collect();

            for pid in candidates {
                // Debounce: don't re-fire within 3 seconds for same PID
                if let Some(last) = self.auto_actions_fired.get(&pid) {
                    if last.elapsed().as_secs() < 3 {
                        continue;
                    }
                }

                let session = match self.sessions.iter().find(|s| s.pid == pid) {
                    Some(s) => s,
                    None => continue,
                };

                let result = claudectl_core::rules::evaluate(&self.rules, session);
                let Some(rule_match) = result else {
                    continue;
                };

                // Log passive observation: static rule fired
                let obs_action = format!("rule_{}", rule_match.action.label());
                let _ = self
                    .runtime
                    .actions
                    .log_observation(observation_from(session, &obs_action));

                let msg = claudectl_core::rules::execute(&rule_match, session);
                match msg {
                    Ok(status) => {
                        claudectl_core::logger::log("AUTO", &status);
                        self.last_rule_action = Some(status.clone());
                        self.status_msg = status;
                    }
                    Err(e) => {
                        self.status_msg = format!("Rule error: {e}");
                    }
                }

                self.auto_actions_fired
                    .insert(pid, std::time::Instant::now());
            }
        } // end if !self.rules.is_empty()

        // Brain inference (opt-in, runs after rules)
        if let Some(ref mut driver) = self.brain_driver {
            // Collect deny-only rules for override checking
            let deny_rules: Vec<_> = self
                .rules
                .iter()
                .filter(|r| r.action == claudectl_core::rules::RuleAction::Deny)
                .cloned()
                .collect();

            let snapshots: Vec<_> = self.sessions.iter().map(snapshot_from).collect();
            let actions = driver.tick(&snapshots, &deny_rules);
            for (_pid, msg) in actions {
                claudectl_core::logger::log("BRAIN", &msg);
                self.status_msg = msg;
            }

            driver.cleanup(&snapshots);

            // Deliver pending mailbox messages to sessions waiting for input.
            // The orchestrator resolves SessionSnapshot back to live sessions
            // internally; we project once here.
            let snapshots: Vec<_> = self.sessions.iter().map(snapshot_from).collect();
            let deliveries = self.runtime.orchestrator.deliver_mailbox(&snapshots);
            for (_pid, msg) in deliveries {
                claudectl_core::logger::log("MAILBOX", &msg);
                self.status_msg = msg;
            }
        }

        // Deliver pending typed interrupts from the coordination bus. The
        // orchestrator handles the SQLite connection internally.
        #[cfg(feature = "coord")]
        {
            let snapshots: Vec<_> = self.sessions.iter().map(snapshot_from).collect();
            let deliveries = self.runtime.orchestrator.deliver_interrupts(&snapshots);
            for (_intr_id, msg) in deliveries {
                claudectl_core::logger::log("INTERRUPT", &msg);
                self.status_msg = msg;
            }
        }
    }

    pub fn handle_auto_approve(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if session.is_remote() {
            self.status_msg = "Remote session \u{2014} action not available".into();
            return;
        }
        let pid = session.pid;
        let name = session.display_name().to_string();

        if self.pending_auto_approve == Some(pid) {
            if self.auto_approve.contains(&pid) {
                self.auto_approve.remove(&pid);
                self.status_msg = format!("Auto-approve OFF for {name}");
            } else {
                self.auto_approve.insert(pid);
                self.status_msg = format!("Auto-approve ON for {name}");
            }
            self.pending_auto_approve = None;
        } else {
            self.pending_auto_approve = Some(pid);
            let action = if self.auto_approve.contains(&pid) {
                "disable"
            } else {
                "enable"
            };
            self.status_msg = format!("Press a again to {action} auto-approve for {name}");
        }
    }

    pub fn cancel_pending_auto_approve(&mut self) {
        self.pending_auto_approve = None;
    }

    pub fn next(&mut self) {
        // Navigate over roster rows (headers + agents), not just agents, so idle
        // projects are reachable (CLAUDE.md §2).
        let len = self.roster_len();
        if len == 0 {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) if i >= len - 1 => 0,
            Some(i) => i + 1,
            None => 0,
        };
        self.table_state.select(Some(i));
        self.enqueue_selected_git(); // landing on a row re-fetches its git status
    }

    pub fn previous(&mut self) {
        let len = self.roster_len();
        if len == 0 {
            return;
        }
        let i = match self.table_state.selected() {
            Some(0) => len - 1,
            Some(i) => i - 1,
            None => 0,
        };
        self.table_state.select(Some(i));
        self.enqueue_selected_git(); // landing on a row re-fetches its git status
    }

    pub fn selected_session(&self) -> Option<&ClaudeSession> {
        let selected = self.table_state.selected()?;
        let (_, rows) = self.roster_layout();
        match rows.get(selected)? {
            RosterRow::Agent(si) => self.sessions.get(*si),
            // A project header is selected — no agent. Callers (kill, switch,
            // input, detail) degrade gracefully on None (CLAUDE.md §2).
            RosterRow::Header(_) => None,
        }
    }

    pub fn handle_kill(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if session.is_remote() {
            self.status_msg = "Remote session \u{2014} action not available".into();
            return;
        }
        let pid = session.pid;
        let name = session.display_name().to_string();

        // Already SIGTERM'd and still here → escalate to SIGKILL. Real signals;
        // the runtime's terminate_session is the inert mock.
        let escalate = self.sigterm_sent.contains(&pid);

        if self.pending_kill == Some(pid) {
            let result = if escalate {
                process::force_kill(pid)
            } else {
                process::terminate(pid)
            };
            match result {
                Ok(()) => {
                    if escalate {
                        self.status_msg = format!("Force-killed {name} (PID {pid}) [SIGKILL]");
                        self.sigterm_sent.remove(&pid);
                    } else {
                        self.status_msg = format!("Sent SIGTERM to {name} (PID {pid})");
                        self.sigterm_sent.insert(pid);
                    }
                    self.auto_approve.remove(&pid);
                    // Don't delete session file yet — let the Finished tombstone show for 30s.
                    // The file will be cleaned up when the tombstone expires.
                    self.refresh();
                }
                Err(e) => self.status_msg = format!("Kill failed: {e}"),
            }
            self.pending_kill = None;
        } else {
            self.pending_kill = Some(pid);
            self.status_msg = if escalate {
                format!("{name} (PID {pid}) ignored SIGTERM — press d again to SIGKILL")
            } else {
                format!("Kill {name} (PID {pid})? Press d again to confirm")
            };
        }
    }

    pub fn cancel_pending_kill(&mut self) {
        if self.pending_kill.is_some() {
            self.pending_kill = None;
            self.status_msg = "Kill cancelled".into();
        }
    }

    /// Handle a key event. Returns false if the application should quit.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.last_user_interaction = std::time::Instant::now();

        // Transition out of idle mode on any key press
        if self.idle_mode_active {
            self.idle_mode_active = false;
            if !self.idle_report.is_empty() {
                let report = self.idle_report.join("; ");
                self.status_msg = format!("Idle report: {report}");
                self.idle_report.clear();
            }
            self.idle_tasks_launched.clear();
        }

        // Help overlay: any key dismisses
        if self.show_help {
            self.show_help = false;
            return true;
        }

        // Launch mode: capture directory for new session
        if self.launch_mode {
            self.handle_launch_key(key);
            return true;
        }

        if self.search_mode {
            self.handle_search_key(key);
            return true;
        }

        // Input mode: capture text for sending to a session
        if self.input_mode {
            self.handle_input_key(key);
            return true;
        }

        // Role-bind mode: capture role name for the selected session (#307)
        if self.role_bind_mode {
            self.handle_role_bind_key(key);
            return true;
        }

        // Skills overlay: dedicated keymap (j/k navigate, s share, h serve, r rescan, Esc/K close)
        if self.show_skills {
            self.handle_skills_key(key);
            return true;
        }

        // Brain overlay: dedicated keymap (j/k navigate, Tab switch, m mark, n note, r refresh, Esc/B close)
        if self.show_brain {
            self.handle_brain_key(key);
            return true;
        }

        // Chat overlay (Phase 4): j/k scroll, g/G ends, Esc/C close.
        if self.show_chat {
            self.handle_chat_key(key);
            return true;
        }

        // Approval inspector modal (Phase 4c): y approve / n deny / i interrupt /
        // r re-capture / Esc cancel. Captured keys never reach normal mode.
        if self.approval_pid.is_some() {
            self.handle_approval_key(key);
            return true;
        }

        // Override reason prompt: waiting for 1/2/3/Esc
        if self.pending_override_reason.is_some() {
            match key.code {
                KeyCode::Char('1') => {
                    self.handle_brain_accept_with_reason(Some("always_safe"));
                }
                KeyCode::Char('2') => {
                    self.handle_brain_accept_with_reason(Some("one_time_exception"));
                }
                KeyCode::Char('3') => {
                    self.handle_brain_accept_with_reason(Some("brain_is_wrong"));
                }
                KeyCode::Esc => {
                    self.pending_override_reason = None;
                    self.status_msg = "Override cancelled".into();
                }
                _ => {}
            }
            return true;
        }

        // Normal mode
        self.handle_normal_key(key);
        !self.should_quit
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                if let Some(pid) = self.input_target_pid {
                    if let Some(session) = self.sessions.iter().find(|s| s.pid == pid) {
                        // Log passive observation: user sent manual input
                        let _ = self
                            .runtime
                            .actions
                            .log_observation(observation_from(session, "user_input"));
                        let text = format!("{}\n", self.input_buffer);
                        match self.runtime.actions.inject_text(&session.session_id, &text) {
                            Ok(()) => {
                                self.status_msg = format!("Sent to {}", session.display_name())
                            }
                            Err(e) => self.status_msg = format!("Error: {e}"),
                        }
                    }
                }
                self.input_mode = false;
                self.input_buffer.clear();
                self.input_target_pid = None;
            }
            KeyCode::Esc => {
                self.input_mode = false;
                self.input_buffer.clear();
                self.input_target_pid = None;
                self.status_msg = "Input cancelled".into();
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                self.search_query = self.search_buffer.trim().to_string();
                self.search_mode = false;
                self.normalize_selection();
                if self.search_query.is_empty() {
                    self.status_msg = "Search cleared".into();
                } else {
                    self.status_msg = format!("Search: {}", self.search_query);
                }
            }
            KeyCode::Esc => {
                self.search_mode = false;
                self.search_buffer.clear();
                self.status_msg = "Search cancelled".into();
            }
            KeyCode::Backspace => {
                self.search_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.search_buffer.push(c);
            }
            _ => {}
        }
    }

    pub fn open_skills_overlay(&mut self) {
        self.refresh_skills();
        self.refresh_hive_view();
        self.skills_selected = 0;
        self.skills_status_msg = None;
        self.hive_join_input_mode = false;
        self.hive_join_buffer.clear();
        self.show_skills = true;
    }

    pub fn refresh_skills(&mut self) {
        let cwd = std::env::current_dir().ok();
        self.skills = claudectl_core::skills::discover(cwd.as_deref());
        self.shared_skill_keys = self.runtime.hive.shared_skill_keys();
        if self.skills_selected >= self.skills.len() {
            self.skills_selected = self.skills.len().saturating_sub(1);
        }
    }

    pub fn refresh_hive_view(&mut self) {
        let snapshot = self.runtime.hive.hive_view_snapshot();
        self.hive_identity = snapshot.identity;
        self.hive_known_peers = snapshot.peers;
    }

    // ── Brain review overlay ──────────────────────────────────────────────

    pub fn open_brain_overlay(&mut self) {
        self.refresh_brain();
        self.brain_review_selected = 0;
        self.brain_status_msg = None;
        self.brain_note_input_mode = false;
        self.brain_note_buffer.clear();
        self.brain_tab = BrainTab::Scorecard;
        self.show_brain = true;
    }

    /// Open the Phase 4 chat overlay for the selected agent, pinned to its PID.
    /// No-op (with a hint) when a project header — not an agent — is selected.
    pub fn open_chat(&mut self) {
        match self.selected_session() {
            Some(s) => {
                self.chat_pid = Some(s.pid);
                self.chat_scroll = 0; // start pinned to the newest message
                self.chat_input.clear();
                self.show_chat = true;
            }
            None => {
                self.status_msg = "Select an agent to open its chat".into();
            }
        }
    }

    /// Chat overlay keymap. The reply box is always focused so you can type the
    /// moment the chat opens: printable keys compose a prompt, Enter sends it into
    /// the agent's pane, Backspace edits. Scrolling uses the arrow/page keys (not
    /// j/k, which type), and Esc closes. Higher `chat_scroll` = further back.
    fn handle_chat_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.show_chat = false;
                self.chat_pid = None;
                self.chat_input.clear();
            }
            KeyCode::Enter => self.send_chat_input(),
            KeyCode::Backspace => {
                self.chat_input.pop();
            }
            KeyCode::Up => self.chat_scroll = self.chat_scroll.saturating_add(1),
            KeyCode::Down => self.chat_scroll = self.chat_scroll.saturating_sub(1),
            KeyCode::PageUp => self.chat_scroll = self.chat_scroll.saturating_add(10),
            KeyCode::PageDown => self.chat_scroll = self.chat_scroll.saturating_sub(10),
            KeyCode::Home => self.chat_scroll = u16::MAX, // oldest (render clamps)
            KeyCode::End => self.chat_scroll = 0,         // newest
            KeyCode::Char(c) => self.chat_input.push(c),
            _ => {}
        }
    }

    /// Phase 4b (CLAUDE.md §0, PHASE4_PLAN.md Part B): send the composed reply
    /// into the agent's existing tmux pane via the inherited keystroke backend —
    /// type the text, then `\r` to submit (mirrors `approve_session`). tmux still
    /// owns the process; we are a remote control. The agent's reply streams back
    /// into the conversation on the next transcript refresh.
    fn send_chat_input(&mut self) {
        let text = self.chat_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if !self.input_supported {
            self.status_msg =
                "Input needs tmux — run agents and herdr inside tmux (see ? help)".into();
            return;
        }
        let Some(pid) = self.chat_pid else {
            return;
        };

        // Compute the result while borrowing `sessions`, so the borrow is dropped
        // before we mutate `self` (status/input) below.
        let outcome = match self.sessions.iter().find(|s| s.pid == pid) {
            None => Err("Agent is no longer running".to_string()),
            Some(s) if s.worker_origin.is_some() => {
                Err("Remote session — input not available".to_string())
            }
            Some(s) => {
                let name = s.display_name().to_string();
                terminals::send_input(s, &text)
                    .and_then(|()| terminals::send_input(s, "\r"))
                    .map(|()| name)
                    .map_err(|e| format!("Send failed: {e}"))
            }
        };

        match outcome {
            Ok(name) => {
                self.status_msg = format!("Sent to {name}");
                self.chat_input.clear();
                self.chat_scroll = 0; // pin to bottom to watch the reply arrive
            }
            Err(msg) => self.status_msg = msg,
        }
    }

    pub fn refresh_brain(&mut self) {
        self.brain_decisions_cache = self.runtime.review.all_decisions();
        self.brain_queue = self.runtime.review.review_queue();
        if self.brain_review_selected >= self.brain_queue.len() {
            self.brain_review_selected = self.brain_queue.len().saturating_sub(1);
        }
    }

    fn handle_brain_key(&mut self, key: KeyEvent) {
        if self.brain_note_input_mode {
            self.handle_brain_note_input(key);
            return;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('M'), _) | (KeyCode::Char('q'), _) => {
                self.show_brain = false;
                self.brain_status_msg = None;
                return;
            }
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                self.brain_tab = self.brain_tab.toggle();
                self.brain_status_msg = None;
                return;
            }
            (KeyCode::Char('r'), _) => {
                self.refresh_brain();
                self.brain_status_msg = Some("Refreshed.".into());
                return;
            }
            _ => {}
        }

        if matches!(self.brain_tab, BrainTab::Review) {
            self.handle_brain_review_tab_key(key);
        }
    }

    fn handle_brain_review_tab_key(&mut self, key: KeyEvent) {
        if self.brain_queue.is_empty() {
            return;
        }
        let last = self.brain_queue.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.brain_review_selected < last {
                    self.brain_review_selected += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.brain_review_selected > 0 {
                    self.brain_review_selected -= 1;
                }
            }
            KeyCode::Char('g') | KeyCode::Home => self.brain_review_selected = 0,
            KeyCode::Char('G') | KeyCode::End => self.brain_review_selected = last,
            KeyCode::Char('m') => self.mark_selected_canonical(None),
            KeyCode::Char('n') => {
                self.brain_note_input_mode = true;
                self.brain_note_buffer.clear();
                self.brain_status_msg = Some("Type a note, Enter to save, Esc to cancel.".into());
            }
            KeyCode::Char('s') | KeyCode::Right | KeyCode::Char('l')
                if self.brain_review_selected < last =>
            {
                self.brain_review_selected += 1;
                self.brain_status_msg = Some("Skipped.".into());
            }
            _ => {}
        }
    }

    fn mark_selected_canonical(&mut self, note: Option<&str>) {
        let Some(item) = self.brain_queue.get(self.brain_review_selected) else {
            return;
        };
        // DecisionSummary stores the id as a String; empty == "no decision_id".
        let id = item.decision.id.clone();
        if id.is_empty() {
            self.brain_status_msg = Some("No decision_id — older record, can't mark.".into());
            return;
        }
        match self
            .runtime
            .actions
            .mark_canonical(&id, note.map(String::from))
        {
            Ok(()) => {
                self.brain_status_msg = Some(if note.is_some() {
                    format!("Marked canonical with note: {id}")
                } else {
                    format!("Marked canonical: {id}")
                });
                // Drop the marked item and advance selection naturally.
                self.brain_queue.remove(self.brain_review_selected);
                if self.brain_review_selected >= self.brain_queue.len() {
                    self.brain_review_selected = self.brain_queue.len().saturating_sub(1);
                }
            }
            Err(e) => {
                self.brain_status_msg = Some(format!("Could not mark canonical: {e}"));
            }
        }
    }

    fn handle_brain_note_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let note = self.brain_note_buffer.trim().to_string();
                self.brain_note_input_mode = false;
                self.brain_note_buffer.clear();
                if note.is_empty() {
                    self.brain_status_msg = Some("Empty note — not saved.".into());
                } else {
                    self.mark_selected_canonical(Some(&note));
                }
            }
            KeyCode::Esc => {
                self.brain_note_input_mode = false;
                self.brain_note_buffer.clear();
                self.brain_status_msg = Some("Note cancelled.".into());
            }
            KeyCode::Backspace => {
                self.brain_note_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.brain_note_buffer.push(c);
            }
            _ => {}
        }
    }

    fn handle_skills_key(&mut self, key: KeyEvent) {
        if self.hive_join_input_mode {
            self.handle_hive_join_input(key);
            return;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('K'), _) | (KeyCode::Char('q'), _) => {
                self.show_skills = false;
                self.skills_status_msg = None;
                return;
            }
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                self.skills_tab = self.skills_tab.toggle();
                self.skills_status_msg = None;
                return;
            }
            _ => {}
        }

        match self.skills_tab {
            SkillsTab::Skills => self.handle_skills_tab_key(key),
            SkillsTab::Hive => self.handle_hive_tab_key(key),
        }
    }

    fn handle_skills_tab_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('j'), _) | (KeyCode::Down, _)
                if !self.skills.is_empty() && self.skills_selected + 1 < self.skills.len() =>
            {
                self.skills_selected += 1;
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) if self.skills_selected > 0 => {
                self.skills_selected -= 1;
            }
            (KeyCode::Char('r'), _) => {
                self.refresh_skills();
                self.skills_status_msg = Some(format!("Rescanned: {} skills", self.skills.len()));
            }
            (KeyCode::Char('s'), _) => {
                self.share_selected_skill();
            }
            _ => {}
        }
    }

    fn handle_hive_tab_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('h'), _) => {
                self.start_hive_listener();
                self.refresh_hive_view();
            }
            (KeyCode::Char('i'), _) => {
                self.generate_hive_invite();
            }
            (KeyCode::Char('J'), _) => {
                self.hive_join_input_mode = true;
                self.hive_join_buffer.clear();
                self.skills_status_msg =
                    Some("Paste invite (relay code, link, or word phrase); Enter to join".into());
            }
            (KeyCode::Char('r'), _) => {
                self.refresh_hive_view();
                self.skills_status_msg =
                    Some(format!("Known peers: {}", self.hive_known_peers.len()));
            }
            _ => {}
        }
    }

    fn handle_hive_join_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let code = self.hive_join_buffer.trim().to_string();
                self.hive_join_input_mode = false;
                if code.is_empty() {
                    self.skills_status_msg = Some("Join cancelled (empty)".into());
                    return;
                }
                match spawn_relay_join(&code) {
                    Ok(()) => {
                        self.skills_status_msg = Some(format!(
                            "Join started (claudectl relay join {} detached)",
                            short_id(&code)
                        ));
                    }
                    Err(e) => {
                        self.skills_status_msg = Some(format!("Join failed: {e}"));
                    }
                }
                self.hive_join_buffer.clear();
            }
            KeyCode::Esc => {
                self.hive_join_input_mode = false;
                self.hive_join_buffer.clear();
                self.skills_status_msg = Some("Join cancelled".into());
            }
            KeyCode::Backspace => {
                self.hive_join_buffer.pop();
            }
            KeyCode::Char(c) if self.hive_join_buffer.len() < 256 => {
                self.hive_join_buffer.push(c);
            }
            _ => {}
        }
    }

    fn generate_hive_invite(&mut self) {
        match generate_invite_via_cli() {
            Ok(invite) => {
                self.skills_status_msg = Some(format!("Invite: {}", invite.relay_code));
                self.hive_last_invite = Some(invite);
            }
            Err(e) => {
                self.skills_status_msg = Some(format!("Invite failed: {e}"));
            }
        }
    }

    fn share_selected_skill(&mut self) {
        let Some(skill) = self.skills.get(self.skills_selected).cloned() else {
            self.skills_status_msg = Some("No skill selected".into());
            return;
        };
        if !cfg!(feature = "hive") {
            self.skills_status_msg = Some("hive feature disabled in this build".into());
            return;
        }
        if !skill.within_share_limit() {
            self.skills_status_msg = Some("Skill exceeds 32kb share limit".into());
            return;
        }
        if self.shared_skill_keys.contains(&skill.semantic_key()) {
            self.skills_status_msg = Some("Already shared".into());
            return;
        }
        match self.runtime.hive.share_skill(&skill) {
            Ok(unit_id) => {
                self.shared_skill_keys.insert(skill.semantic_key());
                self.skills_status_msg = Some(format!(
                    "Shared '{}' → unit {}",
                    skill.name,
                    short_id(&unit_id)
                ));
            }
            Err(e) => {
                self.skills_status_msg = Some(format!("Share failed: {e}"));
            }
        }
    }

    fn start_hive_listener(&mut self) {
        if !cfg!(feature = "relay") {
            self.skills_status_msg =
                Some("relay feature not built — rebuild with --features relay,hive".into());
            return;
        }
        if self.hive_listener_running {
            self.skills_status_msg = Some("Hive listener already running".into());
            return;
        }
        match spawn_relay_serve() {
            Ok(()) => {
                self.hive_listener_running = true;
                self.skills_status_msg =
                    Some("Hive listener started (claudectl relay serve detached)".into());
            }
            Err(e) => {
                self.skills_status_msg = Some(format!("Start failed: {e}"));
            }
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
                self.should_quit = true;
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.next();
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.previous();
            }
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                // Bus role bind (#307). Ctrl+R because plain `r` is refresh.
                // Match before the unconditional `r` arm below, otherwise
                // the wildcard modifier swallows the Control modifier.
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.enter_role_bind_mode();
            }
            (KeyCode::Char('r'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.enqueue_all_git(); // manual refresh re-fetches every repo's git
                self.refresh();
            }
            (KeyCode::Char('R'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.toggle_session_recording();
            }
            (KeyCode::Char('d'), _) | (KeyCode::Char('x'), _) => {
                self.cancel_pending_auto_approve();
                self.handle_kill();
            }
            (KeyCode::Char('y'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.handle_approve();
            }
            (KeyCode::Char('A'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.open_approval_inspector();
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.toggle_brain_gate();
            }
            (KeyCode::Char('b'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.handle_brain_accept();
            }
            (KeyCode::Char('B'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.handle_brain_reject();
            }
            (KeyCode::Char('i'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.enter_input_mode();
            }
            (KeyCode::Char('c'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.handle_compact();
            }
            (KeyCode::Char('?'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.show_help = !self.show_help;
            }
            (KeyCode::Char('K'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.open_skills_overlay();
            }
            (KeyCode::Char('M'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.open_brain_overlay();
            }
            (KeyCode::Char('s'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.cycle_sort();
            }
            (KeyCode::Char('f'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.cycle_status_filter();
            }
            (KeyCode::Char('v'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.cycle_focus_filter();
            }
            (KeyCode::Char('z'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.clear_filters();
            }
            (KeyCode::Char('/'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.enter_search_mode();
            }
            (KeyCode::Char('a'), _) => {
                self.cancel_pending_kill();
                self.handle_auto_approve();
            }
            (KeyCode::Char('n'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.quick_launch();
            }
            (KeyCode::Char('N'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.enter_launch_mode();
            }
            (KeyCode::Char('o'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.stage_selected();
            }
            (KeyCode::Char('P'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.start_git_op(GitOpKind::Push);
            }
            (KeyCode::Char('L'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.start_git_op(GitOpKind::Pull);
            }
            (KeyCode::Char('C'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.open_chat();
            }
            (KeyCode::Char('G'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.show_fleet = !self.show_fleet;
                self.status_msg = if self.show_fleet {
                    "Fleet trend strip on".into()
                } else {
                    "Fleet trend strip off".into()
                };
            }
            (KeyCode::Char('g'), _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.grouped_view = !self.grouped_view;
                self.status_msg = if self.grouped_view {
                    "Grouped by project".into()
                } else {
                    "Flat view".into()
                };
            }
            (KeyCode::Enter, _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.detail_panel = !self.detail_panel;
            }
            (KeyCode::Tab, _) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.handle_switch_terminal();
            }
            #[cfg(feature = "relay")]
            (KeyCode::Char('p'), KeyModifiers::NONE) => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
                self.show_peers_panel = !self.show_peers_panel;
                self.status_msg = if self.show_peers_panel {
                    "Peers panel enabled".into()
                } else {
                    "Peers panel disabled".into()
                };
            }
            _ => {
                self.cancel_pending_kill();
                self.cancel_pending_auto_approve();
            }
        }
    }

    fn handle_launch_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.submit_launch_form();
            }
            KeyCode::Enter => {
                if self.launch_form.is_last_field() {
                    self.submit_launch_form();
                } else {
                    self.launch_form.advance();
                    self.status_msg = self.launch_form.status_hint();
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                self.launch_form.advance();
                self.status_msg = self.launch_form.status_hint();
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.launch_form.retreat();
                self.status_msg = self.launch_form.status_hint();
            }
            KeyCode::Esc => {
                self.launch_mode = false;
                self.launch_form = LaunchForm::default();
                self.status_msg = "Launch cancelled".into();
            }
            KeyCode::Backspace => {
                self.launch_form.active_buffer_mut().pop();
            }
            KeyCode::Char(c) => {
                self.launch_form.active_buffer_mut().push(c);
            }
            _ => {}
        }
    }

    /// Instant launch (no wizard): start an agent in the selected project with
    /// no prompt/resume. `n` uses this; `N` opens the full wizard. The agent
    /// opens in a tmux split pane (see `tmux::launch`).
    fn quick_launch(&mut self) {
        let cwd = self
            .selected_launch_cwd()
            .unwrap_or_else(|| self.parent_dir.clone());
        let cwd = cwd.to_string_lossy().into_owned();

        // One agent visible at a time: break out whatever's currently staged so
        // the new one replaces it in the bottom split rather than stacking.
        let in_tmux = std::env::var("TMUX_PANE").is_ok();
        if in_tmux {
            if let Some(prev) = self.staged_pane.take() {
                let _ = terminals::unstage_pane(&prev);
            }
        }

        let result = launch::prepare(&cwd, None, None).and_then(|req| {
            launch::launch(&req).map(|target| (target, req.cwd_path.display().to_string()))
        });
        self.status_msg = match result {
            Ok((target, path)) => {
                // In tmux, `target` is the new pane id — track it as the stage.
                if in_tmux {
                    let title = stage_title_for(&path);
                    terminals::set_stage_title(&target, &title);
                    self.staged_pane = Some(target);
                    let _ = terminals::resize_stage_top(self.stage_top_rows());
                }
                format!("Launched agent at {path}")
            }
            Err(err) => format!("Launch failed: {err}"),
        };
    }

    /// Swap the bottom split "stage" to show the selected agent's pane (one at a
    /// time). Pressing `o` on the already-staged agent hides it. tmux only.
    /// Re-fit herdr's own pane to its roster height when an agent is staged below.
    /// Called on terminal resize so *growing* the window hands the freed rows back
    /// to the staged agent instead of leaving it compressed at the old size
    /// (BACKLOG "resizing the window changes the agent window size permanently").
    /// Gated on "staged" so it never fights a manual `Ctrl-b` resize when nothing
    /// is staged below herdr.
    pub fn refit_stage(&self) {
        if self.staged_pane.is_some() {
            let _ = terminals::resize_stage_top(self.stage_top_rows());
        }
    }

    fn stage_selected(&mut self) {
        let (pid, tty, remote, name) = match self.selected_session() {
            Some(s) => (
                s.pid,
                s.tty.clone(),
                s.is_remote(),
                s.display_name().to_string(),
            ),
            None => {
                self.status_msg = "Select an agent to view in the split".into();
                return;
            }
        };
        let _ = pid;
        if remote {
            self.status_msg = "Remote session — not available".into();
            return;
        }
        let Some(pane) = terminals::agent_pane(&tty) else {
            self.status_msg = "Agent pane not found — is it running inside tmux?".into();
            return;
        };

        // Toggle off if this agent is already the staged one.
        if self.staged_pane.as_deref() == Some(pane.as_str()) {
            let _ = terminals::unstage_pane(&pane);
            terminals::clear_stage_title();
            self.staged_pane = None;
            self.status_msg = "Hid the agent pane".into();
            return;
        }

        let previous = self.staged_pane.take();
        match terminals::stage_pane(&pane, previous.as_deref()) {
            Ok(()) => {
                terminals::set_stage_title(&pane, &format!("Claude \u{2014} {name}"));
                self.staged_pane = Some(pane);
                let _ = terminals::resize_stage_top(self.stage_top_rows());
                self.status_msg = "Viewing agent (Ctrl-b ↓ to type · o to hide)".into();
            }
            Err(e) => {
                // `previous` was already broken out by stage_pane on the tmux path.
                self.status_msg = format!("View failed: {e}");
            }
        }
    }

    fn enter_launch_mode(&mut self) {
        self.launch_mode = true;
        // Project-first (CLAUDE.md §2, Phase 2): pre-fill the launch cwd from the
        // selected project (or the selected agent's project), so `n` launches
        // into wherever the cursor sits — including an idle, zero-agent repo.
        // Falls back to the CLI default (".") when nothing is selected.
        let mut form = LaunchForm::default();
        if let Some(cwd) = self.selected_launch_cwd() {
            form.cwd = cwd.to_string_lossy().into_owned();
        }
        self.launch_form = form;
        self.status_msg = self.launch_form.status_hint();
    }

    fn submit_launch_form(&mut self) {
        let request = match self.launch_form.request() {
            Ok(request) => request,
            Err(err) => {
                self.launch_form.field = LaunchField::Cwd;
                self.status_msg = format!("Launch failed: {err}");
                return;
            }
        };

        let in_tmux = std::env::var("TMUX_PANE").is_ok();
        if in_tmux {
            if let Some(prev) = self.staged_pane.take() {
                let _ = terminals::unstage_pane(&prev);
            }
        }
        match launch::launch(&request) {
            Ok(target) => {
                self.launch_mode = false;
                self.launch_form = LaunchForm::default();
                if in_tmux {
                    let title = stage_title_for(&request.cwd_path.display().to_string());
                    terminals::set_stage_title(&target, &title);
                    self.staged_pane = Some(target);
                    let _ = terminals::resize_stage_top(self.stage_top_rows());
                }
                self.status_msg = format!(
                    "Launched session at {}{}",
                    request.cwd_path.display(),
                    request.option_summary()
                );
            }
            Err(err) => {
                self.status_msg = format!("Launch failed: {err}");
            }
        }
    }

    fn enter_search_mode(&mut self) {
        self.search_mode = true;
        self.search_buffer = self.search_query.clone();
    }

    pub fn clear_filters(&mut self) {
        self.status_filter = StatusFilter::All;
        self.focus_filter = FocusFilter::All;
        self.search_query.clear();
        self.search_buffer.clear();
        self.search_mode = false;
        self.normalize_selection();
        self.status_msg = "Filters cleared".into();
    }

    pub fn cycle_status_filter(&mut self) {
        self.status_filter = self.status_filter.next();
        self.normalize_selection();
        self.status_msg = format!("Status filter: {}", self.status_filter.label());
    }

    pub fn cycle_focus_filter(&mut self) {
        self.focus_filter = self.focus_filter.next();
        self.normalize_selection();
        self.status_msg = format!("Focus filter: {}", self.focus_filter.label());
    }

    pub fn has_active_filters(&self) -> bool {
        self.status_filter != StatusFilter::All
            || self.focus_filter != FocusFilter::All
            || !self.search_query.trim().is_empty()
    }

    pub fn filter_summary(&self) -> String {
        let mut parts = Vec::new();
        if self.status_filter != StatusFilter::All {
            parts.push(format!("status={}", self.status_filter.label()));
        }
        if self.focus_filter != FocusFilter::All {
            parts.push(format!("focus={}", self.focus_filter.label()));
        }
        if !self.search_query.trim().is_empty() {
            parts.push(format!("search=\"{}\"", self.search_query));
        }
        if parts.is_empty() {
            "filters: none".to_string()
        } else {
            format!("filters: {}", parts.join(" | "))
        }
    }

    pub fn visible_session_indices(&self) -> Vec<usize> {
        self.sessions
            .iter()
            .enumerate()
            .filter_map(|(idx, session)| self.matches_filters(session).then_some(idx))
            .collect()
    }

    pub fn visible_sessions(&self) -> Vec<&ClaudeSession> {
        self.visible_session_indices()
            .into_iter()
            .filter_map(|idx| self.sessions.get(idx))
            .collect()
    }

    pub fn visible_session_count(&self) -> usize {
        self.visible_session_indices().len()
    }

    /// Phase 5: roll the live fleet up by status + total burn for the trend strip.
    /// Counts every agent (not just filtered rows) — the strip is a whole-fleet
    /// glance, independent of the roster's active triage filters.
    pub fn fleet_counts(&self) -> FleetCounts {
        let mut c = FleetCounts::default();
        for s in &self.sessions {
            c.total += 1;
            c.burn_per_hr += s.burn_rate_per_hr;
            match s.status {
                // Both want the user: an approval prompt or an API error to recover from.
                SessionStatus::NeedsInput | SessionStatus::Error => c.needs_input += 1,
                SessionStatus::Processing => c.processing += 1,
                // Waiting on the API, or job done awaiting the next task — not engaged-busy.
                SessionStatus::WaitingInput | SessionStatus::JobDone => c.waiting += 1,
                // Idle/Unknown/Finished all read as "not currently engaged".
                _ => c.idle += 1,
            }
        }
        c
    }

    fn normalize_selection(&mut self) {
        let len = self.roster_len();
        if len == 0 {
            self.table_state.select(None);
        } else if self.table_state.selected().is_none() {
            self.table_state.select(Some(0));
        } else if let Some(sel) = self.table_state.selected() {
            if sel >= len {
                self.table_state.select(Some(len - 1));
            }
        }
    }

    /// The stable identity of the currently selected row (project path for a
    /// header, PID for an agent), or `None` if nothing is selected.
    fn selection_key(&self) -> Option<RosterSelKey> {
        let sel = self.table_state.selected()?;
        let (groups, rows) = self.roster_layout();
        match rows.get(sel)? {
            RosterRow::Header(gi) => groups
                .get(*gi)
                .and_then(|g| g.path.clone())
                .map(RosterSelKey::Project),
            RosterRow::Agent(si) => self.sessions.get(*si).map(|s| RosterSelKey::Agent(s.pid)),
        }
    }

    /// Move the cursor back onto `key`'s row after a re-sort, so selection follows
    /// the same project/agent rather than whatever floated to that index. No-op if
    /// the key is gone (the row no longer exists) — `normalize_selection` already
    /// clamped the index in that case.
    fn reselect_by_key(&mut self, key: Option<RosterSelKey>) {
        let Some(key) = key else { return };
        let (groups, rows) = self.roster_layout();
        let found = rows.iter().position(|row| match (row, &key) {
            (RosterRow::Header(gi), RosterSelKey::Project(p)) => {
                groups.get(*gi).and_then(|g| g.path.as_ref()) == Some(p)
            }
            (RosterRow::Agent(si), RosterSelKey::Agent(pid)) => {
                self.sessions.get(*si).map(|s| s.pid) == Some(*pid)
            }
            _ => false,
        });
        if let Some(idx) = found {
            self.table_state.select(Some(idx));
        }
    }

    fn matches_filters(&self, session: &ClaudeSession) -> bool {
        self.status_filter.matches(session.status)
            && self.matches_focus_filter(session)
            && self.matches_search_query(session)
    }

    fn matches_focus_filter(&self, session: &ClaudeSession) -> bool {
        let over_budget = self
            .budget_usd
            .map(|budget| session.has_usage_metrics() && session.cost_usd >= budget)
            .unwrap_or(false);
        let high_context = session.has_usage_metrics()
            && session.context_percent() >= self.context_warn_threshold as f64;
        let unknown_telemetry = !session.has_usage_metrics();
        let conflict = self.conflict_pids.contains(&session.pid);

        match self.focus_filter {
            FocusFilter::All => true,
            FocusFilter::Attention => {
                session.status == SessionStatus::NeedsInput
                    || over_budget
                    || high_context
                    || unknown_telemetry
                    || conflict
            }
            FocusFilter::OverBudget => over_budget,
            FocusFilter::HighContext => high_context,
            FocusFilter::UnknownTelemetry => unknown_telemetry,
            FocusFilter::Conflict => conflict,
        }
    }

    fn matches_search_query(&self, session: &ClaudeSession) -> bool {
        let query = self.search_query.trim();
        if query.is_empty() {
            return true;
        }

        let query = query.to_ascii_lowercase();
        let fields = [
            session.display_name().to_string(),
            session.project_name.clone(),
            session.model.clone(),
            session.cwd.clone(),
            session.session_id.clone(),
        ];

        fields
            .iter()
            .any(|field| field.to_ascii_lowercase().contains(&query))
    }

    fn handle_approve(&mut self) {
        if let Some(session) = self.selected_session() {
            if session.is_remote() {
                self.status_msg = "Remote session \u{2014} action not available".into();
                return;
            }
            if session.status == SessionStatus::NeedsInput {
                // Log passive observation: user approved without brain involvement
                let _ = self
                    .runtime
                    .actions
                    .log_observation(observation_from(session, "user_approve"));
                match terminals::approve_session(session) {
                    Ok(()) => self.status_msg = format!("Approved {}", session.display_name()),
                    Err(e) => self.status_msg = format!("Error: {e}"),
                }
            } else {
                self.status_msg = "Session is not waiting for input".into();
            }
        }
    }

    /// Phase 4c: open the approval inspector for the selected agent. Scrapes the
    /// agent's tmux pane (`capture-pane`) so the *actual* permission dialog —
    /// invisible in the JSONL — is shown before you approve/deny/interrupt
    /// without switching panes. No-op (with a hint) for non-agents, remote
    /// sessions, or terminals that can't inject keystrokes.
    fn open_approval_inspector(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            self.status_msg = "Select an agent to inspect its prompt".into();
            return;
        };
        if session.is_remote() {
            self.status_msg = "Remote session \u{2014} action not available".into();
            return;
        }
        if !self.input_supported {
            self.status_msg =
                "Approve/deny needs tmux \u{2014} run agents and herdr inside tmux (see ? help)"
                    .into();
            return;
        }
        // Best-effort capture: a failed/empty scrape still opens the modal (with a
        // note), so deny/interrupt stay reachable even when the preview is blank.
        self.approval_preview = terminals::capture_pane(&session).unwrap_or_default();
        self.approval_pid = Some(session.pid);
    }

    /// Re-run the pane capture for the open inspector (the dialog may have
    /// changed since it opened). No-op if the modal is closed.
    fn refresh_approval_preview(&mut self) {
        let Some(pid) = self.approval_pid else {
            return;
        };
        if let Some(session) = self.sessions.iter().find(|s| s.pid == pid) {
            self.approval_preview = terminals::capture_pane(session).unwrap_or_default();
        }
    }

    /// Phase 4c approval-inspector keymap: `y`/Enter approve, `n` deny, `i`
    /// interrupt, `r` re-capture the dialog, `Esc` cancel.
    fn handle_approval_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => self.approval_act(ApprovalAct::Approve),
            KeyCode::Char('n') => self.approval_act(ApprovalAct::Deny),
            KeyCode::Char('i') => self.approval_act(ApprovalAct::Interrupt),
            KeyCode::Char('r') => self.refresh_approval_preview(),
            KeyCode::Esc => {
                self.approval_pid = None;
                self.approval_preview.clear();
                self.status_msg = "Cancelled".into();
            }
            _ => {}
        }
    }

    /// Deliver the chosen action to the inspected agent's pane via the inherited
    /// keystroke backend (CLAUDE.md §0 — we drive the existing pane, tmux owns the
    /// process). Closes the modal on success; keeps it open on failure so the
    /// error is visible and the user can retry.
    fn approval_act(&mut self, act: ApprovalAct) {
        let Some(pid) = self.approval_pid else {
            return;
        };
        let outcome = match self.sessions.iter().find(|s| s.pid == pid) {
            None => Err("Agent is no longer running".to_string()),
            Some(s) if s.is_remote() => {
                Err("Remote session \u{2014} action not available".to_string())
            }
            Some(s) => {
                let name = s.display_name().to_string();
                let result = match act {
                    ApprovalAct::Approve => terminals::approve_session(s),
                    ApprovalAct::Deny => terminals::deny_session(s),
                    ApprovalAct::Interrupt => terminals::interrupt_session(s),
                };
                result.map(|()| name)
            }
        };
        match outcome {
            Ok(name) => {
                self.status_msg = format!("{} {name}", act.past_tense());
                self.approval_pid = None;
                self.approval_preview.clear();
            }
            Err(msg) => self.status_msg = format!("{} failed: {msg}", act.verb()),
        }
    }

    fn handle_brain_accept(&mut self) {
        self.handle_brain_accept_with_reason(None);
    }

    fn handle_brain_accept_with_reason(&mut self, override_reason: Option<&str>) {
        // Clone session data first to avoid borrow conflict with brain_engine
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        if session.is_remote() {
            self.status_msg = "Remote session \u{2014} action not available".into();
            return;
        }
        let pid = session.pid;
        let Some(ref mut driver) = self.brain_driver else {
            self.status_msg = "Brain is not enabled".into();
            return;
        };
        // Get suggestion before accept (for logging)
        let suggestion = driver.pending_for(pid);
        let Some(sg) = suggestion else {
            self.status_msg = "No brain suggestion pending for this session".into();
            return;
        };

        // If brain suggested deny and no override reason yet, prompt for one
        if sg.action == "deny" && override_reason.is_none() {
            self.pending_override_reason = Some(pid);
            self.status_msg =
                "Override reason: [1] Always safe  [2] One-time exception  [3] Brain is wrong  [Esc] Cancel"
                    .into();
            return;
        }

        if let Some(msg) = driver.accept(pid) {
            let _ = self
                .runtime
                .actions
                .log_decision(claudectl_core::runtime::LogDecisionInput {
                    session_pid: pid,
                    project: session.display_name().to_string(),
                    tool: session.pending_tool_name.clone(),
                    command: session.pending_tool_input.clone(),
                    suggestion: sg,
                    user_action: "accept".into(),
                    decision_type: claudectl_core::runtime::DecisionScope::Session,
                    override_reason: override_reason.map(String::from),
                });
            claudectl_core::logger::log("BRAIN", &format!("Accepted: {msg}"));
            self.status_msg = msg;
        }
        self.pending_override_reason = None;
    }

    fn handle_brain_reject(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        if session.is_remote() {
            self.status_msg = "Remote session \u{2014} action not available".into();
            return;
        }
        let pid = session.pid;
        let Some(ref mut driver) = self.brain_driver else {
            self.status_msg = "Brain is not enabled".into();
            return;
        };
        if let Some(suggestion) = driver.reject(pid) {
            let log_input = claudectl_core::runtime::LogDecisionInput {
                session_pid: pid,
                project: session.display_name().to_string(),
                tool: session.pending_tool_name.clone(),
                command: session.pending_tool_input.clone(),
                suggestion: suggestion.clone(),
                user_action: "reject".into(),
                decision_type: claudectl_core::runtime::DecisionScope::Session,
                override_reason: None,
            };
            let _ = self.runtime.actions.log_decision(log_input);
            let msg = format!(
                "Rejected brain suggestion: {} ({})",
                suggestion.action, suggestion.reasoning,
            );
            claudectl_core::logger::log("BRAIN", &msg);
            self.status_msg = msg;
        } else {
            self.status_msg = "No brain suggestion pending for this session".into();
        }
    }

    fn toggle_brain_gate(&mut self) {
        use claudectl_core::runtime::BrainGateMode;
        let current = self.runtime.brain.gate_mode();
        // Toggle: On → Off, Off → On, Auto → Off. The wizard flips through
        // runtime.actions so the on-disk format stays in sync with what
        // BrainView reports next refresh.
        let next = match current {
            BrainGateMode::On => BrainGateMode::Off,
            BrainGateMode::Off => BrainGateMode::On,
            BrainGateMode::Auto => BrainGateMode::Off,
        };
        if let Err(e) = self.runtime.actions.set_gate_mode(next) {
            self.status_msg = format!("Brain: gate-mode update failed: {e}");
            return;
        }

        let description = match next {
            BrainGateMode::On => "active — evaluating tool calls",
            BrainGateMode::Off => "disabled — normal permission flow",
            BrainGateMode::Auto => "auto — automatic decisions",
        };
        self.status_msg = format!("Brain: {description}");
        claudectl_core::logger::log("BRAIN", &format!("Gate mode toggled: {current} → {next}"));
    }

    fn toggle_session_recording(&mut self) {
        let info = self
            .selected_session()
            .map(|s| (s.pid, s.display_name().to_string(), s.jsonl_path.is_some()));
        let Some((pid, name, has_jsonl)) = info else {
            return;
        };

        // Per-session toggle: if this session is recording, stop just this one
        if self.session_recordings.contains_key(&pid) {
            let path = self.session_recordings.remove(&pid).unwrap_or_default();
            self.status_msg = format!("Recording stopped → {path}");
            return;
        }

        // Start recording the selected session
        if !has_jsonl {
            self.status_msg = "Cannot record — no JSONL file for this session".into();
            return;
        }
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let path = format!("{}-{}-{}.gif", name, pid, epoch);
        self.session_recordings.insert(pid, path.clone());
        self.status_msg = format!("Recording {name} → {path} (R to stop)");
    }

    fn handle_compact(&mut self) {
        if let Some(session) = self.selected_session() {
            if session.is_remote() {
                self.status_msg = "Remote session \u{2014} action not available".into();
                return;
            }
            match session.status {
                SessionStatus::WaitingInput | SessionStatus::JobDone | SessionStatus::Idle => {
                    match self
                        .runtime
                        .actions
                        .inject_text(&session.session_id, "/compact\n")
                    {
                        Ok(()) => {
                            self.status_msg = format!("Sent /compact to {}", session.display_name())
                        }
                        Err(e) => self.status_msg = format!("Compact error: {e}"),
                    }
                }
                SessionStatus::NeedsInput => {
                    self.status_msg =
                        "Cannot compact — session is waiting for permission approval".into();
                }
                SessionStatus::Processing => {
                    self.status_msg =
                        "Cannot compact — session is processing (wait until idle)".into();
                }
                SessionStatus::Unknown => {
                    self.status_msg =
                        "Cannot compact — transcript telemetry is unavailable for this session"
                            .into();
                }
                SessionStatus::Finished => {
                    self.status_msg = "Cannot compact — session has finished".into();
                }
                SessionStatus::Error => {
                    self.status_msg = "Cannot compact — session hit an API error".into();
                }
            }
        }
    }

    fn enter_input_mode(&mut self) {
        if let Some(session) = self.selected_session() {
            if session.is_remote() {
                self.status_msg = "Remote session \u{2014} action not available".into();
                return;
            }
        }
        let info = self
            .selected_session()
            .map(|s| (s.pid, s.display_name().to_string()));
        if let Some((pid, name)) = info {
            self.input_mode = true;
            self.input_buffer.clear();
            self.input_target_pid = Some(pid);
            self.status_msg = format!("Input to {name} (Enter to send, Esc to cancel): ");
        }
    }

    /// Open the role-bind prompt for the selected session (#307). Captures
    /// the session's pid and cwd at entry time so a refresh tick or row
    /// move during typing can't change the target.
    fn enter_role_bind_mode(&mut self) {
        let Some(session) = self.selected_session() else {
            self.status_msg = "No session selected".into();
            return;
        };
        if session.is_remote() {
            self.status_msg = "Remote session \u{2014} bind locally instead".into();
            return;
        }
        let pid = session.pid;
        let cwd = session.cwd.clone();
        let name = session.display_name().to_string();
        self.role_bind_mode = true;
        self.role_bind_buffer.clear();
        self.role_bind_target_pid = Some(pid);
        self.role_bind_target_cwd = Some(cwd);
        self.status_msg =
            format!("Bind role for {name} (pid={pid}, Enter to bind, Esc to cancel): ");
    }

    fn handle_role_bind_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let role = self.role_bind_buffer.trim().to_string();
                let pid = self.role_bind_target_pid;
                let cwd = self.role_bind_target_cwd.clone();
                self.role_bind_mode = false;
                self.role_bind_buffer.clear();
                self.role_bind_target_pid = None;
                self.role_bind_target_cwd = None;
                if role.is_empty() {
                    self.status_msg = "Role name required".into();
                    return;
                }
                let (Some(pid), Some(cwd)) = (pid, cwd) else {
                    self.status_msg = "Lost bind target — re-select the session".into();
                    return;
                };
                match self.runtime.actions.bind_bus_role(&role, &cwd, pid) {
                    Ok(()) => {
                        self.status_msg = format!("Bound role {role} -> pid={pid} cwd={cwd}");
                    }
                    Err(e) => {
                        self.status_msg = format!("Bind failed: {e}");
                    }
                }
            }
            KeyCode::Esc => {
                self.role_bind_mode = false;
                self.role_bind_buffer.clear();
                self.role_bind_target_pid = None;
                self.role_bind_target_cwd = None;
                self.status_msg = "Role bind cancelled".into();
            }
            KeyCode::Backspace => {
                self.role_bind_buffer.pop();
            }
            // Role names are short, alpha-numeric with - and _. Cap at 64
            // so a runaway paste can't take the prompt hostage.
            KeyCode::Char(c)
                if self.role_bind_buffer.len() < 64
                    && (c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
            {
                self.role_bind_buffer.push(c);
            }
            _ => {}
        }
    }

    fn handle_switch_terminal(&mut self) {
        if let Some(session) = self.selected_session() {
            if session.is_remote() {
                self.status_msg = "Remote session \u{2014} action not available".into();
                return;
            }
            match terminals::switch_to_terminal(session) {
                Ok(()) => {
                    self.status_msg = format!("Switched to {}", session.display_name());
                }
                Err(e) => {
                    self.status_msg = format!("Error: {e}");
                }
            }
        } else {
            self.status_msg = "No session selected".into();
        }
    }
}

/// One navigable line in the project-first roster (CLAUDE.md §2). The selection
/// ordinal (`App::table_state`) indexes a `Vec<RosterRow>`, so project headers —
/// including idle, zero-agent projects — are selectable, not just agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosterRow {
    /// A project header. Indexes the `groups` vec returned by `roster_layout`.
    Header(usize),
    /// An agent row. Indexes `App::sessions`.
    Agent(usize),
}

/// A *stable* identity for the selected roster row, so the cursor follows the
/// same project/agent across re-sorts instead of staying on a drifting index
/// (BACKLOG: selection should follow the repo an agent was launched into).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosterSelKey {
    Project(PathBuf),
    Agent(u32),
}

#[derive(Debug, Clone)]
pub struct ProjectGroup {
    pub name: String,
    /// Project directory, or `None` for the synthetic "(other)" bucket.
    pub path: Option<PathBuf>,
    pub has_git: bool,
    /// PIDs of the agents in this project, in roster order (empty = idle project).
    pub pids: Vec<u32>,
    pub session_count: usize,
    pub active_count: usize,
    pub total_cost: f64,
    pub avg_context_pct: f64,
    /// Git status of the project dir (Phase 3 light path); `None` for the
    /// "(other)" bucket, non-git dirs, or when `git` is unavailable.
    pub git: Option<GitStatus>,
    /// Actionability rank of the most-urgent agent (lower = more urgent); used to
    /// float projects with a blocked agent to the top. `u8::MAX` when idle.
    pub urgency: u8,
}

/// Short display name for a project path (its final component), for status text.
fn project_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Merge a freshly discovered session into the previous one for the same PID.
/// Normally the accumulated transcript state (tokens, context, byte offset, chat)
/// is preserved and only the ephemeral process fields are refreshed. But when the
/// PID keeps running while its underlying *session id* changes — the user ran
/// `/clear`, which starts a brand-new conversation/transcript in the same process
/// — that accumulated state belongs to the old conversation and must be dropped
/// so context/tokens reset and the new transcript is re-resolved (BACKLOG:
/// context should reset on /clear). The fresh `new` already carries zeroed
/// transcript state and `jsonl_path: None`; we keep `prev`'s CPU history since
/// it's the same OS process.
fn merge_discovered_session(prev: ClaudeSession, new: ClaudeSession) -> ClaudeSession {
    if prev.session_id == new.session_id {
        let mut merged = prev;
        merged.elapsed = new.elapsed;
        merged.started_at = new.started_at;
        merged
    } else {
        let mut fresh = new;
        fresh.cpu_history = prev.cpu_history;
        // Same OS process across /clear — the CPU-time counter keeps climbing, so
        // carry the sampling state or the next tick would see a huge spurious delta.
        fresh.prev_cpu_secs = prev.prev_cpu_secs;
        fresh.prev_cpu_sample_ms = prev.prev_cpu_sample_ms;
        fresh
    }
}

/// Actionability rank for roster ordering (from agent-deck): a blocked agent is
/// more urgent than a busy or expensive one. Lower sorts first.
/// Title for a freshly launched agent's staged pane border: "Claude — <project>"
/// from the launch path's final component.
fn stage_title_for(path: &str) -> String {
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("agent");
    format!("Claude \u{2014} {name}")
}

fn status_urgency(status: SessionStatus) -> u8 {
    match status {
        SessionStatus::NeedsInput => 0,
        SessionStatus::Error => 1,
        SessionStatus::Processing => 2,
        SessionStatus::JobDone => 3,
        SessionStatus::WaitingInput => 4,
        SessionStatus::Idle => 5,
        SessionStatus::Unknown => 6,
        SessionStatus::Finished => 7,
    }
}

impl ProjectGroup {
    fn aggregate(members: &[&ClaudeSession]) -> (usize, f64, f64) {
        let active = members
            .iter()
            .filter(|s| {
                matches!(
                    s.status,
                    SessionStatus::Processing | SessionStatus::NeedsInput
                )
            })
            .count();
        let cost: f64 = members.iter().map(|s| s.cost_usd).sum();
        let avg_ctx = if members.is_empty() {
            0.0
        } else {
            members.iter().map(|s| s.context_percent()).sum::<f64>() / members.len() as f64
        };
        (active, cost, avg_ctx)
    }

    fn new(name: String, path: Option<PathBuf>, has_git: bool, members: &[&ClaudeSession]) -> Self {
        let (active_count, total_cost, avg_context_pct) = Self::aggregate(members);
        let urgency = members
            .iter()
            .map(|s| status_urgency(s.status))
            .min()
            .unwrap_or(u8::MAX);
        Self {
            name,
            path,
            has_git,
            pids: members.iter().map(|s| s.pid).collect(),
            session_count: members.len(),
            active_count,
            total_cost,
            avg_context_pct,
            git: None,
            urgency,
        }
    }
}

impl App {
    /// Project-first roster (CLAUDE.md §2): one group per scanned project —
    /// present whether or not it hosts agents — with each visible session
    /// attached to its project by canonical `cwd` path. Sessions under no
    /// scanned project fall into a synthetic "(other)" group so none are hidden.
    pub fn project_groups(&self) -> Vec<ProjectGroup> {
        // Canonicalize so matching survives symlinks (e.g. /Users vs /private on
        // macOS). Falls back to the raw path if the dir can't be resolved.
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());

        let sessions: Vec<&ClaudeSession> = self.visible_sessions();
        let session_paths: Vec<PathBuf> =
            sessions.iter().map(|s| canon(Path::new(&s.cwd))).collect();
        let mut assigned = vec![false; sessions.len()];

        let mut result: Vec<ProjectGroup> = Vec::with_capacity(self.projects.len() + 1);
        for proj in &self.projects {
            let proj_canon = canon(&proj.path);
            let mut members: Vec<&ClaudeSession> = Vec::new();
            for (i, s) in sessions.iter().enumerate() {
                if !assigned[i] && projects::contains_cwd(&proj_canon, &session_paths[i]) {
                    assigned[i] = true;
                    members.push(s);
                }
            }
            let mut group = ProjectGroup::new(
                proj.name.clone(),
                Some(proj.path.clone()),
                proj.has_git,
                &members,
            );
            group.git = self.git_cache.get(&proj.path).cloned().flatten();
            result.push(group);
        }

        let orphans: Vec<&ClaudeSession> = sessions
            .iter()
            .enumerate()
            .filter(|(i, _)| !assigned[*i])
            .map(|(_, s)| *s)
            .collect();
        if !orphans.is_empty() {
            result.push(ProjectGroup::new(
                "(other)".to_string(),
                None,
                false,
                &orphans,
            ));
        }

        // Projects hosting agents first, then by actionability (most-urgent agent
        // — a NeedsInput beats a busy or merely expensive one), then cost desc,
        // then name. Idle (zero-agent) projects sink below, ordered by name.
        result.sort_by(|a, b| {
            let a_active = a.session_count > 0;
            let b_active = b.session_count > 0;
            b_active
                .cmp(&a_active)
                .then_with(|| a.urgency.cmp(&b.urgency))
                .then_with(|| {
                    b.total_cost
                        .partial_cmp(&a.total_cost)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.name.cmp(&b.name))
        });
        result
    }

    /// The canonical roster ordering shared by selection and render (CLAUDE.md
    /// §2). Grouped view: a `Header` per project group (in `project_groups`
    /// order) followed by that group's agents; flat view: just the visible
    /// agents. The returned groups back the `Header` indices so the renderer and
    /// the selection logic never drift. This is the single source of truth for
    /// what `table_state` selects.
    pub fn roster_layout(&self) -> (Vec<ProjectGroup>, Vec<RosterRow>) {
        if self.grouped_view {
            let groups = self.project_groups();
            let mut rows = Vec::new();
            for (gi, group) in groups.iter().enumerate() {
                rows.push(RosterRow::Header(gi));
                for &pid in &group.pids {
                    if let Some(si) = self.sessions.iter().position(|s| s.pid == pid) {
                        rows.push(RosterRow::Agent(si));
                    }
                }
            }
            (groups, rows)
        } else {
            let rows = self
                .visible_session_indices()
                .into_iter()
                .map(RosterRow::Agent)
                .collect();
            (Vec::new(), rows)
        }
    }

    /// Number of navigable roster rows (headers + agents). Selection navigation
    /// and clamping operate over this, not the bare session count.
    pub fn roster_len(&self) -> usize {
        self.roster_layout().1.len()
    }

    /// Rows herdr's pane needs to show the *agent-bearing* roster without
    /// scrolling: one per header of a project that actually hosts agents, one per
    /// agent (plus its subagent rows), plus chrome (table header, footer, status,
    /// borders). Idle, zero-agent project headers are deliberately excluded — they
    /// sink to the bottom of the roster, so reserving height for them would push
    /// herdr's pane tall and crowd out the staged window when many repos are
    /// present (BACKLOG: auto-height). Clamped so it never eats the whole window.
    /// Used to auto-size herdr's pane when an agent is staged below it.
    fn stage_top_rows(&self) -> u16 {
        let (groups, rows) = self.roster_layout();
        let visual: usize = rows
            .iter()
            .map(|row| match row {
                // Only count a header when its project hosts at least one agent;
                // idle repos add no height.
                RosterRow::Header(gi) => {
                    usize::from(groups.get(*gi).is_some_and(|g| !g.pids.is_empty()))
                }
                RosterRow::Agent(si) => {
                    1 + self
                        .sessions
                        .get(*si)
                        .map_or(0, |s| s.subagent_breakdown().len())
                }
            })
            .sum();
        // +1 for the fleet trend strip when it's shown, so the staged pane below
        // reserves room for it too (auto-height, BACKLOG).
        let fleet = usize::from(self.show_fleet);
        // Floor of 12 rows so a single active agent + open chat isn't a cramped
        // sliver (BACKLOG "Roster bar too small"). `terminals::cap_stage_top_rows`
        // trims this back on short terminals so the agent pane keeps its minimum,
        // so the floor never starves the staged pane.
        ((visual + 4 + fleet) as u16).clamp(12, 40)
    }

    /// Working directory to launch a new agent into, derived from the current
    /// selection (CLAUDE.md §2, Phase 2): the selected project (header), the
    /// project owning the selected agent, or — for an out-of-tree agent with no
    /// owning project — that agent's own cwd. `None` when nothing is selected.
    pub fn selected_launch_cwd(&self) -> Option<PathBuf> {
        let selected = self.table_state.selected()?;
        let (groups, rows) = self.roster_layout();
        match rows.get(selected)? {
            RosterRow::Header(gi) => groups.get(*gi).and_then(|g| g.path.clone()),
            RosterRow::Agent(si) => {
                let s = self.sessions.get(*si)?;
                groups
                    .iter()
                    .find(|g| g.pids.contains(&s.pid))
                    .and_then(|g| g.path.clone())
                    .or_else(|| Some(PathBuf::from(&s.cwd)))
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Hive/relay shell-out helpers — kept at module scope so the App methods stay
// short. The read-side helpers (skill-key collection, hive snapshot) and the
// write-side helper (share skill) moved into `runtime::hive::LiveHiveActions`
// so the future TUI crate (#275) can hold them through the trait surface.
// ────────────────────────────────────────────────────────────────────────────

/// Detach a `claudectl relay serve` child so the TUI keeps running.
#[cfg(feature = "relay")]
fn spawn_relay_serve() -> Result<(), String> {
    use std::process::{Command, Stdio};
    Command::new(std::env::current_exe().unwrap_or_else(|_| "claudectl".into()))
        .args(["relay", "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "relay"))]
fn spawn_relay_serve() -> Result<(), String> {
    Err("relay feature not built".into())
}

/// Detach a `claudectl relay join <code>` child so the TUI keeps running.
#[cfg(feature = "relay")]
fn spawn_relay_join(code: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    Command::new(std::env::current_exe().unwrap_or_else(|_| "claudectl".into()))
        .args(["relay", "join", code])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "relay"))]
fn spawn_relay_join(_code: &str) -> Result<(), String> {
    Err("relay feature not built".into())
}

/// Shell out to `claudectl relay invite --json` and parse the result. We use
/// the existing CLI path rather than re-implementing because invite generation
/// has multiple components (crypto, LAN-IP detection, encoding) that already
/// live there.
#[cfg(feature = "relay")]
fn generate_invite_via_cli() -> Result<HiveInvite, String> {
    use std::process::Command;
    let bin = std::env::current_exe().map_err(|e| e.to_string())?;
    let output = Command::new(bin)
        .args(["--json", "relay", "invite", "--words"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| e.to_string())?;
    let relay_code = parsed
        .get("relay_code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let invite_link = parsed
        .get("invite_link")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let word_phrase = parsed
        .get("word_phrase")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if relay_code.is_empty() {
        return Err("invite payload missing relay_code".into());
    }
    Ok(HiveInvite {
        relay_code,
        invite_link,
        word_phrase,
    })
}

#[cfg(not(feature = "relay"))]
fn generate_invite_via_cli() -> Result<HiveInvite, String> {
    Err("relay feature not built".into())
}

fn short_id(id: &str) -> String {
    if id.len() <= 12 {
        id.to_string()
    } else {
        format!("{}…", &id[..11])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudectl_core::session::{RawSession, TelemetryStatus};

    fn make_session(
        pid: u32,
        project: &str,
        model: &str,
        status: SessionStatus,
        cost_usd: f64,
        context_pct: f64,
        telemetry_available: bool,
    ) -> ClaudeSession {
        let raw = RawSession {
            pid,
            session_id: format!("session-{pid}"),
            cwd: format!("/tmp/{project}"),
            started_at: 0,
        };
        let mut session = ClaudeSession::from_raw(raw);
        session.project_name = project.to_string();
        session.model = model.to_string();
        session.status = status;
        session.cost_usd = cost_usd;
        session.context_max = 100;
        session.context_tokens = context_pct as u64;
        session.telemetry_status = if telemetry_available {
            TelemetryStatus::Available
        } else {
            TelemetryStatus::MissingTranscript
        };
        session.usage_metrics_available = telemetry_available;
        session
    }

    #[test]
    fn merge_resets_transcript_state_when_session_id_changes() {
        // BACKLOG /clear: same PID, new session id → drop the old conversation's
        // accumulated tokens/context/offset/path, keep CPU history (same process).
        let mut prev = ClaudeSession::from_raw(RawSession {
            pid: 100,
            session_id: "old-session".into(),
            cwd: "/tmp/p".into(),
            started_at: 1,
        });
        prev.context_tokens = 62_000;
        prev.own_input_tokens = 5_000;
        prev.cost_usd = 1.23;
        prev.jsonl_path = Some(PathBuf::from("/old.jsonl"));
        prev.jsonl_offset = 999;
        prev.cpu_history = vec![3.0, 4.0];

        let new = ClaudeSession::from_raw(RawSession {
            pid: 100,
            session_id: "new-session".into(),
            cwd: "/tmp/p".into(),
            started_at: 2,
        });

        let merged = merge_discovered_session(prev, new);
        assert_eq!(merged.session_id, "new-session");
        assert_eq!(merged.context_tokens, 0, "context must reset on /clear");
        assert_eq!(merged.own_input_tokens, 0);
        assert_eq!(merged.cost_usd, 0.0);
        assert!(merged.jsonl_path.is_none(), "must re-resolve the new transcript");
        assert_eq!(merged.jsonl_offset, 0);
        assert_eq!(merged.cpu_history, vec![3.0, 4.0], "same process — keep CPU history");
    }

    #[test]
    fn merge_preserves_accumulated_state_for_the_same_session() {
        let mut prev = ClaudeSession::from_raw(RawSession {
            pid: 100,
            session_id: "s".into(),
            cwd: "/tmp/p".into(),
            started_at: 1,
        });
        prev.context_tokens = 62_000;
        prev.jsonl_offset = 999;
        let mut new = ClaudeSession::from_raw(RawSession {
            pid: 100,
            session_id: "s".into(),
            cwd: "/tmp/p".into(),
            started_at: 5,
        });
        new.elapsed = std::time::Duration::from_secs(42);

        let merged = merge_discovered_session(prev, new);
        assert_eq!(merged.context_tokens, 62_000, "same session keeps its context");
        assert_eq!(merged.jsonl_offset, 999);
        assert_eq!(merged.started_at, 5, "ephemeral fields refresh");
        assert_eq!(merged.elapsed, std::time::Duration::from_secs(42));
    }

    #[test]
    fn smooth_burn_steadies_a_spiky_signal() {
        // BACKLOG "$/h is wild": a single high reading must not snap the display
        // to it — the EMA eases toward the sample, so one spike barely moves it.
        let after_spike = smooth_burn(2.0, 100.0);
        assert!(after_spike < 32.0, "one spike must not dominate, got {after_spike}");
        // Repeated equal samples converge toward the true rate.
        let mut r = 0.0;
        for _ in 0..40 {
            r = smooth_burn(r, 5.0);
        }
        assert!((r - 5.0).abs() < 0.1, "should converge to the sustained rate, got {r}");
    }

    #[test]
    fn smooth_burn_decays_toward_zero_when_idle() {
        // A finished agent (sample 0.0) eases down rather than holding its rate.
        let mut r = 50.0;
        for _ in 0..30 {
            r = smooth_burn(r, 0.0);
        }
        assert!(r < 0.01, "idle rate should decay to ~zero, got {r}");
    }

    fn make_test_app() -> App {
        let mut app = App::new();
        app.sessions = vec![
            make_session(
                11,
                "blocked-api",
                "sonnet-4.6",
                SessionStatus::NeedsInput,
                2.0,
                40.0,
                true,
            ),
            make_session(
                12,
                "hot-cost",
                "opus-4.6",
                SessionStatus::Processing,
                7.5,
                30.0,
                true,
            ),
            make_session(
                13,
                "high-context",
                "haiku",
                SessionStatus::WaitingInput,
                1.0,
                90.0,
                true,
            ),
            make_session(
                14,
                "unknown-metrics",
                "",
                SessionStatus::Unknown,
                0.0,
                0.0,
                false,
            ),
        ];
        // Deterministic roster regardless of where tests run: no scanned
        // projects, so the four sessions group under the synthetic "(other)"
        // header (grouped view is the default).
        app.projects = Vec::new();
        app.budget_usd = Some(5.0);
        app.context_warn_threshold = 75;
        app.conflict_pids.insert(13);
        app.normalize_selection();
        app
    }

    #[test]
    fn status_filter_returns_only_matching_sessions() {
        let mut app = make_test_app();
        app.status_filter = StatusFilter::NeedsInput;
        let visible: Vec<u32> = app.visible_sessions().iter().map(|s| s.pid).collect();
        assert_eq!(visible, vec![11]);
    }

    #[test]
    fn focus_filter_attention_matches_high_signal_sessions() {
        let mut app = make_test_app();
        app.focus_filter = FocusFilter::Attention;
        let visible: Vec<u32> = app.visible_sessions().iter().map(|s| s.pid).collect();
        assert_eq!(visible, vec![11, 12, 13, 14]);
    }

    #[test]
    fn search_query_matches_project_and_model() {
        let mut app = make_test_app();
        app.search_query = "sonnet".into();
        let visible: Vec<u32> = app.visible_sessions().iter().map(|s| s.pid).collect();
        assert_eq!(visible, vec![11]);

        app.search_query = "unknown-metrics".into();
        let visible: Vec<u32> = app.visible_sessions().iter().map(|s| s.pid).collect();
        assert_eq!(visible, vec![14]);
    }

    #[test]
    fn normalize_selection_clamps_to_filtered_roster_count() {
        let mut app = make_test_app();
        // Grouped roster after filtering to NeedsInput: [Header(other), Agent(11)].
        app.table_state.select(Some(3));
        app.status_filter = StatusFilter::NeedsInput;
        app.normalize_selection();
        // Clamps to the last roster row — the surviving agent, not its header.
        assert_eq!(app.table_state.selected(), Some(1));
        assert_eq!(app.selected_session().map(|s| s.pid), Some(11));
    }

    #[test]
    fn stage_title_uses_the_path_basename() {
        assert_eq!(stage_title_for("/mnt/c/Work/herdr"), "Claude \u{2014} herdr");
        assert_eq!(stage_title_for("/mnt/c/Work/herdr/"), "Claude \u{2014} herdr");
        assert_eq!(stage_title_for(""), "Claude \u{2014} agent");
    }

    #[test]
    fn reselect_by_key_follows_the_same_entity_across_a_resort() {
        // BACKLOG "selection follows the launched repo": capturing the selection
        // key and re-anchoring must land on the same project/agent regardless of
        // where it now sits, not on whatever drifted into the old index.
        let mut app = make_grouped_app();
        app.table_state.select(Some(1)); // the agent row
        let key = app.selection_key();
        assert_eq!(key, Some(RosterSelKey::Agent(11)));

        // Cursor knocked to a different row (as a re-sort would do)…
        app.table_state.select(Some(0));
        app.reselect_by_key(key.clone());
        // …and re-anchored back onto the same agent.
        assert_eq!(app.selection_key(), key);
        assert_eq!(app.selected_session().map(|s| s.pid), Some(11));
    }

    /// Build an app whose grouped roster is deterministic: one active project
    /// ("alpha", hosting agent 11), one idle project ("idle", no agents). Roster
    /// order is `[Header(alpha), Agent(11), Header(idle)]` (active projects sort
    /// before idle ones).
    fn make_grouped_app() -> App {
        let mut app = App::new();
        app.grouped_view = true;
        app.sessions = vec![make_session(
            11,
            "alpha",
            "sonnet-4.6",
            SessionStatus::Processing,
            2.0,
            40.0,
            true,
        )];
        app.projects = vec![
            Project {
                path: PathBuf::from("/tmp/alpha"),
                name: "alpha".into(),
                has_git: true,
            },
            Project {
                path: PathBuf::from("/tmp/idle"),
                name: "idle".into(),
                has_git: true,
            },
        ];
        app.table_state.select(None);
        app
    }

    #[test]
    fn roster_layout_places_header_then_its_agents() {
        let app = make_grouped_app();
        let (groups, rows) = app.roster_layout();
        assert_eq!(
            rows,
            vec![
                RosterRow::Header(0),
                RosterRow::Agent(0),
                RosterRow::Header(1),
            ]
        );
        // Active project sorts before the idle one; idle project still present.
        assert_eq!(groups[0].name, "alpha");
        assert_eq!(groups[0].session_count, 1);
        assert_eq!(groups[1].name, "idle");
        assert_eq!(groups[1].session_count, 0);
    }

    #[test]
    fn stage_top_rows_ignores_idle_zero_agent_projects() {
        // BACKLOG auto-height: an idle, agent-less project header must NOT reserve
        // pane height. make_grouped_app has one active project ("alpha", agent 11)
        // and one idle project ("idle"). Only alpha's header + its one agent count:
        // 1 header + 1 agent + 4 chrome = 6, raised to the 12-row floor (BACKLOG
        // "Roster bar too small"). Disable the fleet strip so this exercises
        // idle-project handling, not the +1 strip row.
        let mut app = make_grouped_app();
        app.show_fleet = false;
        assert_eq!(app.stage_top_rows(), 12);

        // Adding more idle projects must not grow the pane — only agent-bearing
        // projects do. Height stays at the 12-row floor.
        let mut padded = app;
        for i in 0..20 {
            padded.projects.push(Project {
                path: PathBuf::from(format!("/tmp/idle-{i}")),
                name: format!("idle-{i}"),
                has_git: true,
            });
        }
        assert_eq!(
            padded.stage_top_rows(),
            12,
            "idle repos must not inflate herdr's staged pane height"
        );
    }

    #[test]
    fn project_groups_sorts_by_actionability_over_cost() {
        // Phase 3b: a blocked agent (NeedsInput) outranks a busy one (Processing)
        // even when the busy project costs far more.
        let mut app = App::new();
        app.sessions = vec![
            make_session(11, "busy", "opus", SessionStatus::Processing, 9.0, 10.0, true),
            make_session(
                12,
                "blocked",
                "sonnet",
                SessionStatus::NeedsInput,
                1.0,
                10.0,
                true,
            ),
        ];
        app.projects = vec![
            Project {
                path: PathBuf::from("/tmp/busy"),
                name: "busy".into(),
                has_git: true,
            },
            Project {
                path: PathBuf::from("/tmp/blocked"),
                name: "blocked".into(),
                has_git: true,
            },
        ];
        let groups = app.project_groups();
        assert_eq!(groups[0].name, "blocked", "NeedsInput should float to top");
        assert_eq!(groups[1].name, "busy");
    }

    #[test]
    fn project_groups_attaches_cached_git_status() {
        // Phase 3b: project_groups() copies git status from the cache.
        let mut app = App::new();
        let path = PathBuf::from("/tmp/repo");
        app.projects = vec![Project {
            path: path.clone(),
            name: "repo".into(),
            has_git: true,
        }];
        app.git_cache.insert(
            path,
            Some(GitStatus {
                branch: "main".into(),
                dirty: true,
                ahead: 2,
                behind: 0,
                upstream: true,
                bare: false,
            }),
        );
        let groups = app.project_groups();
        let repo = groups.iter().find(|g| g.name == "repo").unwrap();
        let git = repo
            .git
            .as_ref()
            .expect("git status should propagate from cache");
        assert_eq!(git.branch, "main");
        assert!(git.dirty);
        assert_eq!(git.ahead, 2);
    }

    #[test]
    fn git_status_is_fetched_off_thread_not_inline() {
        // Regression guard (the ~10s /mnt/c freeze): refresh_git_cache must
        // ENQUEUE stale projects for the background worker, never compute
        // `git status` inline on the render thread. So immediately after a
        // refresh, nothing is in the cache yet — the paths are in flight.
        let mut app = App::new();
        app.sessions = Vec::new();
        app.git_cache.clear();
        app.git_inflight.clear();
        app.projects = vec![
            Project {
                path: PathBuf::from("/nonexistent/repo-a"),
                name: "a".into(),
                has_git: true,
            },
            Project {
                path: PathBuf::from("/nonexistent/repo-b"),
                name: "b".into(),
                has_git: true,
            },
        ];

        app.refresh_git_cache();

        assert!(
            app.git_cache.is_empty(),
            "status must not be computed inline — that blocks the render thread"
        );
        assert_eq!(
            app.git_inflight.len(),
            2,
            "both stale projects should be queued on the background worker"
        );
    }

    #[test]
    fn cached_git_project_is_not_re_fetched_on_refresh() {
        // The core "no periodic polling" guarantee: once a project's git status
        // is fetched, plain refreshes (ticks / fs events) never re-request it.
        let mut app = App::new();
        app.sessions = Vec::new();
        let path = PathBuf::from("/nonexistent/repo");
        app.projects = vec![Project {
            path: path.clone(),
            name: "r".into(),
            has_git: true,
        }];
        app.git_inflight.clear();
        app.git_cache.clear();
        app.git_cache.insert(path.clone(), Some(GitStatus::default()));

        app.refresh_git_cache();

        assert!(
            app.git_inflight.is_empty(),
            "a cached project must not be re-fetched on refresh (no periodic polling)"
        );
    }

    #[test]
    fn navigating_onto_a_row_re_fetches_its_git_status() {
        // "Passing over a row polls it" — even if already cached.
        let mut app = App::new();
        app.sessions = Vec::new();
        let path = PathBuf::from("/nonexistent/repo");
        app.projects = vec![Project {
            path: path.clone(),
            name: "r".into(),
            has_git: true,
        }];
        app.git_inflight.clear();
        app.git_cache.clear();
        app.git_cache.insert(path.clone(), Some(GitStatus::default()));
        app.table_state.select(None);

        app.next(); // lands on the project header

        assert!(
            app.git_inflight.contains(&path),
            "landing on a row should re-fetch its git status"
        );
    }

    #[test]
    fn agent_activity_re_fetches_its_project_git() {
        // A session whose JSONL advanced (mapped via cwd) re-fetches its project,
        // even when already cached — event-driven, no timer.
        let mut app = App::new();
        app.sessions = Vec::new();
        let path = PathBuf::from("/tmp/proj");
        app.projects = vec![Project {
            path: path.clone(),
            name: "proj".into(),
            has_git: true,
        }];
        app.git_inflight.clear();
        app.git_cache.clear();
        app.git_cache.insert(path.clone(), Some(GitStatus::default()));

        // An agent working in a subdir of the project had activity.
        app.enqueue_git_for_cwds(&["/tmp/proj/src".to_string()]);

        assert!(
            app.git_inflight.contains(&path),
            "agent activity in a project should re-fetch its git status"
        );
    }

    #[test]
    fn chat_types_immediately_and_esc_closes() {
        let mut app = App::new();
        app.show_chat = true;
        app.chat_pid = Some(1);
        // No `i` step — printable keys compose the prompt right away.
        for c in ['h', 'i'] {
            app.handle_chat_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.chat_input, "hi");
        app.handle_chat_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_chat, "Esc should close the chat");
    }

    #[test]
    fn chat_input_types_and_send_to_missing_agent_keeps_buffer() {
        let mut app = App::new();
        app.sessions = Vec::new();
        app.show_chat = true;
        app.input_supported = true; // exercise the missing-agent guard, not the tmux gate
        app.chat_pid = Some(999); // no such session
        for c in ['h', 'i'] {
            app.handle_chat_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.chat_input, "hi");
        app.handle_chat_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.status_msg.contains("no longer running"));
        assert_eq!(app.chat_input, "hi", "a failed send must not clear the draft");
    }

    #[test]
    fn send_chat_input_blocks_remote_sessions() {
        let mut app = App::new();
        let mut s = make_session(7, "remote-proj", "opus", SessionStatus::Processing, 0.0, 0.0, true);
        s.worker_origin = Some("worker-1".into());
        app.sessions = vec![s];
        app.input_supported = true; // exercise the remote guard, not the tmux gate
        app.chat_pid = Some(7);
        app.chat_input = "do a thing".into();
        app.send_chat_input();
        assert!(app.status_msg.contains("Remote session"));
        assert_eq!(app.chat_input, "do a thing", "remote send must not clear the draft");
    }

    #[test]
    fn send_chat_input_without_tmux_gives_a_clear_hint() {
        let mut app = App::new();
        app.input_supported = false; // e.g. plain WSL terminal, no tmux
        app.chat_pid = Some(1);
        app.chat_input = "hello".into();
        app.send_chat_input();
        assert!(app.status_msg.contains("tmux"));
        assert_eq!(app.chat_input, "hello", "blocked send must not clear the draft");
    }

    // ---- Phase 4c: approval inspector ----------------------------------------

    /// Build a flat (non-grouped) app with one selected agent, so
    /// `selected_session` resolves without project-header bookkeeping.
    fn app_with_selected(session: ClaudeSession) -> App {
        let mut app = App::new();
        app.grouped_view = false;
        app.sessions = vec![session];
        app.table_state.select(Some(0));
        app
    }

    #[test]
    fn approval_inspector_requires_a_selected_agent() {
        let mut app = App::new();
        app.sessions = Vec::new();
        app.table_state.select(None);
        app.open_approval_inspector();
        assert!(app.approval_pid.is_none());
        assert!(app.status_msg.contains("Select an agent"));
    }

    #[test]
    fn approval_inspector_blocks_remote_sessions() {
        let mut s = make_session(7, "remote", "opus", SessionStatus::NeedsInput, 0.0, 0.0, true);
        s.worker_origin = Some("worker-1".into());
        let mut app = app_with_selected(s);
        app.input_supported = true; // exercise the remote guard, not the tmux gate
        app.open_approval_inspector();
        assert!(app.approval_pid.is_none(), "remote session must not open the modal");
        assert!(app.status_msg.contains("Remote session"));
    }

    #[test]
    fn approval_inspector_needs_tmux() {
        let s = make_session(3, "proj", "opus", SessionStatus::NeedsInput, 0.0, 0.0, true);
        let mut app = app_with_selected(s);
        app.input_supported = false; // e.g. plain WSL terminal, no tmux
        app.open_approval_inspector();
        assert!(app.approval_pid.is_none());
        assert!(app.status_msg.contains("tmux"));
    }

    #[test]
    fn approval_inspector_opens_for_selected_agent() {
        let s = make_session(5, "proj", "opus", SessionStatus::NeedsInput, 0.0, 0.0, true);
        let mut app = app_with_selected(s);
        app.input_supported = true; // bypass the tmux gate
        app.open_approval_inspector();
        // Opens pinned to the agent. The preview is best-effort (empty when the
        // capture fails), so we only assert the modal is now open.
        assert_eq!(app.approval_pid, Some(5));
    }

    #[test]
    fn approval_act_on_missing_agent_keeps_modal_open() {
        let mut app = App::new();
        app.sessions = Vec::new();
        app.approval_pid = Some(999); // no such session
        app.approval_act(ApprovalAct::Approve);
        assert!(app.status_msg.contains("Approve failed"));
        assert!(app.status_msg.contains("no longer running"));
        assert_eq!(app.approval_pid, Some(999), "a failed act keeps the modal open to retry");
    }

    #[test]
    fn approval_act_blocks_remote_sessions() {
        let mut s = make_session(7, "remote", "opus", SessionStatus::NeedsInput, 0.0, 0.0, true);
        s.worker_origin = Some("worker-1".into());
        let mut app = App::new();
        app.sessions = vec![s];
        app.approval_pid = Some(7);
        app.approval_act(ApprovalAct::Deny);
        assert!(app.status_msg.contains("Remote session"));
        assert_eq!(app.approval_pid, Some(7));
    }

    #[test]
    fn approval_modal_esc_closes_without_acting() {
        let mut app = App::new();
        app.approval_pid = Some(1);
        app.approval_preview = "Do you want to proceed?".into();
        app.handle_approval_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.approval_pid.is_none());
        assert!(app.approval_preview.is_empty());
        assert!(app.status_msg.contains("Cancelled"));
    }

    #[test]
    fn approval_act_labels_read_naturally() {
        assert_eq!(ApprovalAct::Approve.verb(), "Approve");
        assert_eq!(ApprovalAct::Approve.past_tense(), "Approved");
        assert_eq!(ApprovalAct::Deny.past_tense(), "Denied");
        assert_eq!(ApprovalAct::Interrupt.past_tense(), "Interrupted");
    }

    // ---- Phase 5: fleet trend strip --------------------------------------------

    #[test]
    fn fleet_counts_rolls_status_and_burn() {
        let mut app = App::new();
        let mut a = make_session(1, "a", "opus", SessionStatus::NeedsInput, 0.0, 0.0, true);
        a.burn_rate_per_hr = 1.5;
        let mut b = make_session(2, "b", "opus", SessionStatus::Processing, 0.0, 0.0, true);
        b.burn_rate_per_hr = 2.0;
        let c = make_session(3, "c", "opus", SessionStatus::WaitingInput, 0.0, 0.0, true);
        let d = make_session(4, "d", "opus", SessionStatus::Idle, 0.0, 0.0, true);
        app.sessions = vec![a, b, c, d];

        let f = app.fleet_counts();
        assert_eq!(f.total, 4);
        assert_eq!(f.needs_input, 1);
        assert_eq!(f.processing, 1);
        assert_eq!(f.waiting, 1);
        assert_eq!(f.idle, 1);
        assert!((f.burn_per_hr - 3.5).abs() < 1e-9);
    }

    #[test]
    fn fleet_counts_is_empty_for_no_agents() {
        let mut app = App::new();
        app.sessions = Vec::new(); // App::new() may discover live sessions on the dev box
        assert_eq!(app.fleet_counts(), FleetCounts::default());
    }

    #[test]
    fn toggling_fleet_strip_flips_the_flag() {
        let mut app = App::new();
        assert!(app.show_fleet, "fleet strip defaults on");
        app.handle_normal_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert!(!app.show_fleet);
        app.handle_normal_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert!(app.show_fleet);
    }

    #[test]
    fn git_op_kind_maps_to_subcommand_and_verb() {
        assert_eq!(GitOpKind::Push.arg(), "push");
        assert_eq!(GitOpKind::Pull.arg(), "pull");
        assert_eq!(GitOpKind::Push.verb(), "push");
        assert_eq!(GitOpKind::Pull.verb(), "pull");
    }

    #[test]
    fn project_label_uses_final_path_component() {
        assert_eq!(project_label(Path::new("/tmp/foo/bar")), "bar");
        assert_eq!(project_label(Path::new("/tmp/foo bar")), "foo bar");
    }

    #[test]
    fn start_git_op_with_no_selection_is_a_noop() {
        // Phase 3c: nothing selected → no child spawned, just a status message.
        let mut app = App::new();
        app.projects = Vec::new();
        app.sessions = Vec::new();
        app.table_state.select(None);
        app.start_git_op(GitOpKind::Push);
        assert!(!app.git_op_active(), "no op should be spawned without a target");
        assert!(app.status_msg.contains("No project"));
    }

    /// End-to-end (hermetic, no network): pressing push pushes a new commit to a
    /// local bare remote. Proves the spawn → try_wait reap path. Skipped if `git`
    /// is unavailable.
    #[test]
    fn push_propagates_a_commit_to_a_local_remote() {
        use std::path::Path;
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let work = tmp.path().join("work");
        let git = |dir: &Path, args: &[&str]| {
            Command::new("git").arg("-C").arg(dir).args(args).output().unwrap()
        };

        Command::new("git")
            .args(["init", "--bare"])
            .arg(&origin)
            .output()
            .unwrap();
        Command::new("git")
            .arg("clone")
            .arg(&origin)
            .arg(&work)
            .output()
            .unwrap();
        git(&work, &["config", "user.email", "t@example.com"]);
        git(&work, &["config", "user.name", "t"]);

        // First commit + establish upstream (manual, outside our code path).
        std::fs::write(work.join("a.txt"), "1").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "first"]);
        git(&work, &["push", "-u", "origin", "HEAD"]);

        // A second commit our push must deliver.
        std::fs::write(work.join("b.txt"), "2").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "second"]);

        let mut app = App::new();
        // App::new() discovers real sessions; clear them so the only roster row
        // is our "work" project (otherwise an "(other)" bucket sorts above it).
        app.sessions = Vec::new();
        app.projects = vec![Project {
            path: work.clone(),
            name: "work".into(),
            has_git: true,
        }];
        app.table_state.select(Some(0)); // the "work" project header
        assert_eq!(app.selected_launch_cwd(), Some(work.clone()));

        app.start_git_op(GitOpKind::Push);
        assert!(app.git_op_active(), "push should be in flight");

        // Reap with a bounded wait (try_wait is non-blocking).
        for _ in 0..200 {
            app.poll_git_ops();
            if !app.git_op_active() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(!app.git_op_active(), "push should finish within the timeout");

        // The local branch should no longer be ahead of its upstream.
        let ahead = git(&work, &["rev-list", "--count", "@{u}..HEAD"]);
        let ahead = String::from_utf8_lossy(&ahead.stdout).trim().to_string();
        assert_eq!(ahead, "0", "second commit should have been pushed to origin");
    }

    #[test]
    fn selected_session_is_none_on_a_header_and_some_on_an_agent() {
        let mut app = make_grouped_app();
        app.table_state.select(Some(0)); // alpha header
        assert!(app.selected_session().is_none());
        app.table_state.select(Some(1)); // agent 11
        assert_eq!(app.selected_session().map(|s| s.pid), Some(11));
        app.table_state.select(Some(2)); // idle header
        assert!(app.selected_session().is_none());
    }

    #[test]
    fn selected_launch_cwd_targets_project_for_headers_and_agents() {
        let mut app = make_grouped_app();
        // Header of the active project → that project's path.
        app.table_state.select(Some(0));
        assert_eq!(
            app.selected_launch_cwd(),
            Some(PathBuf::from("/tmp/alpha"))
        );
        // The agent row → its owning project's path, not the bare cwd.
        app.table_state.select(Some(1));
        assert_eq!(
            app.selected_launch_cwd(),
            Some(PathBuf::from("/tmp/alpha"))
        );
        // Idle, zero-agent project header → launchable (the Phase 2 headline).
        app.table_state.select(Some(2));
        assert_eq!(app.selected_launch_cwd(), Some(PathBuf::from("/tmp/idle")));
    }

    #[test]
    fn navigation_reaches_the_idle_project_header() {
        let mut app = make_grouped_app();
        app.table_state.select(Some(0));
        app.next(); // → agent 11
        app.next(); // → idle header
        assert_eq!(app.table_state.selected(), Some(2));
        assert!(app.selected_session().is_none());
        assert_eq!(app.selected_launch_cwd(), Some(PathBuf::from("/tmp/idle")));
    }

    #[test]
    fn enter_launch_mode_prefills_cwd_from_selected_project() {
        let mut app = make_grouped_app();
        app.table_state.select(Some(2)); // idle project
        app.enter_launch_mode();
        assert!(app.launch_mode);
        assert_eq!(app.launch_form.cwd, "/tmp/idle");
    }

    #[test]
    fn launch_wizard_defaults_to_dot_with_no_selection() {
        let mut app = App::new();
        app.table_state.select(None); // nothing selected → CLI default
        app.enter_launch_mode();

        assert!(app.launch_mode);
        assert_eq!(app.launch_form.field, LaunchField::Cwd);
        assert_eq!(app.launch_form.cwd, ".");
        assert!(app.launch_form.prompt.is_empty());
        assert!(app.launch_form.resume.is_empty());
    }

    #[test]
    fn launch_wizard_moves_between_fields() {
        let mut app = App::new();
        app.enter_launch_mode();

        app.handle_launch_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.launch_form.field, LaunchField::Prompt);

        app.handle_launch_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.launch_form.field, LaunchField::Resume);

        app.handle_launch_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.launch_form.field, LaunchField::Prompt);
    }

    #[test]
    fn invalid_launch_keeps_wizard_open_and_reports_error() {
        let mut app = App::new();
        app.enter_launch_mode();
        app.launch_form.cwd = "/tmp/claudectl-this-path-should-not-exist".into();
        app.launch_form.field = LaunchField::Resume;

        app.submit_launch_form();

        assert!(app.launch_mode);
        assert_eq!(app.launch_form.field, LaunchField::Cwd);
        assert!(
            app.status_msg
                .starts_with("Launch failed: Directory not found:")
        );
    }
}
