//! teatop: a combined CPU, memory and GPU monitor for the terminal. The binary
//! is a thin wrapper over [`run`]; the modules are exposed so the integration
//! tests can exercise the pure helpers.

pub mod config;
pub mod hooks;
pub mod metrics;
pub mod style;
pub mod text;
pub mod ui;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEvent};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute, queue, style::Print, terminal};

use metrics::{cpu::Cpu, disk::Disk, gpu::Gpu, memory::Memory, network::Network, process::Processes, system::System};
use ui::App;

pub const VERSION: &str = "3.0.0";

struct Args {
    delay: i64,
    all_cpus: bool,
    config: String,
    version: bool,
    explicit_delay: bool,
    explicit_cores: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { delay: 1000, all_cpus: false, config: String::new(), version: false, explicit_delay: false, explicit_cores: false };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].clone();
        let take_val = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            argv.get(*i).cloned().ok_or_else(|| format!("missing value for {}", arg))
        };
        match argv[i].as_str() {
            "-d" | "-delay" | "--delay" => {
                a.delay = take_val(&mut i)?.parse().map_err(|_| "delay must be an integer".to_string())?;
                a.explicit_delay = true;
            }
            "-c" | "-all-cpus" | "--all-cpus" => {
                a.all_cpus = true;
                a.explicit_cores = true;
            }
            "-config" | "--config" => a.config = take_val(&mut i)?,
            "-version" | "--version" => a.version = true,
            "-h" | "-help" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {}", other)),
        }
        i += 1;
    }
    Ok(a)
}

fn print_help() {
    println!(
        "teatop [options]\n\n  \
         -d, -delay int    update interval in ms (100 to 5000, default 1000)\n  \
         -c, -all-cpus     show the per-core bar chart on startup\n  \
         -config path      config file path (default ~/.config/teatop/config.yaml)\n  \
         -version          print version and exit"
    );
}

/// run is the program entry point invoked by the binary.
pub fn run() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("teatop: {}", e);
            std::process::exit(2);
        }
    };
    if args.version {
        println!("teatop {} - CPU/Memory/GPU monitor (crossterm TUI)", VERSION);
        return;
    }

    // Without a config file (or key) every section stays on.
    let cfg = if !args.config.is_empty() {
        match config::load(&std::path::PathBuf::from(&args.config)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("teatop: config: {}", e.msg);
                std::process::exit(1);
            }
        }
    } else {
        match config::load_default() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("teatop: config: {}", e);
                std::process::exit(1);
            }
        }
    };

    // Remembered view preferences seed cores and the refresh rate, but an
    // explicitly passed flag still wins.
    let state = config::load_state();
    let mut interval = args.delay;
    let mut cores = args.all_cpus;
    if let Some(ref s) = state {
        cores = cores || s.cores;
        if !args.explicit_delay {
            interval = s.interval;
        }
    }
    interval = ui::clamp_interval(interval);
    let _ = args.explicit_cores;

    if let Err(e) = run_app(cfg, interval, cores, state) {
        eprintln!("teatop: {}", e);
        std::process::exit(1);
    }
}

fn run_app(cfg: config::Config, interval: i64, all_cpus: bool, state: Option<config::State>) -> Result<(), String> {
    let mut cpu = Cpu::new().map_err(|e| format!("cpu init: {}", e))?;
    let mut mem = Memory::new().map_err(|e| format!("memory init: {}", e))?;
    let mut net = Network::new().map_err(|e| format!("network init: {}", e))?;
    let mut sys = System::new().map_err(|e| format!("system init: {}", e))?;
    sys.fetch_external_ip(); // header shows the address once resolved
    let mut disk = Disk::new().map_err(|e| format!("disk init: {}", e))?;
    let mut gpu = Gpu::new(); // never fatal; degrades gracefully
    let mut procs = Processes::new();

    // Prime CPU, network, disk and process deltas before the first frame.
    std::thread::sleep(Duration::from_millis(120));
    cpu.update();
    let _ = mem.update();
    net.update();
    let _ = sys.update();
    disk.update();
    gpu.update();
    let stats = gpu.process_stats();
    procs.update(mem.ram.total, &stats);

    let mut app = App::new(cpu, mem, gpu, net, sys, disk, procs, cfg, all_cpus, interval);
    if let Some(ref s) = state {
        app.apply_state(s);
    }

    let result = run_tui(&mut app);
    let _ = config::save_state(&app.state()); // best-effort
    result
}

fn run_tui(app: &mut App) -> Result<(), String> {
    let mut stdout = io::stdout();
    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, cursor::Hide).map_err(|e| e.to_string())?;

    let res = event_loop(app, &mut stdout);

    let _ = execute!(stdout, cursor::Show, DisableMouseCapture, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    res
}

fn event_loop(app: &mut App, stdout: &mut io::Stdout) -> Result<(), String> {
    let (w, h) = terminal::size().map_err(|e| e.to_string())?;
    app.set_size(w as usize, h as usize);

    let mut next_tick = Instant::now() + Duration::from_millis(app.interval_ms() as u64);
    loop {
        let frame = app.view();
        draw(stdout, &frame).map_err(|e| e.to_string())?;
        if app.should_quit {
            break;
        }

        let timeout = next_tick.saturating_duration_since(Instant::now());
        if event::poll(timeout).map_err(|e| e.to_string())? {
            match event::read().map_err(|e| e.to_string())? {
                Event::Key(k) if k.kind != KeyEventKind::Release => app.handle_key(k),
                Event::Mouse(m) => handle_mouse(app, m),
                Event::Resize(w, h) => app.set_size(w as usize, h as usize),
                _ => {}
            }
        } else {
            app.tick();
            next_tick = Instant::now() + Duration::from_millis(app.interval_ms() as u64);
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_mouse(app: &mut App, m: MouseEvent) {
    app.handle_mouse(m);
}

/// draw paints the frame, clearing each line to its end so a shorter frame
/// never leaves stale content behind.
fn draw(stdout: &mut io::Stdout, frame: &str) -> io::Result<()> {
    queue!(stdout, cursor::MoveTo(0, 0))?;
    for (i, line) in frame.split('\n').enumerate() {
        queue!(stdout, cursor::MoveTo(0, i as u16), Print(line), terminal::Clear(terminal::ClearType::UntilNewLine))?;
    }
    queue!(stdout, terminal::Clear(terminal::ClearType::FromCursorDown))?;
    stdout.flush()
}
