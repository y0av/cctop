//! All ratatui rendering. Functions take explicit field references (not `&App`)
//! so disjoint borrows let the agents table render mutably while the rest reads.

use chrono::Local;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::account::Account;
use crate::model::{UsageSource, UsageWindows, Window};
use crate::sessions::LiveAgent;
use crate::theme;
use crate::transcripts::Aggregates;

pub fn draw(f: &mut Frame, account: &Account, agg: &Aggregates, agents: &[LiveAgent],
            usage: &UsageWindows, state: &mut TableState, sort_label: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(6), // gauges
            Constraint::Min(6),    // agents
            Constraint::Length(8), // bottom row
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    header(f, chunks[0], account);
    gauges(f, chunks[1], usage);
    agents_table(f, chunks[2], agents, state);
    bottom(f, chunks[3], agg);
    footer(f, chunks[4], usage, agents.len(), sort_label);
}

fn header(f: &mut Frame, area: Rect, account: &Account) {
    let tier = tier_label(&account.subscription, &account.rate_limit_tier);
    let mut left = vec![
        Span::styled("CCTOP", Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD)),
        Span::styled("  claude ", Style::default().fg(theme::DIM)),
        Span::styled(tier.clone(), Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)),
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
        left.push(Span::styled(who, Style::default().fg(theme::TEXT)));
        left.push(Span::styled(" · ", Style::default().fg(theme::DIM)));
    }
    left.push(Span::styled(clock, Style::default().fg(theme::GREEN)));

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

