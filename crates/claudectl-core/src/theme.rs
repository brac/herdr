use ratatui::style::Color;

// Dracula palette (https://draculatheme.com). The Dark theme is built from these
// so colors stay coherent — and so the selected row uses a muted "current line"
// fill instead of a glaring full-brightness reverse-video bar (BACKLOG: "the
// highlight row color is too bright and hard to read").
const DRAC_CURRENT_LINE: Color = Color::Rgb(68, 71, 90); // #44475a — selection fill
const DRAC_FG: Color = Color::Rgb(248, 248, 242); // #f8f8f2 — foreground
const DRAC_COMMENT: Color = Color::Rgb(98, 114, 164); // #6272a4 — muted / borders
const DRAC_CYAN: Color = Color::Rgb(139, 233, 253); // #8be9fd
const DRAC_GREEN: Color = Color::Rgb(80, 250, 123); // #50fa7b
const DRAC_ORANGE: Color = Color::Rgb(255, 184, 108); // #ffb86c
const DRAC_PINK: Color = Color::Rgb(255, 121, 198); // #ff79c6
const DRAC_PURPLE: Color = Color::Rgb(189, 147, 249); // #bd93f9
const DRAC_RED: Color = Color::Rgb(255, 85, 85); // #ff5555
const DRAC_YELLOW: Color = Color::Rgb(241, 250, 140); // #f1fa8c

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
    None,
}

impl ThemeMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// Detect theme mode: CLI flag > config > NO_COLOR env > default (dark).
    pub fn detect(cli_theme: Option<&str>) -> Self {
        if let Some(t) = cli_theme.and_then(Self::parse) {
            return t;
        }
        if std::env::var_os("NO_COLOR").is_some() {
            return Self::None;
        }
        Self::Dark
    }
}

/// Semantic color palette for the TUI.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    #[allow(dead_code)]
    pub mode: ThemeMode,

    // Status colors
    pub status_needs_input: Color,
    pub status_error: Color,
    pub status_processing: Color,
    pub status_job_done: Color,
    pub status_waiting: Color,
    pub status_unknown: Color,
    pub status_idle: Color,
    pub status_finished: Color,

    // UI chrome
    pub border: Color,
    pub header: Color,
    pub footer: Color,
    pub highlight_key: Color,
    pub text_primary: Color,
    pub text_muted: Color,
    /// Selected-row fill + text. Replaces reverse-video so the highlight is a
    /// calm band rather than a full-brightness inversion (`None` mode leaves
    /// these `Reset` and the table falls back to reverse-video).
    pub selection_bg: Color,
    pub selection_fg: Color,

    // Data colors
    pub cost: Color,
    pub cost_warning: Color,
    pub cost_danger: Color,
    pub context_ok: Color,
    pub context_warning: Color,
    pub context_danger: Color,
    pub burn_rate_low: Color,
    pub burn_rate_mid: Color,
    pub burn_rate_high: Color,
    pub sparkline: Color,
    pub input_accent: Color,
    pub success: Color,
    pub error: Color,
}

