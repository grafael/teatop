//! Renders the teatop dashboard: a host header, CPU/Memory/GPU gauges, a
//! braille utilization history chart, optional per-core bars, an
//! all-filesystems disk view, a full-screen network view of live connections,
//! and an interactive htop-style process table.

pub mod braille;
pub mod corebars;
pub mod disktable;
pub mod gauge;
pub mod nettable;
pub mod proctable;

use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nix::sys::signal::Signal;

use crate::config::{Config, State};
use crate::hooks::Engine;
use crate::metrics::disk::{mounts, Mount};
use crate::metrics::gpu::GpuSummary;
use crate::metrics::process::kill_process;
use crate::metrics::{cpu::Cpu, disk::Disk, gpu::Gpu, memory::Memory, network::Network, procdisk::ProcDisk, procnet::ProcNet, process::Processes, system::System};
use crate::style::{self, Style};
use crate::text::{
    clamp_percent, format_bytes, format_uptime, load_color, pad_line, pad_truncate, spaces, BAR_EMPTY, BAR_FILLED, LAYOUT_GAP,
};

use braille::render_lanes;
use corebars::{core_bars_spaced_height, render_core_bars};
use disktable::{DiskSort, DiskTable};
use gauge::meter_row;
use nettable::{NetSort, NetTable};
use proctable::{ProcSort, ProcTable};

const HISTORY_CAP: usize = 600;
const PAD_X: usize = 2;
const PAD_Y: usize = 1;

fn dim(s: &str) -> String {
    style::dim(s)
}
fn help(s: &str) -> String {
    Style::new().fg(style::WHITE).render(s)
}
fn kill_bar(s: &str) -> String {
    Style::new().fg(style::WHITE).bg(style::RED).bold().render(s)
}
fn help_key(s: &str) -> String {
    Style::new().fg(style::WHITE).bold().render(s)
}
fn help_label(s: &str) -> String {
    Style::new().fg(style::BLACK).bg(style::CYAN).render(s)
}

/// height counts the display lines in a (possibly multi-line) string.
fn height(s: &str) -> usize {
    s.matches('\n').count() + 1
}

pub struct App {
    pub cpu: Cpu,
    pub mem: Memory,
    pub gpu: Gpu,
    pub net: Network,
    pub sys: System,
    pub disk: Disk,
    pub procs: Processes,

    cfg: Config,
    show_cores: bool,
    show_history: bool,
    show_disks: bool,
    show_net: bool,
    mounts: Vec<Mount>,
    procnet: ProcNet,
    procdisk: ProcDisk,
    paused: bool,
    interval_ms: i64,

    kill_pid: i32,
    kill_name: String,
    searching: bool,
    status: String,
    status_ttl: i32,

    pub width: usize,
    pub height: usize,
    table_top: usize,
    table: ProcTable,
    nettable: NetTable,
    disktable: DiskTable,
    hooks: Engine,

    cpu_hist: Vec<f64>,
    mem_hist: Vec<f64>,
    gpu_hist: Vec<f64>,
    rx_hist: Vec<f64>,
    tx_hist: Vec<f64>,
    disk_r_hist: Vec<f64>,
    disk_w_hist: Vec<f64>,

    pub should_quit: bool,
}

impl App {
    pub fn new(
        cpu: Cpu,
        mem: Memory,
        gpu: Gpu,
        net: Network,
        sys: System,
        disk: Disk,
        procs: Processes,
        cfg: Config,
        show_cores: bool,
        interval_ms: i64,
    ) -> App {
        let mut table = ProcTable::new();
        table.show_gpu = cfg.gpu && gpu.available;
        if let Ok(Some(u)) = nix::unistd::User::from_uid(nix::unistd::geteuid()) {
            table.mine_user = u.name;
        }
        table.set_rows(&procs.list);

        let show_cores = show_cores || cfg.cores;
        let show_history = cfg.history;
        let hooks = Engine::new(&cfg.hooks);
        let mut a = App {
            cpu,
            mem,
            gpu,
            net,
            sys,
            disk,
            procs,
            cfg,
            show_cores,
            show_history,
            show_disks: false,
            show_net: false,
            mounts: Vec::new(),
            procnet: ProcNet::new(),
            procdisk: ProcDisk::new(),
            paused: false,
            interval_ms,
            kill_pid: 0,
            kill_name: String::new(),
            searching: false,
            status: String::new(),
            status_ttl: 0,
            width: 0,
            height: 0,
            table_top: 0,
            table,
            nettable: NetTable::new(),
            disktable: DiskTable::new(),
            hooks,
            cpu_hist: Vec::new(),
            mem_hist: Vec::new(),
            gpu_hist: Vec::new(),
            rx_hist: Vec::new(),
            tx_hist: Vec::new(),
            disk_r_hist: Vec::new(),
            disk_w_hist: Vec::new(),
            should_quit: false,
        };
        a.append_histories();
        a
    }

    pub fn interval_ms(&self) -> i64 {
        self.interval_ms
    }

    /// state captures the view preferences worth remembering between runs.
    pub fn state(&self) -> State {
        State {
            sort: self.table.sort_by.as_str().to_string(),
            sort_asc: self.table.asc,
            tree: self.table.tree,
            mine: self.table.mine_only,
            cores: self.show_cores,
            history: self.show_history,
            interval: self.interval_ms,
        }
    }

