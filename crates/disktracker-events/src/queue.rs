use std::collections::HashSet;

/// Raw path bytes used as a key. Always points to a directory.
pub type PathKey = Vec<u8>;

/// Deduplicating set of dirty directory paths.
/// Multiple events on the same directory (or its children) coalesce into
/// a single entry — this is the core anti-thundering-herd mechanism.
pub struct DirtyQueue {
    pub pending: HashSet<PathKey>,
    /// Set when an overflow event is received — full reconcile required.
    pub overflow: bool,
}

impl DirtyQueue {
    pub fn new() -> Self {
        Self {
            pending: HashSet::new(),
            overflow: false,
        }
    }

    /// Mark a directory as needing a rescan.
    pub fn mark_dirty(&mut self, path: Vec<u8>) {
        // Skip empty paths
        if path.is_empty() {
            return;
        }
        // If a parent is already dirty, no need to add the child too —
        // the parent rescan will cover it. However, for simplicity we add
        // every unique path and deduplicate at drain time.
        self.pending.insert(path);
    }

    /// Mark an overflow — the watcher lost events.
    pub fn mark_overflow(&mut self) {
        self.overflow = true;
        self.pending.clear(); // full scan will cover everything
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && !self.overflow
    }

    /// Drain all pending dirty paths. Returns them sorted by path length
    /// (shortest first) so parent rescans subsume child rescans.
    pub fn drain(&mut self) -> (Vec<PathKey>, bool) {
        let overflow = self.overflow;
        self.overflow = false;
        let mut paths: Vec<PathKey> = self.pending.drain().collect();

        // Remove child paths whose parent is also dirty — parent rescan covers them
        paths.sort_by_key(|p| p.len());
        let mut deduplicated: Vec<PathKey> = Vec::new();
        'outer: for path in &paths {
            for existing in &deduplicated {
                if path.starts_with(existing.as_slice())
                    && (path.len() == existing.len()
                        || path[existing.len()] == b'/'
                        || path[existing.len()] == b'\\')
                {
                    // Parent already in set — skip this child
                    continue 'outer;
                }
            }
            deduplicated.push(path.clone());
        }

        (deduplicated, overflow)
    }
}

impl Default for DirtyQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_child_paths() {
        let mut q = DirtyQueue::new();
        q.mark_dirty(b"/home/user".to_vec());
        q.mark_dirty(b"/home/user/projects".to_vec());
        q.mark_dirty(b"/home/user/projects/app".to_vec());
        let (paths, _) = q.drain();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], b"/home/user");
    }

    #[test]
    fn test_independent_paths() {
        let mut q = DirtyQueue::new();
        q.mark_dirty(b"/home/user".to_vec());
        q.mark_dirty(b"/tmp".to_vec());
        let (paths, _) = q.drain();
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_overflow_clears_pending() {
        let mut q = DirtyQueue::new();
        q.mark_dirty(b"/home".to_vec());
        q.mark_overflow();
        let (paths, overflow) = q.drain();
        assert!(paths.is_empty());
        assert!(overflow);
    }
}
