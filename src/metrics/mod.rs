//! Metric collectors: CPU, memory, GPU, network, disk, per-process I/O and the
//! process table. Each collector primes a baseline on construction so the first
//! frame isn't blank, then recomputes deltas on update.

pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod memory;
pub mod network;
pub mod procdisk;
pub mod procnet;
pub mod process;
pub mod system;

/// counter_delta returns cur-prev, treating a decrease (counter reset or inode
/// reuse) as zero rather than a huge spike.
pub fn counter_delta(cur: u64, prev: u64) -> u64 {
    cur.saturating_sub(prev)
}