    /// apply_state restores saved table preferences and the history toggle.
    pub fn apply_state(&mut self, s: &State) {
        self.table.sort_by = ProcSort::parse(&s.sort);
        self.table.asc = s.sort_asc;
        self.table.tree = s.tree;
        self.table.mine_only = s.mine;
        self.show_history = s.history;
        self.table.set_rows(&self.procs.list);
    }

    fn hook_values(&self) -> HashMap<&'static str, f64> {
        let mut v = HashMap::new();
        v.insert("cpu", self.cpu.aggregate().total);
        v.insert("mem", self.mem.ram.used_percent);
        v.insert("disk", self.disk.used_percent);
        if self.cpu.temp > 0.0 {
            v.insert("cpu_temp", self.cpu.temp);
        }
        if self.mem.has_swap() {
            v.insert("swap", self.mem.swap.used_percent);
        }
        if let Some(s) = self.gpu.summary() {
            v.insert("gpu", s.gpu_util);
            v.insert("gpu_mem", s.mem_used_percent);
        }
        v
    }

    fn gpu_summary(&self) -> Option<GpuSummary> {
        if !self.cfg.gpu {
            return None;
        }
        self.gpu.summary()
    }

    /// tick refreshes every metric source (unless paused) and runs hooks.
    pub fn tick(&mut self) {
        if self.paused {
            return;
        }
        self.cpu.update();
        let _ = self.mem.update();
        self.net.update();
        let _ = self.sys.update();
        self.disk.update();
        if self.show_disks {
            self.mounts = mounts();
            self.procdisk.update();
            self.disktable.set_rows(&self.procdisk.procs);
        }
        if self.show_net {
            self.procnet.update();
            self.nettable.set_rows(&self.procnet.conns);
        }
        self.gpu.update();
        let stats = self.gpu.process_stats();
        self.procs.update(self.mem.ram.total, &stats);
        self.table.set_rows(&self.procs.list);
        self.append_histories();
        if self.status_ttl > 0 {
            self.status_ttl -= 1;
            if self.status_ttl == 0 {
                self.status.clear();
            }
        }
        let mut fired = self.hooks.check(&self.hook_values(), Instant::now());
        fired.extend(self.hooks.errors());
        if !fired.is_empty() {
            self.status = fired.join("; ");
            self.status_ttl = 5;
        }
    }

    pub fn set_size(&mut self, w: usize, h: usize) {
        self.width = w;
        self.height = h;
    }

    // ---- input ----------------------------------------------------------

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        if self.show_net {
            match m.kind {
                MouseEventKind::ScrollUp => self.nettable.move_by(-3),
                MouseEventKind::ScrollDown => self.nettable.move_by(3),
                _ => {}
            }
            return;
        }
        if self.show_disks {
            match m.kind {
                MouseEventKind::ScrollUp => self.disktable.move_by(-3),
                MouseEventKind::ScrollDown => self.disktable.move_by(3),
                _ => {}
            }
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.table.move_by(-3),
            MouseEventKind::ScrollDown => self.table.move_by(3),
            MouseEventKind::Down(MouseButton::Left) => {
                if self.kill_pid != 0 || self.table_top == 0 {
                    return;
                }
                let y = m.row as usize;
                let x = m.column as i64;
                if y == self.table_top - 1 {
                    if let Some(s) = self.table.sort_hit_at(x - PAD_X as i64) {
                        self.set_sort(s);
                    }
                    return;
                }
                self.table.select_visible(y as i64 - self.table_top as i64);
            }
            _ => {}
        }
    }

    /// handle_key processes a key event; sets should_quit when the app exits.
    pub fn handle_key(&mut self, ev: KeyEvent) {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let typed = match ev.code {
            KeyCode::Char(c) if !ctrl && !ev.modifiers.contains(KeyModifiers::ALT) => Some(c),
            _ => None,
        };
        let key = key_string(&ev);

        if key == "ctrl+c" {
            self.should_quit = true;
            return;
        }

        // The kill-confirm prompt is modal.
        if self.kill_pid != 0 {
            match key.as_str() {
                "t" | "T" | "enter" => self.do_kill(Signal::SIGTERM, "SIGTERM"),
                "k" | "K" => self.do_kill(Signal::SIGKILL, "SIGKILL"),
                "esc" | "n" | "N" => self.kill_pid = 0,
                _ => {}
            }
            return;
        }

        // Network-view search prompt.
        if self.searching && self.show_net {
            match key.as_str() {
                "enter" => self.searching = false,
                "esc" => {
                    self.searching = false;
                    self.nettable.set_search("", &self.procnet.conns);
                }
                "backspace" => {
                    let mut q: Vec<char> = self.nettable.search.chars().collect();
                    if q.pop().is_some() {
                        self.nettable.set_search(&q.iter().collect::<String>(), &self.procnet.conns);
                    }
                }
                "down" => self.nettable.move_by(1),
                "up" => self.nettable.move_by(-1),
                "pgdown" => self.nettable.page(1),
                "pgup" => self.nettable.page(-1),
                _ => {
                    if let Some(c) = typed {
                        let q = format!("{}{}", self.nettable.search, c);
                        self.nettable.set_search(&q, &self.procnet.conns);
                    }
                }
            }
            return;
        }

        // Disk-view search prompt.
        if self.searching && self.show_disks {
            match key.as_str() {
                "enter" => self.searching = false,
                "esc" => {
                    self.searching = false;
                    self.disktable.set_search("", &self.procdisk.procs);
                }
                "backspace" => {
                    let mut q: Vec<char> = self.disktable.search.chars().collect();
                    if q.pop().is_some() {
                        self.disktable.set_search(&q.iter().collect::<String>(), &self.procdisk.procs);
                    }
                }
                "down" => self.disktable.move_by(1),
                "up" => self.disktable.move_by(-1),
                "pgdown" => self.disktable.page(1),
                "pgup" => self.disktable.page(-1),
                _ => {
                    if let Some(c) = typed {
                        let q = format!("{}{}", self.disktable.search, c);
                        self.disktable.set_search(&q, &self.procdisk.procs);
                    }
                }
            }
            return;
        }

        // Process-table search prompt.
        if self.searching {
            match key.as_str() {
                "enter" => self.searching = false,
                "esc" => {
                    self.searching = false;
                    self.table.search.clear();
                    self.table.set_rows(&self.procs.list);
                }
                "backspace" => {
                    if self.table.search.pop().is_some() {
                        self.table.set_rows(&self.procs.list);
                    }
                }
                "down" => self.table.move_by(1),
                "up" => self.table.move_by(-1),
                "pgdown" => self.table.page(1),
                "pgup" => self.table.page(-1),
                _ => {
                    if let Some(c) = typed {
                        self.table.search.push(c);
                        self.table.set_rows(&self.procs.list);
                    }
                }
            }
            return;
        }

        // Network view captures navigation, sorting, kill and search keys.
        if self.show_net {
            match key.as_str() {
                "down" | "j" => return self.nettable.move_by(1),
                "up" | "k" => return self.nettable.move_by(-1),
                "pgdown" => return self.nettable.page(1),
                "pgup" => return self.nettable.page(-1),
                "home" => return self.nettable.home(),
                "end" => return self.nettable.end(),
                "d" | "D" => return self.nettable.set_sort(NetSort::Down),
                "u" | "U" => return self.nettable.set_sort(NetSort::Up),
                "t" | "T" => return self.nettable.set_sort(NetSort::Total),
                "/" => {
                    self.searching = true;
                    return;
                }
                "esc" => {
                    if !self.nettable.search.is_empty() {
                        self.nettable.set_search("", &self.procnet.conns);
                    } else {
                        self.show_net = false;
                    }
                    return;
                }
                "x" | "X" | "f9" => {
                    if let Some(c) = self.nettable.selected() {
                        self.kill_pid = c.pid;
                        self.kill_name = c.name.clone();
                    }
                    return;
                }
                _ => {}
            }
        }

        // Disk view captures navigation, sorting, kill and search keys.
        if self.show_disks {
            match key.as_str() {
                "down" | "j" => return self.disktable.move_by(1),
                "up" | "k" => return self.disktable.move_by(-1),
                "pgdown" => return self.disktable.page(1),
                "pgup" => return self.disktable.page(-1),
                "home" => return self.disktable.home(),
                "end" => return self.disktable.end(),
                "r" | "R" => return self.disktable.set_sort(DiskSort::Read),
                "w" | "W" => return self.disktable.set_sort(DiskSort::Write),
                "t" | "T" => return self.disktable.set_sort(DiskSort::Total),
                "/" => {
                    self.searching = true;
                    return;
                }
                "esc" => {
                    if !self.disktable.search.is_empty() {
                        self.disktable.set_search("", &self.procdisk.procs);
                    } else {
                        self.show_disks = false;
                    }
                    return;
                }
                "x" | "X" | "f9" => {
                    if let Some(p) = self.disktable.selected() {
                        self.kill_pid = p.pid;
                        self.kill_name = p.name.clone();
                    }
                    return;
                }
                _ => {}
            }
        }

        match key.as_str() {
            "q" | "Q" => self.should_quit = true,
            "/" => self.searching = true,
            "esc" => {
                if self.show_disks || self.show_net {
                    self.show_disks = false;
                    self.show_net = false;
                } else if !self.table.search.is_empty() {
                    self.table.search.clear();
                    self.table.set_rows(&self.procs.list);
                }
            }
            "c" | "C" => self.show_cores = !self.show_cores,
            "h" | "H" => self.show_history = !self.show_history,
            "d" | "D" => {
                if !self.cfg.disk {
                    return;
                }
                self.show_disks = !self.show_disks;
                if self.show_disks {
                    self.show_net = false;
                    self.mounts = mounts();
                    self.procdisk.reset();
                    self.procdisk.update();
                    self.disktable.set_rows(&self.procdisk.procs);
                }
            }
            "n" | "N" => {
                if !self.cfg.system {
                    return;
                }
                self.show_net = !self.show_net;
                if self.show_net {
                    self.show_disks = false;
                    self.procnet.reset();
                    self.procnet.update();
                    self.nettable.set_rows(&self.procnet.conns);
                }
            }
            "t" | "T" => {
                self.table.tree = !self.table.tree;
                self.table.set_rows(&self.procs.list);
            }
            "u" | "U" => {
                self.table.mine_only = !self.table.mine_only;
                self.table.set_rows(&self.procs.list);
            }
            " " => self.paused = !self.paused,
            "-" | "_" => self.set_interval(self.interval_ms - 100),
            "+" | "=" => self.set_interval(self.interval_ms + 100),
            "down" | "j" => self.table.move_by(1),
            "up" | "k" => self.table.move_by(-1),
            "pgdown" => self.table.page(1),
            "pgup" => self.table.page(-1),
            "home" => self.table.home(),
            "end" => self.table.end(),
            "p" | "P" => self.set_sort(ProcSort::Cpu),
            "m" | "M" => self.set_sort(ProcSort::Mem),
            "g" | "G" => {
                if self.table.show_gpu {
                    self.set_sort(ProcSort::Gpu);
                }
            }
            "x" | "X" | "f9" => {
                if let Some(p) = self.table.selected() {
                    self.kill_pid = p.pid;
                    self.kill_name = p.command.clone();
                }
            }
            _ => {}
        }
    }

    fn set_interval(&mut self, ms: i64) {
        self.interval_ms = clamp_interval(ms);
    }

    fn set_sort(&mut self, s: ProcSort) {
        if self.table.sort_by == s {
            self.table.asc = !self.table.asc;
        } else {
            self.table.sort_by = s;
            self.table.asc = false;
        }
        self.table.set_rows(&self.procs.list);
        self.table.select_first();
    }

    fn do_kill(&mut self, sig: Signal, name: &str) {
        match kill_process(self.kill_pid, sig) {
            Err(e) => self.status = format!("kill {} failed: {}", self.kill_pid, e),
            Ok(()) => self.status = format!("sent {} to PID {}", name, self.kill_pid),
        }
        self.kill_pid = 0;
        self.status_ttl = 5;
    }

    fn append_histories(&mut self) {
        append_history(&mut self.cpu_hist, self.cpu.aggregate().total);
        append_history(&mut self.mem_hist, self.mem.ram.used_percent);
        append_history(&mut self.rx_hist, self.net.rx_per_sec);
        append_history(&mut self.tx_hist, self.net.tx_per_sec);
        append_history(&mut self.disk_r_hist, self.disk.read_per_sec);
        append_history(&mut self.disk_w_hist, self.disk.write_per_sec);
        if let Some(s) = self.gpu_summary() {
            append_history(&mut self.gpu_hist, s.gpu_util);
        }
    }

    // ---- rendering ------------------------------------------------------

    pub fn view(&mut self) -> String {
        let w = self.width as i64 - 2 * PAD_X as i64;
        let h = self.height as i64 - 2 * PAD_Y as i64;
        if w < 20 || h < 12 {
            return "terminal too small for teatop".into();
        }
        let (w, h) = (w as usize, h as usize);

        if self.show_net && self.cfg.system {
            return self.view_net_screen(w, h);
        }
        if self.show_disks && self.cfg.disk {
            return self.view_disk_screen(w, h);
        }

        const HELP_H: usize = 1;
        let header_h = if self.cfg.system { 2 } else { 0 };
        let gauges = self.view_gauges(w);
        let mut gauge_h = gauges.len();
        if gauge_h > 0 {
            gauge_h += 1;
        }
        let chart_visible = self.show_history || self.show_cores;
        let body = h as i64 - header_h as i64 - gauge_h as i64 - HELP_H as i64;
        let body = body.max(0) as usize;

        let (mut chart_h, mut table_h) = (0usize, 0usize);
        if chart_visible && self.cfg.processes {
            let mut ch = h * 22 / 100;
            if self.show_cores {
                let cp = self.cores_panel_height(w);
                if cp > ch {
                    ch = cp;
                }
                if body >= 5 && ch > body - 5 {
                    ch = body - 5;
                }
            }
            if ch < 5 {
                ch = 5;
            }
            chart_h = ch;
            let th = body as i64 - chart_h as i64 - 1;
            if th < 4 {
                table_h = 4;
                chart_h = (body as i64 - table_h as i64 - 1).max(0) as usize;
            } else {
                table_h = th as usize;
            }
        } else if chart_visible {
            chart_h = body;
        } else if self.cfg.processes {
            table_h = body;
        }

        let mut rows: Vec<String> = Vec::new();
        if header_h > 0 {
            rows.push(self.view_header(w));
            rows.push(String::new());
        }
        rows.extend(gauges);
        if gauge_h > 0 {
            rows.push(String::new());
        }
        if chart_h > 0 {
            rows.push(self.view_charts(w, chart_h));
            if table_h > 0 {
                rows.push(String::new());
            }
        }
        self.table_top = 0;
        if table_h > 0 {
            let used_before: usize = rows.iter().map(|r| height(r)).sum();
            self.table_top = PAD_Y + used_before + 2;
            rows.push(self.view_table(w, table_h));
        }
        let used: usize = rows.iter().map(|r| height(r)).sum();
        let filler = h as i64 - HELP_H as i64 - used as i64;
        if filler > 0 {
            rows.push("\n".repeat((filler - 1) as usize));
        }
        rows.push(self.view_help(w));
        frame_screen(&rows)
    }

    fn view_disk_screen(&mut self, w: usize, h: usize) -> String {
        const HELP_H: usize = 1;
        let mut pre: Vec<String> = vec![self.view_header(w), String::new(), self.disk_info_line()];
        pre.extend(self.disk_gauges(w));
        pre.push(String::new());

        let mut chart_h = 6;
        if h < 30 {
            chart_h = 4;
        }
        if h < 22 {
            chart_h = 3;
        }
        pre.extend(self.view_disk_chart(w, chart_h));
        pre.push(String::new());

        let used: usize = pre.iter().map(|r| height(r)).sum();
        let mut table_h = h as i64 - used as i64 - HELP_H as i64;
        if table_h < 3 {
            table_h = 3;
        }
        let mut rows = pre;
        rows.extend(self.disktable.view(w, table_h as usize));

        let u: usize = rows.iter().map(|r| height(r)).sum();
        let filler = h as i64 - HELP_H as i64 - u as i64;
        if filler > 0 {
            rows.push("\n".repeat((filler - 1) as usize));
        }
        rows.push(self.view_disk_help(w));
        frame_screen(&rows)
    }

    fn disk_info_line(&self) -> String {
        let mut info = format!("   {} procs   sort: {}", self.disktable.count(), self.disktable.sort_by.as_str());
        if !self.disktable.search.is_empty() {
            info.push_str(&format!("   search: {:?}", self.disktable.search));
        }
        if self.procdisk.restricted {
            info.push_str("   · sudo for all procs");
        }
        format!("{}{}", Style::new().fg(style::WHITE).bold().render("DISK"), dim(&info))
    }

    fn disk_gauges(&self, w: usize) -> Vec<String> {
        let _ = w;
        if self.mounts.is_empty() {
            return vec![dim("no filesystems found")];
        }
        const MAX_ROWS: usize = 6;
        let mut more = 0;
        let mounts: &[Mount] = if self.mounts.len() > MAX_ROWS {
            more = self.mounts.len() - MAX_ROWS;
            &self.mounts[..MAX_ROWS]
        } else {
            &self.mounts
        };
        let mut path_w = 0;
        for m in mounts {
            path_w = path_w.max(m.path.chars().count());
        }
        if path_w > 24 {
            path_w = 24;
        }

        const BAR_W: usize = 20;
        let c = Style::new().fg(style::DISK_COLOR);
        let mut lines: Vec<String> = Vec::new();
        for (i, m) in mounts.iter().enumerate() {
            if i > 0 {
                lines.push(String::new());
            }
            let filled = (clamp_percent(m.used_percent) / 100.0 * BAR_W as f64 + 0.5) as usize;
            let filled = filled.min(BAR_W);
            lines.push(format!(
                "{}  {}{}{}{}{}{}",
                help(&pad_truncate(&m.path, path_w)),
                c.render("["),
                c.render(&BAR_FILLED.repeat(filled)),
                dim(&BAR_EMPTY.repeat(BAR_W - filled)),
                c.render("]"),
                c.render(&format!("{:6.1}%", m.used_percent)),
                dim(&format!("  {:>8} / {:<8}", format_bytes(m.used), format_bytes(m.total))),
            ));
        }
        if more > 0 {
            lines.push(dim(&format!("… {} more filesystems", more)));
        }
        lines
    }

    fn view_disk_chart(&self, w: usize, h: usize) -> Vec<String> {
        let h = h.max(2);
        let mut scale = max_tail(&self.disk_r_hist, w);
        let s = max_tail(&self.disk_w_hist, w);
        if s > scale {
            scale = s;
        }
        if scale <= 0.0 {
            scale = 1.0;
        }
        let header = format!(
            "{}   {}  {}{}",
            Style::new().fg(style::GRAY).bold().render("R/W THROUGHPUT"),
            disktable::disk_read_style().render(&format!("R {}/s", format_bytes(self.disk.read_per_sec as u64))),
            disktable::disk_write_style().render(&format!("W {}/s", format_bytes(self.disk.write_per_sec as u64))),
            dim(&format!("   peak {}/s", format_bytes(scale as u64))),
        );
        let header = truncate_width(&header, w);
        let series = [self.disk_r_hist.as_slice(), self.disk_w_hist.as_slice()];
        let colors = [style::CYAN, style::PURPLE];
        let names = ["R", "W"];
        let mut out = vec![pad_line(&header, w)];
        out.extend(render_lanes(&series, &colors, &names, scale, w, h - 1));
        out
    }

    fn view_disk_help(&self, w: usize) -> String {
        if self.kill_pid != 0 {
            return self.kill_prompt(w);
        }
        if self.searching {
            return self.search_prompt(&self.disktable.search, w);
        }
        let items = [
            ("↑↓", "select"),
            ("r/w/t", "sort"),
            ("/", "search"),
            ("x", "kill"),
            ("spc", "pause"),
            ("d/esc", "back"),
            ("q", "quit"),
        ];
        self.chip_bar(&items)
    }

    fn net_addr_line(&self) -> String {
        let mut ips = String::new();
        if !self.sys.local_ip.is_empty() {
            ips = format!("{}{}", dim("lan "), help(&self.sys.local_ip));
        }
        let ext = self.sys.external_ip();
        if !ext.is_empty() {
            if !ips.is_empty() {
                ips.push_str("   ");
            }
            ips.push_str(&format!("{}{}", dim("wan "), help(&ext)));
        }
        ips
    }

    fn view_net_screen(&mut self, w: usize, h: usize) -> String {
        const HELP_H: usize = 1;
        let mut pre: Vec<String> = vec![self.view_header(w), String::new(), self.net_info_line()];
        let ips = self.net_addr_line();
        if !ips.is_empty() {
            pre.push(ips);
        }

        let mut chart_h = 6;
        if h < 30 {
            chart_h = 4;
        }
        if h < 22 {
            chart_h = 3;
        }
        pre.extend(self.view_net_chart(w, chart_h));
        pre.push(String::new());

        let body: Vec<String> = if self.procnet.need_root {
            vec![
                dim("per-connection bandwidth needs root — rerun with "),
                Style::new().fg(style::WHITE).bold().render("  sudo teatop"),
            ]
        } else {
            let used: usize = pre.iter().map(|r| height(r)).sum();
            let mut table_h = h as i64 - used as i64 - HELP_H as i64;
            if table_h < 3 {
                table_h = 3;
            }
            self.nettable.view(w, table_h as usize)
        };
        let mut rows = pre;
        rows.extend(body);

        let u: usize = rows.iter().map(|r| height(r)).sum();
        let filler = h as i64 - HELP_H as i64 - u as i64;
        if filler > 0 {
            rows.push("\n".repeat((filler - 1) as usize));
        }
        rows.push(self.view_net_help(w));
        frame_screen(&rows)
    }

    fn net_info_line(&self) -> String {
        let mut info = format!("   {} conns   sort: {}", self.nettable.count(), self.nettable.sort_by.as_str());
        if !self.nettable.search.is_empty() {
            info.push_str(&format!("   search: {:?}", self.nettable.search));
        }
        format!("{}{}", Style::new().fg(style::WHITE).bold().render("NETWORK"), dim(&info))
    }

    fn view_net_help(&self, w: usize) -> String {
        if self.kill_pid != 0 {
            return self.kill_prompt(w);
        }
        if self.searching {
            return self.search_prompt(&self.nettable.search, w);
        }
        let items = [
            ("↑↓", "select"),
            ("d/u/t", "sort"),
            ("/", "search"),
            ("x", "kill"),
            ("spc", "pause"),
            ("n/esc", "back"),
            ("q", "quit"),
        ];
        self.chip_bar(&items)
    }

    fn view_net_chart(&self, w: usize, h: usize) -> Vec<String> {
        let h = h.max(2);
        let mut scale = self.net.peak_rx;
        if self.net.peak_tx > scale {
            scale = self.net.peak_tx;
        }
        if scale <= 0.0 {
            scale = 1.0;
        }
        let header = format!(
            "{}   {}  {}{}",
            Style::new().fg(style::GRAY).bold().render("THROUGHPUT"),
            Style::new().fg(style::RED).render(&format!("↓ {}/s", format_bytes(self.net.rx_per_sec as u64))),
            Style::new().fg(style::BLUE).render(&format!("↑ {}/s", format_bytes(self.net.tx_per_sec as u64))),
            dim(&format!("   peak {}/s", format_bytes(scale as u64))),
        );
        let header = truncate_width(&header, w);
        let series = [self.rx_hist.as_slice(), self.tx_hist.as_slice()];
        let colors = [style::RED, style::BLUE];
        let names = ["↓", "↑"];
        let mut out = vec![pad_line(&header, w)];
        out.extend(render_lanes(&series, &colors, &names, scale, w, h - 1));
        out
    }

    fn view_header(&self, w: usize) -> String {
        let mut cores = self.cpu.count as f64;
        if cores < 1.0 {
            cores = 1.0;
        }
        let mut loads = Vec::new();
        for v in [self.sys.load1, self.sys.load5, self.sys.load15] {
            loads.push(Style::new().fg(load_color(v / cores * 100.0)).render(&format!("{:.2}", v)));
        }
        let status = format!(
            "{}{}{}{}",
            dim("load "),
            loads.join(" "),
            dim("   up "),
            help(&format_uptime(self.sys.uptime_secs)),
        );
        header_line(&status, "", w)
    }

    fn meter_specs(&self) -> Vec<MeterSpec> {
        let mut specs = Vec::new();
        if self.cfg.cpu {
            let agg = self.cpu.aggregate();
            let mut detail = String::new();
            let f = mean_freq(&self.cpu.freq);
            if f > 0.0 {
                detail.push_str(&format!("{:.1}GHz ", f / 1000.0));
            }
            if self.cpu.temp > 0.0 {
                detail.push_str(&format!("{:.0}°C ", self.cpu.temp));
            }
            detail.push_str(&format!(" usr {:.0} sys {:.0} iow {:.0}", agg.user, agg.system, agg.iowait));
            specs.push(MeterSpec { label: "CPU", detail, percent: agg.total, color: style::CPU_COLOR });
        }
        if self.cfg.memory {
            let r = self.mem.ram;
            specs.push(MeterSpec {
                label: "MEM",
                detail: format!("{} / {}  cache {}", format_bytes(r.used), format_bytes(r.total), format_bytes(r.cached + r.buffers)),
                percent: r.used_percent,
                color: style::MEM_COLOR,
            });
        }
        if self.cfg.swap && self.mem.has_swap() {
            let s = self.mem.swap;
            specs.push(MeterSpec {
                label: "SWP",
                detail: format!("{} / {}", format_bytes(s.used), format_bytes(s.total)),
                percent: s.used_percent,
                color: style::SWAP_COLOR,
            });
        }
        if self.cfg.disk {
            specs.push(MeterSpec {
                label: "DSK",
                detail: format!("{} / {}", format_bytes(self.disk.used), format_bytes(self.disk.total)),
                percent: self.disk.used_percent,
                color: style::DISK_COLOR,
            });
        }
        if let Some(s) = self.gpu_summary() {
            let mut gp: Vec<String> = Vec::new();
            if s.max_temp > 0 {
                gp.push(format!("{}°C", s.max_temp));
            }
            if s.power_draw > 0 {
                gp.push(format!("{}W", s.power_draw / 1000));
            }
            if s.count > 1 {
                gp.push(format!("×{}", s.count));
            }
            specs.push(MeterSpec { label: "GPU", detail: gp.join(" "), percent: s.gpu_util, color: style::GPU_COLOR });
            specs.push(MeterSpec {
                label: "VRAM",
                detail: format!("{} / {}", format_bytes(s.mem_used), format_bytes(s.mem_total)),
                percent: s.mem_used_percent,
                color: style::GPU_MEM_COLOR,
            });
        }
        specs
    }

    fn view_gauges(&self, w: usize) -> Vec<String> {
        let specs = self.meter_specs();
        if specs.is_empty() {
            return Vec::new();
        }
        let cols = if w >= 110 && specs.len() > 2 { 2 } else { 1 };
        let cw = (w - LAYOUT_GAP * (cols - 1)) / cols;

        let mut rows = Vec::new();
        let mut i = 0;
        while i < specs.len() {
            if !rows.is_empty() {
                rows.push(String::new());
            }
            let s = &specs[i];
            let mut row = meter_row(s.label, &s.detail, s.percent, s.color, cw);
            if cols == 2 && i + 1 < specs.len() {
                let s2 = &specs[i + 1];
                row.push_str(&spaces(LAYOUT_GAP));
                row.push_str(&meter_row(s2.label, &s2.detail, s2.percent, s2.color, cw));
            }
            rows.push(row);
            i += cols;
        }
        rows
    }

    fn chart_split(&self, w: usize) -> (usize, usize) {
        let mut plot_w = 0;
        let mut cores_w = 0;
        if self.show_history {
            plot_w = w;
            if self.show_cores {
                plot_w = w * 62 / 100;
            }
        }
        if self.show_cores {
            cores_w = w - plot_w;
            if plot_w > 0 {
                cores_w -= LAYOUT_GAP;
            }
        }
        (plot_w, cores_w)
    }

    fn view_charts(&self, w: usize, h: usize) -> String {
        let (plot_w, cores_w) = self.chart_split(w);

        let mut plot = String::new();
        if plot_w > 0 {
            let mut series: Vec<&[f64]> = vec![self.cpu_hist.as_slice(), self.mem_hist.as_slice()];
            let mut colors = vec![style::CPU_COLOR, style::MEM_COLOR];
            let mut names = vec!["cpu", "mem"];
            let mut header = format!(
                "{}   {}  {}",
                Style::new().fg(style::GRAY).bold().render("HISTORY"),
                Style::new().fg(style::CPU_COLOR).render(&format!("cpu {:.0}%", self.cpu.aggregate().total)),
                Style::new().fg(style::MEM_COLOR).render(&format!("mem {:.0}%", self.mem.ram.used_percent)),
            );
            if let Some(s) = self.gpu_summary() {
                series.push(self.gpu_hist.as_slice());
                colors.push(style::GPU_COLOR);
                names.push("gpu");
                header.push_str(&format!("  {}", Style::new().fg(style::GPU_COLOR).render(&format!("gpu {:.0}%", s.gpu_util))));
            }
            header.push_str(&dim("  0-100%"));
            let header = truncate_width(&header, plot_w);
            let mut lines = vec![pad_line(&header, plot_w)];
            lines.extend(render_lanes(&series, &colors, &names, 100.0, plot_w, h - 1));
            plot = lines.join("\n");
        }
        if !self.show_cores {
            return plot;
        }

        let data: Vec<f64> = (0..self.cpu.count).map(|i| self.cpu.core(i).total).collect();
        let mut lines = vec![pad_line(&Style::new().fg(style::GRAY).bold().render("CORES"), cores_w)];
        lines.extend(render_core_bars(&data, 100.0, cores_w, h - 1));
        let cores = lines.join("\n");
        if plot.is_empty() {
            return cores;
        }
        join_horizontal_top(&plot, LAYOUT_GAP, &cores)
    }

    fn cores_panel_height(&self, w: usize) -> usize {
        let (_, cores_w) = self.chart_split(w);
        core_bars_spaced_height(self.cpu.count, cores_w) + 1
    }

    fn view_table(&mut self, w: usize, h: usize) -> String {
        let mut lines = vec![dim(&pad_truncate(&self.table.title(), w))];
        lines.extend(self.table.view(w, h - 1));
        lines.join("\n")
    }

    fn view_help(&self, w: usize) -> String {
        if self.kill_pid != 0 {
            return self.kill_prompt(w);
        }
        if self.searching {
            return self.search_prompt(&self.table.search, w);
        }
        let mut sort_key = "p/m".to_string();
        if self.table.show_gpu {
            sort_key.push_str("/g");
        }
        let mut items: Vec<(String, String)> = vec![
            ("q".into(), "quit".into()),
            ("c".into(), format!("cores {}", on_off(self.show_cores))),
            ("h".into(), format!("hist {}", on_off(self.show_history))),
        ];
        if self.cfg.disk {
            items.push(("d".into(), format!("disks {}", on_off(self.show_disks))));
        }
        if self.cfg.system {
            items.push(("n".into(), format!("net {}", on_off(self.show_net))));
        }
        items.push(("t".into(), "tree".into()));
        items.push(("u".into(), "mine".into()));
        items.push((sort_key, "sort".into()));
        items.push(("/".into(), "search".into()));
        items.push(("x".into(), "kill".into()));
        items.push(("spc".into(), "pause".into()));
        items.push(("-/+".into(), format!("{}ms", self.interval_ms)));

        let chips: Vec<String> = items.iter().map(|(k, l)| format!("{}{}", help_key(k), help_label(&format!(" {} ", l)))).collect();
        let prefix = if self.paused { format!("{} ", kill_bar(" PAUSED ")) } else { String::new() };
        let suffix = if !self.status.is_empty() {
            format!("  {}", Style::new().fg(style::YELLOW).render(&self.status))
        } else {
            String::new()
        };
        let line = |gap: usize| format!("{}{}{}", prefix, chips.join(&spaces(gap)), suffix);

        let mut readouts: Vec<String> = Vec::new();
        if self.cfg.system {
            readouts.push(help(&format!(
                "R {}/s W {}/s",
                format_bytes(self.disk.read_per_sec as u64),
                format_bytes(self.disk.write_per_sec as u64)
            )));
        }
        for sys in &readouts {
            for gap in (0..=2).rev() {
                let l = line(gap);
                let pad = w as i64 - style::width(&l) as i64 - style::width(sys) as i64;
                if pad >= 2 {
                    return format!("{}{}{}", l, spaces(pad as usize), sys);
                }
            }
        }
        for gap in (0..=2).rev() {
            let l = line(gap);
            if style::width(&l) <= w {
                return pad_line(&l, w);
            }
        }
        pad_line(&line(0), w)
    }

    fn chip_bar(&self, items: &[(&str, &str)]) -> String {
        let chips: Vec<String> = items.iter().map(|(k, l)| format!("{}{}", help_key(k), help_label(&format!(" {} ", l)))).collect();
        let prefix = if self.paused { format!("{} ", kill_bar(" PAUSED ")) } else { String::new() };
        format!("{}{}", prefix, chips.join(" "))
    }

    fn kill_prompt(&self, w: usize) -> String {
        let name = truncate_width(&self.kill_name, 30);
        kill_bar(&pad_truncate(
            &format!("Kill PID {} ({})?   t = SIGTERM   k = SIGKILL   Esc = cancel", self.kill_pid, name),
            w,
        ))
    }

    fn search_prompt(&self, query: &str, w: usize) -> String {
        help_label(&pad_truncate(&format!("search: {}█   Enter = keep filter   Esc = clear", query), w))
    }
}

