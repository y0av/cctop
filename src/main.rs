//! cctop — a btop-style terminal monitor for Claude usage & running agents.

mod account;
mod demo;
mod model;
mod pricing;
mod sessions;
mod snapshot;
mod theme;
mod transcripts;
mod ui;

use std::cmp::Ordering;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use chrono::Local;
use clap::Parser;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::widgets::TableState;
use ratatui::Terminal;

use account::Account;
use model::{AgentDetail, PlanView, ScopedWindow, Tokens, UsageSource, UsageWindows, Window};
use sessions::LiveAgent;
use transcripts::{Aggregates, Store};

/// Heuristic budgets for the local-estimate fallback (true Max-plan allowances
/// are not exposed). The token count shown is exact; the bar is illustrative.
const EST_5H_BUDGET: u64 = 50_000_000;
const EST_7D_BUDGET: u64 = 1_000_000_000;

#[derive(Parser)]
#[command(name = "cctop", version, about = "btop-style monitor for Claude usage & agents")]
struct Args {
    /// Local refresh interval, in milliseconds.
    #[arg(long, default_value_t = 1000)]
    refresh: u64,
    /// Disable network; estimate plan usage from local transcripts only.
    #[arg(long)]
    no_net: bool,
    /// Print one plain-text snapshot and exit (no TTY required).
    #[arg(long)]
    once: bool,
    /// Theme: cyber, claude, matrix, dracula, nord, synthwave, mono.
    /// Defaults to the last one picked with the `t` key (or cyber).
    #[arg(long)]
    theme: Option<String>,
    /// Render with synthetic demo data (no account access).
    #[arg(long)]
    demo: bool,
    /// Write an HTML screenshot of the demo frame to FILE and exit.
    #[arg(long, value_name = "FILE")]
    snapshot: Option<PathBuf>,
    /// With --snapshot: render the multi-account demo frame instead.
    #[arg(long, hide = true)]
    snapshot_multi: bool,
    /// Extra Claude config dir to also read (repeatable). Pass the dir that
    /// holds `projects/` and `sessions/` — e.g. a CLAUDE_CONFIG_DIR sandbox.
    /// Also settable via the CCTOP_CONFIG_DIRS path-list env var.
    #[arg(long = "config-dir", value_name = "DIR")]
    config_dir: Vec<PathBuf>,
}

#[derive(Clone, Copy)]
enum Sort {
    Burn,
    Uptime,
    Mem,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Burn => "burn",
            Sort::Uptime => "up",
            Sort::Mem => "mem",
        }
    }
    fn next(self) -> Sort {
        match self {
            Sort::Burn => Sort::Uptime,
            Sort::Uptime => Sort::Mem,
            Sort::Mem => Sort::Burn,
        }
    }
}

enum NetMsg {
    Live(usize, UsageWindows),
    Err(usize, String),
}

/// One account whose plan gauges we poll: a config dir with its own
/// `.credentials.json`. The base dir is always a slot (creds or not, so the
/// single-account estimate fallback still appears).
struct PlanSlot {
    label: String,
    creds_path: PathBuf,
    live: Option<UsageWindows>,
    live_at: Option<Instant>,
    note: Option<String>,
    /// Utilization samples per window ("5h" / "7d" / "wk:FABLE"…) feeding the
    /// time-to-cap prediction. Trimmed to the trailing ~90 minutes.
    rate: std::collections::HashMap<String, Vec<(Instant, f64)>>,
}

impl PlanSlot {
    fn record_samples(&mut self, u: &UsageWindows) {
        let now = Instant::now();
        let mut put = |key: String, w: &Option<Window>| {
            if let Some(util) = w.as_ref().and_then(|w| w.utilization) {
                let v = self.rate.entry(key).or_default();
                v.push((now, util));
                v.retain(|(t, _)| now.duration_since(*t) < Duration::from_secs(5400));
            }
        };
        put("5h".into(), &u.five_hour);
        put("7d".into(), &u.seven_day);
        for s in &u.scoped {
            put(format!("wk:{}", s.label), &Some(s.win.clone()));
        }
    }

