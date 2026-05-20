use crate::arena::PathlessArena;
use crate::identity::FsIdentity;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    Dir,
    File,
    Symlink,
    Other,
}

pub struct ClassifiedEntry {
    pub name_bytes: Vec<u8>,
    pub path: std::path::PathBuf,
    pub entry_type: EntryType,
    pub file_size: u64,
}

pub struct DirMeta {
    pub mtime: i64,
    pub identity: FsIdentity,
}

pub fn read_dir_meta(path: &Path) -> DirMeta {
    let mut dir_mtime = 0;
    if let Ok(meta) = std::fs::metadata(path) {
        dir_mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
    }
    DirMeta {
        mtime: dir_mtime,
        identity: FsIdentity::UNKNOWN,
    }
}

pub fn read_dir_entries(
    path: &Path,
    skip_names: &[Vec<u8>],
    max_depth: Option<u16>,
    current_depth: u16,
) -> Result<(Vec<ClassifiedEntry>, u64, u32, u32), (u32,)> {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            let errs = if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("Permission denied: {:?}", path);
                1
            } else {
                eprintln!("Cannot read dir {:?}: {}", path, e);
                1
            };
            return Err((errs,));
        }
    };

    let mut classified: Vec<ClassifiedEntry> = Vec::new();
    let mut file_bytes: u64 = 0;
    let mut file_count: u32 = 0;
    let mut error_count: u32 = 0;

    for entry_res in entries {
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

        if skip_names.iter().any(|s| s == &name_bytes) {
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
            if let Some(max) = max_depth {
                if current_depth + 1 > max {
                    continue;
                }
            }
            classified.push(ClassifiedEntry {
                name_bytes,
                path: entry.path(),
                entry_type: EntryType::Dir,
                file_size: 0,
            });
        } else if meta.is_file() {
            file_bytes += meta.len();
            file_count += 1;
        }
    }

    Ok((classified, file_bytes, file_count, error_count))
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

    let meta = read_dir_meta(path);
    arena.cold[parent_idx as usize].mtime = meta.mtime;
    arena.cold[parent_idx as usize].identity = meta.identity;

    if let Some(ref skip_pred) = config.skip_predicate {
        let path_bytes = arena.materialize_path(parent_idx);
        if let Some(skip_res) = skip_pred(&path_bytes, meta.mtime, meta.identity) {
            arena.hot[parent_idx as usize].total_bytes = skip_res.total_bytes;
            arena.hot[parent_idx as usize].file_count = skip_res.file_count;
            return (skip_res.total_bytes, skip_res.file_count as u64, 0);
        }
    }

    let (entries, file_bytes, file_count_here, mut error_count) =
        match read_dir_entries(path, &config.skip_names, config.max_depth, depth) {
            Ok(res) => res,
            Err((errs,)) => return (0, 0, errs),
        };

    let mut total_bytes: u64 = file_bytes;
    let mut total_files: u64 = file_count_here as u64;

    for entry in entries {
        if config.is_cancelled() {
            break;
        }
        if entry.entry_type != EntryType::Dir {
            continue;
        }

        if config.one_filesystem && root_device != 0 {
            let child_dev = get_volume_serial(&entry.path).unwrap_or(0);
            if child_dev != root_device {
                continue;
            }
        }

        let sym = arena.intern(&entry.name_bytes);
        let child_idx = arena.push_node(parent_idx, sym, depth + 1);

        let (child_bytes, child_files, child_errors) = scan_dir_recursive_windows(
            arena,
            &entry.path,
            child_idx,
            depth + 1,
            config,
            root_device,
        );

        arena.hot[child_idx as usize].total_bytes = child_bytes;
        arena.hot[child_idx as usize].file_count = child_files as u32;

        total_bytes += child_bytes;
        total_files += child_files;
        error_count += child_errors;
    }

    arena.hot[parent_idx as usize].file_count = file_count_here;

    (total_bytes, total_files, error_count)
}

#[cfg(windows)]
pub fn get_volume_serial(path: &Path) -> Option<u64> {
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
pub fn get_volume_serial(_path: &Path) -> Option<u64> {
    None
}
