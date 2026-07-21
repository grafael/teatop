//! Samples per-core and aggregate CPU utilisation. On Linux from /proc/stat
//! (with the full usr/sys/iow breakdown) plus sysfs frequency and temperature;
//! on macOS via sysinfo (total utilisation only — the breakdown, frequency and
//! temperature are unavailable, as on the Go build).

/// CPUTimes holds cumulative CPU time (in USER_HZ ticks) for one CPU. Only
/// ratios between samples matter, so the unit is irrelevant.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Default)]
pub struct CpuTimes {
    pub user: f64,
    pub nice: f64,
    pub system: f64,
    pub idle: f64,
    pub iowait: f64,
    pub irq: f64,
    pub softirq: f64,
    pub steal: f64,
}

#[cfg(target_os = "linux")]
impl CpuTimes {
    fn total(&self) -> f64 {
        self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq + self.steal
    }
}

/// CpuUsage is a percentage breakdown of how a CPU spent the last interval.
/// The full breakdown is sampled on Linux; on macOS only `total` is set.
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
pub struct CpuUsage {
    pub user: f64,
    pub nice: f64,
    pub system: f64,
    pub idle: f64,
    pub iowait: f64,
    pub irq: f64,
    pub softirq: f64,
    pub steal: f64,
    pub total: f64, // 100 - idle, clamped to [0,100]
}

/// Cpu samples per-core and aggregate utilisation plus, where available,
/// frequency and temperature.
pub struct Cpu {
    pub count: usize,
    pub usage: Vec<CpuUsage>, // index 0 = aggregate, 1..N = individual cores
    pub freq: Vec<f64>,       // MHz, per core; zero when unavailable
    pub temp: f64,            // Celsius; zero when unavailable

    #[cfg(target_os = "linux")]
    prev: Vec<CpuTimes>,
    #[cfg(target_os = "linux")]
    curr: Vec<CpuTimes>,
    #[cfg(not(target_os = "linux"))]
    sys: sysinfo::System,
}

impl Cpu {
    pub fn aggregate(&self) -> CpuUsage {
        self.usage.first().copied().unwrap_or_default()
    }

    pub fn core(&self, i: usize) -> CpuUsage {
        self.usage.get(i + 1).copied().unwrap_or_default()
    }
}

// ---- Linux backend ------------------------------------------------------

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::fs;

    impl Cpu {
        /// new establishes a baseline reading so the first update produces a delta.
        pub fn new() -> Result<Cpu, String> {
            let times = read_cpu_times()?;
            let count = (times.len().saturating_sub(1)).max(1);
            Ok(Cpu {
                count,
                usage: vec![CpuUsage::default(); times.len()],
                freq: vec![0.0; count],
                temp: 0.0,
                prev: times.clone(),
                curr: times,
            })
        }

        /// update reads fresh counters and recomputes usage, frequency and temperature.
        pub fn update(&mut self) {
            let times = match read_cpu_times() {
                Ok(t) => t,
                Err(_) => return,
            };
            self.prev = std::mem::replace(&mut self.curr, times);

            let n = self.prev.len().min(self.curr.len());
            if self.usage.len() < n {
                self.usage = vec![CpuUsage::default(); n];
            }
            for i in 0..n {
                self.usage[i] = compute_usage(self.prev[i], self.curr[i]);
            }
            for i in 0..self.count {
                self.freq[i] = read_cpu_freq(i);
            }
            self.temp = read_cpu_temp();
        }
    }

    fn compute_usage(prev: CpuTimes, curr: CpuTimes) -> CpuUsage {
        let total_delta = curr.total() - prev.total();
        if total_delta <= 0.0 {
            return CpuUsage::default();
        }
        let scale = 100.0 / total_delta;
        let idle_delta = (curr.idle + curr.iowait) - (prev.idle + prev.iowait);
        let mut u = CpuUsage {
            user: (curr.user - prev.user) * scale,
            nice: (curr.nice - prev.nice) * scale,
            system: (curr.system - prev.system) * scale,
            idle: idle_delta * scale,
            iowait: (curr.iowait - prev.iowait) * scale,
            irq: (curr.irq - prev.irq) * scale,
            softirq: (curr.softirq - prev.softirq) * scale,
            steal: (curr.steal - prev.steal) * scale,
            total: 0.0,
        };
        u.total = (100.0 - u.idle).clamp(0.0, 100.0);
        u
    }

    /// read_cpu_times samples cumulative CPU times from /proc/stat: index 0 is
    /// the aggregate, the rest are cores. Guest time is already counted inside
    /// user/nice, so the overlap is removed.
    fn read_cpu_times() -> Result<Vec<CpuTimes>, String> {
        let data = fs::read_to_string("/proc/stat").map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for line in data.lines() {
            if !line.starts_with("cpu") {
                continue;
            }
            let mut f = line.split_whitespace();
            let label = f.next().unwrap_or("");
            if label != "cpu" && !label[3..].chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let nums: Vec<f64> = f.map(|s| s.parse::<f64>().unwrap_or(0.0)).collect();
            let get = |i: usize| nums.get(i).copied().unwrap_or(0.0);
            let mut t = CpuTimes {
                user: get(0),
                nice: get(1),
                system: get(2),
                idle: get(3),
                iowait: get(4),
                irq: get(5),
                softirq: get(6),
                steal: get(7),
            };
            let guest = get(8);
            let guest_nice = get(9);
            if t.user >= guest {
                t.user -= guest;
            }
            if t.nice >= guest_nice {
                t.nice -= guest_nice;
            }
            out.push(t);
        }
        if out.is_empty() {
            return Err("no cpu lines in /proc/stat".into());
        }
        Ok(out)
    }

    fn read_cpu_freq(core: usize) -> f64 {
        let path = format!("/sys/devices/system/cpu/cpu{}/cpufreq/scaling_cur_freq", core);
        read_uint_file(&path).map_or(0.0, |khz| khz as f64 / 1000.0)
    }

    fn read_cpu_temp() -> f64 {
        for p in [
            "/sys/class/thermal/thermal_zone0/temp",
            "/sys/class/hwmon/hwmon0/temp1_input",
            "/sys/class/hwmon/hwmon1/temp1_input",
        ] {
            if let Some(v) = read_uint_file(p) {
                if v > 0 {
                    return v as f64 / 1000.0;
                }
            }
        }
        0.0
    }

    fn read_uint_file(path: &str) -> Option<u64> {
        fs::read_to_string(path).ok()?.trim().parse().ok()
    }
}

// ---- macOS (sysinfo) backend --------------------------------------------

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;
    use sysinfo::{CpuRefreshKind, RefreshKind, System};

    impl Cpu {
        pub fn new() -> Result<Cpu, String> {
            let sys = System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()));
            let count = sys.cpus().len().max(1);
            let mut c = Cpu { count, usage: vec![CpuUsage::default(); count + 1], freq: vec![0.0; count], temp: 0.0, sys };
            c.update();
            Ok(c)
        }

        pub fn update(&mut self) {
            self.sys.refresh_cpu_usage();
            let cpus = self.sys.cpus();
            let n = cpus.len();
            if self.usage.len() < n + 1 {
                self.usage = vec![CpuUsage::default(); n + 1];
            }
            self.usage[0] = CpuUsage { total: self.sys.global_cpu_usage() as f64, ..CpuUsage::default() };
            for (i, cpu) in cpus.iter().enumerate() {
                self.usage[i + 1] = CpuUsage { total: cpu.cpu_usage() as f64, ..CpuUsage::default() };
            }
            // frequency and temperature are not sampled on macOS (as in the Go build)
        }
    }
}