    /// Linear time-to-100% from the recorded samples. Needs ≥5 minutes of
    /// history and a visibly climbing window; anything flat returns None.
    fn eta_secs(&self, key: &str, current: f64) -> Option<i64> {
        let v = self.rate.get(key)?;
        let (t_new, u_new) = *v.last()?;
        let (t_old, u_old) =
            *v.iter().find(|(t, _)| t_new.duration_since(*t) <= Duration::from_secs(2700))?;
        let span = t_new.duration_since(t_old).as_secs_f64();
        if span < 300.0 {
            return None;
        }
        let rate = (u_new - u_old) / span; // percent per second
        if rate <= 1e-5 {
            return None; // < ~0.04%/h — effectively flat
        }
        Some((((100.0 - current) / rate) as i64).max(0))
    }

    /// Stamp cap-before-reset warnings onto a cloned live `UsageWindows`.
    fn annotate_eta(&self, u: &mut UsageWindows) {
        let mark = |slot: &Self, key: &str, w: &mut Option<Window>| {
            let Some(w) = w.as_mut() else { return };
            let (Some(util), Some(reset)) = (w.utilization, w.resets_at) else { return };
            let to_reset = (reset.timestamp() - Local::now().timestamp()).max(0);
            if let Some(eta) = slot.eta_secs(key, util) {
                if eta < to_reset {
                    w.eta_secs = Some(eta);
                }
            }
        };
        mark(self, "5h", &mut u.five_hour);
        mark(self, "7d", &mut u.seven_day);
        for s in &mut u.scoped {
            let key = format!("wk:{}", s.label);
            let mut w = Some(s.win.clone());
            mark(self, &key, &mut w);
            if let Some(w) = w {
                s.win = w;
            }
        }
    }
}

struct App {
    sessions_dirs: Vec<(PathBuf, String)>,
    n_sources: usize,
    store: Store,
    account: Account,
    agg: Aggregates,
    agents: Vec<LiveAgent>,
    slots: Vec<PlanSlot>,
    plans: Vec<PlanView>,
    table: TableState,
    sort: Sort,
    theme_idx: usize,
    detail_on: bool,
    no_net: bool,
    demo: bool,
    tick: u64,
    tx: Sender<NetMsg>,
}

impl App {
    fn refresh(&mut self) {
        if self.demo {
            self.account = demo::account();
            self.agg = demo::aggregates();
            let mut agents = demo::agents(self.tick);
            apply_sort(&mut agents, self.sort);
            if self.table.selected().is_none() && !agents.is_empty() {
                self.table.select(Some(0));
            }
            self.agents = agents;
            self.tick = self.tick.wrapping_add(1);
            self.recompute_usage();
            return;
        }
        self.store.refresh();
        let (now_s, now_ms, today) = now_parts();
        self.agg = self.store.aggregate(now_s, today);
        let mut agents = sessions::read(&self.sessions_dirs, &self.store, now_ms);
        apply_sort(&mut agents, self.sort);
        let len = agents.len();
        self.agents = agents;
        if let Some(sel) = self.table.selected() {
            if sel >= len {
                self.table.select(if len == 0 { None } else { Some(len - 1) });
            }
        }
        self.recompute_usage();
    }

    /// Rebuild the per-account plan views from slot state. Single-account keeps
    /// the historical behavior (live → local-estimate fallback); with several
    /// accounts, each not-yet-fetched one shows a pending row instead of a
    /// misleading merged estimate.
    fn recompute_usage(&mut self) {
        // Demo gauges are authored, not derived — never replace them with the
        // estimate (or a live fetch would leak real account data into --demo).
        if self.demo {
            self.plans = vec![PlanView { label: String::new(), usage: demo::usage() }];
            return;
        }
        let single = self.slots.len() == 1;
        let plans: Vec<PlanView> = self
            .slots
            .iter()
            .map(|s| {
                let fresh =
                    s.live_at.map(|t| t.elapsed() < Duration::from_secs(180)).unwrap_or(false);
                let usage = match (&s.live, fresh) {
                    (Some(u), true) => {
                        let mut u = u.clone();
                        s.annotate_eta(&mut u);
                        u
                    }
                    _ => {
                        let note = if self.no_net {
                            Some("network disabled (--no-net)".to_string())
                        } else {
                            s.note.clone().or_else(|| Some("awaiting live data…".to_string()))
                        };
                        if single {
                            estimate(&self.agg, note)
                        } else {
                            UsageWindows {
                                five_hour: None,
                                seven_day: None,
                                scoped: Vec::new(),
                                spend: None,
                                source: UsageSource::Estimate,
                                note,
                            }
                        }
                    }
                };
                PlanView { label: if single { String::new() } else { s.label.clone() }, usage }
            })
            .collect();
        self.plans = plans;
    }

