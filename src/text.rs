//! Text layout helpers shared across the UI: byte/duration formatting, the
//! traffic-light color mapping, and the rune/width-based padding the Go code
//! used for column alignment.

use crate::style::{self, GREEN, RED, YELLOW};

/// layout_gap is the shared breathing room between side-by-side panels and
/// columns: meter pairs, core-bar columns and the history/cores split.
pub const LAYOUT_GAP: usize = 3;

/// bar_filled and bar_empty are the cell glyphs shared by every bar renderer.
pub const BAR_FILLED: &str = "▐";
pub const BAR_EMPTY: &str = "░";

/// format_bytes renders a byte count with a binary-prefix unit (K, M, G, T).
pub fn format_bytes(b: u64) -> String {
    const UNIT: f64 = 1024.0;
    let units = ["B", "K", "M", "G", "T", "P"];
    if b < 1024 {
        return format!("{}{}", b, units[0]);
    }
    let mut size = b as f64;
    let mut i = 0;
    while size >= UNIT && i < units.len() - 1 {
        size /= UNIT;
        i += 1;
    }
    format!("{:.1}{}", size, units[i])
}

/// load_color maps a 0-100 utilisation figure to a traffic-light color index.
pub fn load_color(percent: f64) -> u8 {
    if percent >= 85.0 {
        RED
    } else if percent >= 60.0 {
        YELLOW
    } else {
        GREEN
    }
}

/// clamp_percent bounds a percentage to [0,100].
pub fn clamp_percent(p: f64) -> f64 {
    p.clamp(0.0, 100.0)
}

pub fn spaces(n: usize) -> String {
    " ".repeat(n)
}

/// pad_truncate fits s to exactly w cells (rune-based; s must be plain text).
pub fn pad_truncate(s: &str, w: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= w {
        chars[..w].iter().collect()
    } else {
        let mut out = s.to_string();
        out.push_str(&spaces(w - chars.len()));
        out
    }
}

/// left_pad right-aligns s in a field of w cells (rune-based; plain text).
pub fn left_pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n < w {
        format!("{}{}", spaces(w - n), s)
    } else {
        s.to_string()
    }
}

/// pad_line pads a styled line to w display cells; it never truncates.
pub fn pad_line(line: &str, w: usize) -> String {
    let cur = style::width(line);
    if cur < w {
        format!("{}{}", line, spaces(w - cur))
    } else {
        line.to_string()
    }
}

/// truncate_runes clips s to at most w runes (no padding).
pub fn truncate_runes(s: &str, w: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > w {
        chars[..w].iter().collect()
    } else {
        s.to_string()
    }
}

/// format_uptime renders a duration htop-style: "3d 04:26" or "04:26:09".
pub fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let h = secs % 86400 / 3600;
    let m = secs % 3600 / 60;
    if days > 0 {
        format!("{}d {:02}:{:02}", days, h, m)
    } else {
        format!("{:02}:{:02}:{:02}", h, m, secs % 60)
    }
}
