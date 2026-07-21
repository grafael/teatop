//! Loads the optional teatop YAML config (which dashboard sections to render
//! and the hook rules) plus the machine-written state file (view preferences
//! remembered between runs). Every config key is optional; a missing key keeps
//! its default, so a missing file changes nothing.

use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

fn d_true() -> bool {
    true
}

/// Config selects which dashboard sections teatop renders and which hooks run.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "d_true")]
    pub cpu: bool,
    #[serde(default = "d_true")]
    pub memory: bool,
    #[serde(default = "d_true")]
    pub swap: bool,
    #[serde(default = "d_true")]
    pub disk: bool,
    #[serde(default = "d_true")]
    pub gpu: bool,
    #[serde(default = "d_true")]
    pub history: bool,
    #[serde(default)]
    pub cores: bool,
    #[serde(default = "d_true")]
    pub processes: bool,
    #[serde(default = "d_true")]
    pub system: bool,
    #[serde(default)]
    pub hooks: Vec<Hook>,
}

impl Default for Config {
    /// The configuration used when no file (or key) is present: every section
    /// on, per-core bars off at startup.
    fn default() -> Self {
        Config {
            cpu: true,
            memory: true,
            swap: true,
            disk: true,
            gpu: true,
            history: true,
            cores: false,
            processes: true,
            system: true,
            hooks: Vec::new(),
        }
    }
}

/// Hook runs a shell command when a metric crosses a threshold.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub metric: String,
    pub above: Option<f64>,
    pub below: Option<f64>,
    pub run: String,
    #[serde(default)]
    pub cooldown: String,
    #[serde(default, rename = "for")]
    pub hold_for: String,
    #[serde(default)]
    pub on_recover: String,
}

impl Hook {
    /// validate checks the hook and canonicalises its metric name.
    fn validate(&mut self) -> Result<(), String> {
        let canon = canonical_metric(self.metric.trim());
        match canon {
            Some(m) => self.metric = m.to_string(),
            None => {
                return Err(format!(
                    "unknown metric {:?} (use cpu, cpu_temp, mem, swap, disk, gpu or gpu_mem)",
                    self.metric
                ));
            }
        }
        if self.above.is_some() == self.below.is_some() {
            return Err("set exactly one of above or below".into());
        }
        if self.run.trim().is_empty() {
            return Err("run must be a shell command".into());
        }
        for (name, val) in [("cooldown", &self.cooldown), ("for", &self.hold_for)] {
            if !val.is_empty() && parse_duration(val).is_none() {
                return Err(format!("invalid {}: {:?}", name, val));
            }
        }
        Ok(())
    }

    /// Returns the threshold and whether the hook triggers above it.
    pub fn condition(&self) -> (f64, bool) {
        match self.above {
            Some(v) => (v, true),
            None => (self.below.unwrap(), false),
        }
    }

    pub fn cooldown_duration(&self) -> Duration {
        parse_duration(&self.cooldown).unwrap_or(Duration::ZERO)
    }

    pub fn for_duration(&self) -> Duration {
        parse_duration(&self.hold_for).unwrap_or(Duration::ZERO)
    }
}

/// canonical_metric maps accepted metric spellings to their canonical name.
fn canonical_metric(s: &str) -> Option<&'static str> {
    match s.to_lowercase().as_str() {
        "cpu" => Some("cpu"),
        "cpu_temp" | "cpu-temp" => Some("cpu_temp"),
        "mem" | "memory" => Some("mem"),
        "swap" => Some("swap"),
        "disk" => Some("disk"),
        "gpu" => Some("gpu"),
        "gpu_mem" | "gpu-mem" | "gpumem" => Some("gpu_mem"),
        _ => None,
    }
}

/// parse_duration parses a Go-style duration string ("90s", "5m", "1h30m",
/// "500ms"). Returns None on any malformed input.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = Duration::ZERO;
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut saw_unit = false;
    while i < bytes.len() {
        // parse a (possibly fractional) number
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        if i == start {
            return None;
        }
        let num: f64 = s[start..i].parse().ok()?;
        // parse the unit
        let ustart = i;
        while i < bytes.len() && !bytes[i].is_ascii_digit() && bytes[i] != b'.' {
            i += 1;
        }
        let unit = &s[ustart..i];
        let secs = match unit {
            "ns" => num / 1e9,
            "us" | "µs" => num / 1e6,
            "ms" => num / 1e3,
            "s" => num,
            "m" => num * 60.0,
            "h" => num * 3600.0,
            _ => return None,
        };
        total += Duration::from_secs_f64(secs);
        saw_unit = true;
    }
    if saw_unit { Some(total) } else { None }
}

fn config_dir() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x));
        }
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config"))
}

