use std::time::Instant;

/// Benchmark result from a measured scan operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BenchResult {
    /// Wall-clock time in milliseconds.
    pub wall_ms: u64,
    /// User CPU time in milliseconds.
    pub user_ms: u64,
    /// System CPU time in milliseconds.
    pub sys_ms: u64,
    /// Peak resident set size in kilobytes.
    pub peak_rss_kb: u64,
    /// Total files discovered.
    pub total_files: u64,
    /// Total directories discovered.
    pub total_dirs: u64,
    /// Total bytes across all files.
    pub total_bytes: u64,
    /// Errors encountered (permission denied, etc.).
    pub error_count: u32,
    /// Bytes read from storage (Linux only, 0 otherwise).
    pub read_bytes: u64,
    /// Bytes written to storage (Linux only, 0 otherwise).
    pub write_bytes: u64,
}

/// Resource snapshot taken before/after a measured operation.
struct ResourceSnapshot {
    user_us: u64,
    sys_us: u64,
    read_bytes: u64,
    write_bytes: u64,
}

impl ResourceSnapshot {
    fn capture() -> Self {
        let (user_us, sys_us) = get_rusage();
        let (read_bytes, write_bytes) = get_proc_io();
        Self {
            user_us,
            sys_us,
            read_bytes,
            write_bytes,
        }
    }

    fn delta(&self, after: &Self) -> (u64, u64, u64, u64) {
        (
            after.user_us.saturating_sub(self.user_us),
            after.sys_us.saturating_sub(self.sys_us),
            after.read_bytes.saturating_sub(self.read_bytes),
            after.write_bytes.saturating_sub(self.write_bytes),
        )
    }
}

/// Result payload from a scan, passed into `finish_bench`.
pub struct ScanMetrics {
    pub total_files: u64,
    pub total_dirs: u64,
    pub total_bytes: u64,
    pub error_count: u32,
}

/// Start a benchmark measurement. Returns a handle that must be finished.
pub struct BenchHandle {
    start: Instant,
    snapshot: ResourceSnapshot,
}

impl BenchHandle {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
            snapshot: ResourceSnapshot::capture(),
        }
    }

    /// Finish the measurement and produce a BenchResult.
    pub fn finish(self, metrics: ScanMetrics) -> BenchResult {
        let wall_ms = self.start.elapsed().as_millis() as u64;
        let after = ResourceSnapshot::capture();
        let (user_us, sys_us, read_bytes, write_bytes) = self.snapshot.delta(&after);
        let peak_rss_kb = get_peak_rss_kb();

        BenchResult {
            wall_ms,
            user_ms: user_us / 1000,
            sys_ms: sys_us / 1000,
            peak_rss_kb,
            total_files: metrics.total_files,
            total_dirs: metrics.total_dirs,
            total_bytes: metrics.total_bytes,
            error_count: metrics.error_count,
            read_bytes,
            write_bytes,
        }
    }
}

/// Wrap a closure with timing + resource measurement.
/// The closure must return ScanMetrics.
pub fn measure<F>(f: F) -> BenchResult
where
    F: FnOnce() -> ScanMetrics,
{
    let handle = BenchHandle::start();
    let metrics = f();
    handle.finish(metrics)
}

// ─── Platform: getrusage ─────────────────────────────────────────────────────

#[cfg(unix)]
fn get_rusage() -> (u64, u64) {
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
            let user_us = usage.ru_utime.tv_sec as u64 * 1_000_000 + usage.ru_utime.tv_usec as u64;
            let sys_us = usage.ru_stime.tv_sec as u64 * 1_000_000 + usage.ru_stime.tv_usec as u64;
            (user_us, sys_us)
        } else {
            (0, 0)
        }
    }
}

#[cfg(not(unix))]
fn get_rusage() -> (u64, u64) {
    (0, 0)
}

// ─── Platform: /proc/self/io ─────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn get_proc_io() -> (u64, u64) {
    let content = match std::fs::read_to_string("/proc/self/io") {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };
    let mut read_bytes = 0u64;
    let mut write_bytes = 0u64;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("read_bytes: ") {
            read_bytes = val.trim().parse().unwrap_or(0);
        } else if let Some(val) = line.strip_prefix("write_bytes: ") {
            write_bytes = val.trim().parse().unwrap_or(0);
        }
    }
    (read_bytes, write_bytes)
}

#[cfg(not(target_os = "linux"))]
fn get_proc_io() -> (u64, u64) {
    (0, 0)
}

// ─── Platform: peak RSS ──────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn get_peak_rss_kb() -> u64 {
    let content = match std::fs::read_to_string("/proc/self/status") {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("VmHWM:") {
            // Format: "VmHWM:    12345 kB"
            let val = val.trim();
            if let Some(kb_str) = val.strip_suffix(" kB") {
                return kb_str.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

#[cfg(all(unix, not(target_os = "linux")))]
fn get_peak_rss_kb() -> u64 {
    // macOS: ru_maxrss is in bytes
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
            #[cfg(target_os = "macos")]
            {
                usage.ru_maxrss as u64 / 1024
            }
            #[cfg(not(target_os = "macos"))]
            {
                usage.ru_maxrss as u64
            }
        } else {
            0
        }
    }
}

#[cfg(not(unix))]
fn get_peak_rss_kb() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_measure_basic() {
        let result = measure(|| ScanMetrics {
            total_files: 42,
            total_dirs: 7,
            total_bytes: 1024,
            error_count: 0,
        });
        assert_eq!(result.total_files, 42);
        assert_eq!(result.total_dirs, 7);
        assert_eq!(result.total_bytes, 1024);
        assert_eq!(result.error_count, 0);
        // wall_ms should be tiny for a no-op
        assert!(result.wall_ms < 100);
    }

    #[test]
    fn test_bench_handle() {
        let handle = BenchHandle::start();
        // Do some trivial work
        let sum: u64 = (0..1000).sum();
        assert!(sum > 0);
        let result = handle.finish(ScanMetrics {
            total_files: 100,
            total_dirs: 10,
            total_bytes: 5000,
            error_count: 1,
        });
        assert_eq!(result.total_files, 100);
        assert_eq!(result.error_count, 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_proc_io_readable() {
        // /proc/self/io may require specific permissions.
        // Just verify it doesn't panic and returns a tuple.
        let (_read, _write) = get_proc_io();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_peak_rss_nonzero() {
        let rss = get_peak_rss_kb();
        assert!(rss > 0);
    }
}
