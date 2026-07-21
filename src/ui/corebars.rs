//! Per-core CPU bars (htop-style), wrapping into columns when there are more
//! cores than rows so every core stays visible.

use crate::style::{self, Style};
use crate::text::{load_color, pad_truncate, spaces, BAR_EMPTY, BAR_FILLED, LAYOUT_GAP};

/// render_core_bars renders one horizontal bar per CPU core into a w×h cell
/// canvas. When there are more cores than rows, bars wrap into columns.
pub fn render_core_bars(data: &[f64], max_val: f64, w: usize, h: usize) -> Vec<String> {
    let n = data.len();
    if n == 0 || h < 1 || w < 1 {
        return Vec::new();
    }

    let label_w = (n - 1).to_string().len();
    let min_col_w = label_w + 4;

    // A blank line between bar rows keeps the grid airy. Spacing halves the
    // usable rows, so only use it when every core still fits at the preferred
    // gap with a usable bar; otherwise pack the rows solid rather than hide cores.
    let (mut rows, mut spaced) = (h, false);
    let spaced_rows = (h + 1) / 2;
    if core_bars_fit_cols(n, w) * spaced_rows >= n {
        rows = spaced_rows;
        spaced = true;
    }

    let mut cols = (n + rows - 1) / rows;
    let mut col_gap = LAYOUT_GAP;
    while col_gap > 1 && (w + col_gap) / (min_col_w + col_gap) < cols {
        col_gap -= 1;
    }
    let max_cols = (w + col_gap) / (min_col_w + col_gap);
    if cols > max_cols {
        cols = max_cols;
    }
    if cols < 1 {
        cols = 1;
    }
    let col_w = (w - col_gap * (cols - 1)) / cols;
    let mut per_col = (n + cols - 1) / cols;
    if per_col > rows {
        per_col = rows;
    }

    let mut lines = Vec::with_capacity(h);
    for row in 0..per_col {
        if spaced && row > 0 {
            lines.push(String::new());
        }
        let mut b = String::new();
        for col in 0..cols {
            let i = col * per_col + row;
            if i >= n {
                continue;
            }
            if col > 0 {
                b.push_str(&spaces(col_gap));
            }
            b.push_str(&core_bar(i, data[i], max_val, label_w, col_w));
        }
        lines.push(b);
    }
    while lines.len() < h {
        lines.push(String::new());
    }
    lines
}

/// core_bars_fit_cols is how many core-bar columns fit in w at the preferred
/// gap with a usable bar.
pub fn core_bars_fit_cols(n: usize, w: usize) -> usize {
    let label_w = (n.saturating_sub(1)).to_string().len();
    let cols = (w + LAYOUT_GAP) / (label_w + 7 + LAYOUT_GAP);
    cols.max(1)
}

/// core_bars_spaced_height is the line count at which render_core_bars affords
/// its spaced layout for n cores in width w.
pub fn core_bars_spaced_height(n: usize, w: usize) -> usize {
    let cols = core_bars_fit_cols(n, w);
    ((n + cols - 1) / cols) * 2 - 1
}

/// core_bar renders a single "N [▐▐▐▐▐░░ 42%]" bar of exactly col_w cells.
fn core_bar(idx: usize, v: f64, max_val: f64, label_w: usize, col_w: usize) -> String {
    let inner_w = col_w as i64 - label_w as i64 - 1 - 2; // label, space, brackets
    if inner_w < 1 {
        return pad_truncate(&format!("{:>width$}", idx, width = label_w), col_w);
    }
    let inner_w = inner_w as usize;
    let v = v.clamp(0.0, max_val);

    let mut pct = format!("{:3.0}%", v);
    let bar_w = if inner_w >= pct.len() + 4 {
        inner_w - pct.len() - 1
    } else {
        pct = String::new();
        inner_w
    };
    let mut filled = (v / max_val * bar_w as f64) as usize;
    if filled > bar_w {
        filled = bar_w;
    }

    let label_style = Style::new().fg(style::GRAY);
    let bar_on = Style::new().fg(load_color(v));
    let mut b = String::new();
    b.push_str(&label_style.render(&format!("{:>width$} ", idx, width = label_w)));
    b.push_str(&label_style.render("["));
    b.push_str(&bar_on.render(&BAR_FILLED.repeat(filled)));
    b.push_str(&Style::new().fg(style::GRAY).render(&BAR_EMPTY.repeat(bar_w - filled)));
    if !pct.is_empty() {
        b.push(' ');
        b.push_str(&bar_on.render(&pct));
    }
    b.push_str(&label_style.render("]"));
    b
}
