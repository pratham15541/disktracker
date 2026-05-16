use crate::arena::{DirNode, PathlessArena};
use crate::scan::ScanConfig;
use std::path::Path;

/// Entry point: begin recursive scan from root.
pub fn scan_root(arena: &mut PathlessArena, config: &ScanConfig, root_idx: u32) -> (u64, u64, u32) {
    if config.is_cancelled() {
        return (0, 0, 0);
    }
    let root_dev = if config.one_filesystem {
        get_volume_serial(&config.root).unwrap_or(0)
    } else {
        0
    };
    scan_dir_recursive_windows(arena, &config.root, root_idx, 0, config, root_dev)
}

/// Recursively scan a directory using std::fs::read_dir.
/// Returns (total_bytes, total_file_count, error_count).
pub fn scan_dir_recursive_windows(
    arena: &mut PathlessArena,
    path: &Path,
    parent_idx: u32,
    depth: u16,
    config: &ScanConfig,
    root_device: u64,
) -> (u64, u64, u32) {
    if config.is_cancelled() {
        return (0, 0, 0);
    }
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("Permission denied: {:?}", path);
            } else {
                eprintln!("Cannot read dir {:?}: {}", path, e);
            }
            return (0, 0, 1);
        }
    };

    let mut total_bytes: u64 = 0;
    let mut total_files: u64 = 0;
    let mut error_count: u32 = 0;
    let mut file_count_here: u32 = 0;
    let mut dir_mtime: i64 = 0;

    if let Ok(meta) = std::fs::metadata(path) {
        dir_mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
    }

    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();

    for entry_res in entries {
        if config.is_cancelled() {
            break;
        }
        let entry = match entry_res {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Readdir error in {:?}: {}", path, e);
                error_count += 1;
                continue;
            }
        };

        let file_name = entry.file_name();
        let name_bytes: Vec<u8> = file_name.to_string_lossy().into_owned().into_bytes();

        if config.skip_names.iter().any(|s| s == &name_bytes) {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("metadata error for {:?}: {}", entry.path(), e);
                error_count += 1;
                continue;
            }
        };

        if meta.is_dir() && !meta.is_symlink() {
            if let Some(max) = config.max_depth {
                if depth + 1 > max {
                    continue;
                }
            }
            if config.one_filesystem && root_device != 0 {
                let child_dev = get_volume_serial(&entry.path()).unwrap_or(0);
                if child_dev != root_device {
                    continue;
                }
            }
            subdirs.push(entry.path());
        } else if meta.is_file() {
            total_bytes += meta.len();
            file_count_here += 1;
            total_files += 1;
        }
    }

    for subdir_path in subdirs {
        if config.is_cancelled() {
            break;
        }
        let name_bytes = subdir_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned().into_bytes())
            .unwrap_or_default();

        let sym = arena.intern(&name_bytes);
        let child_idx = arena.push(DirNode {
            parent: PathlessArena::encode_parent(parent_idx),
            name: sym,
            total_bytes: 0,
            file_count: 0,
            mtime: 0,
            depth: depth + 1,
        });

        let (child_bytes, child_files, child_errors) = scan_dir_recursive_windows(
            arena,
            &subdir_path,
            child_idx,
            depth + 1,
            config,
            root_device,
        );

        arena.nodes[child_idx as usize].total_bytes = child_bytes;
        arena.nodes[child_idx as usize].file_count = child_files as u32;

        total_bytes += child_bytes;
        total_files += child_files;
        error_count += child_errors;

        if config.is_cancelled() {
            break;
        }
    }

    arena.nodes[parent_idx as usize].mtime = dir_mtime;
    arena.nodes[parent_idx as usize].file_count = file_count_here;

    (total_bytes, total_files, error_count)
}

#[cfg(windows)]
fn get_volume_serial(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::fileapi::GetVolumeInformationW;

    let root: Vec<u16> = path
        .components()
        .next()
        .map(|c| {
            let mut s: Vec<u16> = c.as_os_str().encode_wide().collect();
            if !s.ends_with(&[b'\\' as u16]) {
                s.push(b'\\' as u16);
            }
            s.push(0); // null terminate
            s
        })
        .unwrap_or_default();

    if root.is_empty() {
        return None;
    }

    let mut serial: u32 = 0;
    unsafe {
        let ok = GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        if ok != 0 {
            Some(serial as u64)
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
fn get_volume_serial(_path: &Path) -> Option<u64> {
    None
}
