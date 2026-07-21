//! Samples root filesystem usage (statvfs), all mounted physical filesystems,
//! and aggregate block-device throughput (/proc/diskstats).

use nix::sys::statvfs::statvfs;
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::fs;
use std::time::Instant;

/// Disk samples root filesystem usage plus aggregate block-device throughput.
pub struct Disk {
    pub total: u64,
    pub used: u64,
    pub used_percent: f64,

    pub read_per_sec: f64,
    pub write_per_sec: f64,

    prev_read: u64,
    prev_write: u64,
    prev_time: Instant,
    primed: bool,
}

impl Disk {
    pub fn new() -> Result<Disk, String> {
        let mut d = Disk {
            total: 0,
            used: 0,
            used_percent: 0.0,
            read_per_sec: 0.0,
            write_per_sec: 0.0,
            prev_read: 0,
            prev_write: 0,
            prev_time: Instant::now(),
            primed: false,
        };
        d.update_usage()?;
        let (r, w) = read_disk_counters();
        d.prev_read = r;
        d.prev_write = w;
        d.prev_time = Instant::now();
        d.primed = true;
        Ok(d)
    }

    pub fn update(&mut self) {
        let _ = self.update_usage();
        let (read, write) = read_disk_counters();
        let now = Instant::now();
        if self.primed {
            let elapsed = now.duration_since(self.prev_time).as_secs_f64();
            if elapsed > 0.0 {
                if read >= self.prev_read {
                    self.read_per_sec = (read - self.prev_read) as f64 / elapsed;
                }
                if write >= self.prev_write {
                    self.write_per_sec = (write - self.prev_write) as f64 / elapsed;
                }
            }
        }
        self.prev_read = read;
        self.prev_write = write;
        self.prev_time = now;
        self.primed = true;
    }

    fn update_usage(&mut self) -> Result<(), String> {
        let u = fs_usage("/")?;
        self.total = u.total;
        self.used = u.used;
        self.used_percent = u.used_percent;
        Ok(())
    }
}

struct Usage {
    total: u64,
    used: u64,
    used_percent: f64,
}

/// fs_usage returns df-style usage for a mountpoint: the used percentage is
/// taken against the space available to unprivileged users.
fn fs_usage(path: &str) -> Result<Usage, String> {
    let s = statvfs(path).map_err(|e| e.to_string())?;
    let bsize = {
        let f = s.fragment_size();
        if f > 0 { f } else { s.block_size() }
    } as u64;
    let blocks = s.blocks() as u64;
    let bfree = s.blocks_free() as u64;
    let bavail = s.blocks_available() as u64;
    let total = blocks * bsize;
    let used = (blocks - bfree) * bsize;
    let avail = bavail * bsize;
    let used_percent = if used + avail > 0 { used as f64 / (used + avail) as f64 * 100.0 } else { 0.0 };
    Ok(Usage { total, used, used_percent })
}

/// Mount is one mounted filesystem's usage snapshot.
#[allow(dead_code)] // device is used for de-duplication, not display
pub struct Mount {
    pub path: String,
    pub device: String,
    pub total: u64,
    pub used: u64,
    pub used_percent: f64,
}

