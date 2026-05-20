use crate::arena::PathlessArena;
use crate::scan::ScanConfig;
use rustix::fs::{AtFlags, OFlags};
use std::ffi::{CStr, CString};

#[cfg(target_os = "linux")]
use crate::identity::FsIdentity;
#[cfg(target_os = "linux")]
use rustix::fs::StatxFlags;

/// Entry point: open the root using its path, then recurse.
pub fn scan_root(arena: &mut PathlessArena, config: &ScanConfig, root_idx: u32) -> (u64, u64, u32) {
    use std::os::unix::ffi::OsStrExt;
    if config.is_cancelled() {
        return (0, 0, 0);
    }
    let root_bytes = config.root.as_os_str().as_bytes();

    let root_dev = if config.one_filesystem {
        get_dev_abs(root_bytes).unwrap_or(0)
    } else {
        0
    };

    let root_fd = match open_dir_abs(root_bytes) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("Cannot open root {:?}: {}", config.root, e);
            return (0, 0, 1);
        }
    };

    scan_dir_recursive(arena, &root_fd, root_idx, 0, config, root_dev)
}

fn bytes_to_cstring(bytes: &[u8]) -> CString {
    let sanitised: Vec<u8> = bytes
        .iter()
        .map(|&b| if b == 0 { b'?' } else { b })
        .collect();
    CString::new(sanitised).unwrap_or_else(|_| CString::new("?").unwrap())
}

