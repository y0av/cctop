//! Synthetic data for `--demo` / `--snapshot` so the tool can be shown off (and
//! the README screenshot generated) without touching the real account.

use chrono::{Duration, Utc};

use crate::account::Account;
use crate::model::{UsageSource, UsageWindows, Window};
use crate::sessions::LiveAgent;
use crate::transcripts::Aggregates;

pub fn account() -> Account {
    Account {
        display_name: "ada".into(),
        email: "ada@example.com".into(),
        org: "Acme Robotics".into(),
        subscription: "max".into(),
        rate_limit_tier: "default_claude_max_20x".into(),
    }
}

pub fn usage() -> UsageWindows {
    let now = Utc::now();
    let mk = |util: f64, mins: i64| {
        Some(Window { utilization: Some(util), tokens: None, resets_at: Some(now + Duration::minutes(mins)) })
    };
    UsageWindows {
        five_hour: mk(71.0, 102),
        seven_day: mk(38.0, 4 * 1440 + 305),
        seven_day_opus: mk(52.0, 4 * 1440 + 305),
        seven_day_sonnet: None,
        source: UsageSource::Live,
        note: None,
    }
}

pub fn agents(tick: u64) -> Vec<LiveAgent> {
    let roll = |base: &[u64], t: u64| -> Vec<u64> {
        let n = base.len();
        (0..n).map(|i| base[(i + t as usize) % n]).collect()
    };
    let a = |pid: i32, project: &str, model: &str, status: &str, up: i64, rss_mb: u64, tps: f64, hist: Vec<u64>| LiveAgent {
        pid,
        session_id: String::new(),
        project: project.into(),
        model: model.into(),
        status: status.into(),
        uptime_secs: up,
        rss_kb: rss_mb * 1024,
        burn_tps: tps,
        burn_hist: hist,
        kind: "interactive".into(),
    };
    vec![
        a(48213, "api-gateway", "claude-opus-4-8", "busy", 264, 345,
            78.0 + (tick % 7) as f64, roll(&[40, 58, 72, 61, 84, 76, 91, 80], tick)),
        a(47190, "web-frontend", "claude-sonnet-4-6", "busy", 743, 410,
            52.0 + (tick % 5) as f64, roll(&[30, 42, 38, 55, 48, 60, 52, 46], tick + 3)),
        a(46055, "data-pipeline", "claude-opus-4-8", "idle", 4080, 467, 0.0, vec![1, 1, 2, 1, 1, 1, 2, 1]),
        a(44820, "infra-terraform", "claude-haiku-4-5", "idle", 13260, 388, 0.0, vec![1; 8]),
        a(43771, "docs-site", "claude-opus-4-8", "shell", 22320, 502, 0.0, vec![1; 8]),
    ]
}

pub fn aggregates() -> Aggregates {
    let buckets24 = (0..24)
        .map(|i| {
            let t = i as f64;
            let v = (t * 0.45).sin() * 0.4 + 0.55 + (t * 0.13).cos() * 0.15;
            (v.max(0.05) * 260_000.0) as u64
        })
        .collect();
    Aggregates {
        by_model: vec![
            ("claude-opus-4-8".into(), 37_400_000, 268.40),
            ("claude-sonnet-4-6".into(), 8_600_000, 38.10),
            ("claude-haiku-4-5".into(), 2_000_000, 5.90),
        ],
        by_project: vec![
            ("web-frontend".into(), 19_600_000, 128.0),
            ("data-pipeline".into(), 12_900_000, 84.0),
            ("api-gateway".into(), 8_600_000, 56.0),
            ("docs-site".into(), 4_300_000, 28.4),
            ("infra-terraform".into(), 2_600_000, 16.0),
        ],
        main_tok: 30_700_000,
        agent_tok: 17_300_000,
        today_tok: 3_400_000,
        today_cost: 42.18,
        buckets24,
        last5h_tok: 0,
        last7d_tok: 0,
        grand_tok: 48_000_000,
        grand_cost: 312.40,
    }
}