struct MeterSpec {
    label: &'static str,
    detail: String,
    percent: f64,
    color: u8,
}

/// clamp_interval bounds a refresh interval to the supported 100-5000ms range.
pub fn clamp_interval(ms: i64) -> i64 {
    ms.clamp(100, 5000)
}

fn append_history(h: &mut Vec<f64>, v: f64) {
    h.push(v);
    if h.len() > HISTORY_CAP {
        let drop = h.len() - HISTORY_CAP;
        h.drain(0..drop);
    }
}

/// max_tail returns the largest value in the last n samples of hist.
fn max_tail(hist: &[f64], n: usize) -> f64 {
    let start = hist.len().saturating_sub(n);
    hist[start..].iter().cloned().fold(0.0, f64::max)
}

fn mean_freq(freqs: &[f64]) -> f64 {
    let mut sum = 0.0;
    let mut n = 0;
    for &f in freqs {
        if f > 0.0 {
            sum += f;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

fn on_off(b: bool) -> &'static str {
    if b { "(on)" } else { "(off)" }
}

/// header_line right-aligns the second segment against the first, dropping it
/// when the line is too narrow for both.
fn header_line(left: &str, right: &str, w: usize) -> String {
    let pad = w as i64 - style::width(left) as i64 - style::width(right) as i64;
    if !right.is_empty() && pad >= 2 {
        format!("{}{}{}", left, spaces(pad as usize), right)
    } else {
        pad_line(&truncate_width(left, w), w)
    }
}

/// truncate_width clips a styled string to at most w display cells, preserving
/// escape sequences up to the cut (a lightweight MaxWidth).
fn truncate_width(s: &str, w: usize) -> String {
    if style::width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut cells = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            if chars.peek() == Some(&'[') {
                out.push(chars.next().unwrap());
                while let Some(&nc) = chars.peek() {
                    out.push(chars.next().unwrap());
                    if ('@'..='~').contains(&nc) {
                        break;
                    }
                }
            }
            continue;
        }
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if cells + cw > w {
            break;
        }
        out.push(c);
        cells += cw;
    }
    out.push_str("\x1b[0m");
    out
}