    fn drain_net(&mut self, rx: &mpsc::Receiver<NetMsg>) {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                NetMsg::Live(i, u) => {
                    if let Some(s) = self.slots.get_mut(i) {
                        s.record_samples(&u);
                        s.live = Some(u);
                        s.live_at = Some(Instant::now());
                        s.note = None;
                    }
                }
                NetMsg::Err(i, e) => {
                    if let Some(s) = self.slots.get_mut(i) {
                        s.note = Some(e);
                    }
                }
            }
        }
        self.recompute_usage();
    }

    /// (index, creds path) pairs for the poller / manual refresh.
    fn creds_paths(&self) -> Vec<(usize, PathBuf)> {
        self.slots.iter().enumerate().map(|(i, s)| (i, s.creds_path.clone())).collect()
    }

    /// Drill-down stats for the selected agent, joined from the transcript
    /// store (session totals, cost, last-activity).
    fn selected_detail(&self) -> Option<AgentDetail> {
        let a = self.agents.get(self.table.selected()?)?;
        if self.demo {
            return Some(demo::detail(a));
        }
        let mut tok = Tokens::default();
        let mut cost = 0.0;
        for r in &self.store.records {
            if r.session == a.session_id {
                tok.add(&r.tok);
                cost += r.cost;
            }
        }
        let idle_secs = self
            .store
            .session_model
            .get(&a.session_id)
            .map(|(ts, _)| (Local::now().timestamp() - ts).max(0));
        Some(AgentDetail {
            name: a.name.clone(),
            project: a.project.clone(),
            account: a.account.clone(),
            model: a.model.clone(),
            session_id: a.session_id.clone(),
            cwd: a.cwd.clone(),
            status: a.status.clone(),
            uptime_secs: a.uptime_secs,
            idle_secs,
            tok,
            cost,
            burn_tps: a.burn_tps,
            burn_hist: a.burn_hist.clone(),
        })
    }

    fn select_next(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let i = self.table.selected().map(|i| (i + 1) % self.agents.len()).unwrap_or(0);
        self.table.select(Some(i));
    }

    fn select_prev(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let n = self.agents.len();
        let i = self.table.selected().map(|i| (i + n - 1) % n).unwrap_or(0);
        self.table.select(Some(i));
    }
}

/// Resolve the Claude config directories cctop should read.
///
/// The base is `$CLAUDE_CONFIG_DIR` when set, otherwise `~/.claude` — matching
/// Claude Code's own resolution, so a lone cctop reads exactly what `claude`
/// would in the same shell. Additional config dirs (each a `.claude`-style dir
/// holding `projects/`, `sessions/` and `.credentials.json`) can be supplied —
/// cross-platform — via the repeatable `--config-dir` flag and the
/// `CCTOP_CONFIG_DIRS` env var (a `:`-separated path list on Unix, `;` on
/// Windows). The base comes first and is always kept; extra dirs are ignored
/// unless they exist (so a typo or stale path can't inflate the source count),
/// and duplicates collapse by canonical path. With none of these set this
/// returns exactly `[~/.claude]`, identical to the previous hard-coded behavior.
fn config_dirs(extra: &[PathBuf]) -> Vec<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".claude"));

    let mut roots: Vec<PathBuf> = vec![base];
    let from_env = std::env::var_os("CCTOP_CONFIG_DIRS")
        .map(|l| std::env::split_paths(&l).collect::<Vec<_>>())
        .unwrap_or_default();
    for d in from_env.into_iter().chain(extra.iter().cloned()) {
        if !d.as_os_str().is_empty() && d.is_dir() {
            roots.push(d);
        }
    }

    let mut seen = std::collections::HashSet::new();
    roots
        .into_iter()
        .filter(|d| seen.insert(std::fs::canonicalize(d).unwrap_or_else(|_| d.clone())))
        .collect()
}

