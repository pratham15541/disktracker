use anyhow::Result;
use disktracker_core::scan::{scan, ScanConfig};
use disktracker_db::delta::DeltaRow;
use disktracker_events::DirtyQueue;
use notify::{Event, EventKind};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

use crate::mmap_index::MmapState;

/// Incremental aggregation engine.
/// Maintains in-memory state of directory sizes and processes dirty queues.
pub struct IncrementalEngine {
    pub state: MmapState,
    pub dirty: DirtyQueue,
    pub scan_roots: Vec<PathBuf>,
    skip_names: Vec<Vec<u8>>,
    one_filesystem: bool,
}

impl IncrementalEngine {
    /// Perform an initial full scan and populate the in-memory state.
    pub fn init(
        root: PathBuf,
        skip_names: Vec<Vec<u8>>,
        one_filesystem: bool,
    ) -> (Self, disktracker_core::scan::ScanResult) {
        let (engine, results) = Self::init_multi(vec![root], skip_names, one_filesystem);
        (engine, results.into_iter().next().unwrap())
    }

    /// Perform initial scans for multiple roots.
    pub fn init_multi(
        roots: Vec<PathBuf>,
        skip_names: Vec<Vec<u8>>,
        one_filesystem: bool,
    ) -> (Self, Vec<disktracker_core::scan::ScanResult>) {
        let mut state = MmapState::new(None);
        let mut results = Vec::new();
        for root in &roots {
            let config = ScanConfig {
                root: root.clone(),
                max_depth: None,
                skip_names: skip_names.clone(),
                one_filesystem,
                cancel_flag: None,
                ..Default::default()
            };
            let result = scan(config);
            for idx in 0..result.arena.node_count() {
                let path = result.arena.materialize_path(idx as u32);
                let bytes = result.arena.hot[idx].total_bytes as i64;
                state.insert(path, bytes);
            }
            results.push(result);
        }
        let engine = Self {
            state,
            dirty: DirtyQueue::new(),
            scan_roots: roots,
            skip_names,
            one_filesystem,
        };
        (engine, results)
    }

    /// Initialize the engine directly from a loaded memory-mapped state.
    pub fn init_from_state(
        roots: Vec<PathBuf>,
        skip_names: Vec<Vec<u8>>,
        one_filesystem: bool,
        state: MmapState,
    ) -> Self {
        Self {
            state,
            dirty: DirtyQueue::new(),
            scan_roots: roots,
            skip_names,
            one_filesystem,
        }
    }

    /// Process a notify event: mark the affected directory dirty.
    pub fn ingest_event(&mut self, event: &Event) {
        if let EventKind::Access(_) = event.kind {
            return;
        }

        for path in &event.paths {
            let dir: PathBuf = if path.is_dir() {
                path.clone()
            } else {
                path.parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| path.clone())
            };

            // Only mark dirty if within one of our scan roots
            if self.scan_roots.iter().any(|r| dir.starts_with(r)) {
                let dir_bytes = path_to_bytes(&dir);
                if !dir_bytes.is_empty() {
                    self.dirty.mark_dirty(dir_bytes);
                }
            }
        }