fn gauges(f: &mut Frame, area: Rect, usage: &UsageWindows) {
    let est = usage.source == UsageSource::Estimate;
    let title = if est {
        Span::styled(" PLAN  ~est (local) ", Style::default().fg(theme::MAGENTA))
    } else {
        Span::styled(" PLAN  live ", Style::default().fg(theme::GREEN))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::FRAME))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let bar_w = (inner.width as i32 - 36).clamp(6, 40) as usize;
    let mut lines = Vec::new();
    push_gauge(&mut lines, "5-HOUR", &usage.five_hour, est, bar_w);
    push_gauge(&mut lines, "WEEKLY", &usage.seven_day, est, bar_w);
    // Model-specific weekly caps only matter once you've actually used them.
    for (label, w) in [("WK·OPUS", &usage.seven_day_opus), ("WK·SONN", &usage.seven_day_sonnet)] {
        if w.as_ref().and_then(|w| w.utilization).unwrap_or(0.0) > 0.0 {
            push_gauge(&mut lines, label, w, est, bar_w);
        }
    }
    if let Some(note) = &usage.note {
        lines.push(Line::from(Span::styled(
            format!("  {note}"),
            Style::default().fg(theme::DIM),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn push_gauge(lines: &mut Vec<Line<'static>>, label: &str, win: &Option<Window>, est: bool, bar_w: usize) {
    let win = match win {
        Some(w) => w,
        None => return,
    };
    let frac = win.utilization.unwrap_or(0.0) / 100.0;
    let color = theme::gauge_color(frac);
    let fill = theme::bar(frac, bar_w, '|', '·');
    let (filled, empty) = fill.split_at(fill.chars().take_while(|c| *c == '|').count());

    let value = if est {
        match win.tokens {
            Some(t) => format!(" {} ~est", theme::human(t)),
            None => format!(" {:.0}%", win.utilization.unwrap_or(0.0)),
        }
    } else {
        format!(" {:.0}%", win.utilization.unwrap_or(0.0))
    };

    let mut spans = vec![
        Span::styled(format!("{label:<7} "), Style::default().fg(theme::DIM)),
        Span::styled("[", Style::default().fg(theme::DIM)),
        Span::styled(filled.to_string(), Style::default().fg(color)),
        Span::styled(empty.to_string(), Style::default().fg(theme::DIM)),
        Span::styled("]", Style::default().fg(theme::DIM)),
        Span::styled(value, Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ];
    if let Some(reset) = win.resets_at {
        let secs = (reset.timestamp() - Local::now().timestamp()).max(0);
        spans.push(Span::styled(
            format!("  resets {}", theme::until(secs)),
            Style::default().fg(theme::DIM),
        ));
    }
    lines.push(Line::from(spans));
}

fn agents_table(f: &mut Frame, area: Rect, agents: &[LiveAgent], state: &mut TableState) {
    let busy = agents.iter().filter(|a| a.status == "busy").count();
    let title = Line::from(vec![
        Span::styled(" LIVE AGENTS ", Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("{} live · {} busy ", agents.len(), busy),
            Style::default().fg(theme::DIM),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::FRAME))
        .title(title);

    let header = Row::new(
        ["PID", "PROJECT", "MODEL", "ST", "UP", "MEM", "BURN", "tok/s"]
            .iter()
            .map(|h| Cell::from(*h)),
    )
    .style(Style::default().fg(theme::DIM).add_modifier(Modifier::BOLD));

    let rows = agents.iter().map(|a| {
        let busy = a.status == "busy";
        let st = if busy {
            Span::styled("●busy", Style::default().fg(theme::GREEN))
        } else if a.status == "idle" {
            Span::styled("○idle", Style::default().fg(theme::DIM))
        } else {
            Span::styled(format!("·{}", a.status), Style::default().fg(theme::DIM))
        };
        let burn_color = if a.burn_tps > 0.5 { theme::GREEN } else { theme::DIM };
        Row::new(vec![
            Cell::from(a.pid.to_string()).style(Style::default().fg(theme::TEXT)),
            Cell::from(truncate(&a.project, 18)).style(Style::default().fg(theme::CYAN)),
            Cell::from(model_short(&a.model)).style(Style::default().fg(theme::TEXT)),
            Cell::from(st),
            Cell::from(theme::uptime(a.uptime_secs)).style(Style::default().fg(theme::TEXT)),
            Cell::from(fmt_mem(a.rss_kb)).style(Style::default().fg(theme::TEXT)),
            Cell::from(Span::styled(theme::spark(&a.burn_hist, 8), Style::default().fg(burn_color))),
            Cell::from(theme::human(a.burn_tps.round() as u64))
                .style(Style::default().fg(if busy { theme::GREEN } else { theme::TEXT })),
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
        .row_highlight_style(Style::default().fg(theme::MAGENTA).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶");
    f.render_stateful_widget(table, area, state);
}

fn bottom(f: &mut Frame, area: Rect, agg: &Aggregates) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(30), Constraint::Percentage(30)])
        .split(area);

    usage_panel(f, cols[0], agg);
    bars_panel(f, cols[1], " BY MODEL ", &agg.by_model, agg.grand_tok, true);
    bars_panel(f, cols[2], " BY PROJECT ", &agg.by_project, agg.grand_tok, false);
}

fn usage_panel(f: &mut Frame, area: Rect, agg: &Aggregates) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::FRAME))
        .title(Span::styled(" USAGE ", Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let spark_w = (inner.width as usize).saturating_sub(18).clamp(6, 24);
    let main = agg.main_tok;
    let agents = agg.agent_tok;
    let denom = (main + agents).max(1);
    let main_frac = main as f64 / denom as f64;

    let lines = vec![
        Line::from(vec![
            Span::styled("today ", Style::default().fg(theme::DIM)),
            Span::styled(theme::spark(&agg.buckets24, spark_w), Style::default().fg(theme::CYAN)),
            Span::styled(format!(" {}", theme::human(agg.today_tok)), Style::default().fg(theme::GREEN)),
        ]),
        Line::from(vec![
            Span::styled("main ", Style::default().fg(theme::DIM)),
            Span::styled(theme::bar(main_frac, 8, '█', '░'), Style::default().fg(theme::GREEN)),
            Span::styled(format!(" {:.0}%", main_frac * 100.0), Style::default().fg(theme::TEXT)),
            Span::styled("  agents ", Style::default().fg(theme::DIM)),
            Span::styled(format!("{:.0}%", (1.0 - main_frac) * 100.0), Style::default().fg(theme::MAGENTA)),
        ]),
        Line::from(vec![
            Span::styled("today $   ", Style::default().fg(theme::DIM)),
            Span::styled(format!("${:.2}", agg.today_cost), Style::default().fg(theme::GREEN)),
        ]),
        Line::from(vec![
            Span::styled("total     ", Style::default().fg(theme::DIM)),
            Span::styled(theme::human(agg.grand_tok), Style::default().fg(theme::TEXT)),
            Span::styled(format!("  ${:.2}", agg.grand_cost), Style::default().fg(theme::TEXT)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn bars_panel(f: &mut Frame, area: Rect, title: &'static str, items: &[(String, u64, f64)],
              grand: u64, model_names: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::FRAME))
        .title(Span::styled(title, Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)));
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
                Style::default().fg(theme::TEXT)),
            Span::styled(theme::bar(frac, bar_w, '█', '░'), Style::default().fg(theme::gauge_color(frac))),
            Span::styled(format!(" {:>2.0}%", frac * 100.0), Style::default().fg(theme::DIM)),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("  (no data)", Style::default().fg(theme::DIM))));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn footer(f: &mut Frame, area: Rect, usage: &UsageWindows, _n: usize, sort_label: &str) {
    let net = match usage.source {
        UsageSource::Live => Span::styled("net:LIVE", Style::default().fg(theme::GREEN)),
        UsageSource::Estimate => Span::styled("net:EST", Style::default().fg(theme::MAGENTA)),
    };
    let line = Line::from(vec![
        Span::styled(" [q]", Style::default().fg(theme::CYAN)),
        Span::styled("uit  ", Style::default().fg(theme::DIM)),
        Span::styled("[↑↓]", Style::default().fg(theme::CYAN)),
        Span::styled("select  ", Style::default().fg(theme::DIM)),
        Span::styled("[s]", Style::default().fg(theme::CYAN)),
        Span::styled(format!("ort:{sort_label}  "), Style::default().fg(theme::DIM)),
        Span::styled("[r]", Style::default().fg(theme::CYAN)),
        Span::styled("efresh   ", Style::default().fg(theme::DIM)),
        net,
    ]);
    f.render_widget(Paragraph::new(line).alignment(Alignment::Left), area);
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
