//! Render the demo frame to a self-contained HTML file (faithful to the TUI,
//! via ratatui's TestBackend) so it can be screenshotted for the README.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::widgets::TableState;
use ratatui::Terminal;

use crate::{demo, ui};

pub fn html(width: u16, height: u16) -> String {
    let account = demo::account();
    let agg = demo::aggregates();
    let agents = demo::agents(0);
    let usage = demo::usage();
    let mut state = TableState::default();
    state.select(Some(0)); // highlight the top (busy) agent

    let mut term = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    term.draw(|f| ui::draw(f, &account, &agg, &agents, &usage, &mut state, "burn", 1)).expect("draw");
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
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
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
</style></head><body><div class="win">
  <div class="bar"><span class="dot" style="background:#ff5f57"></span>
    <span class="dot" style="background:#febc2e"></span>
    <span class="dot" style="background:#28c840"></span>
    <span class="t">cctop — claude usage monitor</span></div>
  <pre>{pre}</pre>
</div></body></html>"#
    )
}
