//! Evaluates the config file's hook rules against live metric samples and runs
//! their shell commands when a threshold is crossed.

use crate::config::Hook;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A rule is one hook plus its runtime edge-trigger state.
struct Rule {
    metric: String,
    threshold: f64,
    above: bool,
    command: String,
    recover: String,
    cooldown: Duration,
    hold_for: Duration,

    breached: bool,
    pending_since: Option<Instant>,
    last_run: Option<Instant>,
}

impl Rule {
    fn op(&self) -> &'static str {
        if self.above { ">" } else { "<" }
    }
    fn describe(&self, v: f64) -> String {
        format!("hook {} {} {:.0} fired ({:.0})", self.metric, self.op(), self.threshold, v)
    }
}

/// Engine holds the hook rules and their edge-trigger state. Command failures
/// surface asynchronously through the shared errs buffer.
pub struct Engine {
    rules: Vec<Rule>,
    errs: Arc<Mutex<Vec<String>>>,
}

impl Engine {
    /// New builds an engine from validated config hooks.
    pub fn new(hooks: &[Hook]) -> Engine {
        let rules = hooks
            .iter()
            .map(|h| {
                let (threshold, above) = h.condition();
                Rule {
                    metric: h.metric.clone(),
                    threshold,
                    above,
                    command: h.run.clone(),
                    recover: h.on_recover.clone(),
                    cooldown: h.cooldown_duration(),
                    hold_for: h.for_duration(),
                    breached: false,
                    pending_since: None,
                    last_run: None,
                }
            })
            .collect();
        Engine { rules, errs: Arc::new(Mutex::new(Vec::new())) }
    }

    /// Check evaluates every rule against the sampled values. A rule fires when
    /// its condition turns true (and has held for hold_for), then again every
    /// cooldown while it stays true, and re-arms once it turns false, running
    /// its recovery command when it had fired. Returns a short description per
    /// fired hook for the status line.
    pub fn check(&mut self, values: &HashMap<&str, f64>, now: Instant) -> Vec<String> {
        let mut fired = Vec::new();
        for r in self.rules.iter_mut() {
            let v = match values.get(r.metric.as_str()) {
                Some(v) => *v,
                None => continue,
            };
            let breach = if r.above { v > r.threshold } else { v < r.threshold };
            if !breach {
                if r.breached && !r.recover.is_empty() {
                    run_command(&self.errs, &r.metric, &r.recover, v, r.threshold);
                    fired.push(format!(
                        "hook {} {} {:.0} recovered ({:.0})",
                        r.metric,
                        r.op(),
                        r.threshold,
                        v
                    ));
                }
                r.breached = false;
                r.pending_since = None;
            } else if r.breached {
                // already fired: only cooldown re-fires
                if r.cooldown > Duration::ZERO
                    && r.last_run.map_or(false, |t| now.duration_since(t) >= r.cooldown)
                {
                    r.last_run = Some(now);
                    run_command(&self.errs, &r.metric, &r.command, v, r.threshold);
                    fired.push(r.describe(v));
                }
            } else {
                // newly (or still pending) breached
                if r.hold_for > Duration::ZERO {
                    let since = *r.pending_since.get_or_insert(now);
                    if now.duration_since(since) < r.hold_for {
                        continue; // condition must hold longer before firing
                    }
                }
                r.breached = true;
                r.last_run = Some(now);
                run_command(&self.errs, &r.metric, &r.command, v, r.threshold);
                fired.push(r.describe(v));
            }
        }
        fired
    }

    /// Errors drains the failure messages collected from hook commands since
    /// the last call, for display in the status bar.
    pub fn errors(&self) -> Vec<String> {
        let mut e = self.errs.lock().unwrap();
        std::mem::take(&mut *e)
    }
}

/// run_command starts the hook via the shell, detached from the UI: stdout goes
/// to /dev/null and the exit status is reaped on a background thread. Failures
/// are queued for the status bar with the first line of stderr. The trigger
/// context is exported as TEATOP_* environment variables.
fn run_command(errs: &Arc<Mutex<Vec<String>>>, metric: &str, command: &str, value: f64, threshold: f64) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .env("TEATOP_METRIC", metric)
        .env("TEATOP_VALUE", format!("{:.1}", value))
        .env("TEATOP_THRESHOLD", format!("{:.1}", threshold))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            report_error(errs, format!("hook {}: {}", metric, e));
            return;
        }
    };

    let errs = Arc::clone(errs);
    let metric = metric.to_string();
    std::thread::spawn(move || {
        let out = child.wait_with_output();
        if let Ok(out) = out {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut msg = stderr.trim().lines().next().unwrap_or("").to_string();
                if msg.len() > 256 {
                    msg.truncate(256);
                }
                if msg.is_empty() {
                    msg = format!("exit status {:?}", out.status.code());
                }
                report_error(&errs, format!("hook {} failed: {}", metric, msg));
            }
        }
    });
}

fn report_error(errs: &Arc<Mutex<Vec<String>>>, msg: String) {
    let mut e = errs.lock().unwrap();
    if e.len() < 4 {
        // a stuck hook must not grow this unboundedly
        e.push(msg);
    }
}
