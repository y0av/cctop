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

/// One tool call from a transcript, split for display: `Bash` + `cargo test`.
#[derive(Clone)]
pub struct ToolCall {
    pub tool: String,
    pub arg: String,
    pub ts: i64,
    /// Still waiting for its result: the agent is executing it right now.
    pub pending: bool,
}

/// A subagent a session spawned, for the drill-down tree.
#[derive(Clone)]
pub struct SubAgentView {
    /// Agent type (`Explore`, `general-purpose`…) and the task description.
    pub kind: String,
    pub desc: String,
    pub tokens: u64,
    pub running: bool,
}

/// Token throughput across every session, bucketed for the live graph.
/// Rates are tokens per second, oldest bucket first.
#[derive(Clone, Default)]
pub struct Series {
    /// Generated tokens (output).
    pub out: Vec<f64>,
    /// Context the model read: input plus cache reads and writes.
    pub inp: Vec<f64>,
    pub window_secs: i64,
    /// Trailing-minute rates, for the panel titles.
    pub out_now: f64,
    pub inp_now: f64,
    /// Totals over the whole window.
    pub tok: u64,
    pub cost: f64,
}

/// Working-tree state of the selected agent's repo.
#[derive(Clone, Default)]
pub struct GitStatus {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    /// Tracked files with changes, and untracked files.
    pub changed: usize,
    pub untracked: usize,
}

/// Everything the drill-down panel shows about the selected agent.
pub struct AgentDetail {
    pub pid: i32,
    /// Session name; empty falls back to the project in the panel title.
    pub name: String,
    /// Cwd-derived session handle and transcript title, when they differ
    /// from `name`.
    pub handle: String,
    pub title: String,
    pub repo: String,
    /// Process-tree resource use (see `procs`).
    pub cpu: f32,
    pub rss_kb: u64,
    pub gpu: Option<f32>,
    pub gpu_mem_kb: u64,
    pub nprocs: usize,
    pub detached: usize,
    pub top: Vec<crate::procs::ProcLine>,
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
    /// Context the last turn ran with, against the model's window.
    pub ctx_tokens: u64,
    pub ctx_window: u64,
    pub compactions: u32,
    /// The pending tool call, else the latest one.
    pub action: Option<ToolCall>,
    pub subagents: Vec<SubAgentView>,
    /// TCP ports the process tree listens on.
    pub ports: Vec<u16>,
    pub git: Option<GitStatus>,
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