pub fn open_dir_abs(path: &[u8]) -> rustix::io::Result<rustix::fd::OwnedFd> {
    let cstr = bytes_to_cstring(path);
    rustix::fs::openat(
        rustix::fs::CWD,
        cstr.as_c_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
}

fn open_dir_at(
    parent_fd: rustix::fd::BorrowedFd<'_>,
    name: &CStr,
) -> rustix::io::Result<rustix::fd::OwnedFd> {
    rustix::fs::openat(
        parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
}

#[cfg(target_os = "linux")]
pub fn get_dev_abs(path: &[u8]) -> Option<u64> {
    let cstr = bytes_to_cstring(path);
    rustix::fs::statx(
        rustix::fs::CWD,
        cstr.as_c_str(),
        AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::TYPE,
    )
    .ok()
    .map(|s| make_dev(s.stx_dev_major, s.stx_dev_minor))
}

#[cfg(not(target_os = "linux"))]
pub fn get_dev_abs(path: &[u8]) -> Option<u64> {
    let cstr = bytes_to_cstring(path);
    rustix::fs::statat(rustix::fs::CWD, cstr.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
        .ok()
        .map(|s| s.st_dev as u64)
}

#[cfg(target_os = "linux")]
pub fn get_dev_at(dirfd: rustix::fd::BorrowedFd<'_>, name: &CStr) -> Option<u64> {
    rustix::fs::statx(dirfd, name, AtFlags::SYMLINK_NOFOLLOW, StatxFlags::TYPE)
        .ok()
        .map(|s| make_dev(s.stx_dev_major, s.stx_dev_minor))
}

#[cfg(not(target_os = "linux"))]
pub fn get_dev_at(dirfd: rustix::fd::BorrowedFd<'_>, name: &CStr) -> Option<u64> {
    rustix::fs::statat(dirfd, name, AtFlags::SYMLINK_NOFOLLOW)
        .ok()
        .map(|s| s.st_dev as u64)
}

/// Get file size and identity from a single statx/fstatat call.
/// On Linux, requests SIZE | INO to get both in one syscall.
#[cfg(target_os = "linux")]
fn get_file_meta(dirfd: rustix::fd::BorrowedFd<'_>, name: &CStr) -> (u64, FsIdentity) {
    match rustix::fs::statx(
        dirfd,
        name,
        AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::SIZE | StatxFlags::INO,
    ) {
        Ok(s) => (s.stx_size, FsIdentity::from_statx(&s)),
        Err(_) => (0, FsIdentity::UNKNOWN),
    }
}

#[cfg(not(target_os = "linux"))]
fn get_file_meta(
    dirfd: rustix::fd::BorrowedFd<'_>,
    name: &CStr,
) -> (u64, crate::identity::FsIdentity) {
    match rustix::fs::statat(dirfd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(s) => (s.st_size as u64, crate::identity::FsIdentity::from_stat(&s)),
        Err(_) => (0, crate::identity::FsIdentity::UNKNOWN),
    }
}

/// Get directory mtime and identity from the directory fd itself.
#[cfg(target_os = "linux")]
fn get_dir_meta(dirfd: rustix::fd::BorrowedFd<'_>) -> (i64, FsIdentity) {
    match rustix::fs::statx(
        dirfd,
        rustix::cstr!(""),
        AtFlags::EMPTY_PATH | AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::MTIME | StatxFlags::INO,
    ) {
        Ok(s) => (s.stx_mtime.tv_sec, FsIdentity::from_statx(&s)),
        Err(_) => (0, FsIdentity::UNKNOWN),
    }
}

#[cfg(not(target_os = "linux"))]
fn get_dir_meta(dirfd: rustix::fd::BorrowedFd<'_>) -> (i64, crate::identity::FsIdentity) {
    match rustix::fs::statat(dirfd, rustix::cstr!("."), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(s) => (s.st_mtime, crate::identity::FsIdentity::from_stat(&s)),
        Err(_) => (0, crate::identity::FsIdentity::UNKNOWN),
    }
}

#[cfg(target_os = "linux")]
fn make_dev(major: u32, minor: u32) -> u64 {
    ((major as u64) << 32) | (minor as u64)
}

// ─── Extracted: read_dir_entries ──────────────────────────────────────────────

/// Type of entry as determined by d_type or fallback stat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    Dir,
    File,
    Symlink,
    Other,
}

/// A classified directory entry with metadata.
pub struct ClassifiedEntry {
    pub name_bytes: Vec<u8>,
    pub cname: CString,
    pub entry_type: EntryType,
    /// File size in bytes. 0 for dirs/symlinks.
    pub file_size: u64,
}

/// Metadata about the directory itself (from stat of the open fd).
pub struct DirMeta {
    pub mtime: i64,
    pub identity: crate::identity::FsIdentity,
}

/// Enumerate + classify all entries in an open directory fd.
///
/// Uses d_type from getdents64 to avoid stat where possible.
/// Only stats files for size and unknowns for type classification.
///
/// This function does NOT interact with the arena or recurse. It is a pure
/// I/O function that both single-threaded and parallel scan paths can call.
pub fn read_dir_entries(
    dir_fd: rustix::fd::BorrowedFd<'_>,
    skip_names: &[Vec<u8>],
    max_depth: Option<u16>,
    current_depth: u16,
) -> Result<(Vec<ClassifiedEntry>, u64, u32, u32), (u32,)> {
    // Returns: (entries, file_bytes, file_count, error_count)
    // Err((error_count,)) on readdir open failure

    let mut dir = match rustix::fs::Dir::read_from(dir_fd) {
        Ok(d) => d,
        Err(_) => {
            return Err((1,));
        }
    };

    let mut entries: Vec<ClassifiedEntry> = Vec::new();
    let mut file_bytes: u64 = 0;
    let mut file_count: u32 = 0;
    let mut error_count: u32 = 0;

    loop {
        let entry = match dir.read() {
            Some(Ok(e)) => e,
            None => break,
            Some(Err(_)) => {
                error_count += 1;
                continue;
            }
        };

        let name = entry.file_name();
        let name_bytes = name.to_bytes();

        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }

        if skip_names.iter().any(|s| s == name_bytes) {
            continue;
        }

        match entry.file_type() {
            rustix::fs::FileType::Directory => {
                if let Some(max) = max_depth {
                    if current_depth + 1 > max {
                        continue;
                    }
                }
                let cname = match CString::new(name_bytes.to_vec()) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                entries.push(ClassifiedEntry {
                    name_bytes: name_bytes.to_vec(),
                    cname,
                    entry_type: EntryType::Dir,
                    file_size: 0,
                });
            }
            rustix::fs::FileType::RegularFile => {
                let (size, _identity) = get_file_meta(dir_fd, name);
                file_bytes += size;
                file_count += 1;
            }
            rustix::fs::FileType::Symlink => {
                // Never follow symlinks
            }
            _ => {
                // DT_UNKNOWN: stat to determine actual type
                let (size, _identity) = get_file_meta(dir_fd, name);
                if size > 0 {
                    file_bytes += size;
                    file_count += 1;
                }
            }
        }
    }

    Ok((entries, file_bytes, file_count, error_count))
}

/// Get metadata for the directory fd itself.
pub fn read_dir_meta(dir_fd: rustix::fd::BorrowedFd<'_>) -> DirMeta {
    let (mtime, identity) = get_dir_meta(dir_fd);
    DirMeta { mtime, identity }
}

// ─── Recursive scanner (uses read_dir_entries) ───────────────────────────────

/// Recursively scan a directory.
/// Returns (total_bytes, total_file_count, error_count).
pub fn scan_dir_recursive(
    arena: &mut PathlessArena,
    dir_fd: &rustix::fd::OwnedFd,
    parent_idx: u32,
    depth: u16,
    config: &ScanConfig,
    root_dev: u64,
) -> (u64, u64, u32) {
    use rustix::fd::AsFd;
    if config.is_cancelled() {
        return (0, 0, 0);
    }

    let meta = read_dir_meta(dir_fd.as_fd());

    // Store cold metadata for this directory
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
        match read_dir_entries(dir_fd.as_fd(), &config.skip_names, config.max_depth, depth) {
            Ok(result) => result,
            Err((errs,)) => {
                eprintln!("readdir error at depth {}", depth);
                return (0, 0, errs);
            }
        };

    let mut total_bytes: u64 = file_bytes;
    let mut total_files: u64 = file_count_here as u64;

    if config.is_cancelled() {
        arena.hot[parent_idx as usize].file_count = file_count_here;
        return (total_bytes, total_files, error_count);
    }

    // Recurse into subdirectories
    for entry in entries {
        if entry.entry_type != EntryType::Dir {
            continue;
        }
        if config.is_cancelled() {
            break;
        }
        if config.one_filesystem && root_dev != 0 {
            let child_dev = get_dev_at(dir_fd.as_fd(), entry.cname.as_c_str()).unwrap_or(0);
            if child_dev != root_dev {
                continue;
            }
        }

        let child_fd = match open_dir_at(dir_fd.as_fd(), entry.cname.as_c_str()) {
            Ok(fd) => fd,
            Err(e) => {
                let is_perm = e == rustix::io::Errno::ACCESS || e == rustix::io::Errno::PERM;
                if is_perm {
                    eprintln!("Permission denied: {:?}", entry.cname);
                } else {
                    eprintln!("Cannot open dir {:?}: {}", entry.cname, e);
                }
                error_count += 1;
                continue;
            }
        };

        let sym = arena.intern(&entry.name_bytes);
        let child_idx = arena.push_node(parent_idx, sym, depth + 1);

        let (child_bytes, child_files, child_errors) =
            scan_dir_recursive(arena, &child_fd, child_idx, depth + 1, config, root_dev);

        arena.hot[child_idx as usize].total_bytes = child_bytes;
        arena.hot[child_idx as usize].file_count = child_files as u32;

        total_bytes += child_bytes;
        total_files += child_files;
        error_count += child_errors;

        if config.is_cancelled() {
            break;
        }
    }

    arena.hot[parent_idx as usize].file_count = file_count_here;

    (total_bytes, total_files, error_count)
}