/// join_horizontal_top places two multi-line blocks side by side, separated by
/// gap spaces on every row, aligning their tops (lipgloss.JoinHorizontal).
fn join_horizontal_top(left: &str, gap: usize, right: &str) -> String {
    let l: Vec<&str> = left.split('\n').collect();
    let r: Vec<&str> = right.split('\n').collect();
    let lw = l.iter().map(|s| style::width(s)).max().unwrap_or(0);
    let n = l.len().max(r.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let ll = l.get(i).copied().unwrap_or("");
        let rr = r.get(i).copied().unwrap_or("");
        out.push(format!("{}{}{}", pad_line(ll, lw), spaces(gap), rr));
    }
    out.join("\n")
}

/// frame_screen joins full-screen rows and applies the padX/padY margin.
fn frame_screen(rows: &[String]) -> String {
    let content = rows.join("\n");
    let mut out = String::new();
    for _ in 0..PAD_Y {
        out.push('\n');
    }
    let pad = spaces(PAD_X);
    for line in content.split('\n') {
        out.push_str(&pad);
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// key_string maps a crossterm key event to bubbletea-style key names.
fn key_string(ev: &KeyEvent) -> String {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    match ev.code {
        KeyCode::Char(c) => {
            if ctrl {
                format!("ctrl+{}", c.to_ascii_lowercase())
            } else {
                c.to_string()
            }
        }
        KeyCode::Esc => "esc".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::PageUp => "pgup".into(),
        KeyCode::PageDown => "pgdown".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::F(n) => format!("f{}", n),
        _ => String::new(),
    }
}
