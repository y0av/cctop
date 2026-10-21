//! Synthetic data for `--demo` / `--snapshot` so the tool can be shown off (and
//! the README screenshot generated) without touching the real account.

use chrono::{Duration, Utc};

use crate::account::Account;
use crate::procs::{GpuStats, ProcLine, SysStats};
use crate::model::{
    AgentDetail, GitStatus, PlanView, ScopedWindow, Series, Spend, SubAgentView, Tokens, ToolCall, UsageSource,
    UsageWindows, Window,
};
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
    let mk = |util: f64, mins: i64| Window {
        utilization: Some(util),
        tokens: None,
        resets_at: Some(now + Duration::minutes(mins)),
        eta_secs: None,
    };
    // The 5-hour window carries a cap-before-reset warning to show off the
    // burn-rate prediction.
    let mut five_hour = mk(71.0, 102);
    five_hour.eta_secs = Some(58 * 60);
    UsageWindows {
        five_hour: Some(five_hour),
        seven_day: Some(mk(38.0, 4 * 1440 + 305)),
        scoped: vec![
            ScopedWindow { label: "FABLE".into(), win: mk(62.0, 4 * 1440 + 305) },
            ScopedWindow { label: "OPUS".into(), win: mk(41.0, 4 * 1440 + 305) },
        ],
        spend: Some(Spend { used: 12.60, limit: Some(50.0), percent: Some(25.0) }),
        source: UsageSource::Live,
        note: None,
    }
}

/// Three synthetic accounts for the multi-account README shot: one healthy,
/// one nearly capped, one still waiting on its first fetch.
pub fn multi_plans() -> Vec<PlanView> {
    let mut capped = usage();
    if let Some(w) = capped.seven_day.as_mut() {
        w.utilization = Some(96.0);
    }
    if let Some(s) = capped.scoped.first_mut() {
        s.win.utilization = Some(100.0);
    }
    capped.spend = None;
    let pending = UsageWindows {
        five_hour: None,
        seven_day: None,
        scoped: Vec::new(),
        spend: None,
        source: UsageSource::Estimate,
        note: Some("awaiting live data…".into()),
    };
    vec![
        PlanView { label: "ada".into(), usage: usage() },
        PlanView { label: "grace".into(), usage: capped },
        PlanView { label: "lin".into(), usage: pending },
    ]
}

