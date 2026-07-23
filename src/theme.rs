//! Theme system: each theme is a complete look — full color palette, border
//! glyph family, and the fill/empty characters for gauges and ratio bars.
//! Also home to the shared glyph/format helpers (sparklines, humanized counts).

use std::path::PathBuf;

use ratatui::style::Color;
use ratatui::widgets::BorderType;

pub struct Theme {
    pub name: &'static str,
    /// Headline accent: logo, "live" tag, hero numbers.
    pub primary: Color,
    /// Secondary accent: panel titles, key hints, project names.
    pub secondary: Color,
    /// Hot accent: selection, estimate tag, the subagent share.
    pub accent: Color,
    pub text: Color,
    pub dim: Color,
    pub frame: Color,
    /// Gauge ramp: fine → getting warm → nearly out.
    pub ok: Color,
    pub warn: Color,
    pub crit: Color,
    pub border: BorderType,
    pub gauge_fill: char,
    pub gauge_empty: char,
    pub bar_fill: char,
    pub bar_empty: char,
}

pub static THEMES: &[Theme] = &[
    // Cyberpunk neon — green/cyan with magenta accents on black. The classic.
    Theme {
        name: "cyber",
        primary: Color::Rgb(57, 255, 140),
        secondary: Color::Rgb(0, 229, 255),
        accent: Color::Rgb(255, 46, 196),
        text: Color::Rgb(198, 222, 232),
        dim: Color::Rgb(92, 104, 128),
        frame: Color::Rgb(40, 70, 92),
        ok: Color::Rgb(57, 255, 140),
        warn: Color::Rgb(0, 229, 255),
        crit: Color::Rgb(255, 46, 196),
        border: BorderType::Plain,
        gauge_fill: '|',
        gauge_empty: '·',
        bar_fill: '█',
        bar_empty: '░',
    },
    // Warm Anthropic terracotta & cream, rounded corners.
    Theme {
        name: "claude",
        primary: Color::Rgb(230, 140, 95),
        secondary: Color::Rgb(212, 162, 127),
        accent: Color::Rgb(196, 93, 60),
        text: Color::Rgb(237, 230, 219),
        dim: Color::Rgb(140, 124, 110),
        frame: Color::Rgb(110, 86, 70),
        ok: Color::Rgb(230, 140, 95),
        warn: Color::Rgb(255, 183, 77),
        crit: Color::Rgb(229, 77, 58),
        border: BorderType::Rounded,
        gauge_fill: '█',
        gauge_empty: '░',
        bar_fill: '█',
        bar_empty: '░',
    },
    // Green phosphor CRT. There is no spoon.
    Theme {
        name: "matrix",
        primary: Color::Rgb(0, 255, 65),
        secondary: Color::Rgb(0, 220, 90),
        accent: Color::Rgb(215, 255, 215),
        text: Color::Rgb(150, 235, 160),
        dim: Color::Rgb(0, 122, 48),
        frame: Color::Rgb(0, 95, 40),
        ok: Color::Rgb(0, 255, 65),
        warn: Color::Rgb(160, 255, 90),
        crit: Color::Rgb(240, 255, 240),
        border: BorderType::Plain,
        gauge_fill: '█',
        gauge_empty: '·',
        bar_fill: '█',
        bar_empty: '░',
    },
    // The classic purple/pink dark palette.
    Theme {
        name: "dracula",
        primary: Color::Rgb(189, 147, 249),
        secondary: Color::Rgb(139, 233, 253),
        accent: Color::Rgb(255, 121, 198),
        text: Color::Rgb(248, 248, 242),
        dim: Color::Rgb(98, 114, 164),
        frame: Color::Rgb(68, 71, 90),
        ok: Color::Rgb(80, 250, 123),
        warn: Color::Rgb(241, 250, 140),
        crit: Color::Rgb(255, 85, 85),
        border: BorderType::Rounded,
        gauge_fill: '▓',
        gauge_empty: '░',
        bar_fill: '█',
        bar_empty: '░',
    },
    // Calm arctic frost.
    Theme {
        name: "nord",
        primary: Color::Rgb(136, 192, 208),
        secondary: Color::Rgb(129, 161, 193),
        accent: Color::Rgb(180, 142, 173),
        text: Color::Rgb(236, 239, 244),
        dim: Color::Rgb(97, 110, 136),
        frame: Color::Rgb(76, 86, 106),
        ok: Color::Rgb(163, 190, 140),
        warn: Color::Rgb(235, 203, 139),
        crit: Color::Rgb(191, 97, 106),
        border: BorderType::Plain,
        gauge_fill: '━',
        gauge_empty: '─',
        bar_fill: '█',
        bar_empty: '░',
    },
    // Hot pink sunset, double-line frames.
    Theme {
        name: "synthwave",
        primary: Color::Rgb(255, 126, 219),
        secondary: Color::Rgb(54, 249, 246),
        accent: Color::Rgb(254, 222, 93),
        text: Color::Rgb(255, 245, 249),
        dim: Color::Rgb(122, 92, 141),
        frame: Color::Rgb(109, 59, 115),
        ok: Color::Rgb(54, 249, 246),
        warn: Color::Rgb(254, 222, 93),
        crit: Color::Rgb(254, 68, 80),
        border: BorderType::Double,
        gauge_fill: '▚',
        gauge_empty: '░',
        bar_fill: '█',
        bar_empty: '▒',
    },
    // Grayscale ASCII brutalism.
    Theme {
        name: "mono",
        primary: Color::Rgb(255, 255, 255),
        secondary: Color::Rgb(200, 200, 200),
        accent: Color::Rgb(255, 255, 255),
        text: Color::Rgb(220, 220, 220),
        dim: Color::Rgb(120, 120, 120),
        frame: Color::Rgb(90, 90, 90),
        ok: Color::Rgb(170, 170, 170),
        warn: Color::Rgb(220, 220, 220),
        crit: Color::Rgb(255, 255, 255),
        border: BorderType::Thick,
        gauge_fill: '#',
        gauge_empty: '-',
        bar_fill: '#',
        bar_empty: '-',
    },
];

