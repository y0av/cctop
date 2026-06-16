//! Per-model pricing and cost computation.
//!
//! Anthropic's local caches (`additionalModelCostsCache` in ~/.claude.json) are
//! empty on this machine, so the rates below are the authoritative table,
//! sourced from the `claude-api` skill (USD per 1M tokens):
//!   Opus 4.x  $5 in / $25 out   Sonnet 4.x $3 / $15
//!   Haiku 4.5 $1 / $5           Fable/Mythos 5 $10 / $50
//! Cache: read = 0.1x input, 5m-write = 1.25x input, 1h-write = 2.0x input.
//! Opus 4.8's 1M context is standard-priced — the `[1m]` variant is identical,
//! so we strip any `[...]` suffix before lookup.

use crate::model::Tokens;

/// Input/output price per 1M tokens, in USD.
struct Price {
    input: f64,
    output: f64,
}

fn price_for(model: &str) -> Price {
    // Normalize: drop a trailing "[1m]"-style tag and lowercase.
    let m = model.split('[').next().unwrap_or(model).to_ascii_lowercase();

    // Order matters: check the more specific families first.
    if m.contains("haiku") {
        Price { input: 1.0, output: 5.0 }
    } else if m.contains("sonnet") {
        Price { input: 3.0, output: 15.0 }
    } else if m.contains("fable") || m.contains("mythos") {
        Price { input: 10.0, output: 50.0 }
    } else if m.contains("opus-4-1") || m.contains("opus-4-0") || m.contains("opus-4-20") {
        // Pre-4.5 Opus billed at the old $15/$75 tier.
        Price { input: 15.0, output: 75.0 }
    } else if m.contains("opus") {
        // 4.5 / 4.6 / 4.7 / 4.8
        Price { input: 5.0, output: 25.0 }
    } else {
        // Unknown model — assume Opus-tier so we don't undercount.
        Price { input: 5.0, output: 25.0 }
    }
}

/// USD cost of a set of token counts for a given model.
pub fn cost(model: &str, t: &Tokens) -> f64 {
    let p = price_for(model);
    let cache_read = p.input * 0.10;
    let write_5m = p.input * 1.25;
    let write_1h = p.input * 2.0;
    (t.input as f64 * p.input
        + t.output as f64 * p.output
        + t.cache_read as f64 * cache_read
        + t.cache_write_5m as f64 * write_5m
        + t.cache_write_1h as f64 * write_1h)
        / 1_000_000.0
}
