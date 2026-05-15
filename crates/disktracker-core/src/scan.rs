use crate::arena::PathlessArena;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub struct ScanConfig {
    pub root: PathBuf,
    /// Max directory depth. None = unlimited.
    pub max_depth: Option<u16>,
    /// Skip directories matching these exact names (e.g. ["proc", "sys"]).
    pub skip_names: Vec<Vec<u8>>,
    /// Skip mount points on Linux/macOS.
    pub one_filesystem: bool,
    /// Optional cancellation flag checked during traversal.
    pub cancel_flag: Option<Arc<AtomicBool>>,
}

impl ScanConfig {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel_flag
            .as_ref()
            .map(|flag| flag.load(Ordering::Relaxed))
            .unwrap_or(false)
    }
}

pub struct ScanResult {
    pub arena: PathlessArena,
    pub total_files: u64,
    pub total_bytes: u64,
    pub scan_duration_ms: u64,
    /// Permission denied etc — logged, not fatal.
    pub error_count: u32,
}

#[cfg(unix)]
fn do_scan(arena: &mut PathlessArena, config: &ScanConfig, root_idx: u32) -> (u64, u64, u32) {
    crate::platform::unix::scan_root(arena, config, root_idx)
}

#[cfg(windows)]
fn do_scan(arena: &mut PathlessArena, config: &ScanConfig, root_idx: u32) -> (u64, u64, u32) {
    crate::platform::windows::scan_root(arena, config, root_idx)
}

pub fn scan(config: ScanConfig) -> ScanResult {
    let start = Instant::now();
    // Preallocate: 64K nodes, 4 MB byte pool — grows as needed.
    let mut arena = PathlessArena::with_capacity(65536, 4 * 1024 * 1024);

    let root_bytes: Vec<u8> = {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            config.root.as_os_str().as_bytes().to_vec()
        }
        #[cfg(windows)]
        {
            config.root.to_string_lossy().into_owned().into_bytes()
        }
    };

    let root_sym = arena.intern(&root_bytes);
    let root_idx = arena.push(crate::arena::DirNode {
        parent: None,
        name: root_sym,
        total_bytes: 0,
        file_count: 0,
        mtime: 0,
        depth: 0,
    });

    let (total_bytes, total_files, error_count) = do_scan(&mut arena, &config, root_idx);

    // Patch root node with aggregated totals
    arena.nodes[root_idx as usize].total_bytes = total_bytes;

    ScanResult {
        total_bytes,
        total_files,
        arena,
        scan_duration_ms: start.elapsed().as_millis() as u64,
        error_count,
    }
}
