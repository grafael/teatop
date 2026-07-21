//! Samples aggregate and per-interface network throughput from /proc/net/dev,
//! skipping loopback and deriving rates from the change in byte counters over
//! the elapsed wall-clock time.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::fs;
use std::time::Instant;

/// Iface is one interface's current throughput.
#[allow(dead_code)] // per-interface rates are sampled for completeness
pub struct Iface {
    pub name: String,
    pub rx_per_sec: f64,
    pub tx_per_sec: f64,
}

pub struct Network {
    pub rx_per_sec: f64, // download rate, bytes/sec, all interfaces
    pub tx_per_sec: f64, // upload rate, bytes/sec
    pub peak_rx: f64,
    pub peak_tx: f64,
    pub ifaces: Vec<Iface>,

    prev_rx: u64,
    prev_tx: u64,
    prev_per: HashMap<String, (u64, u64)>,
    prev_time: Instant,
    primed: bool,
}

impl Network {
    pub fn new() -> Result<Network, String> {
        let mut n = Network {
            rx_per_sec: 0.0,
            tx_per_sec: 0.0,
            peak_rx: 0.0,
            peak_tx: 0.0,
            ifaces: Vec::new(),
            prev_rx: 0,
            prev_tx: 0,
            prev_per: HashMap::new(),
            prev_time: Instant::now(),
            primed: false,
        };
        let counters = read_net_counters()?;
        n.ingest(counters, Instant::now());
        Ok(n)
    }

    pub fn update(&mut self) {
        if let Ok(counters) = read_net_counters() {
            self.ingest(counters, Instant::now());
        }
    }

    fn ingest(&mut self, counters: Vec<(String, u64, u64)>, now: Instant) {
        let mut rx = 0u64;
        let mut tx = 0u64;
        let mut per = HashMap::with_capacity(counters.len());
        let elapsed = if self.primed { now.duration_since(self.prev_time).as_secs_f64() } else { 0.0 };

        let mut ifaces = Vec::with_capacity(counters.len());
        for (name, brx, btx) in &counters {
            rx += brx;
            tx += btx;
            per.insert(name.clone(), (*brx, *btx));
            if *brx == 0 && *btx == 0 {
                continue;
            }
            let mut ifc = Iface { name: name.clone(), rx_per_sec: 0.0, tx_per_sec: 0.0 };
            if let Some(&(prx, ptx)) = self.prev_per.get(name) {
                if elapsed > 0.0 {
                    if *brx >= prx {
                        ifc.rx_per_sec = (brx - prx) as f64 / elapsed;
                    }
                    if *btx >= ptx {
                        ifc.tx_per_sec = (btx - ptx) as f64 / elapsed;
                    }
                }
            }
            ifaces.push(ifc);
        }
        ifaces.sort_by(|a, b| a.name.cmp(&b.name));
        self.ifaces = ifaces;

        if elapsed > 0.0 {
            if rx >= self.prev_rx {
                self.rx_per_sec = (rx - self.prev_rx) as f64 / elapsed;
            }
            if tx >= self.prev_tx {
                self.tx_per_sec = (tx - self.prev_tx) as f64 / elapsed;
            }
        }
        self.peak_rx = self.peak_rx.max(self.rx_per_sec);
        self.peak_tx = self.peak_tx.max(self.tx_per_sec);
        self.prev_rx = rx;
        self.prev_tx = tx;
        self.prev_per = per;
        self.prev_time = now;
        self.primed = true;
    }
}

/// read_net_counters lists per-interface (name, bytes_recv, bytes_sent) from
/// /proc/net/dev, skipping loopback.
#[cfg(target_os = "linux")]
fn read_net_counters() -> Result<Vec<(String, u64, u64)>, String> {
    let data = fs::read_to_string("/proc/net/dev").map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for line in data.lines() {
        let (name, rest) = match line.split_once(':') {
            Some(x) => x,
            None => continue,
        };
        let name = name.trim();
        if name == "lo" || name.starts_with("lo0") {
            continue;
        }
        let nums: Vec<u64> = rest.split_whitespace().map(|s| s.parse().unwrap_or(0)).collect();
        if nums.len() < 9 {
            continue;
        }
        // recv bytes @0, transmit bytes @8
        out.push((name.to_string(), nums[0], nums[8]));
    }
    Ok(out)
}

/// read_net_counters lists per-interface cumulative byte counters via sysinfo
/// on macOS, skipping loopback.
#[cfg(not(target_os = "linux"))]
fn read_net_counters() -> Result<Vec<(String, u64, u64)>, String> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut out = Vec::new();
    for (name, data) in &networks {
        if name == "lo" || name.starts_with("lo0") {
            continue;
        }
        out.push((name.clone(), data.total_received(), data.total_transmitted()));
    }
    Ok(out)
}
