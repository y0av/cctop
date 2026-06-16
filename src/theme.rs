//! Cyberpunk-neon palette and ASCII/Unicode glyph helpers.
//! No Claude orange/cream — neon green + cyan with magenta accents on black.

use ratatui::style::Color;

pub const GREEN: Color = Color::Rgb(57, 255, 140); // primary neon
pub const CYAN: Color = Color::Rgb(0, 229, 255); // secondary
pub const MAGENTA: Color = Color::Rgb(255, 46, 196); // accent / danger
pub const TEXT: Color = Color::Rgb(198, 222, 232); // light readout
pub const DIM: Color = Color::Rgb(92, 104, 128); // empty / labels
pub const FRAME: Color = Color::Rgb(40, 70, 92); // borders

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Color a utilization fraction green → cyan → magenta as it climbs.
pub fn gauge_color(frac: f64) -> Color {
    if frac >= 0.85 {
        MAGENTA
    } else if frac >= 0.6 {
        CYAN
    } else {
        GREEN
    }
}

/// A unicode block sparkline of the (up to) last `width` values.
pub fn spark(values: &[u64], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let slice: &[u64] = if values.len() > width { &values[values.len() - width..] } else { values };
    let max = slice.iter().copied().max().unwrap_or(0);
    let mut s = String::with_capacity(width);
    // Left-pad with the baseline glyph so the spark is right-aligned at `width`.
    for _ in 0..width.saturating_sub(slice.len()) {
        s.push(SPARK[0]);
    }
    for &v in slice {
        let idx = if max == 0 { 0 } else { ((v as f64 / max as f64) * 7.0).round() as usize };
        s.push(SPARK[idx.min(7)]);
    }
    s
}

/// A filled/empty bar string (used inside bracketed gauges).
pub fn bar(frac: f64, width: usize, fill: char, empty: char) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    let mut s = String::with_capacity(width);
    for _ in 0..filled.min(width) {
        s.push(fill);
    }
    for _ in filled.min(width)..width {
        s.push(empty);
    }
    s
}

/// Human-readable token count: 1.2M / 982k / 42.
pub fn human(n: u64) -> String {
    let f = n as f64;
    if f >= 1e9 {
        format!("{:.1}B", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1}M", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0}k", f / 1e3)
    } else {
        n.to_string()
    }
}

/// Compact duration since start: MM:SS (<1h), Hh MMm (<1d), Dd HHh (>=1d).
pub fn uptime(secs: i64) -> String {
    let s = secs.max(0);
    if s < 3600 {
        format!("{:02}:{:02}", s / 60, s % 60)
    } else if s < 86400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d{:02}h", s / 86400, (s % 86400) / 3600)
    }
}

/// Compact "time until": 4d05h / 1h42m / 12m / now.
pub fn until(secs: i64) -> String {
    if secs <= 0 {
        return "now".to_string();
    }
    if secs >= 86400 {
        format!("{}d{:02}h", secs / 86400, (secs % 86400) / 3600)
    } else if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}m", (secs / 60).max(1))
    }
}
