use crate::identity::FsIdentity;
use crate::scan::{SkipResult, WarmSkipPredicate};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CachedDir {
    pub total_bytes: u64,
    pub file_count: u32,
    pub mtime: i64,
}

#[derive(Debug, Clone, Default)]
pub struct SnapshotIndex {
    pub by_identity: HashMap<FsIdentity, CachedDir>,
    pub by_path: HashMap<Vec<u8>, CachedDir>,
    pub non_leaves: HashSet<Vec<u8>>,
}

fn parent_path_bytes(path: &[u8]) -> Vec<u8> {
    #[cfg(windows)]
    const SEP: u8 = b'\\';
    #[cfg(not(windows))]
    const SEP: u8 = b'/';

    let trimmed = path.strip_suffix(&[SEP]).unwrap_or(path);
    if let Some(pos) = trimmed.iter().rposition(|&b| b == SEP) {
        if pos == 0 {
            return vec![SEP];
        }
        #[cfg(windows)]
        {
            if pos == 2 && trimmed.get(1) == Some(&b':') {
                return trimmed[..=pos].to_vec();
            }
        }
        trimmed[..pos].to_vec()
    } else {
        vec![]
    }
}

impl SnapshotIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_identity(&mut self, identity: FsIdentity, cached: CachedDir) {
        if identity.is_known() {
            self.by_identity.insert(identity, cached.clone());
        }
    }

    pub fn insert_path(&mut self, path: Vec<u8>, cached: CachedDir) {
        let parent = parent_path_bytes(&path);
        if !parent.is_empty() {
            self.non_leaves.insert(parent);
        }
        self.by_path.insert(path, cached);
    }

    pub fn build_skip_predicate(self) -> Arc<WarmSkipPredicate> {
        Arc::new(move |path_bytes, current_mtime, identity| {
            // A non-leaf directory cannot be skipped entirely because mtime is non-recursive.
            if self.non_leaves.contains(path_bytes) {
                return None;
            }

            // 1. Look up by FsIdentity first (fast, correct across renames)
            if identity.is_known() {
                if let Some(cached) = self.by_identity.get(&identity) {
                    if current_mtime <= cached.mtime {
                        return Some(SkipResult {
                            total_bytes: cached.total_bytes,
                            file_count: cached.file_count,
                        });
                    }
                }
            }
            // 2. Fallback to path bytes (for NFS/FUSE where inode may not be stable)
            if let Some(cached) = self.by_path.get(path_bytes) {
                if current_mtime <= cached.mtime {
                    return Some(SkipResult {
                        total_bytes: cached.total_bytes,
                        file_count: cached.file_count,
                    });
                }
            }
            None
        })
    }
}
