//! All ratatui rendering. Functions take explicit field references (not `&App`)
//! so disjoint borrows let the agents table render mutably while the rest reads.

use chrono::Local;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::account::Account;
use crate::model::{AgentDetail, PlanView, Spend, UsageSource, UsageWindows, Window};
use crate::procs::{self, SysStats};
use crate::sessions::LiveAgent;
use crate::theme::Theme;
use crate::theme;
use crate::transcripts::Aggregates;

/// Minimum terminal width before the drill-down panel is offered.
const DETAIL_MIN_W: u16 = 96;
const DETAIL_W: u16 = 40;
/// Lines taken by the system strip under the header.
const SYS_H: u16 = 2;
/// Floor for the agents table's flexible NAME column.
const NAME_MIN: u16 = 14;

/// How wide the single `Min` column of a table ends up: everything the fixed
/// columns, the panel borders, the selection marker and the inter-column
/// spacing don't claim. Mirrors what ratatui's layout will do, so a name can be
/// truncated to exactly the room it has rather than to a guessed constant.
fn flex_width(area_w: u16, widths: &[Constraint]) -> u16 {
    let fixed: u16 = widths
        .iter()
        .map(|c| match c {
            Constraint::Length(n) => *n,
            _ => 0,
        })
        .sum();
    let gaps = widths.len().saturating_sub(1) as u16; // one space between columns
    area_w.saturating_sub(2 + 1 + gaps + fixed).max(NAME_MIN) // borders + "▶"
}

#[allow(clippy::too_many_arguments)]
pub fn draw(f: &mut Frame, th: &Theme, account: &Account, agg: &Aggregates, agents: &[LiveAgent],
            plans: &[PlanView], sys: &SysStats, detail: Option<&AgentDetail>,
            state: &mut TableState, sort_label: &str, n_sources: usize, update: Option<&str>) {
    let gauge_h = gauges_height(plans);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),       // header
            Constraint::Length(SYS_H),   // system strip
            Constraint::Length(gauge_h), // gauges
            Constraint::Min(6),          // agents
            Constraint::Length(8),       // bottom row
            Constraint::Length(1),       // footer
        ])
        .split(f.area());

    header(f, th, chunks[0], account, update);
    sys_strip(f, th, chunks[1], sys, agents);
    gauges(f, th, chunks[2], plans);
    match detail {
        Some(d) if chunks[3].width >= DETAIL_MIN_W => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(40), Constraint::Length(DETAIL_W)])
                .split(chunks[3]);
            agents_table(f, th, cols[0], agents, sys, state, n_sources > 1, true);
            detail_panel(f, th, cols[1], d, sys);
        }
        _ => agents_table(f, th, chunks[3], agents, sys, state, n_sources > 1, false),
    }
    bottom(f, th, chunks[4], agg);
    footer(f, th, chunks[5], plans, n_sources, sort_label);
}

/// CPU cell color: one full core is notable, half the machine (capped at four
/// cores) is the agent to look at.
fn cpu_color(th: &Theme, cpu: f32, ncpu: usize) -> ratatui::style::Color {
    let crit = ((ncpu as f32 * 100.0) / 2.0).clamp(100.0, 400.0);
    if cpu >= crit {
        th.crit
    } else if cpu >= 100.0 {
        th.warn
    } else if cpu < 1.0 {
        th.dim
    } else {
        th.text
    }
}

fn mem_color(th: &Theme, kb: u64, total_kb: u64) -> ratatui::style::Color {
    let frac = kb as f64 / total_kb.max(1) as f64;
    if frac >= 0.25 {
        th.crit
    } else if frac >= 0.10 {
        th.warn
    } else {
        th.text
    }
}

