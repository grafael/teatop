//! Minimal ANSI styling, mirroring the subset of lipgloss teatop uses: 16-color
//! foreground/background, bold, plus display-width measurement that ignores the
//! escape sequences (the Go code leaned on lipgloss.Width for the same thing).

use unicode_width::UnicodeWidthStr;

// Traffic-light palette shared by the gauges, core bars and plot. Values are
// ANSI color indices, matching lipgloss.Color("N") in the Go original.
pub const GREEN: u8 = 2;
pub const YELLOW: u8 = 3;
pub const RED: u8 = 1;
pub const BLUE: u8 = 4;
pub const PURPLE: u8 = 5;
pub const CYAN: u8 = 6;
pub const WHITE: u8 = 7;
pub const BLACK: u8 = 0;
pub const GRAY: u8 = 8; // dim: unfilled bar segments, details
pub const BRIGHT_MAGENTA: u8 = 13;

// Identity colors: every metric keeps one color across gauges and chart.
pub const CPU_COLOR: u8 = GREEN;
pub const MEM_COLOR: u8 = YELLOW;
pub const SWAP_COLOR: u8 = PURPLE;
pub const DISK_COLOR: u8 = CYAN;
pub const GPU_COLOR: u8 = BLUE;
pub const GPU_MEM_COLOR: u8 = BRIGHT_MAGENTA;

/// A foreground/background/bold styling, applied by wrapping text in SGR codes.
#[derive(Clone, Copy, Default)]
pub struct Style {
    fg: Option<u8>,
    bg: Option<u8>,
    bold: bool,
}

impl Style {
    pub fn new() -> Self {
        Style::default()
    }
    pub fn fg(mut self, c: u8) -> Self {
        self.fg = Some(c);
        self
    }
    pub fn bg(mut self, c: u8) -> Self {
        self.bg = Some(c);
        self
    }
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// render wraps s in the style's SGR codes, resetting afterwards. Plain
    /// (unstyled) text is returned unchanged so width math stays cheap.
    pub fn render(&self, s: &str) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".into());
        }
        if let Some(c) = self.fg {
            codes.push(fg_code(c).to_string());
        }
        if let Some(c) = self.bg {
            codes.push(bg_code(c).to_string());
        }
        if codes.is_empty() {
            return s.to_string();
        }
        format!("\x1b[{}m{}\x1b[0m", codes.join(";"), s)
    }
}

fn fg_code(c: u8) -> u16 {
    if c < 8 {
        30 + c as u16
    } else {
        90 + (c as u16 - 8)
    }
}

fn bg_code(c: u8) -> u16 {
    if c < 8 {
        40 + c as u16
    } else {
        100 + (c as u16 - 8)
    }
}

pub fn dim(s: &str) -> String {
    Style::new().fg(GRAY).render(s)
}

/// width returns the display width of s in terminal cells, ignoring ANSI SGR
/// sequences (the analogue of lipgloss.Width).
pub fn width(s: &str) -> usize {
    strip_ansi(s).width()
}

/// strip_ansi removes CSI escape sequences (ESC [ ... final-byte) from s.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // consume until a final byte in the @..~ range
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&nc) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}