impl Theme {
    /// Color a utilization fraction ok → warn → crit as it climbs.
    pub fn gauge_color(&self, frac: f64) -> Color {
        if frac >= 0.85 {
            self.crit
        } else if frac >= 0.6 {
            self.warn
        } else {
            self.ok
        }
    }

    /// Filled/empty halves of a bracketed gauge, as separate strings so the
    /// caller can style them independently.
    pub fn gauge_parts(&self, frac: f64, width: usize) -> (String, String) {
        let filled = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
        let filled = filled.min(width);
        (
            std::iter::repeat(self.gauge_fill).take(filled).collect(),
            std::iter::repeat(self.gauge_empty).take(width - filled).collect(),
        )
    }

    /// A solid ratio bar in this theme's bar glyphs.
    pub fn bar(&self, frac: f64, width: usize) -> String {
        let filled = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
        let filled = filled.min(width);
        let mut s: String = std::iter::repeat(self.bar_fill).take(filled).collect();
        s.extend(std::iter::repeat(self.bar_empty).take(width - filled));
        s
    }
}

/// Index of a theme by (case-insensitive) name.
pub fn index_of(name: &str) -> Option<usize> {
    THEMES.iter().position(|t| t.name.eq_ignore_ascii_case(name))
}

pub fn names() -> Vec<&'static str> {
    THEMES.iter().map(|t| t.name).collect()
}

fn saved_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("cctop").join("theme"))
}

/// The persisted theme choice (written when the user cycles with `t`).
pub fn load_saved() -> Option<usize> {
    let name = std::fs::read_to_string(saved_path()?).ok()?;
    index_of(name.trim())
}

pub fn save(name: &str) {
    if let Some(p) = saved_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, name);
    }
}

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

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

/// A `width`×`height` column chart of `values` using eighth-block glyphs,
/// returned top row first. Values are right-aligned (newest last); any
/// non-zero value shows at least a baseline sliver.
pub fn columns(values: &[u64], width: usize, height: usize) -> Vec<String> {
    let slice: &[u64] = if values.len() > width { &values[values.len() - width..] } else { values };
    let max = slice.iter().copied().max().unwrap_or(0).max(1);
    let pad = width.saturating_sub(slice.len());
    let mut rows = vec![" ".repeat(pad); height];
    for &v in slice {
        let mut lvl = ((v as f64 / max as f64) * (height * 8) as f64).round() as usize;
        if v > 0 {
            lvl = lvl.max(1);
        }
        for (r, row) in rows.iter_mut().enumerate() {
            let below = (height - 1 - r) * 8; // eighths consumed by lower rows
            row.push(match lvl.saturating_sub(below) {
                0 => ' ',
                x @ 1..=8 => SPARK[x - 1],
                _ => '█',
            });
        }
    }
    rows
}

/// Compact dollar amount: $3.20 / $42 / $312 / $11.8k.
pub fn money(v: f64) -> String {
    if v >= 10_000.0 {
        format!("${:.1}k", v / 1000.0)
    } else if v >= 100.0 {
        format!("${v:.0}")
    } else {
        format!("${v:.2}")
    }
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
