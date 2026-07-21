//! Attributes live disk I/O to processes by sampling each process's cumulative
//! read/written byte counters and diffing successive samples into rates. On
//! Linux from /proc/<pid>/io; on macOS from proc_pid_rusage (libproc).

use std::collections::HashMap;
use std::time::Instant;

use super::counter_delta;

/// ProcIO is one process's current disk throughput.
#[derive(Clone, Default)]
pub struct ProcIO {
    pub pid: i32,
    pub name: String,
    pub read_per_sec: f64,
    pub write_per_sec: f64,
}

/// ProcDisk attributes live disk I/O to processes. Restricted is set when the
/// sampler ran without root, so the table can note that coverage is partial.
pub struct ProcDisk {
    pub procs: Vec<ProcIO>,
    pub restricted: bool,
    prev: HashMap<i32, (u64, u64)>,
    prev_time: Instant,
    primed: bool,
}

impl ProcDisk {
    pub fn new() -> ProcDisk {
        ProcDisk { procs: Vec::new(), restricted: false, prev: HashMap::new(), prev_time: Instant::now(), primed: false }
    }

    /// reset drops the baseline so the next update starts a fresh measurement
    /// window (used when the disk page is opened).
    pub fn reset(&mut self) {
        self.primed = false;
    }

    pub fn update(&mut self) {
        let (cur, names, restricted) = sample_proc_disk();
        self.restricted = restricted;
        let now = Instant::now();
        let dt = now.duration_since(self.prev_time).as_secs_f64();

        if self.primed && dt > 0.0 {
            let mut procs = Vec::with_capacity(cur.len());
            for (pid, &(r, w)) in &cur {
                let (pr, pw) = match self.prev.get(pid) {
                    Some(v) => *v,
                    None => continue,
                };
                let dr = counter_delta(r, pr);
                let dw = counter_delta(w, pw);
                if dr == 0 && dw == 0 {
                    continue;
                }
                procs.push(ProcIO {
                    pid: *pid,
                    name: names.get(pid).cloned().unwrap_or_default(),
                    read_per_sec: dr as f64 / dt,
                    write_per_sec: dw as f64 / dt,
                });
            }
            self.procs = procs;
        }

        self.prev = cur;
        self.prev_time = now;
        self.primed = true;
    }
}

// ---- Linux backend ------------------------------------------------------

/// sample_proc_disk reads each process's cumulative disk read/written bytes
/// from /proc/<pid>/io, plus a name from /proc/<pid>/comm.
#[cfg(target_os = "linux")]
fn sample_proc_disk() -> (HashMap<i32, (u64, u64)>, HashMap<i32, String>, bool) {
    use std::fs;
    let mut counters = HashMap::new();
    let mut names = HashMap::new();
    let restricted = nix::unistd::geteuid().as_raw() != 0;

    let entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return (counters, names, restricted),
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let pid: i32 = match fname.to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let (read, write) = match read_proc_io(&format!("/proc/{}/io", pid)) {
            Some(v) => v,
            None => continue,
        };
        counters.insert(pid, (read, write));
        names.insert(pid, proc_name(pid));
    }
    (counters, names, restricted)
}

#[cfg(target_os = "linux")]
fn read_proc_io(path: &str) -> Option<(u64, u64)> {
    let data = std::fs::read_to_string(path).ok()?;
    let mut read = 0;
    let mut write = 0;
    for line in data.lines() {
        if let Some((field, val)) = line.split_once(": ") {
            match field {
                "read_bytes" => read = val.trim().parse().unwrap_or(0),
                "write_bytes" => write = val.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    Some((read, write))
}

/// proc_name reads a process's command name from /proc/<pid>/comm.
#[cfg(target_os = "linux")]
pub fn proc_name(pid: i32) -> String {
    match std::fs::read_to_string(format!("/proc/{}/comm", pid)) {
        Ok(s) => s.trim().to_string(),
        Err(_) => "?".into(),
    }
}

// ---- macOS (libproc) backend --------------------------------------------

/// sample_proc_disk reads each process's cumulative disk I/O from the kernel
/// via proc_pid_rusage, plus a short name from proc_pidinfo. proc_pid_rusage
/// succeeds for the caller's own processes without privileges; other users'
/// need root, so restricted is set when we are not root.
#[cfg(not(target_os = "linux"))]
fn sample_proc_disk() -> (HashMap<i32, (u64, u64)>, HashMap<i32, String>, bool) {
    let mut counters = HashMap::new();
    let mut names = HashMap::new();
    let restricted = nix::unistd::geteuid().as_raw() != 0;

    for pid in list_all_pids() {
        if pid <= 0 {
            continue;
        }
        let mut ri: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, &mut ri as *mut _ as *mut libc::rusage_info_t)
        };
        if rc != 0 {
            continue; // not permitted (other user) or gone
        }
        counters.insert(pid, (ri.ri_diskio_bytesread, ri.ri_diskio_byteswritten));
        names.insert(pid, proc_name(pid));
    }
    (counters, names, restricted)
}

/// list_all_pids returns every process ID the kernel reports, growing the
/// buffer until it holds them all.
#[cfg(not(target_os = "linux"))]
fn list_all_pids() -> Vec<i32> {
    let mut n = 512usize;
    loop {
        let mut buf = vec![0i32; n];
        let bytes = (buf.len() * std::mem::size_of::<i32>()) as libc::c_int;
        let count = unsafe { libc::proc_listallpids(buf.as_mut_ptr() as *mut libc::c_void, bytes) };
        if count <= 0 {
            return Vec::new();
        }
        let count = count as usize;
        if count < n {
            buf.truncate(count);
            return buf;
        }
        n *= 2; // buffer was full; there may be more
    }
}

/// proc_name returns a process's short command name via proc_pidinfo.
#[cfg(not(target_os = "linux"))]
pub fn proc_name(pid: i32) -> String {
    let mut bi: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let rc = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut bi as *mut _ as *mut libc::c_void, size)
    };
    if rc <= 0 {
        return "?".into();
    }
    let comm: &[libc::c_char] = &bi.pbi_comm;
    let bytes: Vec<u8> = comm.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
