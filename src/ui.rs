//! All ratatui rendering. Functions take explicit field references (not `&App`)
//! so disjoint borrows let the agents table render mutably while the rest reads.

use chrono::Local;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::account::Account;
use crate::model::{PlanView, Spend, UsageSource, UsageWindows, Window};
use crate::sessions::LiveAgent;
use crate::theme::Theme;
use crate::theme;
use crate::transcripts::Aggregates;

pub fn draw(f: &mut Frame, th: &Theme, account: &Account, agg: &Aggregates, agents: &[LiveAgent],
            plans: &[PlanView], state: &mut TableState, sort_label: &str, n_sources: usize) {
    // The plan panel grows with accounts / scoped weeklies / spend / note.
    let multi = plans.len() > 1;
    let lines: u16 = plans.iter().map(|p| plan_lines(&p.usage, multi)).sum();
    let gauge_h = (2 + lines).clamp(6, 18);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),       // header
            Constraint::Length(gauge_h), // gauges
            Constraint::Min(6),          // agents
            Constraint::Length(8),       // bottom row
            Constraint::Length(1),       // footer
        ])
        .split(f.area());

    header(f, th, chunks[0], account);
    gauges(f, th, chunks[1], plans);
    agents_table(f, th, chunks[2], agents, state);
    bottom(f, th, chunks[3], agg);
    footer(f, th, chunks[4], plans, n_sources, sort_label);
}

fn shown(w: &Window) -> bool {
    w.utilization.unwrap_or(0.0) > 0.0
}

/// Rendered line count for one account's gauges. In multi-account mode each
/// account gets a header row and its note moves inline (into that row).
fn plan_lines(u: &UsageWindows, multi: bool) -> u16 {
    let n = u.five_hour.is_some() as u16
        + u.seven_day.is_some() as u16
        + u.scoped.iter().filter(|s| shown(&s.win)).count() as u16
        + u.spend.is_some() as u16;
    if multi { n + 1 } else { n + u.note.is_some() as u16 }
}

fn panel<'a>(th: &Theme, title: Line<'a>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(th.border)
        .border_style(Style::default().fg(th.frame))
        .title(title)
}

fn header(f: &mut Frame, th: &Theme, area: Rect, account: &Account) {
    let tier = tier_label(&account.subscription, &account.rate_limit_tier);
    let mut left = vec![
        Span::styled("CCTOP", Style::default().fg(th.primary).add_modifier(Modifier::BOLD)),
        Span::styled(concat!(" v", env!("CARGO_PKG_VERSION")), Style::default().fg(th.dim)),
        Span::styled("  claude ", Style::default().fg(th.dim)),
        Span::styled(tier.clone(), Style::default().fg(th.secondary).add_modifier(Modifier::BOLD)),
    ];

    let who = {
        let mut s = String::new();
        if !account.display_name.is_empty() {
            s.push_str(&account.display_name);
        }
        if !account.org.is_empty() {
            if !s.is_empty() {
                s.push_str(" · ");
            }
            s.push_str(&account.org);
        }
        s
    };
    let clock = Local::now().format("%H:%M:%S").to_string();

    let left_w: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_w = who.chars().count() + 3 + clock.chars().count();
    let total = area.width as usize;
    let pad = total.saturating_sub(left_w + right_w);
    left.push(Span::raw(" ".repeat(pad)));
    if !who.is_empty() {
        left.push(Span::styled(who, Style::default().fg(th.text)));
        left.push(Span::styled(" · ", Style::default().fg(th.dim)));
    }
    left.push(Span::styled(clock, Style::default().fg(th.primary)));

    f.render_widget(Paragraph::new(Line::from(left)), area);
}

fn tier_label(sub: &str, tier: &str) -> String {
    let sub = if sub.is_empty() { "—" } else { sub };
    // e.g. default_claude_max_20x -> "20x"
    let mult = tier.rsplit('_').next().filter(|s| s.ends_with('x')).unwrap_or("");
    if mult.is_empty() {
        sub.to_uppercase()
    } else {
        format!("{} {}", sub.to_uppercase(), mult)
    }
}

