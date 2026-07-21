//! A scrolling list of network connections with a movable selection, substring
//! filter and sort column. Rows reorder every refresh; the selection is pinned
//! to its connection, not its slot.

use crate::metrics::procnet::ProcConn;
use crate::style::{Style, BLACK, BLUE, CYAN, GRAY, WHITE};
use crate::text::{format_bytes, left_pad, pad_truncate};
use crate::ui::proctable::selected_style;

fn net_header_style() -> Style {
    Style::new().fg(BLACK).bg(BLUE)
}
fn net_sort_chip_style() -> Style {
    Style::new().fg(BLACK).bg(CYAN).bold()
}

#[derive(Clone, Copy, PartialEq)]
pub enum NetSort {
    Total,
    Down,
    Up,
}

impl NetSort {
    pub fn as_str(self) -> &'static str {
        match self {
            NetSort::Down => "down",
            NetSort::Up => "up",
            NetSort::Total => "total",
        }
    }
}

pub struct NetTable {
    pub sort_by: NetSort,
    pub asc: bool,
    pub search: String,

    rows: Vec<ProcConn>,
    sel_key: String,
    sel_idx: usize,
    scroll: usize,
    last_visible: usize,
}

fn conn_key(c: &ProcConn) -> String {
    format!("{}|{}", c.pid, c.remote)
}

impl NetTable {
    pub fn new() -> NetTable {
        NetTable {
            sort_by: NetSort::Total,
            asc: false,
            search: String::new(),
            rows: Vec::new(),
            sel_key: String::new(),
            sel_idx: 0,
            scroll: 0,
            last_visible: 10,
        }
    }

    pub fn set_rows(&mut self, all: &[ProcConn]) {
        let query = self.search.to_lowercase();
        let mut rows: Vec<ProcConn> = all
            .iter()
            .filter(|c| query.is_empty() || net_matches(c, &query))
            .cloned()
            .collect();
        self.sort_rows(&mut rows);
        self.rows = rows;

        let idx = self.rows.iter().position(|c| conn_key(c) == self.sel_key);
        match idx {
            Some(i) => self.sel_idx = i,
            None => {
                if self.sel_idx >= self.rows.len() {
                    self.sel_idx = self.rows.len().saturating_sub(1);
                }
                self.sel_key = self.rows.get(self.sel_idx).map_or(String::new(), conn_key);
            }
        }
    }