/// Build the plan-gauge accounts: the base dir always (so the single-account
/// estimate fallback still appears without creds), plus every extra dir that
/// carries its own `.credentials.json`. The same login mounted through two
/// dirs collapses to one slot. With one config dir this is exactly one slot —
/// the default single-account layout is untouched.
fn plan_slots(roots: &[PathBuf]) -> Vec<PlanSlot> {
    let mut slots = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, root) in roots.iter().enumerate() {
        let creds = root.join(".credentials.json");
        if i > 0 && !creds.exists() {
            continue;
        }
        let acc = account::read_account(root);
        let label = if !acc.display_name.is_empty() {
            acc.display_name.clone()
        } else if !acc.email.is_empty() {
            acc.email.split('@').next().unwrap_or(&acc.email).to_string()
        } else {
            dir_label(root)
        };
        let key = if acc.email.is_empty() { root.display().to_string() } else { acc.email.clone() };
        if !seen.insert(key) {
            continue;
        }
        slots.push(PlanSlot {
            label,
            creds_path: creds,
            live: None,
            live_at: None,
            note: None,
            rate: std::collections::HashMap::new(),
        });
    }
    slots
}

/// Compact human name for a config dir: the last path component, except that a
/// generic ".claude"/"claude" leaf defers to its parent (e.g. envs/bobo/claude
/// → "bobo").
fn dir_label(root: &Path) -> String {
    let mut comps = root.iter().rev().filter_map(|c| c.to_str());
    match comps.next() {
        Some(".claude") | Some("claude") => comps.next().unwrap_or("claude").to_string(),
        Some(other) => other.to_string(),
        None => "?".to_string(),
    }
}

fn main() {
    let args = Args::parse();

    // Theme precedence: --theme flag > choice saved by the `t` key > default.
    let theme_idx = match &args.theme {
        Some(name) => theme::index_of(name).unwrap_or_else(|| {
            eprintln!("cctop: unknown theme '{name}' — available: {}", theme::names().join(", "));
            std::process::exit(2);
        }),
        None => theme::load_saved().unwrap_or(0),
    };

    if let Some(path) = &args.snapshot {
        let height = if args.snapshot_multi { 31 } else { 27 };
        std::fs::write(path, snapshot::html(100, height, &theme::THEMES[theme_idx], args.snapshot_multi))
            .expect("write snapshot html");
        return;
    }

    let roots = config_dirs(&args.config_dir);
    let n_sources = roots.len();
    // The base (first) dir owns the header identity; every dir with its own
    // credentials becomes a plan-gauge account of its own.
    let base = roots.first().cloned().unwrap_or_else(|| PathBuf::from(".claude"));
    let sessions_dirs: Vec<(PathBuf, String)> =
        roots.iter().map(|d| (d.join("sessions"), dir_label(d))).collect();
    let projects_roots: Vec<PathBuf> = roots.iter().map(|d| d.join("projects")).collect();
    let slots = plan_slots(&roots);

    let (tx, rx) = mpsc::channel();
    let mut app = App {
        sessions_dirs,
        n_sources,
        store: Store::new(projects_roots),
        account: account::read_account(&base),
        agg: empty_aggregates(),
        agents: Vec::new(),
        slots,
        plans: Vec::new(),
        table: TableState::default(),
        sort: Sort::Burn,
        theme_idx,
        detail_on: true,
        no_net: args.no_net,
        demo: args.demo,
        tick: 0,
        tx: tx.clone(),
    };
    app.refresh();

    if args.once {
        if !args.no_net && !args.demo {
            let now_ms = Local::now().timestamp_millis();
            for i in 0..app.slots.len() {
                match account::ensure_and_fetch(&app.slots[i].creds_path, now_ms) {
                    Ok(u) => {
                        app.slots[i].live = Some(u);
                        app.slots[i].live_at = Some(Instant::now());
                    }
                    Err(e) => app.slots[i].note = Some(e),
                }
            }
            app.recompute_usage();
        }
        print_once(&app);
        return;
    }

    // Background poller for live plan usage — one thread walks every account.
    if !args.no_net && !args.demo {
        let cps = app.creds_paths();
        let txc = tx.clone();
        std::thread::spawn(move || loop {
            for (i, cp) in &cps {
                let now_ms = Local::now().timestamp_millis();
                let msg = match account::ensure_and_fetch(cp, now_ms) {
                    Ok(u) => NetMsg::Live(*i, u),
                    Err(e) => NetMsg::Err(*i, e),
                };
                if txc.send(msg).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(60));
        });
    }

    if let Err(e) = run_tui(&mut app, &rx, args.refresh) {
        eprintln!("cctop: {e}");
    }
}

