# teatop

A CPU, memory and GPU monitor for the terminal on Linux and macOS, written in
Rust with [crossterm](https://github.com/crossterm-rs/crossterm).

- htop-style meters for CPU, memory, swap, disk, GPU and VRAM, each with
  its own identity color across gauges and chart.
- Braille history chart, one solid-color lane per metric.
- Per-core bars (toggle with `c`), process table with sort, substring
  search, tree view, own-processes filter and kill — mouse works too.
- All-filesystems disk view (`d`): usage bars for every mounted disk plus a
  full-screen per-process I/O page.
- Network view (`n`): a full-screen page of live connections — owning
  process, remote endpoint and per-connection download/upload — with sort by
  download/upload/total (`d`/`u`/`t`), substring search, a movable selection
  and kill, alongside the throughput chart and local/external IPs kept off
  the main screen (screenshot-safe).
- Header with load averages (colored against your core count) and uptime.
- NVIDIA GPU telemetry via [nvml-wrapper](https://github.com/Cldfire/nvml-wrapper),
  including per-process GPU usage; degrades gracefully without a GPU.
- Hooks: run a shell command when a metric crosses a threshold.
- Remembers your sort, filters, toggles and refresh rate between runs — in
  `~/.config/teatop/state.yaml`.

## Install

Requires a recent Rust toolchain on Linux or macOS.

```sh
cargo build --release      # produces ./target/release/teatop
```

**Linux** uses `/proc`, `sysfs`, `statvfs` and NVIDIA NVML directly. GPU
monitoring needs the driver (`libnvidia-ml.so`); without it the GPU section
hides itself. Per-connection bandwidth comes from the kernel's `sock_diag`
(netlink), and per-process disk I/O from `/proc/<pid>/io` — seeing every
process's connections/IO needs `sudo`.

**macOS** uses the cross-platform [`sysinfo`](https://crates.io/crates/sysinfo)
backend, plus `nettop` for per-connection bandwidth (no root needed) and
`proc_pid_rusage` (libproc) for per-process disk I/O (other users' processes
need `sudo`). The GPU section hides itself, and CPU frequency/temperature and
the aggregate disk-throughput readout are not available.

## Usage

```
teatop [options]

  -d, -delay int    update interval in ms (100 to 5000, default 1000)
  -c, -all-cpus     show the per-core bar chart on startup
  -config path      config file path (default ~/.config/teatop/config.yaml)
  -version          print version and exit
```

Keys: `q` quit · `c` cores · `h` history · `d` disks view ·
`n` network view · `t` tree · `u` mine · `p`/`m`/`g` sort (again to reverse) ·
`/` search · `x` kill · `space` pause · `-`/`+` refresh rate ·
arrows/`j`/`k`/PgUp/PgDn/Home/End navigate · `Esc` closes the popup or view,
or clears the search filter. Click a row to select, a column header to sort;
the wheel scrolls. On the network view `d`/`u`/`t` sort by
download/upload/total, and `/`, `x` and `space` search, kill and pause its
connections.

## Configuration

teatop reads `~/.config/teatop/config.yaml` (or `-config <path>`) to show or
hide dashboard sections and to declare hooks — shell commands that run when a
metric crosses a threshold:

```yaml
gpu: false           # hide a section (all sections default to on)
hooks:
  - metric: mem      # cpu, mem, swap, disk, gpu, gpu_mem, cpu_temp
    above: 85        # or below:
    run: notify-send "teatop" "memory above 85%"
```

[`config.example.yaml`](config.example.yaml) documents every option,
including hook hold time, cooldown, recovery commands and the environment
variables passed to the command.

## Development

```sh
cargo build          # compile
cargo test           # run the suite
cargo clippy         # lints
```

## Implementation notes

A hand-rolled crossterm event loop drives the UI, with a small ANSI helper for
styling. The metric layer splits per platform:

- **Linux** — direct `/proc` and `sysfs` reads (via the `procfs` crate) plus
  `statvfs`, `nvml-wrapper` for GPU telemetry, and sock_diag netlink over a
  `libc` raw socket.
- **macOS** — the `sysinfo` crate for CPU/memory/load/uptime/network/disk/
  processes, a `nettop` parser for per-connection bandwidth, and
  `proc_pid_rusage`/`proc_pidinfo` (libproc) for per-process disk I/O.

The Linux build is verified end to end; the macOS build is type-checked
against `aarch64-apple-darwin`.