        // Handle overflow events — need full reconcile
        if matches!(event.kind, EventKind::Other) {
            self.dirty.mark_overflow();
        }
    }

    /// Process all pending dirty directories.
    /// Returns a vec of delta rows suitable for DB storage.
    pub fn process_dirty_batch(&mut self, _conn: &Connection) -> Result<Vec<DeltaRow>> {
        let (dirty_paths, overflow) = self.dirty.drain();

        if overflow {
            eprintln!("[watch] Event overflow — triggering full reconcile of all roots");
            let mut all_deltas = Vec::new();
            for root in self.scan_roots.clone() {
                let deltas = self.process_one_dirty(&root)?;
                all_deltas.extend(deltas);
            }
            return Ok(all_deltas);
        }

        let mut all_deltas: Vec<DeltaRow> = Vec::new();
        for path_bytes in dirty_paths {
            let path = bytes_to_path(&path_bytes);
            let deltas = self.process_one_dirty(&path)?;
            all_deltas.extend(deltas);
        }
        Ok(all_deltas)
    }

    fn process_one_dirty(&mut self, path: &Path) -> Result<Vec<DeltaRow>> {
        let path_bytes = path_to_bytes(path);
        let old_size = self.state.get(&path_bytes).unwrap_or(0);

        if !path.exists() {
            // Directory was deleted — size goes to zero
            if old_size == 0 {
                return Ok(vec![]);
            }
            // Recursively remove path_bytes and all of its subdirectories from self.state
            self.state.remove_subtree(&path_bytes);
            let delta = -old_size;
            self.propagate_delta(&path_bytes, delta);
            return Ok(vec![DeltaRow::new(path_bytes, Some(old_size), 0)]);
        }

        let mut new_size = 0;
        let mut subdirs_to_rescan = Vec::new();
        let mut expected_subdirs = std::collections::HashSet::new();

        // 1. Identify direct subdirectories currently registered in self.state under `path`
        for (k, _) in self.state.get_active_entries() {
            if parent_path_bytes(&k) == path_bytes {
                expected_subdirs.insert(k);
            }
        }

        // 2. Scan direct entries non-recursively
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    let entry_path = entry.path();
                    let entry_bytes = path_to_bytes(&entry_path);
                    if meta.is_dir() {
                        let name = entry
                            .file_name()
                            .to_string_lossy()
                            .into_owned()
                            .into_bytes();
                        if self.skip_names.contains(&name) {
                            continue;
                        }
                        if let Some(sub_bytes) = self.state.get(&entry_bytes) {
                            new_size += sub_bytes;
                            expected_subdirs.remove(&entry_bytes);
                        } else {
                            subdirs_to_rescan.push(entry_path);
                        }
                    } else {
                        new_size += meta.len() as i64;
                    }
                }
            }
        }

        // 3. Clean up subdirectories that are no longer physically present
        for deleted_sub in expected_subdirs {
            self.state.remove_subtree(&deleted_sub);
        }

        // 4. Recursively scan brand new directories
        for subdir in subdirs_to_rescan {
            let config = ScanConfig {
                root: subdir.clone(),
                max_depth: None,
                skip_names: self.skip_names.clone(),
                one_filesystem: self.one_filesystem,
                cancel_flag: None,
                ..Default::default()
            };
            let result = scan(config);
            // Insert all scanned subdirectories into state
            for idx in 0..result.arena.node_count() {
                let sub_path = result.arena.materialize_path(idx as u32);
                let sub_bytes = result.arena.hot[idx].total_bytes as i64;
                self.state.insert(sub_path, sub_bytes);
            }
            let sub_bytes = result.total_bytes as i64;
            new_size += sub_bytes;
        }

        let delta = new_size - old_size;
        if delta == 0 {
            return Ok(vec![]);
        }

        // Update path_bytes size in state
        self.state.insert(path_bytes.clone(), new_size);

        // Propagate net delta up the parent chain
        self.propagate_delta(&path_bytes, delta);

        Ok(vec![DeltaRow::new(
            path_bytes,
            Some(old_size).filter(|&v| v != 0),
            new_size,
        )])
    }

    /// Propagate a size delta up the parent chain of `path_bytes`.
    /// Stops at any scan root or filesystem root.
    fn propagate_delta(&mut self, path_bytes: &[u8], delta: i64) {
        let mut current = path_bytes.to_vec();
        loop {
            let parent = parent_path_bytes(&current);
            if parent.is_empty() || parent == current {
                break;
            }
            let old_size = self.state.get(&parent).unwrap_or(0);
            self.state.insert(parent.clone(), old_size + delta);
            // Stop propagating once we've updated any scan root
            if self.scan_roots.iter().any(|r| parent == path_to_bytes(r)) {
                break;
            }
            current = parent;
        }
    }

    /// Current size of a path according to in-memory state.
    pub fn current_size(&self, path: &[u8]) -> i64 {
        self.state.get(path).unwrap_or(0)
    }

    /// Top N largest directories by current size.
    pub fn top_dirs(&self, n: usize) -> Vec<(Vec<u8>, i64)> {
        let mut entries = self.state.get_active_entries();
        entries.sort_by_key(|b| std::cmp::Reverse(b.1));
        entries.truncate(n);
        entries
    }
}

// ─── Path utilities ──────────────────────────────────────────────────────────

pub fn path_to_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

pub fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(windows)]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
    }
}

pub fn parent_path_bytes(path: &[u8]) -> Vec<u8> {
    #[cfg(windows)]
    const SEP: u8 = b'\\';
    #[cfg(not(windows))]
    const SEP: u8 = b'/';

    let trimmed = path.strip_suffix(&[SEP]).unwrap_or(path);
    if let Some(pos) = trimmed.iter().rposition(|&b| b == SEP) {
        if pos == 0 {
            return vec![SEP]; // filesystem root
        }
        trimmed[..pos].to_vec()
    } else {
        vec![] // no parent
    }
}

// Serialize a list of PathBuf into a byte vector separated by \0
pub fn roots_to_bytes(roots: &[PathBuf]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (i, r) in roots.iter().enumerate() {
        if i > 0 {
            bytes.push(0);
        }
        bytes.extend_from_slice(&path_to_bytes(r));
    }
    bytes
}

// Deserialize a byte vector separated by \0 back into Vec<PathBuf>
pub fn bytes_to_roots(bytes: &[u8]) -> Vec<PathBuf> {
    if bytes.is_empty() {
        return vec![];
    }
    bytes.split(|&b| b == 0).map(bytes_to_path).collect()
}
