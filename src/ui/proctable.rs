//! An htop-style process list: fixed header, movable selection, scrolling,
//! sort by CPU/memory/GPU usage, substring search, tree view and own-processes
//! filter.

use std::collections::{HashMap, HashSet};

use crate::metrics::process::Process;
use crate::style::{self, Style, BLACK, CYAN, GRAY, GREEN, WHITE};
use crate::text::{format_bytes, load_color, pad_truncate};

/// procSort selects the column the process table orders (and filters) by.
#[derive(Clone, Copy, PartialEq)]
pub enum ProcSort {
    Cpu,
    Mem,
    Gpu,
}

impl ProcSort {
    pub fn as_str(self) -> &'static str {
        match self {
            ProcSort::Mem => "mem",
            ProcSort::Gpu => "gpu",
            ProcSort::Cpu => "cpu",
        }
    }
    pub fn parse(s: &str) -> ProcSort {
        match s {
            "mem" => ProcSort::Mem,
            "gpu" => ProcSort::Gpu,
            _ => ProcSort::Cpu,
        }
    }
    fn column_label(self) -> &'static str {
        match self {
            ProcSort::Mem => "MEM%",
            ProcSort::Gpu => "GPU%",
            ProcSort::Cpu => "CPU%",
        }
    }
}

fn header_style() -> Style {
    Style::new().fg(BLACK).bg(GREEN)
}
fn sort_chip_style() -> Style {
    Style::new().fg(BLACK).bg(CYAN).bold()
}
pub fn selected_style() -> Style {
    Style::new().fg(WHITE).bg(GRAY).bold()
}

pub struct ProcTable {
    pub sort_by: ProcSort,
    pub asc: bool,
    pub search: String,
    pub tree: bool,
    pub mine_only: bool,
    pub mine_user: String,
    pub show_gpu: bool,

    rows: Vec<Process>,
    prefixes: Option<Vec<String>>,
    sel_pid: i32,
    sel_idx: usize,
    scroll: usize,
    last_visible: usize,
}

impl ProcTable {
    pub fn new() -> ProcTable {
        ProcTable {
            sort_by: ProcSort::Cpu,
            asc: false,
            search: String::new(),
            tree: false,
            mine_only: false,
            mine_user: String::new(),
            show_gpu: false,
            rows: Vec::new(),
            prefixes: None,
            sel_pid: 0,
            sel_idx: 0,
            scroll: 0,
            last_visible: 20,
        }
    }

    /// set_rows filters, sorts and installs a fresh snapshot, keeping the
    /// selection pinned to the same PID when it survives.
    pub fn set_rows(&mut self, all: &[Process]) {
        let query = self.search.to_lowercase();
        let mut rows: Vec<Process> = all
            .iter()
            .filter(|r| {
                if !query.is_empty() && !matches(r, &query) {
                    return false;
                }
                if self.mine_only && r.user != self.mine_user {
                    return false;
                }
                true
            })
            .cloned()
            .collect();

        self.sort_rows(&mut rows);
        self.prefixes = None;
        // Tree order needs the full parent chain; searching or the user filter
        // removes ancestors, so those views stay flat.
        if self.tree && query.is_empty() && !self.mine_only {
            let (ordered, prefixes) = tree_order(rows);
            rows = ordered;
            self.prefixes = Some(prefixes);
        }
        self.rows = rows;

        // Re-find the selected PID; if it vanished, keep the same visual slot.
        let idx = self.rows.iter().position(|r| r.pid == self.sel_pid);
        match idx {
            Some(i) => self.sel_idx = i,
            None => {
                if self.sel_idx >= self.rows.len() {
                    self.sel_idx = self.rows.len().saturating_sub(1);
                }
                self.sel_pid = self.rows.get(self.sel_idx).map_or(0, |r| r.pid);
            }
        }
    }

