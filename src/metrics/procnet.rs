//! Attributes live per-connection network throughput to processes. On Linux via
//! the kernel's sock_diag (netlink) interface plus /proc; on macOS by shelling
//! out to `nettop`. Both diff successive cumulative samples into rates.

use std::collections::HashMap;
use std::time::Instant;

use super::counter_delta;

/// ProcConn is one live connection's current throughput, attributed to its
/// owning process.
#[derive(Clone, Default)]
#[allow(dead_code)] // state is captured from the kernel but not shown in the table
pub struct ProcConn {
    pub pid: i32,
    pub name: String,
    pub remote: String, // e.g. "140.82.121.4:443"
    pub state: String,  // TCP state, e.g. "ESTABLISHED"
    pub rx_per_sec: f64,
    pub tx_per_sec: f64,
}

// ---- Linux backend ------------------------------------------------------

#[cfg(target_os = "linux")]
pub use linux::ProcNet;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::metrics::procdisk::proc_name;

    #[derive(Clone, Default)]
    struct SockSample {
        tx: u64,
        rx: u64,
        remote: String,
        state: String,
    }

    pub struct ProcNet {
        pub conns: Vec<ProcConn>,
        pub need_root: bool,
        prev: HashMap<u32, SockSample>,
        prev_time: Instant,
        primed: bool,
    }

    impl ProcNet {
        pub fn new() -> ProcNet {
            ProcNet { conns: Vec::new(), need_root: false, prev: HashMap::new(), prev_time: Instant::now(), primed: false }
        }

        pub fn reset(&mut self) {
            self.primed = false;
        }

        pub fn update(&mut self) {
            if nix::unistd::geteuid().as_raw() != 0 {
                self.need_root = true;
                self.conns = Vec::new();
                return;
            }
            self.need_root = false;

            let socks = tcp_sockets();
            let now = Instant::now();
            let dt = now.duration_since(self.prev_time).as_secs_f64();

            if self.primed && dt > 0.0 {
                let mut inode_pid: Option<HashMap<u32, i32>> = None;
                let mut conns = Vec::new();
                for (inode, cur) in &socks {
                    let prev = match self.prev.get(inode) {
                        Some(p) => p,
                        None => continue,
                    };
                    let dtx = counter_delta(cur.tx, prev.tx);
                    let drx = counter_delta(cur.rx, prev.rx);
                    if dtx == 0 && drx == 0 {
                        continue;
                    }
                    if cur.remote.is_empty() {
                        continue; // listener or unconnected socket
                    }
                    let map = inode_pid.get_or_insert_with(inode_to_pid);
                    let pid = match map.get(inode) {
                        Some(p) => *p,
                        None => continue,
                    };
                    conns.push(ProcConn {
                        pid,
                        name: proc_name(pid),
                        remote: cur.remote.clone(),
                        state: cur.state.clone(),
                        rx_per_sec: drx as f64 / dt,
                        tx_per_sec: dtx as f64 / dt,
                    });
                }
                self.conns = conns;
            }

            self.prev = socks;
            self.prev_time = now;
            self.primed = true;
        }
    }

    const SOCK_DIAG_BY_FAMILY: u16 = 20;
    const INET_DIAG_INFO: u16 = 2;
    const NETLINK_INET_DIAG: i32 = 4;
    const NLM_F_REQUEST: u16 = 1;
    const NLM_F_DUMP: u16 = 0x300;
    const NLMSG_ERROR: u16 = 2;
    const NLMSG_DONE: u16 = 3;

    fn tcp_state_name(s: u8) -> String {
        match s {
            1 => "ESTABLISHED",
            2 => "SYN_SENT",
            3 => "SYN_RECV",
            4 => "FIN_WAIT1",
            5 => "FIN_WAIT2",
            6 => "TIME_WAIT",
            7 => "CLOSE",
            8 => "CLOSE_WAIT",
            9 => "LAST_ACK",
            10 => "LISTEN",
            11 => "CLOSING",
            _ => "",
        }
        .to_string()
    }

    fn tcp_sockets() -> HashMap<u32, SockSample> {
        let mut out = HashMap::new();
        dump_diag(libc::AF_INET as u8, &mut out);
        dump_diag(libc::AF_INET6 as u8, &mut out);
        out
    }

    fn dump_diag(family: u8, out: &mut HashMap<u32, SockSample>) {
        unsafe {
            let fd = libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, NETLINK_INET_DIAG);
            if fd < 0 {
                return;
            }
            let req = build_diag_req(family);
            let mut addr: libc::sockaddr_nl = std::mem::zeroed();
            addr.nl_family = libc::AF_NETLINK as u16;
            let sent = libc::sendto(
                fd,
                req.as_ptr() as *const libc::c_void,
                req.len(),
                0,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            );
            if sent < 0 {
                libc::close(fd);
                return;
            }
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = libc::recvfrom(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0, std::ptr::null_mut(), std::ptr::null_mut());
                if n <= 0 {
                    break;
                }
                if parse_netlink(&buf[..n as usize], family, out) {
                    break;
                }
            }
            libc::close(fd);
        }
    }

    fn build_diag_req(family: u8) -> Vec<u8> {
        let mut b = vec![0u8; 72];
        b[0..4].copy_from_slice(&72u32.to_ne_bytes());
        b[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
        b[6..8].copy_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
        b[16] = family;
        b[17] = libc::IPPROTO_TCP as u8;
        b[18] = 1 << (INET_DIAG_INFO - 1);
        b[20..24].copy_from_slice(&0xffff_ffffu32.to_ne_bytes());
        b
    }

    fn parse_netlink(d: &[u8], family: u8, out: &mut HashMap<u32, SockSample>) -> bool {
        let mut off = 0usize;
        while off + 16 <= d.len() {
            let len = u32::from_ne_bytes(d[off..off + 4].try_into().unwrap()) as usize;
            let mtype = u16::from_ne_bytes(d[off + 4..off + 6].try_into().unwrap());
            if len < 16 || off + len > d.len() {
                break;
            }
            if mtype == NLMSG_DONE || mtype == NLMSG_ERROR {
                return true;
            }
            parse_diag_msg(family, &d[off + 16..off + len], out);
            off += (len + 3) & !3;
        }
        false
    }

    fn parse_diag_msg(family: u8, d: &[u8], out: &mut HashMap<u32, SockSample>) {
        if d.len() < 72 {
            return;
        }
        let inode = u32::from_ne_bytes(d[68..72].try_into().unwrap());
        if inode == 0 {
            return;
        }
        let mut s = SockSample { state: tcp_state_name(d[1]), ..SockSample::default() };
        let dport = u16::from_be_bytes(d[6..8].try_into().unwrap());
        if dport != 0 {
            s.remote = match family {
                f if f == libc::AF_INET as u8 => {
                    let ip = std::net::Ipv4Addr::new(d[24], d[25], d[26], d[27]);
                    format!("{}:{}", ip, dport)
                }
                f if f == libc::AF_INET6 as u8 => {
                    let mut o = [0u8; 16];
                    o.copy_from_slice(&d[24..40]);
                    let ip = std::net::Ipv6Addr::from(o);
                    format!("[{}]:{}", ip, dport)
                }
                _ => String::new(),
            };
        }
        let mut off = 72usize;
        while off + 4 <= d.len() {
            let alen = u16::from_ne_bytes(d[off..off + 2].try_into().unwrap()) as usize;
            let atype = u16::from_ne_bytes(d[off + 2..off + 4].try_into().unwrap());
            if alen < 4 || off + alen > d.len() {
                break;
            }
            if atype == INET_DIAG_INFO {
                let p = &d[off + 4..off + alen];
                if p.len() >= 136 {
                    s.tx = u64::from_ne_bytes(p[120..128].try_into().unwrap());
                    s.rx = u64::from_ne_bytes(p[128..136].try_into().unwrap());
                }
            }
            off += (alen + 3) & !3;
        }
        out.insert(inode, s);
    }

    /// inode_to_pid maps every socket inode to an owning PID by reading each
    /// process's open file descriptors.
    ///
    /// ponytail: full /proc/*/fd scan every measurement tick; cache by mtime if
    /// it ever shows up in a profile.
    fn inode_to_pid() -> HashMap<u32, i32> {
        let mut m = HashMap::new();
        let entries = match std::fs::read_dir("/proc") {
            Ok(e) => e,
            Err(_) => return m,
        };
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let pid: i32 = match fname.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let dir = format!("/proc/{}/fd", pid);
            let fds = match std::fs::read_dir(&dir) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for fd in fds.flatten() {
                if let Ok(link) = std::fs::read_link(fd.path()) {
                    let link = link.to_string_lossy();
                    if let Some(rest) = link.strip_prefix("socket:[") {
                        if let Some(num) = rest.strip_suffix(']') {
                            if let Ok(inode) = num.parse::<u32>() {
                                m.insert(inode, pid);
                            }
                        }
                    }
                }
            }
        }
        m
    }
}