/// mounts lists usage for every mounted physical filesystem, sorted by
/// mountpoint. Pseudo filesystems (from /proc/filesystems' nodev list),
/// squashfs images (snaps) and further mounts of an already-listed device are
/// skipped, as are filesystems that fail to stat.
#[cfg(target_os = "linux")]
pub fn mounts() -> Vec<Mount> {
    let nodev = nodev_fstypes();
    let data = match fs::read_to_string("/proc/mounts") {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for line in data.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let device = unescape_mount(f[0]);
        let mountpoint = unescape_mount(f[1]);
        let fstype = f[2];
        if fstype == "squashfs" || nodev.contains(fstype) || seen.contains(&device) {
            continue;
        }
        let u = match fs_usage(&mountpoint) {
            Ok(u) if u.total > 0 => u,
            _ => continue,
        };
        seen.insert(device.clone());
        out.push(Mount {
            path: mountpoint,
            device,
            total: u.total,
            used: u.used,
            used_percent: u.used_percent,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// nodev_fstypes returns the pseudo (non-block-backed) filesystem types listed
/// in /proc/filesystems, which gopsutil's Partitions(false) excludes.
#[cfg(target_os = "linux")]
fn nodev_fstypes() -> HashSet<String> {
    let mut set = HashSet::new();
    if let Ok(data) = fs::read_to_string("/proc/filesystems") {
        for line in data.lines() {
            if let Some(rest) = line.strip_prefix("nodev") {
                let ty = rest.trim();
                if !ty.is_empty() {
                    set.insert(ty.to_string());
                }
            }
        }
    }
    set
}

/// unescape_mount decodes the octal escapes (\040 space, \011 tab, ...) the
/// kernel uses for special characters in /proc/mounts fields.
#[cfg(target_os = "linux")]
fn unescape_mount(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(code) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(code as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// read_disk_counters sums the byte counters of every whole block device from
/// /proc/diskstats. Errors leave the counters at zero.
#[cfg(target_os = "linux")]
fn read_disk_counters() -> (u64, u64) {
    let data = match fs::read_to_string("/proc/diskstats") {
        Ok(d) => d,
        Err(_) => return (0, 0),
    };
    let mut read = 0u64;
    let mut write = 0u64;
    for line in data.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 {
            continue;
        }
        if !is_whole_device(f[2]) {
            continue;
        }
        // sectors read @5, sectors written @9; a sector is 512 bytes.
        let sr: u64 = f[5].parse().unwrap_or(0);
        let sw: u64 = f[9].parse().unwrap_or(0);
        read += sr * 512;
        write += sw * 512;
    }
    (read, write)
}

/// is_whole_device matches unpartitioned device names (sdX/vdX/xvdX/nvme…nN/
/// mmcblkN/diskN) so partitions are not double-counted on top of their parent.
#[cfg(target_os = "linux")]
fn is_whole_device(name: &str) -> bool {
    let alpha_suffix = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase());
    let digit_suffix = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if let Some(s) = name.strip_prefix("nvme") {
        // nvme\d+n\d+
        if let Some((a, b)) = s.split_once('n') {
            return digit_suffix(a) && digit_suffix(b);
        }
        return false;
    }
    for p in ["xvd", "sd", "vd"] {
        if let Some(s) = name.strip_prefix(p) {
            return alpha_suffix(s);
        }
    }
    for p in ["mmcblk", "disk"] {
        if let Some(s) = name.strip_prefix(p) {
            return digit_suffix(s);
        }
    }
    false
}

// ---- macOS backend ------------------------------------------------------

/// mounts lists real filesystems on macOS via sysinfo, running each through
/// statvfs for df-style usage and hiding OS-internal helper volumes.
#[cfg(not(target_os = "linux"))]
pub fn mounts() -> Vec<Mount> {
    use std::collections::HashSet;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for d in disks.list() {
        let mountpoint = d.mount_point().to_string_lossy().to_string();
        let device = d.name().to_string_lossy().to_string();
        let fstype = d.file_system().to_string_lossy().to_string();
        if fstype == "squashfs" || seen.contains(&device) || system_mount(&mountpoint, &fstype) {
            continue;
        }
        let u = match fs_usage(&mountpoint) {
            Ok(u) if u.total > 0 => u,
            _ => continue,
        };
        seen.insert(device.clone());
        out.push(Mount { path: mountpoint, device, total: u.total, used: u.used, used_percent: u.used_percent });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// system_mount reports whether a macOS mountpoint is an OS-internal helper
/// (APFS firmlinked helper volumes under /System/Volumes, devfs, automounter)
/// rather than a volume a user thinks of as storage.
#[cfg(not(target_os = "linux"))]
fn system_mount(mountpoint: &str, fstype: &str) -> bool {
    matches!(fstype, "devfs" | "autofs") || mountpoint.starts_with("/System/Volumes/")
}

/// Aggregate per-device disk throughput is not sampled on macOS (it needs
/// IOKit); the R/W readout stays at zero, as noted in the README.
#[cfg(not(target_os = "linux"))]
fn read_disk_counters() -> (u64, u64) {
    (0, 0)
}
