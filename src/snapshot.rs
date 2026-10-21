//! Render the demo frame to a self-contained HTML file (faithful to the TUI,
//! via ratatui's TestBackend) so it can be screenshotted for the README.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::widgets::TableState;
use ratatui::Terminal;

use crate::model::PlanView;
use crate::theme::Theme;
use crate::{demo, ui};

pub fn html(width: u16, height: u16, th: &Theme, multi: bool) -> String {
    let account = demo::account();
    let agg = demo::aggregates();
    let agents = demo::agents(0);
    let sys = demo::sys();
    let plans = if multi {
        demo::multi_plans()
    } else {
        vec![PlanView { label: String::new(), usage: demo::usage() }]
    };
    let mut state = TableState::default();
    state.select(Some(0)); // highlight the top (busy) agent
    let detail = demo::detail(&agents[0]);
    let n_sources = if multi { 3 } else { 1 };
    let panels = ui::Panels::default();
    let series = demo::series(0, ui::graph_points(width, panels), 900);

    let mut term = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    term.draw(|f| {
        ui::draw(f, th, &account, &agg, &agents, &plans, &sys, Some(&detail), &mut state, "burn",
                 n_sources, None, &series, panels, None)
    })
    .expect("draw");
    wrap(&buffer_to_pre(term.backend().buffer()))
}

fn buffer_to_pre(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut x = 0;
        while x < area.width {
            let cell = match buf.cell((x, y)) {
                Some(c) => c,
                None => break,
            };
            let fg = cell.fg;
            let bold = cell.modifier.contains(Modifier::BOLD);
            // Coalesce a run of same-style cells into one span.
            let mut text = String::new();
            let mut xx = x;
            while xx < area.width {
                if let Some(c) = buf.cell((xx, y)) {
                    if c.fg != fg || c.modifier.contains(Modifier::BOLD) != bold {
                        break;
                    }
                    text.push_str(c.symbol());
                    xx += 1;
                } else {
                    break;
                }
            }
            let esc = escape(&text);
            let weight = if bold { ";font-weight:700" } else { "" };
            match css_color(fg) {
                Some(hex) => out.push_str(&format!("<span style=\"color:{hex}{weight}\">{esc}</span>")),
                None if bold => out.push_str(&format!("<span style=\"font-weight:700\">{esc}</span>")),
                None => out.push_str(&esc),
            }
            x = xx;
        }
        out.push('\n');
    }
    out
}

fn css_color(c: Color) -> Option<String> {
    match c {
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::White => Some("#ffffff".into()),
        Color::Black => Some("#000000".into()),
        _ => None,
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // Browser braille comes from a proportional fallback font with
            // pinprick dots; draw the dots instead, the way a terminal does.
            c @ '\u{2801}'..='\u{28ff}' => out.push_str(&braille_svg(c as u32 - 0x2800)),
            // ⎇ and the like may fall back too; pin each such glyph to one
            // cell so the columns line up. Box drawing and block elements are
            // in every mono font, left bare.
            c if !c.is_ascii() && !('\u{2500}'..='\u{259f}').contains(&c) => {
                out.push_str("<i>");
                out.push(c);
                out.push_str("</i>");
            }
            c => out.push(c),
        }
    }
    out
}

/// One braille cell as a 2×4 dot grid (dots 1-3/7 left column, 4-6/8 right).
fn braille_svg(bits: u32) -> String {
    const DOTS: [(u32, u8, u8); 8] =
        [(0x01, 0, 0), (0x02, 0, 1), (0x04, 0, 2), (0x40, 0, 3), (0x08, 1, 0), (0x10, 1, 1), (0x20, 1, 2), (0x80, 1, 3)];
    let mut svg = String::from(r#"<svg class="b" viewBox="0 0 2 4" preserveAspectRatio="none">"#);
    for (bit, x, y) in DOTS {
        if bits & bit != 0 {
            svg.push_str(&format!(r#"<circle cx="{x}.5" cy="{y}.5" r=".4"/>"#));
        }
    }
    svg.push_str("</svg>");
    svg
}

fn wrap(pre: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
  html,body{{margin:0;padding:0}}
  body{{min-height:100vh;display:flex;align-items:center;justify-content:center;
        background:radial-gradient(1100px 620px at 50% -8%, #161a26 0%, #0b0d14 70%);
        font-family:'DejaVu Sans Mono','Noto Sans Mono',monospace;}}
  .win{{border-radius:11px;overflow:hidden;border:1px solid #20283a;
        box-shadow:0 26px 70px rgba(0,0,0,.65);}}
  .bar{{height:30px;background:#11141d;display:flex;align-items:center;gap:8px;
        padding:0 13px;border-bottom:1px solid #20283a;}}
  .dot{{width:11px;height:11px;border-radius:50%}}
  .t{{margin-left:8px;color:#5c6880;font-size:12px}}
  pre{{margin:0;padding:14px 16px;background:#0a0a0f;color:#c6dee8;
       font-size:15px;line-height:1.34;white-space:pre}}
  pre i{{font-style:normal;display:inline-block;width:1ch;text-align:center}}
  pre svg.b{{display:inline-block;width:1ch;height:1.34em;vertical-align:top;fill:currentColor}}
</style></head><body><div class="win">
  <div class="bar"><span class="dot" style="background:#ff5f57"></span>
    <span class="dot" style="background:#febc2e"></span>
    <span class="dot" style="background:#28c840"></span>
    <span class="t">cctop — claude usage monitor</span></div>
  <pre>{pre}</pre>
</div><script>
  // A fallback glyph wider than its cell (⎇ in DejaVu Sans) would spill into
  // the next one; squeeze it back to the cell width instead.
  for (const el of document.querySelectorAll('pre i')) {{
    const g = document.createElement('span');
    g.textContent = el.textContent;
    el.replaceChildren(g);
    const cell = el.getBoundingClientRect().width, w = g.getBoundingClientRect().width;
    if (w > cell * 1.05) g.style.cssText = `display:inline-block;transform-origin:0 50%;transform:scaleX(${{cell / w}})`;
  }}
</script></body></html>"#
    )
}