pub fn agents(tick: u64) -> Vec<LiveAgent> {
    let roll = |base: &[u64], t: u64| -> Vec<u64> {
        let n = base.len();
        (0..n).map(|i| base[(i + t as usize) % n]).collect()
    };
    // `name` carries a trailing "*" in the demo table to mark a chosen (rather
    // than cwd-derived) name, matching what `named` highlights for real runs.
    let a = |pid: i32, name: &str, project: &str, model: &str, status: &str, up: i64, rss_mb: u64, tps: f64, hist: Vec<u64>| LiveAgent {
        pid,
        session_id: String::new(),
        name: name.trim_end_matches('*').into(),
        named: name.ends_with('*'),
        title: name.trim_end_matches('*').into(),
        handle: format!("{project}-a4"),
        repo: if project == "web-frontend" { "web/checkout-wt".into() } else { project.into() },
        cpu: 0.0,
        gpu: Some(0.0),
        gpu_mem_kb: 0,
        nprocs: 3,
        detached: 0,
        top: Vec::new(),
        project: project.into(),
        account: "ada".into(),
        cwd: format!("/home/ada/dev/{project}"),
        model: model.into(),
        status: status.into(),
        uptime_secs: up,
        rss_kb: rss_mb * 1024,
        burn_tps: tps,
        burn_hist: hist,
        ctx_tokens: 0,
        ctx_window: 1_000_000,
        compactions: 0,
        action: None,
        subagents_running: 0,
        ports: Vec::new(),
        pids: Vec::new(),
        kind: "interactive".into(),
    };
    let mut v = vec![
        a(48213, "rate-limit-fix*", "api-gateway", "claude-fable-5", "busy", 264, 345,
            78.0 + (tick % 7) as f64, roll(&[40, 58, 72, 61, 84, 76, 91, 80], tick)),
        a(47190, "checkout-redesign*", "web-frontend", "claude-sonnet-4-6", "busy", 743, 410,
            52.0 + (tick % 5) as f64, roll(&[30, 42, 38, 55, 48, 60, 52, 46], tick + 3)),
        a(46055, "data-pipeline-7b", "data-pipeline", "claude-opus-4-8", "idle", 4080, 467, 0.0, vec![1, 1, 2, 1, 1, 1, 2, 1]),
        a(44820, "infra-terraform-2c", "infra-terraform", "claude-haiku-4-5", "idle", 13260, 388, 0.0, vec![1; 8]),
        a(43771, "docs-site-e4", "docs-site", "claude-opus-4-8", "shell", 22320, 502, 0.0, vec![1; 8]),
    ];
    // Resource load: the redesign agent is running a test suite and a
    // headless browser; one idle agent left a dev server spinning.
    let load = [(118.0, 96, 6, 2u64), (372.0, 2140, 14, 31), (8.0, 610, 4, 0), (1.0, 402, 2, 0), (64.0, 890, 5, 0)];
    for (ag, (cpu, mem_mb, n, gpu)) in v.iter_mut().zip(load) {
        ag.cpu = cpu + (tick % 9) as f32;
        ag.rss_kb = mem_mb * 1024;
        ag.nprocs = n;
        ag.gpu = Some(gpu as f32);
        ag.top = demo_top(ag.pid, ag.cpu, ag.rss_kb, &ag.project);
    }
    // Context, the call in flight, subagents and ports: the redesign agent
    // is close to an auto-compact and its dev server holds two ports.
    let now = Utc::now().timestamp();
    let call = |tool: &str, arg: &str, ago: i64, pending: bool| {
        Some(ToolCall { tool: tool.into(), arg: arg.into(), ts: now - ago, pending })
    };
    let set = |ag: &mut LiveAgent, ctx: u64, win: u64, compactions: u32, action, subs: usize, ports: &[u16]| {
        ag.ctx_tokens = ctx;
        ag.ctx_window = win;
        ag.compactions = compactions;
        ag.action = action;
        ag.subagents_running = subs;
        ag.ports = ports.to_vec();
    };
    set(&mut v[0], 412_000 + tick * 900, 1_000_000, 0, call("Bash", "cargo test -p gateway --release", 14, true), 2, &[]);
    set(&mut v[1], 931_000, 1_000_000, 1, call("Edit", "CheckoutForm.tsx", 3, true), 0, &[5173, 9222]);
    set(&mut v[2], 88_000, 1_000_000, 0, call("Read", "etl/schedule.py", 3480, false), 0, &[]);
    set(&mut v[3], 61_000, 200_000, 0, call("Bash", "terraform plan -out=tf.plan", 12360, false), 0, &[]);
    set(&mut v[4], 140_000, 1_000_000, 2, call("Bash", "npm run dev", 21920, false), 0, &[3000]);
    v
}

/// A synthetic token stream for the live graph: a few overlapping waves
/// with turn-sized bursts, scrolling one sample per tick.
pub fn series(tick: u64, points: usize, window: i64) -> Series {
    let n = points.max(2);
    let wave = |i: usize, phase: f64| -> f64 {
        let t = (i as u64 + tick) as f64;
        let base = ((t * 0.07 + phase).sin() * 0.5 + 0.5) * ((t * 0.023 + phase).cos() * 0.3 + 0.7);
        // Deterministic "turn finished" bursts, smeared over a few points the
        // way the live series' trailing average smears them.
        let hit = |k: u64| (k.wrapping_mul(2_654_435_761) >> 13).is_multiple_of(11);
        let at = i as u64 + tick;
        let burst = (0..3u64)
            .filter(|&k| at >= k && hit(at - k))
            .map(|k| 0.45 * (1.0 - k as f64 / 3.0))
            .fold(0.0, f64::max);
        (base * 0.8 + burst).min(1.0)
    };
    let out: Vec<f64> = (0..n).map(|i| 30.0 + 160.0 * wave(i, 0.0)).collect();
    let inp: Vec<f64> = (0..n).map(|i| 6_000.0 + 58_000.0 * wave(i, 0.9)).collect();
    let span = window as f64 / n as f64;
    let tok = ((out.iter().sum::<f64>() + inp.iter().sum::<f64>()) * span) as u64;
    Series {
        out_now: out[n - 1],
        inp_now: inp[n - 1],
        out,
        inp,
        window_secs: window,
        tok,
        cost: tok as f64 / 1e6 * 1.9,
    }
}

