//! NVIDIA GPU telemetry via NVML. Degrades gracefully: if NVML or a GPU is
//! missing, `available` is false and `err` explains why, but the program keeps
//! running.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use nvml_wrapper::Nvml;
#[cfg(target_os = "linux")]
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
#[cfg(target_os = "linux")]
use nvml_wrapper::enums::device::UsedGpuMemory;

/// GpuDevice is a snapshot of one NVIDIA GPU's telemetry.
#[derive(Default, Clone)]
pub struct GpuDevice {
    pub index: usize,
    pub name: String,
    pub gpu_util: u32, // %
    pub mem_total: u64,
    pub mem_used: u64,
    pub mem_used_percent: f64,
    pub temperature: u32, // Celsius
    pub power_draw: u32,  // milliwatts
    pub power_limit: u32, // milliwatts
}

/// GpuSummary aggregates telemetry across every device.
#[derive(Default, Clone)]
pub struct GpuSummary {
    pub count: usize,
    pub gpu_util: f64, // mean across devices, %
    pub mem_used: u64,
    pub mem_total: u64,
    pub mem_used_percent: f64,
    pub max_temp: u32,
    pub power_draw: u32,  // milliwatts, summed
    pub power_limit: u32, // milliwatts, summed
}

/// GpuProcStat aggregates one PID's GPU usage across all devices.
#[derive(Default, Clone, Copy)]
pub struct GpuProcStat {
    pub sm_util: u32,  // %
    pub mem_used: u64, // bytes
}

#[cfg(target_os = "linux")]
pub struct Gpu {
    pub available: bool,
    pub err: String,
    pub devices: Vec<GpuDevice>,
    nvml: Option<Nvml>,
}

#[cfg(target_os = "linux")]
impl Gpu {
    /// new initialises NVML and enumerates devices. It never fails; check
    /// `available` to know whether GPU data is present.
    pub fn new() -> Gpu {
        let mut g = Gpu { available: false, err: String::new(), devices: Vec::new(), nvml: None };

        let nvml = match Nvml::init() {
            Ok(n) => n,
            Err(e) => {
                g.err = format!("NVML unavailable: {}", e);
                return g;
            }
        };
        let count = match nvml.device_count() {
            Ok(c) => c,
            Err(e) => {
                g.err = format!("cannot query GPU count: {}", e);
                g.nvml = Some(nvml);
                return g;
            }
        };
        if count == 0 {
            g.err = "no NVIDIA GPU detected".into();
            g.nvml = Some(nvml);
            return g;
        }
        for i in 0..count {
            if let Ok(dev) = nvml.device_by_index(i) {
                let name = dev.name().unwrap_or_default();
                g.devices.push(GpuDevice { index: i as usize, name, ..GpuDevice::default() });
            }
        }
        if g.devices.is_empty() {
            g.err = "no usable NVIDIA GPU".into();
            g.nvml = Some(nvml);
            return g;
        }
        g.available = true;
        g.nvml = Some(nvml);
        g
    }

    /// update refreshes telemetry for every device. A no-op when unavailable.
    pub fn update(&mut self) {
        if !self.available {
            return;
        }
        let nvml = self.nvml.as_ref().unwrap();
        for d in self.devices.iter_mut() {
            let dev = match nvml.device_by_index(d.index as u32) {
                Ok(dev) => dev,
                Err(_) => continue,
            };
            if let Ok(u) = dev.utilization_rates() {
                d.gpu_util = u.gpu;
            }
            if let Ok(m) = dev.memory_info() {
                d.mem_total = m.total;
                d.mem_used = m.used;
                if m.total > 0 {
                    d.mem_used_percent = m.used as f64 / m.total as f64 * 100.0;
                }
            }
            if let Ok(t) = dev.temperature(TemperatureSensor::Gpu) {
                d.temperature = t;
            }
            if let Ok(p) = dev.power_usage() {
                d.power_draw = p;
            }
            if let Ok(l) = dev.enforced_power_limit() {
                d.power_limit = l;
            }
        }
    }

    /// summary reduces all devices to one GpuSummary; None when no GPU data.
    pub fn summary(&self) -> Option<GpuSummary> {
        if !self.available || self.devices.is_empty() {
            return None;
        }
        let mut s = GpuSummary { count: self.devices.len(), ..GpuSummary::default() };
        for d in &self.devices {
            s.gpu_util += d.gpu_util as f64;
            s.mem_used += d.mem_used;
            s.mem_total += d.mem_total;
            s.power_draw += d.power_draw;
            s.power_limit += d.power_limit;
            s.max_temp = s.max_temp.max(d.temperature);
        }
        s.gpu_util /= s.count as f64;
        if s.mem_total > 0 {
            s.mem_used_percent = s.mem_used as f64 / s.mem_total as f64 * 100.0;
        }
        Some(s)
    }

    /// process_stats maps PIDs to their GPU utilisation and memory. Utilisation
    /// needs driver support and root-visible processes; entries degrade to
    /// memory-only (or are absent) when NVML withholds the data.
    pub fn process_stats(&self) -> HashMap<u32, GpuProcStat> {
        let mut out: HashMap<u32, GpuProcStat> = HashMap::new();
        if !self.available {
            return out;
        }
        let nvml = self.nvml.as_ref().unwrap();

        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros().saturating_sub(2_000_000) as u64)
            .unwrap_or(0);

        for d in &self.devices {
            let dev = match nvml.device_by_index(d.index as u32) {
                Ok(dev) => dev,
                Err(_) => continue,
            };
            let mut add_mem = |procs: Vec<nvml_wrapper::struct_wrappers::device::ProcessInfo>| {
                for pi in procs {
                    if let UsedGpuMemory::Used(bytes) = pi.used_gpu_memory {
                        out.entry(pi.pid).or_default().mem_used += bytes;
                    }
                }
            };
            if let Ok(procs) = dev.running_compute_processes() {
                add_mem(procs);
            }
            if let Ok(procs) = dev.running_graphics_processes() {
                add_mem(procs);
            }
            if let Ok(samples) = dev.process_utilization_stats(since) {
                // Keep only the newest sample per PID on this device, then sum.
                let mut latest: HashMap<u32, u32> = HashMap::new();
                let mut ts: HashMap<u32, u64> = HashMap::new();
                for s in samples {
                    let newer = ts.get(&s.pid).map_or(true, |&t| s.timestamp > t);
                    if newer {
                        ts.insert(s.pid, s.timestamp);
                        latest.insert(s.pid, s.sm_util);
                    }
                }
                for (pid, util) in latest {
                    out.entry(pid).or_default().sm_util += util;
                }
            }
        }
        out
    }
}

// Non-Linux stub: NVML is Linux/Windows only, so the GPU section reports itself
// unavailable and the dashboard hides it (matching the Go build on macOS).
#[cfg(not(target_os = "linux"))]
pub struct Gpu {
    pub available: bool,
    pub err: String,
    pub devices: Vec<GpuDevice>,
}

#[cfg(not(target_os = "linux"))]
impl Gpu {
    pub fn new() -> Gpu {
        Gpu {
            available: false,
            err: "GPU telemetry requires Linux with the NVIDIA driver".into(),
            devices: Vec::new(),
        }
    }
    pub fn update(&mut self) {}
    pub fn summary(&self) -> Option<GpuSummary> {
        None
    }
    pub fn process_stats(&self) -> HashMap<u32, GpuProcStat> {
        HashMap::new()
    }
}
