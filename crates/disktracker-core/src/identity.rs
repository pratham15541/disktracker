use std::collections::HashSet;

/// Canonical filesystem identity.
///
/// On Unix: `(dev_t, ino_t)` from statx/fstatat.
/// On Windows: `(volume_serial, FILE_ID)` from GetFileInformationByHandle.
///
/// # Identity Semantics
///
/// ## Rename
/// Preserves identity — only the path changes. In watch mode, rename events
/// update the path mapping, not the identity.
///
/// ## Hardlink
/// Multiple paths share the same identity. DiskTracker counts bytes ONCE
/// per identity when `nlink > 1` is detected during scan.
///
/// ## Symlink
/// Never followed, never resolved. Symlink size = size of the link itself.
///
/// ## Mount Boundary
/// Different `dev` = different identity space. `--one-filesystem` uses
/// dev comparison to stop at mount points.
///
/// ## APFS Clone / Reflink
/// Distinct `ino`, shared physical blocks. Phase A reports LOGICAL size
/// (matches `du`). Phase D will report PHYSICAL size (matches Finder)
/// via FIEMAP / F_LOG2PHYS.
///
/// ## Bind Mount
/// May share or differ in `dev` (kernel-dependent). `VisitedSet` prevents
/// double-counting regardless.
///
/// ## Stability
/// Inodes are stable within a single mount across renames. They are NOT
/// stable across remounts, mkfs, or on some FUSE/NFS filesystems.
/// Warm scan falls back to path-keyed lookup when identity is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub struct FsIdentity {
    pub dev: u64,
    pub ino: u64,
}

impl FsIdentity {
    /// Sentinel value for "identity not available" (FUSE, NFS, etc.).
    pub const UNKNOWN: Self = Self { dev: 0, ino: 0 };

    /// Returns true if this identity was actually populated from the filesystem.
    #[inline]
    pub fn is_known(&self) -> bool {
        // ino 0 is not a valid inode on any real filesystem
        self.ino != 0
    }
}

#[cfg(target_os = "linux")]
impl FsIdentity {
    /// Construct from a Linux statx result.
    pub fn from_statx(stx: &rustix::fs::Statx) -> Self {
        let dev = ((stx.stx_dev_major as u64) << 32) | (stx.stx_dev_minor as u64);
        Self {
            dev,
            ino: stx.stx_ino,
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
impl FsIdentity {
    /// Construct from a BSD/macOS stat result.
    pub fn from_stat(st: &rustix::fs::Stat) -> Self {
        Self {
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
        }
    }
}

/// Tracks visited directories to prevent double-traversal.
///
/// Used to handle:
/// - Bind mounts that expose the same directory tree twice
/// - Hardlinked directories (rare but possible on some filesystems)
/// - Any configuration that creates overlapping directory trees
///
/// Only needed when `--one-filesystem` is NOT set, since mount boundaries
/// already prevent cross-device traversal.
pub struct VisitedSet {
    seen: HashSet<FsIdentity>,
}

impl VisitedSet {
    pub fn new() -> Self {
        Self {
            seen: HashSet::with_capacity(1024),
        }
    }

    /// Record a directory as visited. Returns `true` if this is the first visit.
    /// Returns `true` for unknown identities (can't dedup what we can't identify).
    #[inline]
    pub fn visit(&mut self, id: FsIdentity) -> bool {
        if !id.is_known() {
            return true;
        }
        self.seen.insert(id)
    }

    /// Number of unique directories visited.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

impl Default for VisitedSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_unknown() {
        assert!(!FsIdentity::UNKNOWN.is_known());
        assert!(!FsIdentity::default().is_known());
    }

    #[test]
    fn test_identity_known() {
        let id = FsIdentity { dev: 1, ino: 42 };
        assert!(id.is_known());
    }

    #[test]
    fn test_identity_equality() {
        let a = FsIdentity { dev: 1, ino: 100 };
        let b = FsIdentity { dev: 1, ino: 100 };
        let c = FsIdentity { dev: 2, ino: 100 };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_visited_set_dedup() {
        let mut vs = VisitedSet::new();
        let id = FsIdentity { dev: 1, ino: 42 };
        assert!(vs.visit(id)); // first visit
        assert!(!vs.visit(id)); // duplicate
        assert_eq!(vs.len(), 1);
    }

    #[test]
    fn test_visited_set_unknown_always_allowed() {
        let mut vs = VisitedSet::new();
        // Unknown identities always return true (can't dedup)
        assert!(vs.visit(FsIdentity::UNKNOWN));
        assert!(vs.visit(FsIdentity::UNKNOWN));
        // But they don't get inserted (ino=0 is filtered)
        assert_eq!(vs.len(), 0);
    }

    #[test]
    fn test_visited_set_multiple() {
        let mut vs = VisitedSet::new();
        let a = FsIdentity { dev: 1, ino: 10 };
        let b = FsIdentity { dev: 1, ino: 20 };
        let c = FsIdentity { dev: 2, ino: 10 }; // same ino, different dev
        assert!(vs.visit(a));
        assert!(vs.visit(b));
        assert!(vs.visit(c));
        assert_eq!(vs.len(), 3);
        assert!(!vs.visit(a)); // already seen
    }
}