impl Theme {
    pub fn from_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
            ThemeMode::None => Self::none(),
        }
    }

    /// Dracula-flavored dark theme (BACKLOG: "default color should resemble
    /// Dracula Dark"). Built from the `DRAC_*` palette so accents stay coherent.
    fn dark() -> Self {
        Self {
            mode: ThemeMode::Dark,
            status_needs_input: DRAC_PURPLE, // the "purple Need Input" notice
            status_error: DRAC_RED,
            status_processing: DRAC_GREEN,
            status_job_done: DRAC_CYAN, // task complete, awaiting you — distinct from Waiting
            status_waiting: DRAC_YELLOW,
            status_unknown: DRAC_COMMENT,
            status_idle: DRAC_COMMENT,
            status_finished: DRAC_RED,
            border: DRAC_CURRENT_LINE,
            header: DRAC_CYAN,
            footer: DRAC_COMMENT,
            highlight_key: DRAC_ORANGE,
            text_primary: DRAC_FG,
            text_muted: DRAC_COMMENT,
            cost: DRAC_YELLOW,
            cost_warning: DRAC_ORANGE,
            cost_danger: DRAC_RED,
            context_ok: DRAC_GREEN,
            context_warning: DRAC_YELLOW,
            context_danger: DRAC_RED,
            burn_rate_low: DRAC_COMMENT,
            burn_rate_mid: DRAC_ORANGE,
            burn_rate_high: DRAC_RED,
            sparkline: DRAC_CYAN,
            input_accent: DRAC_PINK,
            success: DRAC_GREEN,
            error: DRAC_RED,
            selection_bg: DRAC_CURRENT_LINE,
            selection_fg: DRAC_FG,
        }
    }

    fn light() -> Self {
        Self {
            mode: ThemeMode::Light,
            status_needs_input: Color::Magenta,
            status_error: Color::Red,
            status_processing: Color::Blue,
            status_job_done: Color::Cyan,
            status_waiting: Color::Rgb(180, 140, 0), // Dark yellow
            status_unknown: Color::Gray,
            status_idle: Color::Gray,
            status_finished: Color::Red,
            border: Color::Gray,
            header: Color::Blue,
            footer: Color::Gray,
            highlight_key: Color::Blue,
            text_primary: Color::Black,
            text_muted: Color::Gray,
            cost: Color::Rgb(180, 140, 0),
            cost_warning: Color::Red,
            cost_danger: Color::LightRed,
            context_ok: Color::Blue,
            context_warning: Color::Rgb(180, 140, 0),
            context_danger: Color::Red,
            burn_rate_low: Color::Gray,
            burn_rate_mid: Color::Rgb(180, 140, 0),
            burn_rate_high: Color::Red,
            sparkline: Color::Blue,
            input_accent: Color::Blue,
            success: Color::Blue,
            error: Color::Red,
            selection_bg: Color::Rgb(208, 208, 216), // soft gray band
            selection_fg: Color::Black,
        }
    }

    fn none() -> Self {
        // Monochrome — no color, use terminal defaults
        Self {
            mode: ThemeMode::None,
            status_needs_input: Color::Reset,
            status_error: Color::Reset,
            status_processing: Color::Reset,
            status_job_done: Color::Reset,
            status_waiting: Color::Reset,
            status_unknown: Color::Reset,
            status_idle: Color::Reset,
            status_finished: Color::Reset,
            border: Color::Reset,
            header: Color::Reset,
            footer: Color::Reset,
            highlight_key: Color::Reset,
            text_primary: Color::Reset,
            text_muted: Color::Reset,
            cost: Color::Reset,
            cost_warning: Color::Reset,
            cost_danger: Color::Reset,
            context_ok: Color::Reset,
            context_warning: Color::Reset,
            context_danger: Color::Reset,
            burn_rate_low: Color::Reset,
            burn_rate_mid: Color::Reset,
            burn_rate_high: Color::Reset,
            sparkline: Color::Reset,
            input_accent: Color::Reset,
            success: Color::Reset,
            error: Color::Reset,
            selection_bg: Color::Reset,
            selection_fg: Color::Reset,
        }
    }

    /// Get the color for a session status.
    pub fn status_color(&self, status: &crate::session::SessionStatus) -> Color {
        use crate::session::SessionStatus;
        match status {
            SessionStatus::NeedsInput => self.status_needs_input,
            SessionStatus::Error => self.status_error,
            SessionStatus::Processing => self.status_processing,
            SessionStatus::JobDone => self.status_job_done,
            SessionStatus::WaitingInput => self.status_waiting,
            SessionStatus::Unknown => self.status_unknown,
            SessionStatus::Idle => self.status_idle,
            SessionStatus::Finished => self.status_finished,
        }
    }
}