/// Two lines under the header: machine gauges, then what (if anything) is
/// wrong and which agent to blame.
fn sys_strip(f: &mut Frame, th: &Theme, area: Rect, s: &SysStats, agents: &[LiveAgent]) {
    let lbl = |t: &str| Span::styled(t.to_string(), Style::default().fg(th.dim));
    let bar = |frac: f64, w: usize| -> Vec<Span<'static>> {
        let (fill, empty) = th.gauge_parts(frac, w);
        let c = th.gauge_color(frac);
        vec![
            Span::styled("[", Style::default().fg(th.dim)),
            Span::styled(fill, Style::default().fg(c)),
            Span::styled(empty, Style::default().fg(th.dim)),
            Span::styled("]", Style::default().fg(th.dim)),
        ]
    };
    let bw = if area.width >= 150 { 10 } else { 6 };

    // Segments in display order, each with a priority; on a narrow terminal
    // the least important ones drop out instead of the line being clipped.
    let mut segs: Vec<(u8, Vec<Span<'static>>)> = Vec::new();
    let cpu_frac = s.cpu as f64 / 100.0;
    let mut seg = vec![lbl("cpu ")];
    seg.extend(bar(cpu_frac, bw));
    seg.push(Span::styled(
        format!(" {:.0}%", s.cpu),
        Style::default().fg(th.gauge_color(cpu_frac)).add_modifier(Modifier::BOLD),
    ));
    seg.push(lbl(&format!(" {}c", s.ncpu)));
    segs.push((9, seg));

    let mem_frac = s.mem_used_kb as f64 / s.mem_total_kb.max(1) as f64;
    let mut seg = vec![lbl("mem ")];
    seg.extend(bar(mem_frac, bw));
    seg.push(Span::styled(
        format!(" {}/{}", procs::fmt_kb(s.mem_used_kb), procs::fmt_kb(s.mem_total_kb)),
        Style::default().fg(th.gauge_color(mem_frac)).add_modifier(Modifier::BOLD),
    ));
    segs.push((9, seg));
    if s.swap_used_kb > 0 {
        let sf = s.swap_used_kb as f64 / s.swap_total_kb.max(1) as f64;
        segs.push((if sf >= 0.5 { 8 } else { 4 }, vec![
            lbl("swap "),
            Span::styled(
                format!("{}/{}", procs::fmt_kb(s.swap_used_kb), procs::fmt_kb(s.swap_total_kb)),
                Style::default().fg(th.gauge_color(sf)),
            ),
        ]));
    }
    if let Some(g) = &s.gpu {
        let gf = g.busy.unwrap_or(0.0) as f64 / 100.0;
        let mut seg = vec![lbl("gpu ")];
        seg.extend(bar(gf, bw));
        seg.push(Span::styled(
            g.busy.map(|b| format!(" {b:.0}%")).unwrap_or_else(|| " —".into()),
            Style::default().fg(th.gauge_color(gf)).add_modifier(Modifier::BOLD),
        ));
        segs.push((7, seg));
        if g.vram_total_kb > 0 {
            let vf = g.vram_used_kb as f64 / g.vram_total_kb as f64;
            segs.push((3, vec![
                lbl("vram "),
                Span::styled(
                    format!("{}/{}", procs::fmt_kb(g.vram_used_kb), procs::fmt_kb(g.vram_total_kb)),
                    Style::default().fg(th.gauge_color(vf)),
                ),
            ]));
        }
        if g.gtt_used_kb > 0 && g.gtt_total_kb > 0 {
            segs.push((2, vec![lbl("gtt "), Span::styled(procs::fmt_kb(g.gtt_used_kb), Style::default().fg(th.text))]));
        }
    }
    if let (Some(c), Some(m), Some(io)) = (s.psi_cpu, s.psi_mem, s.psi_io) {
        // Pressure: share of the last 10 s something stalled waiting.
        let pc = |v: f32| if v >= 20.0 { th.crit } else if v >= 5.0 { th.warn } else { th.dim };
        let hot = c.max(m).max(io) >= 5.0;
        segs.push((if hot { 8 } else { 5 }, vec![
            lbl("stall "),
            Span::styled(format!("cpu {c:.0}%"), Style::default().fg(pc(c))),
            Span::styled(format!(" mem {m:.0}%"), Style::default().fg(pc(m))),
            Span::styled(format!(" io {io:.0}%"), Style::default().fg(pc(io))),
        ]));
    }
    segs.push((1, vec![lbl(&format!("load {:.1}", s.load.0))]));

    let width = |v: &[(u8, Vec<Span<'static>>)]| -> usize {
        5 + v.iter().map(|(_, sp)| 2 + sp.iter().map(|x| x.content.chars().count()).sum::<usize>()).sum::<usize>()
    };
    while width(&segs) > area.width as usize && segs.len() > 1 {
        let (idx, _) = segs.iter().enumerate().min_by_key(|(i, (p, _))| (*p, usize::MAX - i)).unwrap();
        segs.remove(idx);
    }
    let mut l1 = vec![Span::styled(" SYS ", Style::default().fg(th.secondary).add_modifier(Modifier::BOLD))];
    for (i, (_, sp)) in segs.into_iter().enumerate() {
        if i > 0 {
            l1.push(Span::raw("  "));
        }
        l1.extend(sp);
    }

    let l2 = Line::from(alerts(th, s, agents));
    let lines = vec![Line::from(l1), l2];
    f.render_widget(Paragraph::new(lines), area);
}

/// The diagnosis line: the hungriest agent and what inside it is hungry, plus
/// orphans and machine-level pressure.
fn alerts(th: &Theme, s: &SysStats, agents: &[LiveAgent]) -> Vec<Span<'static>> {
    let mut out = vec![Span::raw("     ")];
    let sep = |out: &mut Vec<Span<'static>>| {
        if out.len() > 1 {
            out.push(Span::styled("  ·  ", Style::default().fg(th.dim)));
        }
    };

    let hottest = agents
        .iter()
        .filter(|a| a.status != "orphan")
        .max_by(|a, b| a.cpu.partial_cmp(&b.cpu).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(a) = hottest {
        let c = cpu_color(th, a.cpu, s.ncpu);
        let hot = a.cpu >= 100.0;
        out.push(Span::styled(
            if hot { "▲ hottest " } else { "top " },
            Style::default().fg(if hot { c } else { th.dim }),
        ));
        out.push(Span::styled(
            truncate(if a.name.is_empty() { &a.project } else { &a.name }, 28),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        ));
        out.push(Span::styled(
            format!(" {} {}", procs::fmt_cores(a.cpu), procs::fmt_kb(a.rss_kb)),
            Style::default().fg(c).add_modifier(Modifier::BOLD),
        ));
        if let Some(p) = a.top.first().filter(|_| a.cpu >= 5.0) {
            out.push(Span::styled(
                format!(" ← {}", truncate(&p.cmd, 32)),
                Style::default().fg(th.text),
            ));
        }
    }

    let orphans: Vec<&LiveAgent> = agents.iter().filter(|a| a.status == "orphan").collect();
    if !orphans.is_empty() {
        sep(&mut out);
        let n: usize = orphans.iter().map(|a| a.nprocs).sum();
        let cpu: f32 = orphans.iter().map(|a| a.cpu).sum();
        out.push(Span::styled(
            format!("† {n} orphan procs from dead sessions, {}", procs::fmt_cores(cpu)),
            Style::default().fg(th.crit).add_modifier(Modifier::BOLD),
        ));
    }

    let warn = |out: &mut Vec<Span<'static>>, text: String| {
        sep(out);
        out.push(Span::styled(text, Style::default().fg(th.crit).add_modifier(Modifier::BOLD)));
    };
    if s.psi_mem.unwrap_or(0.0) >= 10.0 {
        warn(&mut out, "memory pressure: system is thrashing".into());
    } else if s.mem_used_kb as f64 / s.mem_total_kb.max(1) as f64 >= 0.9 {
        warn(&mut out, "RAM nearly full".into());
    }
    if s.psi_cpu.unwrap_or(0.0) >= 40.0 || s.cpu >= 95.0 {
        warn(&mut out, "CPU saturated".into());
    }
    if s.psi_io.unwrap_or(0.0) >= 30.0 {
        warn(&mut out, "IO stalls".into());
    }
    if s.swap_total_kb > 0 && s.swap_used_kb as f64 / s.swap_total_kb as f64 >= 0.5 {
        warn(&mut out, "swap over half full".into());
    }
    if let Some(g) = &s.gpu {
        if g.vram_total_kb > 0 && g.vram_used_kb as f64 / g.vram_total_kb as f64 >= 0.95 {
            // On an APU the VRAM carve-out is tiny and spills into system RAM
            // (GTT), so this only matters when GTT is absent or also full.
            if g.gtt_total_kb == 0 || g.gtt_used_kb as f64 / g.gtt_total_kb as f64 >= 0.8 {
                warn(&mut out, "GPU memory full".into());
            }
        }
    }
    out
}

