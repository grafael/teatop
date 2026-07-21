//! Braille sparkline rendering: one solid-color lane per series, each in its
//! own horizontal band, falling back to a shared canvas when the panel is too
//! short to give each series a usable lane.

use crate::style::{self, Style};
use crate::text::{pad_line, spaces};

/// render_lanes stacks one solid-color braille sparkline per series, each in its
/// own band of the w×h canvas (taller bands first when h does not divide
/// evenly). When h is too short to give each series a usable lane, it falls
/// back to the shared canvas.
pub fn render_lanes(
    series: &[&[f64]],
    colors: &[u8],
    labels: &[&str],
    max_val: f64,
    w: usize,
    h: usize,
) -> Vec<String> {
    let n = series.len();
    if n == 0 || h < 1 {
        return Vec::new();
    }

    // A right-edge gutter frames the floating dots with each lane's series name.
    const GUTTER_W: usize = 4;
    let plot_w = if w >= 24 { w - GUTTER_W } else { w };
    let scale = |lines: Vec<String>, marks: &std::collections::HashMap<usize, String>| -> Vec<String> {
        if plot_w == w {
            return lines;
        }
        lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                if let Some(m) = marks.get(&i) {
                    format!("{}{}{}", pad_line(&line, plot_w), spaces(GUTTER_W - style::width(m)), m)
                } else {
                    pad_line(&line, w)
                }
            })
            .collect()
    };

    if h < 2 * n {
        let mut marks = std::collections::HashMap::new();
        if h >= 2 {
            marks.insert(0, style::dim("100"));
            marks.insert(h - 1, style::dim("0"));
            if h >= 4 {
                marks.insert(h / 2, style::dim("50"));
            }
        }
        return scale(render_braille(series, colors, max_val, plot_w, h), &marks);
    }

    let base = h / n;
    let extra = h % n;
    let mut out = Vec::with_capacity(h);
    for i in 0..series.len() {
        let lh = if i < extra { base + 1 } else { base };
        let lane = render_braille(&series[i..i + 1], &colors[i..i + 1], max_val, plot_w, lh);
        let mut marks = std::collections::HashMap::new();
        if i < labels.len() && !labels[i].is_empty() {
            let mut label = labels[i].to_string();
            let chars: Vec<char> = label.chars().collect();
            if chars.len() > GUTTER_W - 1 {
                label = chars[..GUTTER_W - 1].iter().collect();
            }
            marks.insert(lh / 2, Style::new().fg(colors[i]).render(&label));
        }
        out.extend(scale(lane, &marks));
    }
    out
}

/// brailleBits maps a (dotX, dotY) offset inside one cell to its bit in the
/// braille pattern block (U+2800 + bits).
const BRAILLE_BITS: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// render_braille plots the series as braille line charts over a w×h cell canvas
/// (2w×4h dots) scaled to [0, max_val]. Only the trailing 2w samples of each
/// series are drawn, newest at the right edge. Where several series contribute
/// to a cell their colors alternate by column so close lines read as
/// interleaved rather than one repainting the others.
pub fn render_braille(series: &[&[f64]], colors: &[u8], max_val: f64, w: usize, h: usize) -> Vec<String> {
    if w < 1 || h < 1 {
        return Vec::new();
    }
    let dots_w = 2 * w;
    let dots_h = 4 * h;
    let mut cells = vec![0u32; w * h];
    let mut cell_series = vec![0u32; w * h];

    let scale_y = |v: f64| -> i64 {
        let v = v.clamp(0.0, max_val);
        ((1.0 - v / max_val) * (dots_h as f64 - 1.0)) as i64
    };

    for (si, data) in series.iter().enumerate() {
        let data: &[f64] = if data.len() > dots_w { &data[data.len() - dots_w..] } else { data };
        let offset = dots_w as i64 - data.len() as i64; // right-align so "now" is the right edge
        let mut prev_y = 0i64;
        for (i, &v) in data.iter().enumerate() {
            let y = scale_y(v);
            let x = offset + i as i64;
            set_dot(&mut cells, &mut cell_series, dots_w, dots_h, w, x, y, si);
            if i > 0 {
                let (lo, hi) = if y > prev_y { (prev_y, y) } else { (y, prev_y) };
                for yy in lo + 1..hi {
                    set_dot(&mut cells, &mut cell_series, dots_w, dots_h, w, x, yy, si);
                }
            }
            prev_y = y;
        }
    }

    let styles: Vec<Style> = colors.iter().map(|&c| Style::new().fg(c)).collect();

    // Resolve each cell's color: sole contributor, or rotate through the
    // contributors by column when several lines share the cell.
    let mut cell_color = vec![0usize; w * h]; // series index + 1; 0 = unset
    for ci in 0..cells.len() {
        let mask = cell_series[ci];
        if mask == 0 {
            continue;
        }
        let mut k = (ci % w) % (mask.count_ones() as usize);
        for si in 0..series.len() {
            if mask & (1 << si) == 0 {
                continue;
            }
            if k == 0 {
                cell_color[ci] = si + 1;
                break;
            }
            k -= 1;
        }
    }

    let mut lines = Vec::with_capacity(h);
    for y in 0..h {
        let mut b = String::new();
        let mut run_start = 0usize;
        let mut run = String::new();
        for x in 0..w {
            let ci = y * w + x;
            if x > run_start && cell_color[ci] != cell_color[y * w + run_start] {
                flush_run(&mut b, &mut run, &styles, cell_color[y * w + run_start]);
                run_start = x;
            }
            if cells[ci] == 0 {
                run.push(' ');
            } else {
                run.push(char::from_u32(0x2800 + cells[ci]).unwrap_or(' '));
            }
        }
        flush_run(&mut b, &mut run, &styles, cell_color[y * w + run_start]);
        lines.push(b);
    }
    lines
}

fn set_dot(cells: &mut [u32], cell_series: &mut [u32], dots_w: usize, dots_h: usize, w: usize, x: i64, y: i64, si: usize) {
    if x < 0 || x >= dots_w as i64 || y < 0 || y >= dots_h as i64 {
        return;
    }
    let (x, y) = (x as usize, y as usize);
    let ci = (y / 4) * w + x / 2;
    cells[ci] |= BRAILLE_BITS[y % 4][x % 2];
    cell_series[ci] |= 1 << si;
}

fn flush_run(b: &mut String, run: &mut String, styles: &[Style], color: usize) {
    if run.is_empty() {
        return;
    }
    if color > 0 {
        b.push_str(&styles[color - 1].render(run));
    } else {
        b.push_str(run);
    }
    run.clear();
}