// ---- macOS (nettop) backend ---------------------------------------------

#[cfg(not(target_os = "linux"))]
pub use macos::ProcNet;

#[cfg(not(target_os = "linux"))]
mod macos {
    use super::*;

    #[derive(Clone, Default)]
    struct ConnSample {
        rx: u64,
        tx: u64,
        pid: i32,
        name: String,
        remote: String,
        state: String,
    }

    /// ProcNet attributes per-connection throughput on macOS by shelling out to
    /// nettop, which lists every process's connections with cumulative byte
    /// counters without root — so NeedRoot stays false.
    pub struct ProcNet {
        pub conns: Vec<ProcConn>,
        pub need_root: bool,
        prev: HashMap<String, ConnSample>,
        prev_time: Instant,
        primed: bool,
    }

    impl ProcNet {
        pub fn new() -> ProcNet {
            ProcNet { conns: Vec::new(), need_root: false, prev: HashMap::new(), prev_time: Instant::now(), primed: false }
        }

        pub fn reset(&mut self) {
            self.primed = false;
        }

        pub fn update(&mut self) {
            let cur = sample_nettop_conns();
            let now = Instant::now();
            let dt = now.duration_since(self.prev_time).as_secs_f64();

            if self.primed && dt > 0.0 {
                let mut conns = Vec::with_capacity(cur.len());
                for (key, c) in &cur {
                    let prev = match self.prev.get(key) {
                        Some(p) => p,
                        None => continue,
                    };
                    let drx = counter_delta(c.rx, prev.rx);
                    let dtx = counter_delta(c.tx, prev.tx);
                    if drx == 0 && dtx == 0 {
                        continue;
                    }
                    conns.push(ProcConn {
                        pid: c.pid,
                        name: c.name.clone(),
                        remote: c.remote.clone(),
                        state: c.state.clone(),
                        rx_per_sec: drx as f64 / dt,
                        tx_per_sec: dtx as f64 / dt,
                    });
                }
                self.conns = conns;
            }

            self.prev = cur;
            self.prev_time = now;
            self.primed = true;
        }
    }