    fn sort_rows(&self, rows: &mut [ProcConn]) {
        let (by, asc) = (self.sort_by, self.asc);
        rows.sort_by(|a, b| {
            let (av, bv) = match by {
                NetSort::Down => (a.rx_per_sec, b.rx_per_sec),
                NetSort::Up => (a.tx_per_sec, b.tx_per_sec),
                NetSort::Total => (a.rx_per_sec + a.tx_per_sec, b.rx_per_sec + b.tx_per_sec),
            };
            if av != bv {
                if (av > bv) != asc {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            } else {
                a.pid.cmp(&b.pid)
            }
        });
    }

    pub fn set_search(&mut self, query: &str, conns: &[ProcConn]) {
        self.search = query.to_string();
        self.set_rows(conns);
    }

    pub fn set_sort(&mut self, s: NetSort) {
        if self.sort_by == s {
            self.asc = !self.asc;
        } else {
            self.sort_by = s;
            self.asc = false;
        }
        let mut rows = std::mem::take(&mut self.rows);
        self.sort_rows(&mut rows);
        self.rows = rows;
        if let Some(i) = self.rows.iter().position(|c| conn_key(c) == self.sel_key) {
            self.sel_idx = i;
        }
    }

    pub fn count(&self) -> usize {
        self.rows.len()
    }

    pub fn move_by(&mut self, delta: i64) {
        if self.rows.is_empty() {
            return;
        }
        let idx = (self.sel_idx as i64 + delta).clamp(0, self.rows.len() as i64 - 1);
        self.sel_idx = idx as usize;
        self.sel_key = conn_key(&self.rows[self.sel_idx]);
    }
    pub fn page(&mut self, dir: i64) {
        self.move_by(dir * self.last_visible as i64);
    }
    pub fn home(&mut self) {
        self.move_by(-(self.rows.len() as i64));
    }
    pub fn end(&mut self) {
        self.move_by(self.rows.len() as i64);
    }

    pub fn selected(&self) -> Option<&ProcConn> {
        self.rows.get(self.sel_idx)
    }

    pub fn view(&mut self, w: usize, h: usize) -> Vec<String> {
        if w < 1 || h < 2 {
            return Vec::new();
        }
        let visible = h - 1;
        self.last_visible = visible;

        if self.rows.len() > visible {
            let max_scroll = self.rows.len() - visible;
            if self.scroll > max_scroll {
                self.scroll = max_scroll;
            }
        } else {
            self.scroll = 0;
        }
        if self.sel_idx < self.scroll {
            self.scroll = self.sel_idx;
        }
        if self.sel_idx >= self.scroll + visible {
            self.scroll = self.sel_idx - visible + 1;
        }

        let rw = remote_width(w);
        let mut lines = Vec::with_capacity(h);
        lines.push(self.styled_header(w, rw));
        for i in 0..visible {
            let ri = self.scroll + i;
            if ri >= self.rows.len() {
                break;
            }
            if ri == self.sel_idx {
                let (pid, name, remote, down, up) = row_cells(&self.rows[ri], rw);
                let line = format!("{}  {} {} {} {}", pid, name, remote, down, up);
                lines.push(selected_style().render(&pad_truncate(&line, w)));
            } else {
                lines.push(row_line(&self.rows[ri], rw));
            }
        }
        lines
    }

    fn header_line(&self, rw: usize) -> String {
        let mark = if self.asc { "▲" } else { "▼" };
        let (mut down, mut up) = ("DOWN".to_string(), "UP".to_string());
        match self.sort_by {
            NetSort::Down => down.push_str(mark),
            NetSort::Up => up.push_str(mark),
            NetSort::Total => {}
        }
        format!(
            "{:>pidw$}  {:<procw$} {:<rw$} {:>ratew$} {:>ratew$}",
            "PID",
            "PROCESS",
            "REMOTE",
            down,
            up,
            pidw = NET_PID_W,
            procw = NET_PROC_W,
            rw = rw,
            ratew = NET_RATE_W,
        )
    }

    fn styled_header(&self, w: usize, rw: usize) -> String {
        let header = pad_truncate(&self.header_line(rw), w);
        if self.sort_by == NetSort::Total {
            return net_header_style().render(&header);
        }
        let mark = if self.asc { "▲" } else { "▼" };
        let label = if self.sort_by == NetSort::Up { format!("UP{}", mark) } else { format!("DOWN{}", mark) };
        if let Some(b) = header.find(&label) {
            format!(
                "{}{}{}",
                net_header_style().render(&header[..b]),
                net_sort_chip_style().render(&label),
                net_header_style().render(&header[b + label.len()..]),
            )
        } else {
            net_header_style().render(&header)
        }
    }
}

const NET_PID_W: usize = 7;
const NET_PROC_W: usize = 16;
const NET_RATE_W: usize = 11;
const NET_MIN_REMOTE_W: usize = 15;

fn remote_width(w: usize) -> usize {
    let rw = w as i64 - NET_PID_W as i64 - 2 - NET_PROC_W as i64 - 1 - 1 - NET_RATE_W as i64 - 1 - NET_RATE_W as i64;
    if rw < NET_MIN_REMOTE_W as i64 {
        NET_MIN_REMOTE_W
    } else {
        rw as usize
    }
}

fn net_matches(c: &ProcConn, query: &str) -> bool {
    c.name.to_lowercase().contains(query)
        || c.remote.to_lowercase().contains(query)
        || c.pid.to_string().contains(query)
}

fn row_cells(c: &ProcConn, rw: usize) -> (String, String, String, String, String) {
    let n = if c.name.is_empty() { "?" } else { &c.name };
    let pid = left_pad(&c.pid.to_string(), NET_PID_W);
    let name = pad_truncate(n, NET_PROC_W);
    let remote = pad_truncate(&c.remote, rw);
    let down = left_pad(&format!("{}/s", format_bytes(c.rx_per_sec as u64)), NET_RATE_W);
    let up = left_pad(&format!("{}/s", format_bytes(c.tx_per_sec as u64)), NET_RATE_W);
    (pid, name, remote, down, up)
}

fn row_line(c: &ProcConn, rw: usize) -> String {
    let (pid, name, remote, down, up) = row_cells(c, rw);
    format!(
        "{}  {} {} {} {}",
        Style::new().fg(GRAY).render(&pid),
        Style::new().fg(WHITE).render(&name),
        Style::new().fg(WHITE).render(&remote),
        Style::new().fg(crate::style::RED).render(&down),
        Style::new().fg(BLUE).render(&up),
    )
}