fn run_tui(app: &mut App, rx: &mpsc::Receiver<NetMsg>, refresh_ms: u64) -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture, Hide)?;

    // Restore the terminal even on panic.
    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, Show);
        orig(p);
    }));

    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    let tick = Duration::from_millis(refresh_ms);
    let mut last = Instant::now();
    let res = loop {
        app.drain_net(rx);
        let sort_label = app.sort.label();
        let n_sources = app.n_sources;
        let detail = if app.detail_on { app.selected_detail() } else { None };
        let th = &theme::THEMES[app.theme_idx];
        term.draw(|f| {
            ui::draw(f, th, &app.account, &app.agg, &app.agents, &app.plans, detail.as_ref(),
                     &mut app.table, sort_label, n_sources)
        })?;

        let timeout = tick.saturating_sub(last.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        break Ok(())
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                    KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
                    KeyCode::Enter | KeyCode::Char('d') => app.detail_on = !app.detail_on,
                    KeyCode::Char('s') => {
                        app.sort = app.sort.next();
                        apply_sort(&mut app.agents, app.sort);
                    }
                    KeyCode::Char('t') => {
                        app.theme_idx = (app.theme_idx + 1) % theme::THEMES.len();
                        theme::save(theme::THEMES[app.theme_idx].name);
                    }
                    KeyCode::Char('r') if !app.no_net => {
                        let cps = app.creds_paths();
                        let txc = app.tx.clone();
                        std::thread::spawn(move || {
                            for (i, cp) in &cps {
                                let now_ms = Local::now().timestamp_millis();
                                let msg = match account::ensure_and_fetch(cp, now_ms) {
                                    Ok(u) => NetMsg::Live(*i, u),
                                    Err(e) => NetMsg::Err(*i, e),
                                };
                                if txc.send(msg).is_err() {
                                    return;
                                }
                            }
                        });
                    }
                    _ => {}
                },
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollDown => app.select_next(),
                    MouseEventKind::ScrollUp => app.select_prev(),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let size = term.size()?;
                        if let Some(i) = ui::agents_row_at(size, &app.plans, m.row) {
                            let i = i + app.table.offset();
                            if i < app.agents.len() {
                                app.table.select(Some(i));
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        if last.elapsed() >= tick {
            app.refresh();
            last = Instant::now();
        }
    };

    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, Show)?;
    res
}

fn apply_sort(agents: &mut [LiveAgent], sort: Sort) {
    let busy_first = |a: &LiveAgent, b: &LiveAgent| {
        ((b.status == "busy") as u8).cmp(&((a.status == "busy") as u8))
    };
    match sort {
        Sort::Burn => agents.sort_by(|a, b| {
            busy_first(a, b)
                .then(b.burn_tps.partial_cmp(&a.burn_tps).unwrap_or(Ordering::Equal))
                .then(b.uptime_secs.cmp(&a.uptime_secs))
        }),
        Sort::Uptime => agents.sort_by(|a, b| busy_first(a, b).then(b.uptime_secs.cmp(&a.uptime_secs))),
        Sort::Mem => agents.sort_by(|a, b| busy_first(a, b).then(b.rss_kb.cmp(&a.rss_kb))),
    }
}

fn estimate(agg: &Aggregates, note: Option<String>) -> UsageWindows {
    let mk = |tok: u64, budget: u64| Window {
        utilization: Some((tok as f64 / budget as f64 * 100.0).min(999.0)),
        tokens: Some(tok),
        resets_at: None,
        eta_secs: None,
    };
    let scoped = agg
        .last7d_by_family
        .iter()
        .map(|(fam, tok)| ScopedWindow { label: fam.to_string(), win: mk(*tok, EST_7D_BUDGET) })
        .collect();
    UsageWindows {
        five_hour: Some(mk(agg.last5h_tok, EST_5H_BUDGET)),
        seven_day: Some(mk(agg.last7d_tok, EST_7D_BUDGET)),
        scoped,
        spend: None,
        source: UsageSource::Estimate,
        note,
    }
}

