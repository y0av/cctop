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
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use chrono::Local;
use clap::Parser;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::widgets::TableState;
use ratatui::Terminal;

use account::Account;
use model::{UsageSource, UsageWindows, Window};
use sessions::LiveAgent;
use transcripts::{Aggregates, Store};

/// Heuristic budgets for the local-estimate fallback (true Max-plan allowances
/// are not exposed). The token count shown is exact; the bar is illustrative.
const EST_5H_BUDGET: u64 = 50_000_000;
const EST_7D_BUDGET: u64 = 1_000_000_000;

#[derive(Parser)]
#[command(name = "cctop", about = "btop-style monitor for Claude usage & agents")]
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
    /// Color theme (only "cyber" is implemented).
    #[arg(long, default_value = "cyber")]
    theme: String,
    /// Render with synthetic demo data (no account access).
    #[arg(long)]
    demo: bool,
    /// Write an HTML screenshot of the demo frame to FILE and exit.
    #[arg(long, value_name = "FILE")]
    snapshot: Option<PathBuf>,
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
    Live(UsageWindows),
    Err(String),
}

struct App {
    sessions_dir: PathBuf,
    creds_path: PathBuf,
    store: Store,
    account: Account,
    agg: Aggregates,
    agents: Vec<LiveAgent>,
    usage: UsageWindows,
    live_usage: Option<UsageWindows>,
    live_at: Option<Instant>,
    net_note: Option<String>,
    table: TableState,
    sort: Sort,
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
            self.usage = demo::usage();
            self.tick = self.tick.wrapping_add(1);
            return;
        }
        self.store.refresh();
        let (now_s, now_ms, today) = now_parts();
        self.agg = self.store.aggregate(now_s, today);
        let mut agents = sessions::read(&self.sessions_dir, &self.store, now_ms);
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

    fn recompute_usage(&mut self) {
        let fresh = self.live_at.map(|t| t.elapsed() < Duration::from_secs(180)).unwrap_or(false);
        self.usage = match (&self.live_usage, fresh) {
            (Some(u), true) => u.clone(),
            _ => {
                let note = if self.no_net {
                    Some("network disabled (--no-net)".to_string())
                } else {
                    self.net_note.clone().or_else(|| Some("awaiting live data…".to_string()))
                };
                estimate(&self.agg, note)
            }
        };
    }

    fn drain_net(&mut self, rx: &mpsc::Receiver<NetMsg>) {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                NetMsg::Live(u) => {
                    self.live_usage = Some(u);
                    self.live_at = Some(Instant::now());
                    self.net_note = None;
                }
                NetMsg::Err(e) => self.net_note = Some(e),
            }
        }
        self.recompute_usage();
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

fn main() {
    let args = Args::parse();

    if let Some(path) = &args.snapshot {
        std::fs::write(path, snapshot::html(100, 26)).expect("write snapshot html");
        return;
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let creds_path = home.join(".claude/.credentials.json");
    let sessions_dir = home.join(".claude/sessions");
    let projects_root = home.join(".claude/projects");

    let (tx, rx) = mpsc::channel();
    let mut app = App {
        sessions_dir,
        creds_path: creds_path.clone(),
        store: Store::new(projects_root),
        account: account::read_account(&home),
        agg: empty_aggregates(),
        agents: Vec::new(),
        usage: estimate(&empty_aggregates(), Some("starting…".to_string())),
        live_usage: None,
        live_at: None,
        net_note: None,
        table: TableState::default(),
        sort: Sort::Burn,
        no_net: args.no_net,
        demo: args.demo,
        tick: 0,
        tx: tx.clone(),
    };
    app.refresh();

    if args.once {
        if !args.no_net {
            let now_ms = Local::now().timestamp_millis();
            match account::ensure_and_fetch(&creds_path, now_ms) {
                Ok(u) => {
                    app.live_usage = Some(u);
                    app.live_at = Some(Instant::now());
                }
                Err(e) => app.net_note = Some(e),
            }
            app.recompute_usage();
        }
        print_once(&app);
        return;
    }

    // Background poller for live plan usage.
    if !args.no_net && !args.demo {
        let cp = creds_path.clone();
        let txc = tx.clone();
        std::thread::spawn(move || loop {
            let now_ms = Local::now().timestamp_millis();
            let msg = match account::ensure_and_fetch(&cp, now_ms) {
                Ok(u) => NetMsg::Live(u),
                Err(e) => NetMsg::Err(e),
            };
            if txc.send(msg).is_err() {
                break;
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
    execute!(stdout(), EnterAlternateScreen, Hide)?;

    // Restore the terminal even on panic.
    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, Show);
        orig(p);
    }));

    let backend = CrosstermBackend::new(stdout());
    let mut term = Terminal::new(backend)?;

    let tick = Duration::from_millis(refresh_ms);
    let mut last = Instant::now();
    let res = loop {
        app.drain_net(rx);
        let sort_label = app.sort.label();
        term.draw(|f| {
            ui::draw(f, &app.account, &app.agg, &app.agents, &app.usage, &mut app.table, sort_label)
        })?;

        let timeout = tick.saturating_sub(last.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            break Ok(())
                        }
                        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                        KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
                        KeyCode::Char('s') => {
                            app.sort = app.sort.next();
                            apply_sort(&mut app.agents, app.sort);
                        }
                        KeyCode::Char('r') if !app.no_net => {
                            let cp = app.creds_path.clone();
                            let txc = app.tx.clone();
                            std::thread::spawn(move || {
                                let now_ms = Local::now().timestamp_millis();
                                let _ = txc.send(match account::ensure_and_fetch(&cp, now_ms) {
                                    Ok(u) => NetMsg::Live(u),
                                    Err(e) => NetMsg::Err(e),
                                });
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        if last.elapsed() >= tick {
            app.refresh();
            last = Instant::now();
        }
    };

    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, Show)?;
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
    let mk = |tok: u64, budget: u64| {
        Some(Window {
            utilization: Some((tok as f64 / budget as f64 * 100.0).min(999.0)),
            tokens: Some(tok),
            resets_at: None,
        })
    };
    UsageWindows {
        five_hour: mk(agg.last5h_tok, EST_5H_BUDGET),
        seven_day: mk(agg.last7d_tok, EST_7D_BUDGET),
        seven_day_opus: None,
        seven_day_sonnet: None,
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
        main_tok: 0,
        agent_tok: 0,
        today_tok: 0,
        today_cost: 0.0,
        buckets24: vec![0; 24],
        last5h_tok: 0,
        last7d_tok: 0,
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
    println!();
    println!(
        "PLAN  [{}]",
        if app.usage.source == UsageSource::Live { "live" } else { "local estimate" }
    );
    print_window("  5-HOUR ", &app.usage.five_hour);
    print_window("  WEEKLY ", &app.usage.seven_day);
    for (label, w) in [("  WK·OPUS", &app.usage.seven_day_opus), ("  WK·SONN", &app.usage.seven_day_sonnet)] {
        if w.as_ref().and_then(|w| w.utilization).unwrap_or(0.0) > 0.0 {
            print_window(label, w);
        }
    }
    if let Some(n) = &app.usage.note {
        println!("  note: {n}");
    }

    println!();
    println!("LIVE AGENTS ({} running)", app.agents.len());
    println!("  {:<8} {:<18} {:<11} {:<6} {:>8} {:>8} {:>8}", "PID", "PROJECT", "MODEL", "ST", "UP", "MEM", "tok/s");
    for ag in &app.agents {
        println!(
            "  {:<8} {:<18} {:<11} {:<6} {:>8} {:>7}M {:>8}",
            ag.pid,
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
