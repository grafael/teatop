//! Samples RAM and swap usage. On Linux from /proc/meminfo (matching gopsutil's
//! accounting); on macOS via sysinfo (buffers/cache are not broken out).

/// Ram holds physical memory figures in bytes plus derived percentages.
#[derive(Default, Clone, Copy)]
#[allow(dead_code)] // free/shared mirror the source data model
pub struct Ram {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
    pub shared: u64,
    pub used_percent: f64,
    pub available_percent: f64,
    pub cache_percent: f64,
}

/// Swap holds swap figures in bytes plus the used percentage.
#[derive(Default, Clone, Copy)]
#[allow(dead_code)] // free mirrors the source data model
pub struct Swap {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub used_percent: f64,
}

/// Memory samples RAM and swap usage.
pub struct Memory {
    pub ram: Ram,
    pub swap: Swap,
    #[cfg(not(target_os = "linux"))]
    sys: sysinfo::System,
}

impl Memory {
    pub fn has_swap(&self) -> bool {
        self.swap.total > 0
    }
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 / whole as f64 * 100.0 }
}

// ---- Linux backend ------------------------------------------------------

#[cfg(target_os = "linux")]
impl Memory {
    pub fn new() -> Result<Memory, String> {
        let mut m = Memory { ram: Ram::default(), swap: Swap::default() };
        m.update()?;
        Ok(m)
    }

    pub fn update(&mut self) -> Result<(), String> {
        use std::collections::HashMap;
        let data = std::fs::read_to_string("/proc/meminfo").map_err(|e| e.to_string())?;
        let mut kv: HashMap<&str, u64> = HashMap::new();
        for line in data.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let n: u64 = v.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
                kv.insert(k, n * 1024); // kB -> bytes
            }
        }
        let g = |k: &str| kv.get(k).copied().unwrap_or(0);

        let total = g("MemTotal");
        let free = g("MemFree");
        let buffers = g("Buffers");
        let cached = g("Cached") + g("SReclaimable");
        let shared = g("Shmem");
        let available = if kv.contains_key("MemAvailable") { g("MemAvailable") } else { cached + free };
        let used = total.saturating_sub(free).saturating_sub(buffers).saturating_sub(cached);

        let mut r = Ram { total, used, free, available, buffers, cached, shared, ..Ram::default() };
        if total > 0 {
            r.used_percent = pct(r.used, total);
            r.available_percent = pct(r.available, total);
            r.cache_percent = pct(r.cached + r.buffers, total);
        }
        self.ram = r;

        let stotal = g("SwapTotal");
        let sfree = g("SwapFree");
        let sused = stotal.saturating_sub(sfree);
        self.swap = Swap { total: stotal, used: sused, free: sfree, used_percent: if stotal > 0 { pct(sused, stotal) } else { 0.0 } };
        Ok(())
    }
}

// ---- macOS (sysinfo) backend --------------------------------------------

#[cfg(not(target_os = "linux"))]
impl Memory {
    pub fn new() -> Result<Memory, String> {
        let mut m = Memory { ram: Ram::default(), swap: Swap::default(), sys: sysinfo::System::new() };
        m.update()?;
        Ok(m)
    }

    pub fn update(&mut self) -> Result<(), String> {
        self.sys.refresh_memory();
        let total = self.sys.total_memory();
        let used = self.sys.used_memory();
        let available = self.sys.available_memory();
        let free = self.sys.free_memory();
        let mut r = Ram { total, used, free, available, ..Ram::default() };
        if total > 0 {
            r.used_percent = pct(used, total);
            r.available_percent = pct(available, total);
        }
        self.ram = r;

        let stotal = self.sys.total_swap();
        let sused = self.sys.used_swap();
        let sfree = self.sys.free_swap();
        self.swap = Swap { total: stotal, used: sused, free: sfree, used_percent: if stotal > 0 { pct(sused, stotal) } else { 0.0 } };
        Ok(())
    }
}