/// (now_secs, now_millis, local_today_start_secs)
fn now_parts() -> (i64, i64, i64) {
    let now = Local::now();
    let today = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|d| d.and_local_timezone(Local).single())
        .map(|d| d.timestamp())
        .unwrap_or(now.timestamp());
    (now.timestamp(), now.timestamp_millis(), today)
}

fn empty_aggregates() -> Aggregates {
    Aggregates {
        by_model: Vec::new(),
        by_project: Vec::new(),
        last7d_by_family: Vec::new(),
        main_tok: 0,
        agent_tok: 0,
        today_tok: 0,
        today_cost: 0.0,
        buckets24: vec![0; 24],
        last5h_tok: 0,
        last7d_tok: 0,
        last7d_cost: 0.0,
        grand_tok: 0,
        grand_cost: 0.0,
    }
}

fn print_once(app: &App) {
    let a = &app.account;
    println!("CCTOP — claude {} {}", a.subscription, a.rate_limit_tier);
    if !a.display_name.is_empty() || !a.org.is_empty() {
        println!("  {} · {}", a.display_name, a.org);
    }
    for p in &app.plans {
        println!();
        let src = if p.usage.source == UsageSource::Live { "live" } else { "local estimate" };
        if p.label.is_empty() {
            println!("PLAN  [{src}]");
        } else {
            println!("PLAN  {}  [{src}]", p.label);
        }
        print_window("  5-HOUR ", &p.usage.five_hour);
        print_window("  WEEKLY ", &p.usage.seven_day);
        for s in &p.usage.scoped {
            if s.win.utilization.unwrap_or(0.0) > 0.0 {
                print_window(&format!("  WK·{}", s.label), &Some(s.win.clone()));
            }
        }
        if let Some(sp) = &p.usage.spend {
            match sp.limit {
                Some(l) => println!("  EXTRA $ {:.2} / {:.0}", sp.used, l),
                None => println!("  EXTRA $ {:.2}", sp.used),
            }
        }
        if let Some(n) = &p.usage.note {
            println!("  note: {n}");
        }
    }

    println!();
    if app.n_sources > 1 {
        println!("LIVE AGENTS ({} running · {} config dirs)", app.agents.len(), app.n_sources);
    } else {
        println!("LIVE AGENTS ({} running)", app.agents.len());
    }
    println!("  {:<8} {:<22} {:<18} {:<11} {:<6} {:>8} {:>8} {:>8}",
        "PID", "NAME", "PROJECT", "MODEL", "ST", "UP", "MEM", "tok/s");
    for ag in &app.agents {
        println!(
            "  {:<8} {:<22} {:<18} {:<11} {:<6} {:>8} {:>7}M {:>8}",
            ag.pid,
            short(if ag.name.is_empty() { &ag.project } else { &ag.name }, 22),
            short(&ag.project, 18),
            ag.model.strip_prefix("claude-").unwrap_or(&ag.model),
            ag.status,
            theme::uptime(ag.uptime_secs),
            ag.rss_kb / 1024,
            theme::human(ag.burn_tps.round() as u64),
        );
    }

    println!();
    let g = &app.agg;
    println!(
        "USAGE  today {} (${:.2})   total {} (${:.2})   main {} / agents {}",
        theme::human(g.today_tok),
        g.today_cost,
        theme::human(g.grand_tok),
        g.grand_cost,
        theme::human(g.main_tok),
        theme::human(g.agent_tok),
    );
    println!("BY MODEL");
    for (m, tok, cost) in g.by_model.iter().take(6) {
        println!("  {:<24} {:>8}  ${:.2}", m, theme::human(*tok), cost);
    }
    println!("BY PROJECT");
    for (p, tok, cost) in g.by_project.iter().take(8) {
        println!("  {:<24} {:>8}  ${:.2}", short(p, 24), theme::human(*tok), cost);
    }
}

fn print_window(label: &str, win: &Option<Window>) {
    let win = match win {
        Some(w) => w,
        None => return,
    };
    let val = match win.tokens {
        Some(t) => format!("{} tok ~est", theme::human(t)),
        None => format!("{:.0}%", win.utilization.unwrap_or(0.0)),
    };
    let reset = win
        .resets_at
        .map(|r| format!("  resets {}", theme::until((r.timestamp() - Local::now().timestamp()).max(0))))
        .unwrap_or_default();
    println!("{label} {val}{reset}");
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}
