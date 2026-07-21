//! Host-level odds and ends: load averages, uptime and the machine's
//! addresses.

#[cfg(target_os = "linux")]
use std::fs;
use std::net::{IpAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct System {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub uptime_secs: u64,
    pub local_ip: String, // primary outbound IPv4; "" if undeterminable

    ext_ip: Arc<Mutex<String>>,
}

impl System {
    pub fn new() -> Result<System, String> {
        let mut s = System {
            load1: 0.0,
            load5: 0.0,
            load15: 0.0,
            uptime_secs: 0,
            local_ip: local_ip(),
            ext_ip: Arc::new(Mutex::new(String::new())),
        };
        s.update()?;
        Ok(s)
    }

    /// fetch_external_ip asks a public echo service for the host's
    /// internet-facing address on a background thread; failures are silent.
    pub fn fetch_external_ip(&self) {
        let slot = Arc::clone(&self.ext_ip);
        std::thread::spawn(move || {
            // ureq defaults to Rustls, which is only compiled in on Linux; on
            // macOS the native-tls provider has to be selected explicitly or
            // the request panics.
            let provider = if cfg!(target_os = "linux") {
                ureq::tls::TlsProvider::Rustls
            } else {
                ureq::tls::TlsProvider::NativeTls
            };
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .tls_config(ureq::tls::TlsConfig::builder().provider(provider).build())
                .build()
                .into();
            if let Ok(mut resp) = agent.get("https://api.ipify.org").call() {
                if let Ok(body) = resp.body_mut().read_to_string() {
                    let ip = body.trim();
                    if ip.parse::<IpAddr>().is_ok() {
                        *slot.lock().unwrap() = ip.to_string();
                    }
                }
            }
        });
    }

    /// external_ip returns the address found by fetch_external_ip, or "" while
    /// the lookup is still pending or has failed.
    pub fn external_ip(&self) -> String {
        self.ext_ip.lock().unwrap().clone()
    }

    /// update refreshes the load averages and uptime.
    #[cfg(target_os = "linux")]
    pub fn update(&mut self) -> Result<(), String> {
        let la = fs::read_to_string("/proc/loadavg").map_err(|e| e.to_string())?;
        let mut f = la.split_whitespace();
        self.load1 = f.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        self.load5 = f.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        self.load15 = f.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);

        let up = fs::read_to_string("/proc/uptime").map_err(|e| e.to_string())?;
        let secs: f64 = up.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        self.uptime_secs = secs as u64;
        Ok(())
    }

    /// update refreshes the load averages and uptime via sysinfo on macOS.
    #[cfg(not(target_os = "linux"))]
    pub fn update(&mut self) -> Result<(), String> {
        let la = sysinfo::System::load_average();
        self.load1 = la.one;
        self.load5 = la.five;
        self.load15 = la.fifteen;
        self.uptime_secs = sysinfo::System::uptime();
        Ok(())
    }
}

/// local_ip reports the primary outbound IPv4 address: connecting a UDP socket
/// makes the kernel pick the interface it would route external traffic
/// through, without sending any packet.
fn local_ip() -> String {
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if sock.connect("8.8.8.8:53").is_err() {
        return String::new();
    }
    match sock.local_addr() {
        Ok(a) => a.ip().to_string(),
        Err(_) => String::new(),
    }
}
