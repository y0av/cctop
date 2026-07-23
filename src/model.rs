//! Shared data types used across the data layer and the UI.

use chrono::{DateTime, Utc};

/// Token counts broken out by billing category. All counts are raw tokens.
#[derive(Clone, Copy, Default, Debug)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

impl Tokens {
    /// Sum across every category — the headline "tokens moved" number.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write_5m + self.cache_write_1h
    }

    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
    }
}

/// One rate-limit window (the claude.ai/settings/usage gauges).
///
/// `utilization` is a percentage (0..=100+) when known. In the local-estimate
/// fallback we can't know the plan's true allowance, so `tokens` carries the
/// raw trailing-window token count and `utilization` is a heuristic bar only.
#[derive(Clone)]
pub struct Window {
    pub utilization: Option<f64>,
    pub tokens: Option<u64>,
    pub resets_at: Option<DateTime<Utc>>,
    /// Seconds until this window hits 100% at the current climb rate,
    /// derived from live utilization samples. Only set when that moment
    /// lands *before* the reset — i.e. when it's an actionable warning.
    pub eta_secs: Option<i64>,
}

/// A model-scoped weekly window (e.g. the Fable or Opus weekly cap), labeled
/// with the short model name the API reports.
#[derive(Clone)]
pub struct ScopedWindow {
    pub label: String,
    pub win: Window,
}

/// Extra-usage spend (overage credits), shown only when the account has it
/// enabled. Dollar amounts; `limit`/`percent` may be unknown.
#[derive(Clone)]
pub struct Spend {
    pub used: f64,
    pub limit: Option<f64>,
    pub percent: Option<f64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    Live,
    Estimate,
}

/// One account's plan gauges, ready for display. `label` is empty in
/// single-account mode — the common case, where the panel title alone carries
/// the state and the layout stays identical to a one-account cctop.
#[derive(Clone)]
pub struct PlanView {
    pub label: String,
    pub usage: UsageWindows,
}

/// Everything the drill-down panel shows about the selected agent.
pub struct AgentDetail {
    pub project: String,
    pub account: String,
    pub model: String,
    pub session_id: String,
    pub cwd: String,
    pub status: String,
    pub uptime_secs: i64,
    /// Seconds since the session's last assistant turn, when known.
    pub idle_secs: Option<i64>,
    pub tok: Tokens,
    pub cost: f64,
    pub burn_tps: f64,
    pub burn_hist: Vec<u64>,
}

/// The full set of plan gauges shown in the header block.
#[derive(Clone)]
pub struct UsageWindows {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    /// Model-scoped weeklies (Fable / Opus / Sonnet / whatever the API sends).
    pub scoped: Vec<ScopedWindow>,
    pub spend: Option<Spend>,
    pub source: UsageSource,
    /// e.g. the network error that forced the estimate fallback.
    pub note: Option<String>,
}