/// Path returns the default config file location, usually
/// ~/.config/teatop/config.yaml.
pub fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("teatop").join("config.yaml"))
}

/// StatePath returns the state file location, alongside the config file.
pub fn state_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("teatop").join("state.yaml"))
}

const DEFAULT_FILE: &str = r#"# teatop configuration
#
# Every key is optional; a missing key keeps its default.

# Dashboard sections
cpu: true        # CPU gauge (utilization, frequency, temperature)
memory: true     # memory gauge
swap: true       # swap gauge (only shown when swap exists)
disk: true       # root filesystem gauge
gpu: true        # GPU gauges, chart series and table columns
history: true    # utilization history chart
cores: false     # per-core bars at startup (toggle with c)
processes: true  # process table
system: true     # status bar network/disk/load/uptime readout

# Hooks: run a shell command when a metric crosses a threshold.
# Metrics: cpu, mem, swap, disk, gpu, gpu_mem (percentages, 0-100)
# and cpu_temp (degrees Celsius). The command runs in the background
# via sh -c with TEATOP_METRIC, TEATOP_VALUE and TEATOP_THRESHOLD in
# its environment; failures show in the status bar. YAML-quote the
# whole command (single quotes) when it contains ": ".
#
# hooks:
#   - metric: mem
#     above: 85
#     run: notify-send "teatop" "memory above 85% (${TEATOP_VALUE}%)"
#     for: 30s        # condition must hold this long before firing
#     cooldown: 10m   # re-fire while it stays breached
#     on_recover: notify-send "teatop" "memory back below 85%"
"#;

/// LoadDefault reads the config from the default path. On first run it writes a
/// documented default file (best-effort) and returns the defaults.
pub fn load_default() -> Result<Config, String> {
    let p = match path() {
        Some(p) => p,
        None => return Ok(Config::default()), // no resolvable config dir
    };
    match load(&p) {
        Ok(c) => Ok(c),
        Err(e) if e.kind == ErrorKindTag::NotFound => {
            let _ = write_default(&p);
            Ok(Config::default())
        }
        Err(e) => Err(e.msg),
    }
}

fn write_default(p: &PathBuf) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, DEFAULT_FILE)
}

/// A load error carrying enough to distinguish a missing file from a parse
/// failure (so an explicit --config never falls back silently).
pub struct LoadError {
    pub msg: String,
    pub kind: ErrorKindTag,
}

#[derive(PartialEq)]
pub enum ErrorKindTag {
    NotFound,
    Other,
}

/// Load reads and parses the YAML file at path. Missing keys keep their
/// defaults; unknown keys are errors. A missing file is a NotFound error.
pub fn load(p: &PathBuf) -> Result<Config, LoadError> {
    let data = std::fs::read_to_string(p).map_err(|e| LoadError {
        msg: format!("{}: {}", p.display(), e),
        kind: if e.kind() == ErrorKind::NotFound {
            ErrorKindTag::NotFound
        } else {
            ErrorKindTag::Other
        },
    })?;
    if data.trim().is_empty() {
        return Ok(Config::default()); // empty file: run with defaults
    }
    let mut cfg: Config = serde_yaml::from_str(&data).map_err(|e| LoadError {
        msg: format!("{}: {}", p.display(), e),
        kind: ErrorKindTag::Other,
    })?;
    for (i, h) in cfg.hooks.iter_mut().enumerate() {
        h.validate().map_err(|e| LoadError {
            msg: format!("{}: hooks[{}]: {}", p.display(), i, e),
            kind: ErrorKindTag::Other,
        })?;
    }
    Ok(cfg)
}

/// State is the view preferences teatop remembers between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub sort: String, // "cpu", "mem" or "gpu"
    #[serde(default)]
    pub sort_asc: bool,
    #[serde(default)]
    pub tree: bool,
    #[serde(default)]
    pub mine: bool,
    #[serde(default)]
    pub cores: bool,
    #[serde(default)]
    pub history: bool,
    #[serde(default, rename = "interval_ms")]
    pub interval: i64,
}

/// LoadState reads the saved preferences. None when nothing has been written
/// yet (or it is unreadable), so callers keep their config/flag defaults.
pub fn load_state() -> Option<State> {
    let p = state_path()?;
    let data = std::fs::read_to_string(&p).ok()?;
    serde_yaml::from_str(&data).ok()
}

/// SaveState writes the preferences to the state file (best-effort).
pub fn save_state(s: &State) -> Result<(), String> {
    let p = state_path().ok_or("no config dir")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let data = serde_yaml::to_string(s).map_err(|e| e.to_string())?;
    std::fs::write(&p, data).map_err(|e| e.to_string())
}