fn demo_top(pid: i32, cpu: f32, rss_kb: u64, project: &str) -> Vec<ProcLine> {
    let p = |off: i32, share: f32, mem: f32, cmd: &str| ProcLine {
        pid: pid + off,
        cpu: cpu * share,
        rss_kb: (rss_kb as f32 * mem) as u64,
        gpu: None,
        cmd: cmd.into(),
        place: String::new(),
    };
    match project {
        "web-frontend" => vec![
            p(212, 0.48, 0.30, "node vitest run --threads"),
            p(305, 0.31, 0.38, "chrome --headless=new"),
            p(0, 0.12, 0.18, "claude"),
        ],
        "docs-site" => vec![p(88, 0.92, 0.40, "node next dev"), p(0, 0.05, 0.50, "claude")],
        _ => vec![p(0, 0.85, 0.7, "claude"), p(41, 0.10, 0.2, "zsh")],
    }
}

pub fn sys() -> SysStats {
    SysStats {
        cpu: 38.0,
        ncpu: 16,
        load: (6.2, 5.1, 4.4),
        mem_used_kb: 19_400 * 1024,
        mem_total_kb: 32_000 * 1024,
        swap_used_kb: 1_200 * 1024,
        swap_total_kb: 8_000 * 1024,
        psi_cpu: Some(4.0),
        psi_mem: Some(0.0),
        psi_io: Some(1.0),
        gpu: Some(GpuStats {
            busy: Some(31.0),
            vram_used_kb: 2_900 * 1024,
            vram_total_kb: 8_000 * 1024,
            gtt_used_kb: 0,
            gtt_total_kb: 0,
        }),
    }
}

/// Authored drill-down stats for a demo agent (no transcript store to join).
pub fn detail(a: &LiveAgent) -> AgentDetail {
    let busy = a.status == "busy";
    AgentDetail {
        pid: a.pid,
        handle: a.handle.clone(),
        title: a.title.clone(),
        repo: a.repo.clone(),
        cpu: a.cpu,
        rss_kb: a.rss_kb,
        gpu: a.gpu,
        gpu_mem_kb: a.gpu_mem_kb,
        nprocs: a.nprocs,
        detached: a.detached,
        top: a.top.clone(),
        name: a.name.clone(),
        project: a.project.clone(),
        account: a.account.clone(),
        model: a.model.clone(),
        session_id: "3f2a9c1d-demo".into(),
        cwd: a.cwd.clone(),
        status: a.status.clone(),
        uptime_secs: a.uptime_secs,
        idle_secs: Some(if busy { 3 } else { 480 }),
        tok: Tokens {
            input: 84_000,
            output: 61_000,
            cache_read: 1_620_000,
            cache_write_5m: 210_000,
            cache_write_1h: 96_000,
        },
        cost: 12.41,
        burn_tps: a.burn_tps,
        burn_hist: a.burn_hist.clone(),
        ctx_tokens: a.ctx_tokens,
        ctx_window: a.ctx_window,
        compactions: a.compactions,
        action: a.action.clone(),
        subagents: if a.subagents_running > 0 {
            vec![
                SubAgentView { kind: "Explore".into(), desc: "map retry paths in middleware".into(), tokens: 412_000, running: true },
                SubAgentView { kind: "general-purpose".into(), desc: "load test for the limiter".into(), tokens: 286_000, running: true },
                SubAgentView { kind: "Explore".into(), desc: "find rate-limit config".into(), tokens: 91_000, running: false },
            ]
        } else {
            Vec::new()
        },
        ports: a.ports.clone(),
        git: Some(GitStatus { branch: "fix/rate-limit".into(), ahead: 2, behind: 0, changed: 3, untracked: 1 }),
    }
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
            ("claude-opus-4-8".into(), 24_800_000, 178.20),
            ("claude-fable-5".into(), 12_600_000, 90.20),
            ("claude-sonnet-4-6".into(), 8_600_000, 38.10),
            ("claude-haiku-4-5".into(), 2_000_000, 5.90),
        ],
        last7d_by_family: vec![("OPUS", 24_800_000), ("FABLE", 12_600_000), ("SONNET", 8_600_000)],
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
        last5h_tok: 2_100_000,
        last7d_tok: 46_700_000,
        last7d_cost: 297.60,
        grand_tok: 48_000_000,
        grand_cost: 312.40,
    }
}
