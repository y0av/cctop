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
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    Live,
    Estimate,
}

/// The full set of plan gauges shown in the header block.
#[derive(Clone)]
pub struct UsageWindows {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub seven_day_opus: Option<Window>,
    pub seven_day_sonnet: Option<Window>,
    pub source: UsageSource,
    /// e.g. the network error that forced the estimate fallback.
    pub note: Option<String>,
}