    /// sample_nettop_conns runs nettop once and returns a cumulative sample per
    /// connection. On any failure it returns an empty map.
    fn sample_nettop_conns() -> HashMap<String, ConnSample> {
        // -x prints raw byte counts, -L 1 takes a single sample and exits, -J
        // selects the columns we parse.
        let out = std::process::Command::new("nettop")
            .args(["-x", "-L", "1", "-J", "state,bytes_in,bytes_out"])
            .output();
        match out {
            Ok(o) if o.status.success() => parse_nettop_conns(&String::from_utf8_lossy(&o.stdout)),
            _ => HashMap::new(),
        }
    }

    /// parse_nettop_conns reads nettop's CSV output: a process row
    /// "<name>.<pid>,,<in>,<out>," is followed by that process's connection rows
    /// "<proto> <local><->remote>,<state>,<in>,<out>,", which inherit the most
    /// recent process row's PID and name.
    fn parse_nettop_conns(data: &str) -> HashMap<String, ConnSample> {
        let mut out = HashMap::new();
        let mut cur_pid = 0i32;
        let mut cur_name = String::new();
        let mut have_pid = false;

        for line in data.lines() {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split(',').collect();
            if fields.len() < 4 {
                continue;
            }
            let desc = fields[0];

            // A connection row's descriptor is "<proto> <local><->remote>".
            if let Some(sep) = desc.find("<->") {
                if desc.contains(' ') {
                    if !have_pid {
                        continue;
                    }
                    let remote = &desc[sep + 3..];
                    if remote.is_empty() || remote.contains('*') {
                        continue; // listener or unbound socket
                    }
                    let (rx, tx) = match (fields[2].parse::<u64>(), fields[3].parse::<u64>()) {
                        (Ok(rx), Ok(tx)) => (rx, tx),
                        _ => continue,
                    };
                    let key = format!("{} {}", cur_pid, desc);
                    out.insert(
                        key,
                        ConnSample {
                            rx,
                            tx,
                            pid: cur_pid,
                            name: cur_name.clone(),
                            remote: remote.to_string(),
                            state: fields[1].to_string(),
                        },
                    );
                    continue;
                }
            }

            // Process row: "<name>.<pid>"; the name may contain dots.
            match desc.rfind('.') {
                Some(dot) => match desc[dot + 1..].parse::<i32>() {
                    Ok(pid) => {
                        cur_pid = pid;
                        cur_name = desc[..dot].to_string();
                        have_pid = true;
                    }
                    Err(_) => have_pid = false,
                },
                None => have_pid = false,
            }
        }
        out
    }
}
