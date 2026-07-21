//! Scans the process table and computes per-process CPU usage. On Linux from
//! /proc via the procfs crate (cumulative CPU-time deltas); on macOS via
//! sysinfo (which computes the deltas itself).

use nix::sys::signal::Signal;
use nix::unistd::Pid;
use std::collections::HashMap;

use crate::metrics::gpu::GpuProcStat;

/// Process is a snapshot of one running process for the table view.
#[derive(Clone, Default)]
pub struct Process {
    pub pid: i32,
    pub ppid: i32,
    pub user: String,

    pub cpu_percent: f64, // % of one core, htop-style (can exceed 100)
    pub mem_percent: f64, // RSS as a share of MemTotal
    pub rss: u64,         // bytes

    pub gpu_percent: f64, // SM utilisation attributed to this PID
    pub gpu_mem: u64,     // GPU memory in use, bytes

    pub command: String, // full cmdline, or [name] when no cmdline is readable
}

/// kill_process sends sig to pid.
pub fn kill_process(pid: i32, sig: Signal) -> Result<(), String> {
    nix::sys::signal::kill(Pid::from_raw(pid), sig).map_err(|e| e.to_string())
}

// ---- Linux backend ------------------------------------------------------

#[cfg(target_os = "linux")]
pub struct Processes {
    pub list: Vec<Process>,
    prev_cpu: HashMap<i32, f64>, // pid -> cumulative CPU seconds at previous update
    prev_time: std::time::Instant,
    user_cache: HashMap<u32, String>,
    ticks: f64,
    page_size: u64,
}

#[cfg(target_os = "linux")]
impl Processes {
    pub fn new() -> Processes {
        Processes {
            list: Vec::new(),
            prev_cpu: HashMap::new(),
            prev_time: std::time::Instant::now(),
            user_cache: HashMap::new(),
            ticks: procfs::ticks_per_second() as f64,
            page_size: procfs::page_size(),
        }
    }

    /// update rescans the process table. mem_total scales mem_percent;
    /// gpu_stats attributes GPU usage to PIDs.
    pub fn update(&mut self, mem_total: u64, gpu_stats: &HashMap<u32, GpuProcStat>) {
        let procs = match procfs::process::all_processes() {
            Ok(p) => p,
            Err(_) => return,
        };
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.prev_time).as_secs_f64();

        let mut list = Vec::new();
        let mut cpu_now: HashMap<i32, f64> = HashMap::new();
        for pr in procs.flatten() {
            let stat = match pr.stat() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut proc = Process { pid: pr.pid(), ..Process::default() };
            proc.command = command(&pr, &stat);
            if proc.command.is_empty() {
                continue;
            }
            proc.ppid = stat.ppid;
            proc.rss = stat.rss * self.page_size;
            if mem_total > 0 {
                proc.mem_percent = proc.rss as f64 / mem_total as f64 * 100.0;
            }
            proc.user = self.username(&pr);

            let secs = (stat.utime + stat.stime) as f64 / self.ticks;
            cpu_now.insert(proc.pid, secs);
            if let Some(&prev) = self.prev_cpu.get(&proc.pid) {
                if elapsed > 0.0 && secs >= prev {
                    proc.cpu_percent = (secs - prev) / elapsed * 100.0;
                }
            }
            if let Some(gs) = gpu_stats.get(&(proc.pid as u32)) {
                proc.gpu_percent = gs.sm_util as f64;
                proc.gpu_mem = gs.mem_used;
            }
            list.push(proc);
        }
        self.list = list;
        self.prev_cpu = cpu_now;
        self.prev_time = now;
    }

    fn username(&mut self, pr: &procfs::process::Process) -> String {
        let uid = match pr.uid() {
            Ok(u) => u,
            Err(_) => return "?".into(),
        };
        if let Some(name) = self.user_cache.get(&uid) {
            return name.clone();
        }
        let name = match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
            Ok(Some(u)) => u.name,
            _ => uid.to_string(),
        };
        self.user_cache.insert(uid, name.clone());
        name
    }
}

/// command returns the full cmdline, "[comm]" for processes without one
/// (kernel threads), or "" when nothing is readable at all.
#[cfg(target_os = "linux")]
fn command(pr: &procfs::process::Process, stat: &procfs::process::Stat) -> String {
    if let Ok(args) = pr.cmdline() {
        let joined = args.join(" ");
        if !joined.trim().is_empty() {
            return joined;
        }
    }
    if !stat.comm.is_empty() {
        return format!("[{}]", stat.comm);
    }
    String::new()
}

// ---- macOS (sysinfo) backend --------------------------------------------

#[cfg(not(target_os = "linux"))]
pub struct Processes {
    pub list: Vec<Process>,
    sys: sysinfo::System,
    users: sysinfo::Users,
}

#[cfg(not(target_os = "linux"))]
impl Processes {
    pub fn new() -> Processes {
        Processes {
            list: Vec::new(),
            sys: sysinfo::System::new(),
            users: sysinfo::Users::new_with_refreshed_list(),
        }
    }

    /// update rescans the process table via sysinfo. Fields the platform
    /// withholds for other users' processes (CPU time, RSS) stay zero, matching
    /// the Go build's behaviour without sudo.
    pub fn update(&mut self, mem_total: u64, gpu_stats: &HashMap<u32, GpuProcStat>) {
        self.sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let mut list = Vec::new();
        for (pid, pr) in self.sys.processes() {
            let mut proc = Process { pid: pid.as_u32() as i32, ..Process::default() };
            let cmd = pr.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            proc.command = if cmd.trim().is_empty() {
                let name = pr.name().to_string_lossy();
                if name.is_empty() { continue } else { format!("[{}]", name) }
            } else {
                cmd
            };
            proc.ppid = pr.parent().map_or(0, |p| p.as_u32() as i32);
            proc.rss = pr.memory();
            if mem_total > 0 {
                proc.mem_percent = proc.rss as f64 / mem_total as f64 * 100.0;
            }
            proc.user = pr
                .user_id()
                .and_then(|uid| self.users.get_user_by_id(uid))
                .map_or_else(|| "?".to_string(), |u| u.name().to_string());
            proc.cpu_percent = pr.cpu_usage() as f64;
            if let Some(gs) = gpu_stats.get(&(proc.pid as u32)) {
                proc.gpu_percent = gs.sm_util as f64;
                proc.gpu_mem = gs.mem_used;
            }
            list.push(proc);
        }
        self.list = list;
    }
}