fn gauges(f: &mut Frame, th: &Theme, area: Rect, plans: &[PlanView]) {
    let multi = plans.len() > 1;
    let title = if multi {
        Span::styled(
            format!(" PLAN  {} accounts ", plans.len()),
            Style::default().fg(th.primary),
        )
    } else if plans.first().map(|p| p.usage.source) == Some(UsageSource::Estimate) {
        Span::styled(" PLAN  ~est (local) ", Style::default().fg(th.accent))
    } else {
        Span::styled(" PLAN  live ", Style::default().fg(th.primary))
    };
    let block = panel(th, Line::from(title));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let bar_w = (inner.width as i32 - 38).clamp(6, 40) as usize;
    let mut lines = Vec::new();
    for p in plans {
        let usage = &p.usage;
        let est = usage.source == UsageSource::Estimate;
        if multi {
            // Account header row; a stale/pending account carries its note here.
            let mut spans = vec![
                Span::styled("▸ ", Style::default().fg(th.dim)),
                Span::styled(p.label.clone(), Style::default().fg(th.secondary).add_modifier(Modifier::BOLD)),
            ];
            match (&usage.note, est) {
                (Some(n), _) => spans.push(Span::styled(format!("  {n}"), Style::default().fg(th.dim))),
                (None, false) => spans.push(Span::styled("  live", Style::default().fg(th.primary))),
                (None, true) => spans.push(Span::styled("  ~est", Style::default().fg(th.accent))),
            }
            lines.push(Line::from(spans));
        }
        push_gauge(&mut lines, th, "5-HOUR", &usage.five_hour, est, bar_w);
        push_gauge(&mut lines, th, "WEEKLY", &usage.seven_day, est, bar_w);
        // Model-scoped weekly caps (Fable / Opus / Sonnet …) only matter once
        // you've actually used the model.
        for s in &usage.scoped {
            if shown(&s.win) {
                push_gauge(&mut lines, th, &format!("WK·{}", s.label), &Some(s.win.clone()), est, bar_w);
            }
        }
        if let Some(sp) = &usage.spend {
            push_spend(&mut lines, th, sp, bar_w);
        }
        if !multi {
            if let Some(note) = &usage.note {
                lines.push(Line::from(Span::styled(
                    format!("  {note}"),
                    Style::default().fg(th.dim),
                )));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn push_gauge(lines: &mut Vec<Line<'static>>, th: &Theme, label: &str, win: &Option<Window>,
              est: bool, bar_w: usize) {
    let win = match win {
        Some(w) => w,
        None => return,
    };
    let frac = win.utilization.unwrap_or(0.0) / 100.0;
    let color = th.gauge_color(frac);
    let (filled, empty) = th.gauge_parts(frac, bar_w);

    let value = if est {
        match win.tokens {
            Some(t) => format!(" {} ~est", theme::human(t)),
            None => format!(" {:.0}%", win.utilization.unwrap_or(0.0)),
        }
    } else {
        format!(" {:.0}%", win.utilization.unwrap_or(0.0))
    };

    let mut spans = vec![
        Span::styled(format!("{label:<9} "), Style::default().fg(th.dim)),
        Span::styled("[", Style::default().fg(th.dim)),
        Span::styled(filled, Style::default().fg(color)),
        Span::styled(empty, Style::default().fg(th.dim)),
        Span::styled("]", Style::default().fg(th.dim)),
        Span::styled(value, Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ];
    if let Some(reset) = win.resets_at {
        let secs = (reset.timestamp() - Local::now().timestamp()).max(0);
        spans.push(Span::styled(
            format!("  resets {}", theme::until(secs)),
            Style::default().fg(th.dim),
        ));
    }
    lines.push(Line::from(spans));
}

/// Extra-usage credits line, shown only when the account has overage enabled.
fn push_spend(lines: &mut Vec<Line<'static>>, th: &Theme, sp: &Spend, bar_w: usize) {
    let frac = sp
        .percent
        .or_else(|| sp.limit.filter(|l| *l > 0.0).map(|l| sp.used / l * 100.0))
        .unwrap_or(0.0)
        / 100.0;
    let color = th.gauge_color(frac);
    let (filled, empty) = th.gauge_parts(frac, bar_w);
    let value = match sp.limit {
        Some(l) => format!(" ${:.2} / ${:.0}", sp.used, l),
        None => format!(" ${:.2}", sp.used),
    };
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9} ", "EXTRA $"), Style::default().fg(th.dim)),
        Span::styled("[", Style::default().fg(th.dim)),
        Span::styled(filled, Style::default().fg(color)),
        Span::styled(empty, Style::default().fg(th.dim)),
        Span::styled("]", Style::default().fg(th.dim)),
        Span::styled(value, Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ]));
}

fn agents_table(f: &mut Frame, th: &Theme, area: Rect, agents: &[LiveAgent], state: &mut TableState) {
    let busy = agents.iter().filter(|a| a.status == "busy").count();
    let title = Line::from(vec![
        Span::styled(" LIVE AGENTS ", Style::default().fg(th.secondary).add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("{} live · {} busy ", agents.len(), busy),
            Style::default().fg(th.dim),
        ),
    ]);
    let block = panel(th, title);

    let header = Row::new(
        ["PID", "PROJECT", "MODEL", "ST", "UP", "MEM", "BURN", "tok/s"]
            .iter()
            .map(|h| Cell::from(*h)),
    )
    .style(Style::default().fg(th.dim).add_modifier(Modifier::BOLD));

    let rows = agents.iter().map(|a| {
        let busy = a.status == "busy";
        let st = if busy {
            Span::styled("●busy", Style::default().fg(th.primary))
        } else if a.status == "idle" {
            Span::styled("○idle", Style::default().fg(th.dim))
        } else {
            Span::styled(format!("·{}", a.status), Style::default().fg(th.dim))
        };
        let burn_color = if a.burn_tps > 0.5 { th.primary } else { th.dim };
        Row::new(vec![
            Cell::from(a.pid.to_string()).style(Style::default().fg(th.text)),
            Cell::from(truncate(&a.project, 18)).style(Style::default().fg(th.secondary)),
            Cell::from(model_short(&a.model)).style(Style::default().fg(th.text)),
            Cell::from(st),
            Cell::from(theme::uptime(a.uptime_secs)).style(Style::default().fg(th.text)),
            Cell::from(fmt_mem(a.rss_kb)).style(Style::default().fg(th.text)),
            Cell::from(Span::styled(theme::spark(&a.burn_hist, 8), Style::default().fg(burn_color))),
            Cell::from(theme::human(a.burn_tps.round() as u64))
                .style(Style::default().fg(if busy { th.primary } else { th.text })),
        ])
    });

    let widths = [
        Constraint::Length(8),
        Constraint::Min(12),
        Constraint::Length(11),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(9),
        Constraint::Length(7),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().fg(th.accent).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶");
    f.render_stateful_widget(table, area, state);
}

fn bottom(f: &mut Frame, th: &Theme, area: Rect, agg: &Aggregates) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(30), Constraint::Percentage(30)])
        .split(area);

    usage_panel(f, th, cols[0], agg);
    bars_panel(f, th, cols[1], " BY MODEL ", &agg.by_model, agg.grand_tok, true);
    bars_panel(f, th, cols[2], " BY PROJECT ", &agg.by_project, agg.grand_tok, false);
}

