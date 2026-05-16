use crate::arena::{DirNode, PathlessArena};
use crate::scan::ScanConfig;
use rustix::fs::{AtFlags, OFlags};
use std::ffi::{CStr, CString};

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

fn open_dir_abs(path: &[u8]) -> rustix::io::Result<rustix::fd::OwnedFd> {
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
fn get_dev_abs(path: &[u8]) -> Option<u64> {
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
fn get_dev_abs(path: &[u8]) -> Option<u64> {
    let cstr = bytes_to_cstring(path);
    rustix::fs::statat(rustix::fs::CWD, cstr.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
        .ok()
        .map(|s| s.st_dev as u64)
}

#[cfg(target_os = "linux")]
fn get_dev_at(dirfd: rustix::fd::BorrowedFd<'_>, name: &CStr) -> Option<u64> {
    rustix::fs::statx(dirfd, name, AtFlags::SYMLINK_NOFOLLOW, StatxFlags::TYPE)
        .ok()
        .map(|s| make_dev(s.stx_dev_major, s.stx_dev_minor))
}

#[cfg(not(target_os = "linux"))]
fn get_dev_at(dirfd: rustix::fd::BorrowedFd<'_>, name: &CStr) -> Option<u64> {
    rustix::fs::statat(dirfd, name, AtFlags::SYMLINK_NOFOLLOW)
        .ok()
        .map(|s| s.st_dev as u64)
}

#[cfg(target_os = "linux")]
fn get_file_size(dirfd: rustix::fd::BorrowedFd<'_>, name: &CStr) -> u64 {
    rustix::fs::statx(dirfd, name, AtFlags::SYMLINK_NOFOLLOW, StatxFlags::SIZE)
        .ok()
        .map(|s| s.stx_size)
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn get_file_size(dirfd: rustix::fd::BorrowedFd<'_>, name: &CStr) -> u64 {
    rustix::fs::statat(dirfd, name, AtFlags::SYMLINK_NOFOLLOW)
        .ok()
        .map(|s| s.st_size as u64)
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn get_dir_mtime(dirfd: rustix::fd::BorrowedFd<'_>) -> i64 {
    rustix::fs::statx(
        dirfd,
        rustix::cstr!(""),
        AtFlags::EMPTY_PATH | AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::MTIME,
    )
    .ok()
    .map(|s| s.stx_mtime.tv_sec)
    .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn get_dir_mtime(dirfd: rustix::fd::BorrowedFd<'_>) -> i64 {
    rustix::fs::statat(dirfd, rustix::cstr!("."), AtFlags::SYMLINK_NOFOLLOW)
        .ok()
        .map(|s| s.st_mtime)
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn make_dev(major: u32, minor: u32) -> u64 {
    ((major as u64) << 32) | (minor as u64)
}

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

    let mut dir = match rustix::fs::Dir::read_from(dir_fd) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("readdir error at depth {}: {}", depth, e);
            return (0, 0, 1);
        }
    };

    let mut total_bytes: u64 = 0;
    let mut total_files: u64 = 0;
    let mut error_count: u32 = 0;
    let mut file_count_here: u32 = 0;

    // Collect subdirs to recurse into after the Dir borrow is done
    struct SubdirEntry {
        name_bytes: Vec<u8>,
        cname: CString,
    }
    let mut subdirs: Vec<SubdirEntry> = Vec::new();

    loop {
        if config.is_cancelled() {
            break;
        }
        let entry = match dir.read() {
            Some(Ok(e)) => e,
            None => break,
            Some(Err(e)) => {
                eprintln!("readdir entry error: {}", e);
                error_count += 1;
                continue;
            }
        };

        let name = entry.file_name();
        let name_bytes = name.to_bytes();

        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }

        if config.skip_names.iter().any(|s| s == name_bytes) {
            continue;
        }

        match entry.file_type() {
            rustix::fs::FileType::Directory => {
                if let Some(max) = config.max_depth {
                    if depth + 1 > max {
                        continue;
                    }
                }
                let cname = match CString::new(name_bytes.to_vec()) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                subdirs.push(SubdirEntry {
                    name_bytes: name_bytes.to_vec(),
                    cname,
                });
            }
            rustix::fs::FileType::RegularFile => {
                let size = get_file_size(dir_fd.as_fd(), name);
                total_bytes += size;
                file_count_here += 1;
                total_files += 1;
            }
            rustix::fs::FileType::Symlink => {
                // Never follow symlinks
            }
            _ => {
                // Stat to determine actual type for DT_UNKNOWN
                let size = get_file_size(dir_fd.as_fd(), name);
                if size > 0 {
                    total_bytes += size;
                    file_count_here += 1;
                    total_files += 1;
                }
            }
        }
    }
    drop(dir);

    let dir_mtime = get_dir_mtime(dir_fd.as_fd());

    if config.is_cancelled() {
        arena.nodes[parent_idx as usize].mtime = dir_mtime;
        arena.nodes[parent_idx as usize].file_count = file_count_here;
        return (total_bytes, total_files, error_count);
    }

    // Recurse into subdirectories
    for sub in subdirs {
        if config.is_cancelled() {
            break;
        }
        if config.one_filesystem && root_dev != 0 {
            let child_dev = get_dev_at(dir_fd.as_fd(), sub.cname.as_c_str()).unwrap_or(0);
            if child_dev != root_dev {
                continue;
            }
        }

        let child_fd = match open_dir_at(dir_fd.as_fd(), sub.cname.as_c_str()) {
            Ok(fd) => fd,
            Err(e) => {
                let is_perm = e == rustix::io::Errno::ACCESS || e == rustix::io::Errno::PERM;
                if is_perm {
                    eprintln!("Permission denied: {:?}", sub.cname);
                } else {
                    eprintln!("Cannot open dir {:?}: {}", sub.cname, e);
                }
                error_count += 1;
                continue;
            }
        };

        let sym = arena.intern(&sub.name_bytes);
        let child_idx = arena.push(DirNode {
            parent: PathlessArena::encode_parent(parent_idx),
            name: sym,
            total_bytes: 0,
            file_count: 0,
            mtime: 0,
            depth: depth + 1,
        });

        let (child_bytes, child_files, child_errors) =
            scan_dir_recursive(arena, &child_fd, child_idx, depth + 1, config, root_dev);

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
