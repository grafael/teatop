//! htop-style meter rows: label, segmented bar, percentage and dim detail.

use crate::style::{self, Style};
use crate::text::{clamp_percent, pad_line, pad_truncate, truncate_runes, BAR_EMPTY, BAR_FILLED};

const METER_BAR_CAP: usize = 40;
const METER_BAR_MIN: usize = 10;
const DETAIL_RESERVE: usize = 32;

/// meter_row renders one htop-style meter line of exactly w cells, e.g.
/// `CPU  [▐▐▐▐▐▐▐▐▐▐░░░░░░░░]  58.7%  detail`.
///
/// Label, brackets, filled segments and percentage share the metric's identity
/// color; unfilled segments and the detail text are dim.
pub fn meter_row(label: &str, detail: &str, percent: f64, color: u8, w: usize) -> String {
    let percent = clamp_percent(percent);
    let pct = format!("{:5.1}%", percent);

    const LABEL_W: usize = 5; // longest label ("VRAM") plus one space
    let mut bar_w = w as i64 - LABEL_W as i64 - pct.len() as i64 - 4; // brackets plus spaces
    // Reserve the detail area up front so a packed layout shrinks the bar
    // instead of dropping it.
    if bar_w > METER_BAR_MIN as i64 {
        bar_w -= DETAIL_RESERVE as i64;
        if bar_w < METER_BAR_MIN as i64 {
            bar_w = METER_BAR_MIN as i64;
        }
    }
    if bar_w > METER_BAR_CAP as i64 {
        bar_w = METER_BAR_CAP as i64;
    }

    let c = Style::new().fg(color);
    if bar_w < 3 {
        // too narrow for a bar: label and value only
        return pad_line(&format!("{}{}", c.bold().render(&pad_truncate(label, LABEL_W)), c.render(&pct)), w);
    }
    let bar_w = bar_w as usize;
    let filled = (percent / 100.0 * bar_w as f64 + 0.5) as usize;

    let mut line = format!(
        "{}{}{}{}{}  {}",
        c.bold().render(&pad_truncate(label, LABEL_W)),
        c.render("["),
        c.render(&BAR_FILLED.repeat(filled)),
        Style::new().fg(style::GRAY).render(&BAR_EMPTY.repeat(bar_w - filled)),
        c.render("]"),
        c.render(&pct),
    );
    // The detail is optional context: truncate it to the remaining width.
    let avail = w as i64 - style::width(&line) as i64 - 2;
    if !detail.is_empty() && avail > 3 {
        let detail = truncate_runes(detail, avail as usize);
        line.push_str("  ");
        line.push_str(&Style::new().fg(style::GRAY).render(&detail));
    }
    pad_line(&line, w)
}