    fn sort_rows(&self, rows: &mut [Process]) {
        let (by, asc) = (self.sort_by, self.asc);
        // stable sort matching Go's SliceStable: primary metric desc (flipped
        // by asc), tie-break by PID ascending (never flipped).
        rows.sort_by(|a, b| {
            if let Some(before) = compare(by, a, b) {
                if before != asc {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            } else {
                a.pid.cmp(&b.pid)
            }
        });
    }

    pub fn title(&self) -> String {
        let mut extra = String::new();
        if self.tree && self.prefixes.is_some() {
            extra.push_str("   tree");
        }
        if self.mine_only {
            extra.push_str(&format!("   user: {}", self.mine_user));
        }
        if !self.search.is_empty() {
            extra.push_str(&format!("   search: {:?}", self.search));
        }
        format!("PROCESSES {}   sort: {}{}", self.rows.len(), self.sort_by.as_str(), extra)
    }

    pub fn move_by(&mut self, delta: i64) {
        if self.rows.is_empty() {
            return;
        }
        let mut idx = self.sel_idx as i64 + delta;
        idx = idx.clamp(0, self.rows.len() as i64 - 1);
        self.sel_idx = idx as usize;
        self.sel_pid = self.rows[self.sel_idx].pid;
    }

    pub fn select_first(&mut self) {
        self.sel_idx = 0;
        self.scroll = 0;
        self.sel_pid = self.rows.first().map_or(0, |r| r.pid);
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

    /// sort_hit_at maps an x offset inside the table's inner width to the sort
    /// column whose header label sits there.
    pub fn sort_hit_at(&self, x: i64) -> Option<ProcSort> {
        let header = self.header_line();
        for (label, sort) in [("CPU%", ProcSort::Cpu), ("MEM%", ProcSort::Mem), ("GPU%", ProcSort::Gpu)] {
            if sort == ProcSort::Gpu && !self.show_gpu {
                continue;
            }
            if let Some(b) = header.find(label) {
                let start = header[..b].chars().count() as i64;
                if x >= start && x < start + label.len() as i64 + 1 {
                    return Some(sort);
                }
            }
        }
        None
    }

    /// select_visible moves the selection to the offset-th data row of the
    /// current viewport (0 = first row below the header).
    pub fn select_visible(&mut self, offset: i64) -> bool {
        if offset < 0 || offset >= self.last_visible as i64 {
            return false;
        }
        let idx = self.scroll + offset as usize;
        if idx >= self.rows.len() {
            return false;
        }
        self.sel_idx = idx;
        self.sel_pid = self.rows[idx].pid;
        true
    }

    pub fn selected(&self) -> Option<&Process> {
        self.rows.get(self.sel_idx)
    }

    /// view renders the header plus as many rows as fit into a w×h cell canvas.
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

        let mut lines = Vec::with_capacity(h);
        lines.push(self.styled_header(w));
        for i in 0..visible {
            let ri = self.scroll + i;
            if ri >= self.rows.len() {
                break;
            }
            let prefix = self.prefixes.as_ref().map_or("", |p| p[ri].as_str());
            let line = pad_truncate(&self.row_line(&self.rows[ri], prefix), w);
            if ri == self.sel_idx {
                lines.push(selected_style().render(&line));
            } else {
                lines.push(self.colorize_row(&line, &self.rows[ri]));
            }
        }
        lines
    }

    fn styled_header(&self, w: usize) -> String {
        let header = pad_truncate(&self.header_line(), w);
        let mark = if self.asc { "▲" } else { "▼" };
        let label = format!("{}{}", self.sort_by.column_label(), mark);
        if let Some(b) = header.find(&label) {
            format!(
                "{}{}{}",
                header_style().render(&header[..b]),
                sort_chip_style().render(&label),
                header_style().render(&header[b + label.len()..]),
            )
        } else {
            header_style().render(&header)
        }
    }

    fn header_line(&self) -> String {
        let mark = if self.asc { "▲" } else { "▼" };
        let (mut cpu_l, mut mem_l, mut gpu_l) = ("CPU%".to_string(), "MEM%".to_string(), "GPU%".to_string());
        match self.sort_by {
            ProcSort::Mem => mem_l.push_str(mark),
            ProcSort::Gpu => gpu_l.push_str(mark),
            ProcSort::Cpu => cpu_l.push_str(mark),
        }
        let mut s = format!("{:>7} {:<8} {:>6} {:>6} {:>8}", "PID", "USER", cpu_l, mem_l, "RSS");
        if self.show_gpu {
            s.push_str(&format!(" {:>5} {:>8}", gpu_l, "VRAM"));
        }
        s.push_str(" COMMAND");
        s
    }

    /// colorize_row tints the fixed columns of an already-padded, plain row.
    fn colorize_row(&self, line: &str, r: &Process) -> String {
        let c: Vec<char> = line.chars().collect();
        let mut spans: Vec<(usize, usize, u8)> = vec![
            (0, 7, GRAY),
            (8, 16, CYAN),
            (17, 23, load_color(r.cpu_percent)),
            (24, 30, load_color(r.mem_percent)),
            (31, 39, WHITE),
        ];
        let mut cmd = 40usize;
        if self.show_gpu {
            spans.push((40, 45, style::GPU_COLOR));
            spans.push((46, 54, style::GPU_MEM_COLOR));
            cmd = 55;
        }

        let clamp = |x: usize| x.min(c.len());
        let slice = |a: usize, b: usize| -> String { c[a..b].iter().collect() };
        let fg = |col: u8, a: usize, b: usize| -> String {
            if a >= b {
                String::new()
            } else {
                Style::new().fg(col).render(&slice(a, b))
            }
        };

        let mut sb = String::new();
        let mut cursor = 0usize;
        for (a, b, col) in spans {
            let (a, b) = (clamp(a), clamp(b));
            sb.push_str(&slice(clamp(cursor), a)); // separators, plain
            sb.push_str(&fg(col, a, b));
            cursor = b;
        }
        let cd = clamp(cmd);
        sb.push_str(&slice(clamp(cursor), cd)); // separator before command
        sb.push_str(&fg(GREEN, cd, c.len()));
        sb
    }

    fn row_line(&self, r: &Process, tree_prefix: &str) -> String {
        let mut s = format!(
            "{:>7} {:<8.8} {:>6.1} {:>6.1} {:>8}",
            r.pid,
            r.user,
            r.cpu_percent,
            r.mem_percent,
            format_bytes(r.rss),
        );
        if self.show_gpu {
            // NVML only reports per-process SM utilisation while a process is
            // actively computing; a dash means "no GPU work now", not 0%.
            let gpu_pct = if r.gpu_percent > 0.0 { format!("{:.0}", r.gpu_percent) } else { "-".into() };
            let gpu_mem = if r.gpu_mem > 0 { format_bytes(r.gpu_mem) } else { "-".into() };
            s.push_str(&format!(" {:>5} {:>8}", gpu_pct, gpu_mem));
        }
        format!("{} {}{}", s, tree_prefix, r.command)
    }
}

/// compare orders a before b descending on the sort metric. None when the
/// metric ties, letting the caller fall back to PID order.
fn compare(by: ProcSort, a: &Process, b: &Process) -> Option<bool> {
    match by {
        ProcSort::Mem => {
            if a.rss != b.rss {
                return Some(a.rss > b.rss);
            }
        }
        ProcSort::Gpu => {
            if a.gpu_percent != b.gpu_percent {
                return Some(a.gpu_percent > b.gpu_percent);
            }
            if a.gpu_mem != b.gpu_mem {
                return Some(a.gpu_mem > b.gpu_mem);
            }
        }
        ProcSort::Cpu => {
            if a.cpu_percent != b.cpu_percent {
                return Some(a.cpu_percent > b.cpu_percent);
            }
        }
    }
    None
}

/// matches reports whether the query substring occurs in the process command,
/// user name or PID (query must already be lower-cased).
fn matches(r: &Process, query: &str) -> bool {
    r.command.to_lowercase().contains(query)
        || r.user.to_lowercase().contains(query)
        || r.pid.to_string().contains(query)
}

/// tree_order arranges the (already sorted) rows depth-first by parentage, so
/// siblings keep the sort order, and returns a branch prefix per row.
fn tree_order(rows: Vec<Process>) -> (Vec<Process>, Vec<String>) {
    let present: HashSet<i32> = rows.iter().map(|r| r.pid).collect();
    let mut children: HashMap<i32, Vec<Process>> = HashMap::new();
    for r in rows {
        let parent = if !present.contains(&r.ppid) || r.ppid == r.pid { 0 } else { r.ppid };
        children.entry(parent).or_default().push(r);
    }

    let mut out = Vec::new();
    let mut prefixes = Vec::new();
    let mut visited: HashSet<i32> = HashSet::new();
    walk(0, "", &children, &mut visited, &mut out, &mut prefixes);
    (out, prefixes)
}

fn walk(
    pid: i32,
    indent: &str,
    children: &HashMap<i32, Vec<Process>>,
    visited: &mut HashSet<i32>,
    out: &mut Vec<Process>,
    prefixes: &mut Vec<String>,
) {
    if let Some(kids) = children.get(&pid) {
        let n = kids.len();
        for (i, c) in kids.iter().enumerate() {
            if visited.contains(&c.pid) {
                continue;
            }
            visited.insert(c.pid);
            let (prefix, child_indent) = if pid != 0 {
                if i == n - 1 {
                    (format!("{}└─ ", indent), format!("{}   ", indent))
                } else {
                    (format!("{}├─ ", indent), format!("{}│  ", indent))
                }
            } else {
                (String::new(), String::new())
            };
            out.push(c.clone());
            prefixes.push(prefix);
            walk(c.pid, &child_indent, children, visited, out, prefixes);
        }
    }
}