/// Height of the plan block for the current set of accounts — shared with the
/// mouse hit-testing below so clicks and pixels agree.
pub fn gauges_height(plans: &[PlanView]) -> u16 {
    let multi = plans.len() > 1;
    let lines: u16 = plans.iter().map(|p| plan_lines(&p.usage, multi)).sum();
    (2 + lines).clamp(6, 18)
}

/// Which visible agents-table row (0-based, pre-scroll) a click at terminal
/// row `y` lands on, if any.
pub fn agents_row_at(size: ratatui::layout::Size, plans: &[PlanView], y: u16) -> Option<usize> {
    let top = 1 + SYS_H + gauges_height(plans); // header + system strip + plan block
    let table_h = size.height.saturating_sub(top + 8 + 1); // bottom row + footer
    // First two rows are the block border and the column header; the last is
    // the bottom border.
    if table_h < 4 || y < top + 2 || y >= top + table_h - 1 {
        return None;
    }
    Some((y - top - 2) as usize)
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

fn header(f: &mut Frame, th: &Theme, area: Rect, account: &Account, update: Option<&str>) {
    let tier = tier_label(&account.subscription, &account.rate_limit_tier);
    let mut left = vec![
        Span::styled("CCTOP", Style::default().fg(th.primary).add_modifier(Modifier::BOLD)),
        Span::styled(concat!(" v", env!("CARGO_PKG_VERSION")), Style::default().fg(th.dim)),
    ];
    if let Some(tag) = update {
        left.push(Span::styled(
            format!(" ↑{tag}"),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        ));
    }
    left.extend([
        Span::styled("  claude ", Style::default().fg(th.dim)),
        Span::styled(tier.clone(), Style::default().fg(th.secondary).add_modifier(Modifier::BOLD)),
    ]);

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
    // Predictive warning: at the current climb rate this window caps out
    // before it resets.
    if let Some(eta) = win.eta_secs {
        spans.push(Span::styled(
            format!("  ▲cap {}", theme::until(eta)),
            Style::default().fg(th.crit).add_modifier(Modifier::BOLD),
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

#[allow(clippy::too_many_arguments)]
fn agents_table(f: &mut Frame, th: &Theme, area: Rect, agents: &[LiveAgent], sys: &SysStats,
                state: &mut TableState, show_acc: bool, compact: bool) {
    let busy = agents.iter().filter(|a| a.status == "busy").count();
    let total_cpu: f32 = agents.iter().map(|a| a.cpu).sum();
    let title = Line::from(vec![
        Span::styled(" LIVE AGENTS ", Style::default().fg(th.secondary).add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("{} live · {} busy · {} ", agents.len(), busy, procs::fmt_cores(total_cpu)),
            Style::default().fg(th.dim),
        ),
    ]);
    let block = panel(th, title);
    let show_gpu = sys.gpu.is_some() || agents.iter().any(|a| a.gpu.is_some());

    // Column set. With the detail panel open the table is narrow: PID, MODEL,
    // UP and the sparkline move into the panel and SESSION/REPO keep the room.
    // CPU/MEM/GPU always stay — they're what you scan for a runaway agent.
    let wide = !compact && area.width >= 118;
    let mut cols: Vec<(&str, Constraint)> = Vec::new();
    if !compact {
        cols.push(("PID", Constraint::Length(7)));
    }
    cols.push(("SESSION", Constraint::Min(NAME_MIN)));
    cols.push(("REPO", Constraint::Length(if compact { 15 } else { 20 })));
    if show_acc {
        cols.push(("ACC", Constraint::Length(8)));
    }
    if !compact {
        cols.push(("MODEL", Constraint::Length(11)));
    }
    cols.push(("ST", Constraint::Length(6)));
    if !compact {
        cols.push(("UP", Constraint::Length(7)));
    }
    cols.push(("CORES", Constraint::Length(5)));
    cols.push(("MEM", Constraint::Length(6)));
    if show_gpu {
        cols.push(("GPU", Constraint::Length(4)));
    }
    if wide {
        cols.push(("BURN", Constraint::Length(9)));
    }
    if !compact {
        cols.push(("tok/s", Constraint::Length(6)));
    }

    let header = Row::new(cols.iter().map(|(h, _)| {
        // Right-align numeric headers over their right-aligned values.
        if matches!(*h, "CORES" | "MEM" | "GPU") {
            Cell::from(Line::from(*h).alignment(Alignment::Right))
        } else {
            Cell::from(*h)
        }
    }))
    .style(Style::default().fg(th.dim).add_modifier(Modifier::BOLD));
    let widths: Vec<Constraint> = cols.iter().map(|(_, c)| *c).collect();
    let name_w = flex_width(area.width, &widths) as usize;
    let repo_w = if compact { 15 } else { 20 };

    let right = |s: String, color| Cell::from(Line::from(s).alignment(Alignment::Right)).style(Style::default().fg(color));

    let rows = agents.iter().map(|a| {
        let busy = a.status == "busy";
        let st = match a.status.as_str() {
            "busy" => Span::styled("●busy", Style::default().fg(th.primary)),
            "idle" => Span::styled("○idle", Style::default().fg(th.dim)),
            "orphan" => Span::styled("†orph", Style::default().fg(th.crit).add_modifier(Modifier::BOLD)),
            "unreg" => Span::styled("?unrg", Style::default().fg(th.warn)),
            other => Span::styled(format!("·{other}"), Style::default().fg(th.dim)),
        };
        let burn_color = if a.burn_tps > 0.5 { th.primary } else { th.dim };
        // A chosen name (agent- or user-set) gets the accent; generated titles
        // and the cwd-derived default stay quiet. Sessions from older releases
        // have no name at all, so fall back to the project.
        let (name, name_style) = if a.status == "orphan" {
            (a.name.as_str(), Style::default().fg(th.crit))
        } else if a.name.is_empty() {
            (a.project.as_str(), Style::default().fg(th.secondary))
        } else if a.named {
            (a.name.as_str(), Style::default().fg(th.accent).add_modifier(Modifier::BOLD))
        } else {
            (a.name.as_str(), Style::default().fg(th.secondary))
        };
        let mut cells = Vec::new();
        if !compact {
            cells.push(Cell::from(a.pid.to_string()).style(Style::default().fg(th.text)));
        }
        cells.push(Cell::from(truncate(name, name_w)).style(name_style));
        cells.push(Cell::from(truncate_left(&a.repo, repo_w)).style(Style::default().fg(th.dim)));
        if show_acc {
            cells.push(Cell::from(truncate(&a.account, 7)).style(Style::default().fg(th.dim)));
        }
        if !compact {
            cells.push(Cell::from(model_short(&a.model)).style(Style::default().fg(th.text)));
        }
        cells.push(Cell::from(st));
        if !compact {
            cells.push(Cell::from(theme::uptime(a.uptime_secs)).style(Style::default().fg(th.text)));
        }
        cells.push(right(procs::fmt_cpu(a.cpu), cpu_color(th, a.cpu, sys.ncpu)));
        cells.push(right(procs::fmt_kb(a.rss_kb), mem_color(th, a.rss_kb, sys.mem_total_kb)));
        if show_gpu {
            let (g, c) = match a.gpu {
                Some(g) if g >= 50.0 => (format!("{g:.0}%"), th.crit),
                Some(g) if g >= 1.0 => (format!("{g:.0}%"), th.warn),
                Some(g) => (format!("{g:.0}%"), th.dim),
                None => ("—".into(), th.dim),
            };
            cells.push(right(g, c));
        }
        if wide {
            cells.push(Cell::from(Span::styled(theme::spark(&a.burn_hist, 8), Style::default().fg(burn_color))));
        }
        if !compact {
            cells.push(
                Cell::from(theme::human(a.burn_tps.round() as u64))
                    .style(Style::default().fg(if busy { th.primary } else { th.text })),
            );
        }
        Row::new(cells)
    });

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(
            Style::default().bg(th.frame).fg(th.text).add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶");
    f.render_stateful_widget(table, area, state);
}

/// Drill-down for the selected agent: what its process tree is doing to the
/// machine, then session totals, cost and identity.
fn detail_panel(f: &mut Frame, th: &Theme, area: Rect, d: &AgentDetail, sys: &SysStats) {
    let head = if d.name.is_empty() { &d.project } else { &d.name };
    // Borders plus the " ▶ " prefix and the trailing space.
    let head_w = area.width.saturating_sub(6) as usize;
    let title = Line::from(Span::styled(
        format!(" ▶ {} ", truncate(head, head_w)),
        Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
    ));
    let block = panel(th, title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;

    let lbl = |s: &str| Span::styled(format!("{s:<8}"), Style::default().fg(th.dim));
    let busy = d.status == "busy";
    let st_color = match d.status.as_str() {
        "busy" => th.primary,
        "orphan" => th.crit,
        "unreg" => th.warn,
        _ => th.dim,
    };
    let idle = match d.idle_secs {
        Some(s) if s < 90 => format!("{s}s ago"),
        Some(s) => format!("{} ago", theme::until(s)),
        None => "—".to_string(),
    };
    let cache = d.tok.cache_read + d.tok.cache_write_5m + d.tok.cache_write_1h;

    let mut lines = vec![
        Line::from(vec![
            lbl("state"),
            Span::styled(d.status.clone(), Style::default().fg(st_color)),
            Span::styled(format!(" · up {} · pid {}", theme::uptime(d.uptime_secs), d.pid),
                Style::default().fg(th.dim)),
        ]),
        Line::from(vec![
            lbl("load"),
            Span::styled(procs::fmt_cores(d.cpu),
                Style::default().fg(cpu_color(th, d.cpu, sys.ncpu)).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}", procs::fmt_kb(d.rss_kb)),
                Style::default().fg(mem_color(th, d.rss_kb, sys.mem_total_kb)).add_modifier(Modifier::BOLD)),
            Span::styled(
                match d.gpu {
                    Some(g) if d.gpu_mem_kb > 0 => format!(" gpu {g:.0}% {}", procs::fmt_kb(d.gpu_mem_kb)),
                    Some(g) => format!(" gpu {g:.0}%"),
                    None => String::new(),
                },
                Style::default().fg(th.text),
            ),
        ]),
        Line::from(vec![
            lbl("procs"),
            Span::styled(d.nprocs.to_string(), Style::default().fg(th.text)),
            Span::styled(
                if d.detached > 0 { format!(" ({} detached)", d.detached) } else { String::new() },
                Style::default().fg(th.warn),
            ),
        ]),
    ];
    // The processes doing the work, hottest first.
    for p in d.top.iter().take(4) {
        let place = place_tag(&p.place, &d.repo);
        let cmd_w = w.saturating_sub(14 + place.chars().count());
        lines.push(Line::from(vec![
            Span::styled(format!("{:>5}", procs::fmt_cpu(p.cpu)), Style::default().fg(cpu_color(th, p.cpu, sys.ncpu))),
            Span::styled(format!(" {:>6} ", procs::fmt_kb(p.rss_kb)), Style::default().fg(th.dim)),
            Span::styled(
                match p.gpu {
                    Some(g) if g >= 1.0 => format!("g{g:.0}% "),
                    _ => String::new(),
                },
                Style::default().fg(th.warn),
            ),
            Span::styled(place, Style::default().fg(th.secondary)),
            Span::styled(truncate(&p.cmd, cmd_w), Style::default().fg(th.text)),
        ]));
    }
    if !d.title.is_empty() && d.title != d.name {
        lines.push(Line::from(vec![lbl("title"), Span::styled(truncate(&d.title, w.saturating_sub(8)), Style::default().fg(th.text))]));
    }
    lines.push(Line::from(vec![
        lbl("repo"),
        Span::styled(truncate_left(&d.repo, w.saturating_sub(8)), Style::default().fg(th.secondary)),
    ]));
    lines.extend([
        Line::from(vec![
            lbl("model"),
            Span::styled(model_short(&d.model), Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![lbl("active"), Span::styled(idle, Style::default().fg(th.text))]),
        Line::from(vec![
            lbl("burn"),
            Span::styled(theme::spark(&d.burn_hist, 8), Style::default().fg(if busy { th.primary } else { th.dim })),
            Span::styled(format!(" {} tok/s", theme::human(d.burn_tps.round() as u64)),
                Style::default().fg(th.text)),
        ]),
        Line::from(vec![
            lbl("session"),
            Span::styled(theme::human(d.tok.total()), Style::default().fg(th.primary).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", theme::money(d.cost)), Style::default().fg(th.text)),
        ]),
        Line::from(vec![
            lbl("in/out"),
            Span::styled(
                format!("{} / {}", theme::human(d.tok.input), theme::human(d.tok.output)),
                Style::default().fg(th.text),
            ),
            Span::styled(format!(" · c{}", theme::human(cache)), Style::default().fg(th.dim)),
        ]),
        Line::from(vec![
            lbl("account"),
            Span::styled(d.account.clone(), Style::default().fg(th.secondary)),
            Span::styled(
                format!("  {} {}", d.handle, d.session_id.chars().take(8).collect::<String>()),
                Style::default().fg(th.dim),
            ),
        ]),
        Line::from(vec![
            lbl("cwd"),
            Span::styled(truncate_left(&d.cwd, w.saturating_sub(9)),
                Style::default().fg(th.dim)),
        ]),
    ]);
    f.render_widget(Paragraph::new(lines), inner);
}

fn bottom(f: &mut Frame, th: &Theme, area: Rect, agg: &Aggregates) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(30), Constraint::Percentage(30)])
        .split(area);

    activity_panel(f, th, cols[0], agg);
    bars_panel(f, th, cols[1], " BY MODEL ", &agg.by_model, agg.grand_tok, true);
    bars_panel(f, th, cols[2], " BY PROJECT ", &agg.by_project, agg.grand_tok, false);
}

/// 24-hour token column chart with the headline numbers alongside.
fn activity_panel(f: &mut Frame, th: &Theme, area: Rect, agg: &Aggregates) {
    let block = panel(
        th,
        Line::from(Span::styled(" ACTIVITY 24h ", Style::default().fg(th.secondary).add_modifier(Modifier::BOLD))),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let stats_w: u16 = 20;
    let chart_w = inner.width.saturating_sub(stats_w + 1) as usize;
    let chart_h = inner.height as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(chart_h);
    for row in theme::columns(&agg.buckets24, chart_w.max(1), chart_h.max(1)) {
        // Highlight the current hour (rightmost column).
        let split = row.chars().count().saturating_sub(1);
        let head: String = row.chars().take(split).collect();
        let tail: String = row.chars().skip(split).collect();
        lines.push(Line::from(vec![
            Span::styled(head, Style::default().fg(th.secondary)),
            Span::styled(tail, Style::default().fg(th.primary)),
        ]));
    }
    let chart_area = Rect { width: chart_w as u16, ..inner };
    f.render_widget(Paragraph::new(lines), chart_area);

    let main = agg.main_tok;
    let denom = (main + agg.agent_tok).max(1);
    let main_pct = main as f64 / denom as f64 * 100.0;
    let stat = |label: &str, val: String, color| {
        Line::from(vec![
            Span::styled(format!("{label:<6}"), Style::default().fg(th.dim)),
            Span::styled(val, Style::default().fg(color)),
        ])
    };
    let stats = vec![
        stat("today", format!("{} {}", theme::human(agg.today_tok), theme::money(agg.today_cost)), th.primary),
        stat("7d", format!("{} {}", theme::human(agg.last7d_tok), theme::money(agg.last7d_cost)), th.text),
        stat("total", format!("{} {}", theme::human(agg.grand_tok), theme::money(agg.grand_cost)), th.text),
        stat("split", format!("{main_pct:.0}% main"), th.text),
        Line::from(vec![
            Span::raw("      "),
            Span::styled(format!("{:.0}% agents", 100.0 - main_pct), Style::default().fg(th.accent)),
        ]),
    ];
    let stats_area = Rect {
        x: inner.x + inner.width.saturating_sub(stats_w),
        width: stats_w.min(inner.width),
        ..inner
    };
    f.render_widget(Paragraph::new(stats), stats_area);
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
        Span::styled("[d]", Style::default().fg(th.secondary)),
        Span::styled("etail  ", Style::default().fg(th.dim)),
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

/// Where a process runs, relative to its agent: empty when it's the agent's
/// own folder, otherwise the part of the repo label that differs
/// (`wt-mt-1-5/backend `), with a trailing space for layout.
pub fn place_tag(place: &str, agent_repo: &str) -> String {
    if place.is_empty() || place == agent_repo {
        return String::new();
    }
    let repo = agent_repo.split('/').next().unwrap_or("");
    let rel = place.strip_prefix(repo).and_then(|r| r.strip_prefix('/')).unwrap_or(place);
    format!("{rel} ")
}

fn model_short(m: &str) -> String {
    m.strip_prefix("claude-").unwrap_or(m).to_string()
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

/// Keep the tail of a path-like string: "…/Dev/claude_test".
fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        let mut t = String::from("…");
        t.extend(s.chars().skip(n - max.saturating_sub(1)));
        t
    }
}
