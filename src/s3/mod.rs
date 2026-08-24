// yo-s3: burn a specified amount of AWS cost, in a controlled way, by writing
// large objects to S3 at a bounded random rate. Cross-Region Replication (CRR)
// traffic is the main cost engine: it is the only cost item that accrues
// immediately and linearly with bytes written, so the tool can stop precisely
// when the budget is reached. See specs/yo_s3.md.

pub mod accel;
pub mod body;
pub mod budget;
pub mod checkpoint;
pub mod client;
pub mod commands;
pub mod config;
pub mod cost;
pub mod crr;
pub mod limiter;
pub mod lock;
pub mod metrics;
pub mod modes;
pub mod netpath;
pub mod pool;
pub mod registry;
pub mod sweep;
pub mod uploader;

pub const KIB: u64 = 1 << 10;
pub const MIB: u64 = 1 << 20;
pub const GIB: u64 = 1 << 30;
pub const TIB: u64 = 1 << 40;

// S3 multipart hard limits (validated at startup, clear errors instead of 400s)
pub const MIN_PART_SIZE: u64 = 5 * MIB;
pub const MAX_PART_SIZE: u64 = 5 * GIB;
pub const MAX_PARTS_PER_OBJECT: u64 = 10_000;
pub const MAX_OBJECT_SIZE: u64 = 5 * TIB;

/// Byte length of the unique header written at the start of every object.
/// Guarantees objects are mutually distinct so any dedup on the target side
/// stays defeated (dedup would make billed bytes diverge from written bytes).
pub const OBJECT_HEADER_LEN: u64 = 64;

/// Format a byte count in binary units for terminal display.
pub fn fmt_bytes(n: u64) -> String {
    let f = n as f64;
    if n >= TIB {
        format!("{:.2} TiB", f / TIB as f64)
    } else if n >= GIB {
        format!("{:.2} GiB", f / GIB as f64)
    } else if n >= MIB {
        format!("{:.2} MiB", f / MIB as f64)
    } else if n >= KIB {
        format!("{:.2} KiB", f / KIB as f64)
    } else {
        format!("{} B", n)
    }
}

/// Format a byte rate for terminal display.
pub fn fmt_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", fmt_bytes(bytes_per_sec))
}

/// Format micro-dollars as `$12.34`.
pub fn fmt_usd(micro: u64) -> String {
    format!("${:.2}", micro as f64 / 1_000_000.0)
}