fn usage_panel(f: &mut Frame, th: &Theme, area: Rect, agg: &Aggregates) {
    let block = panel(
        th,
        Line::from(Span::styled(" USAGE ", Style::default().fg(th.secondary).add_modifier(Modifier::BOLD))),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let spark_w = (inner.width as usize).saturating_sub(18).clamp(6, 24);
    let main = agg.main_tok;
    let agents = agg.agent_tok;
    let denom = (main + agents).max(1);
    let main_frac = main as f64 / denom as f64;

    let lines = vec![
        Line::from(vec![
            Span::styled("today ", Style::default().fg(th.dim)),
            Span::styled(theme::spark(&agg.buckets24, spark_w), Style::default().fg(th.secondary)),
            Span::styled(format!(" {}", theme::human(agg.today_tok)), Style::default().fg(th.primary)),
        ]),
        Line::from(vec![
            Span::styled("main ", Style::default().fg(th.dim)),
            Span::styled(th.bar(main_frac, 8), Style::default().fg(th.primary)),
            Span::styled(format!(" {:.0}%", main_frac * 100.0), Style::default().fg(th.text)),
            Span::styled("  agents ", Style::default().fg(th.dim)),
            Span::styled(format!("{:.0}%", (1.0 - main_frac) * 100.0), Style::default().fg(th.accent)),
        ]),
        Line::from(vec![
            Span::styled("today $   ", Style::default().fg(th.dim)),
            Span::styled(format!("${:.2}", agg.today_cost), Style::default().fg(th.primary)),
        ]),
        Line::from(vec![
            Span::styled("total     ", Style::default().fg(th.dim)),
            Span::styled(theme::human(agg.grand_tok), Style::default().fg(th.text)),
            Span::styled(format!("  ${:.2}", agg.grand_cost), Style::default().fg(th.text)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn bars_panel(f: &mut Frame, th: &Theme, area: Rect, title: &'static str,
              items: &[(String, u64, f64)], grand: u64, model_names: bool) {
    let block = panel(
        th,
        Line::from(Span::styled(title, Style::default().fg(th.secondary).add_modifier(Modifier::BOLD))),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let denom = grand.max(1) as f64;
    let name_w = 10usize;
    let bar_w = (inner.width as usize).saturating_sub(name_w + 6).clamp(4, 18);
    let rows = inner.height as usize;

    let mut lines = Vec::new();
    for (name, tok, _cost) in items.iter().take(rows) {
        let frac = *tok as f64 / denom;
        let label = if model_names { model_short(name) } else { truncate(name, name_w) };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<width$}", truncate(&label, name_w), width = name_w),
                Style::default().fg(th.text)),
            Span::styled(th.bar(frac, bar_w), Style::default().fg(th.gauge_color(frac))),
            Span::styled(format!(" {:>2.0}%", frac * 100.0), Style::default().fg(th.dim)),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("  (no data)", Style::default().fg(th.dim))));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn footer(f: &mut Frame, th: &Theme, area: Rect, plans: &[PlanView], n_sources: usize, sort_label: &str) {
    let live_n = plans.iter().filter(|p| p.usage.source == UsageSource::Live).count();
    let net = if plans.len() > 1 {
        let color = if live_n == plans.len() { th.primary } else { th.accent };
        Span::styled(format!("net:{live_n}/{}", plans.len()), Style::default().fg(color))
    } else if live_n == 1 {
        Span::styled("net:LIVE", Style::default().fg(th.primary))
    } else {
        Span::styled("net:EST", Style::default().fg(th.accent))
    };
    let mut spans = vec![
        Span::styled(" [q]", Style::default().fg(th.secondary)),
        Span::styled("uit  ", Style::default().fg(th.dim)),
        Span::styled("[↑↓]", Style::default().fg(th.secondary)),
        Span::styled("select  ", Style::default().fg(th.dim)),
        Span::styled("[s]", Style::default().fg(th.secondary)),
        Span::styled(format!("ort:{sort_label}  "), Style::default().fg(th.dim)),
        Span::styled("[t]", Style::default().fg(th.secondary)),
        Span::styled(format!("heme:{}  ", th.name), Style::default().fg(th.dim)),
        Span::styled("[r]", Style::default().fg(th.secondary)),
        Span::styled("efresh   ", Style::default().fg(th.dim)),
        net,
    ];
    // Only surfaced when reading more than the default dir, so the common
    // single-config footer stays exactly as it was.
    if n_sources > 1 {
        spans.push(Span::styled(format!("  src:{n_sources}"), Style::default().fg(th.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).alignment(Alignment::Left), area);
}

fn model_short(m: &str) -> String {
    m.strip_prefix("claude-").unwrap_or(m).to_string()
}

fn fmt_mem(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.1}G", kb as f64 / 1024.0 / 1024.0)
    } else {
        format!("{}M", kb / 1024)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
