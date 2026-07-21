//! A scrolling list of processes ranked by live disk I/O, with a movable
//! selection, substring filter and sort column.

use crate::metrics::procdisk::ProcIO;
use crate::style::{Style, BLACK, BLUE, CYAN, GRAY, PURPLE, WHITE};
use crate::text::{format_bytes, left_pad, pad_truncate};
use crate::ui::proctable::selected_style;

fn disk_header_style() -> Style {
    Style::new().fg(BLACK).bg(CYAN)
}
fn disk_sort_chip_style() -> Style {
    Style::new().fg(BLACK).bg(BLUE).bold()
}
pub fn disk_read_style() -> Style {
    Style::new().fg(CYAN)
}
pub fn disk_write_style() -> Style {
    Style::new().fg(PURPLE)
}

#[derive(Clone, Copy, PartialEq)]
pub enum DiskSort {
    Total,
    Read,
    Write,
}

impl DiskSort {
    pub fn as_str(self) -> &'static str {
        match self {
            DiskSort::Read => "read",
            DiskSort::Write => "write",
            DiskSort::Total => "total",
        }
    }
}

pub struct DiskTable {
    pub sort_by: DiskSort,
    pub asc: bool,
    pub search: String,

    rows: Vec<ProcIO>,
    sel_pid: i32,
    sel_idx: usize,
    scroll: usize,
    last_visible: usize,
}

impl DiskTable {
    pub fn new() -> DiskTable {
        DiskTable {
            sort_by: DiskSort::Total,
            asc: false,
            search: String::new(),
            rows: Vec::new(),
            sel_pid: 0,
            sel_idx: 0,
            scroll: 0,
            last_visible: 10,
        }
    }

    pub fn set_rows(&mut self, all: &[ProcIO]) {
        let query = self.search.to_lowercase();
        let mut rows: Vec<ProcIO> = all
            .iter()
            .filter(|p| query.is_empty() || disk_matches(p, &query))
            .cloned()
            .collect();
        self.sort_rows(&mut rows);
        self.rows = rows;

        let idx = self.rows.iter().position(|p| p.pid == self.sel_pid);
        match idx {
            Some(i) => self.sel_idx = i,
            None => {
                if self.sel_idx >= self.rows.len() {
                    self.sel_idx = self.rows.len().saturating_sub(1);
                }
                self.sel_pid = self.rows.get(self.sel_idx).map_or(0, |p| p.pid);
            }
        }
    }

    fn sort_rows(&self, rows: &mut [ProcIO]) {
        let (by, asc) = (self.sort_by, self.asc);
        rows.sort_by(|a, b| {
            let (av, bv) = match by {
                DiskSort::Read => (a.read_per_sec, b.read_per_sec),
                DiskSort::Write => (a.write_per_sec, b.write_per_sec),
                DiskSort::Total => (a.read_per_sec + a.write_per_sec, b.read_per_sec + b.write_per_sec),
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

    pub fn set_search(&mut self, query: &str, procs: &[ProcIO]) {
        self.search = query.to_string();
        self.set_rows(procs);
    }

    pub fn set_sort(&mut self, s: DiskSort) {
        if self.sort_by == s {
            self.asc = !self.asc;
        } else {
            self.sort_by = s;
            self.asc = false;
        }
        let mut rows = std::mem::take(&mut self.rows);
        self.sort_rows(&mut rows);
        self.rows = rows;
        if let Some(i) = self.rows.iter().position(|p| p.pid == self.sel_pid) {
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
        self.sel_pid = self.rows[self.sel_idx].pid;
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

    pub fn selected(&self) -> Option<&ProcIO> {
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

        let pw = proc_width(w);
        let mut lines = Vec::with_capacity(h);
        lines.push(self.styled_header(w, pw));
        for i in 0..visible {
            let ri = self.scroll + i;
            if ri >= self.rows.len() {
                break;
            }
            if ri == self.sel_idx {
                let (pid, name, read, write) = row_cells(&self.rows[ri], pw);
                let line = format!("{}  {} {} {}", pid, name, read, write);
                lines.push(selected_style().render(&pad_truncate(&line, w)));
            } else {
                lines.push(row_line(&self.rows[ri], pw));
            }
        }
        lines
    }

    fn header_line(&self, pw: usize) -> String {
        let mark = if self.asc { "▲" } else { "▼" };
        let (mut read, mut write) = ("READ".to_string(), "WRITE".to_string());
        match self.sort_by {
            DiskSort::Read => read.push_str(mark),
            DiskSort::Write => write.push_str(mark),
            DiskSort::Total => {}
        }
        format!(
            "{:>pidw$}  {:<pw$} {:>ratew$} {:>ratew$}",
            "PID",
            "PROCESS",
            read,
            write,
            pidw = DISK_PID_W,
            pw = pw,
            ratew = DISK_RATE_W,
        )
    }

    fn styled_header(&self, w: usize, pw: usize) -> String {
        let header = pad_truncate(&self.header_line(pw), w);
        if self.sort_by == DiskSort::Total {
            return disk_header_style().render(&header);
        }
        let mark = if self.asc { "▲" } else { "▼" };
        let label = if self.sort_by == DiskSort::Write { format!("WRITE{}", mark) } else { format!("READ{}", mark) };
        if let Some(b) = header.find(&label) {
            format!(
                "{}{}{}",
                disk_header_style().render(&header[..b]),
                disk_sort_chip_style().render(&label),
                disk_header_style().render(&header[b + label.len()..]),
            )
        } else {
            disk_header_style().render(&header)
        }
    }
}

const DISK_PID_W: usize = 7;
const DISK_RATE_W: usize = 12;
const DISK_MIN_PROC_W: usize = 12;

fn proc_width(w: usize) -> usize {
    let pw = w as i64 - DISK_PID_W as i64 - 2 - 1 - DISK_RATE_W as i64 - 1 - DISK_RATE_W as i64;
    if pw < DISK_MIN_PROC_W as i64 {
        DISK_MIN_PROC_W
    } else {
        pw as usize
    }
}

fn disk_matches(p: &ProcIO, query: &str) -> bool {
    p.name.to_lowercase().contains(query) || p.pid.to_string().contains(query)
}

fn row_cells(p: &ProcIO, pw: usize) -> (String, String, String, String) {
    let n = if p.name.is_empty() { "?" } else { &p.name };
    let pid = left_pad(&p.pid.to_string(), DISK_PID_W);
    let name = pad_truncate(n, pw);
    let read = left_pad(&format!("{}/s", format_bytes(p.read_per_sec as u64)), DISK_RATE_W);
    let write = left_pad(&format!("{}/s", format_bytes(p.write_per_sec as u64)), DISK_RATE_W);
    (pid, name, read, write)
}

fn row_line(p: &ProcIO, pw: usize) -> String {
    let (pid, name, read, write) = row_cells(p, pw);
    format!(
        "{}  {} {} {}",
        Style::new().fg(GRAY).render(&pid),
        Style::new().fg(WHITE).render(&name),
        disk_read_style().render(&read),
        disk_write_style().render(&write),
    )
}
