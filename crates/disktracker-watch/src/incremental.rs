use anyhow::Result;
use disktracker_core::scan::{scan, ScanConfig};
use disktracker_db::delta::DeltaRow;
use disktracker_events::DirtyQueue;
use notify::{Event, EventKind};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// In-memory directory size map.
/// Key: raw path bytes. Value: total recursive bytes.
pub type SizeMap = HashMap<Vec<u8>, i64>;

/// Incremental aggregation engine.
/// Maintains in-memory state of directory sizes and processes dirty queues.
pub struct IncrementalEngine {
    pub state: SizeMap,
    pub dirty: DirtyQueue,
    scan_root: PathBuf,
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
        let config = ScanConfig {
            root: root.clone(),
            max_depth: None,
            skip_names: skip_names.clone(),
            one_filesystem,
            cancel_flag: None,
        };
        let result = scan(config);
        let mut state: SizeMap = HashMap::with_capacity(result.arena.nodes.len());
        for idx in 0..result.arena.nodes.len() {
            let path = result.arena.materialize_path(idx as u32);
            let bytes = result.arena.nodes[idx].total_bytes as i64;
            state.insert(path, bytes);
        }
        let engine = Self {
            state,
            dirty: DirtyQueue::new(),
            scan_root: root,
            skip_names,
            one_filesystem,
        };
        (engine, result)
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

            // Only mark dirty if within our scan root
            if dir.starts_with(&self.scan_root) {
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
            eprintln!("[watch] Event overflow — triggering full reconcile of root");
            return self.process_one_dirty(&self.scan_root.clone());
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
        let old_size = self.state.get(&path_bytes).copied().unwrap_or(0);

        if !path.exists() {
            // Directory was deleted — size goes to zero
            if old_size == 0 {
                return Ok(vec![]);
            }
            self.state.remove(&path_bytes);
            let delta = -old_size;
            self.propagate_delta(&path_bytes, delta);
            return Ok(vec![DeltaRow::new(path_bytes, Some(old_size), 0)]);
        }

        // Rescan this subtree only
        let config = ScanConfig {
            root: path.to_path_buf(),
            max_depth: None,
            skip_names: self.skip_names.clone(),
            one_filesystem: self.one_filesystem,
            cancel_flag: None,
        };
        let result = scan(config);
        let new_size = result.total_bytes as i64;
        let delta = new_size - old_size;

        if delta == 0 {
            return Ok(vec![]);
        }

        // Update in-memory state for all dirs in the rescanned subtree
        for idx in 0..result.arena.nodes.len() {
            let sub_path = result.arena.materialize_path(idx as u32);
            let sub_bytes = result.arena.nodes[idx].total_bytes as i64;
            self.state.insert(sub_path, sub_bytes);
        }

        // Propagate net delta up the parent chain (outside the rescanned subtree)
        self.propagate_delta(&path_bytes, delta);

        Ok(vec![DeltaRow::new(
            path_bytes,
            Some(old_size).filter(|&v| v != 0),
            new_size,
        )])
    }

    /// Propagate a size delta up the parent chain of `path_bytes`.
    /// Stops at the scan root or filesystem root.
    fn propagate_delta(&mut self, path_bytes: &[u8], delta: i64) {
        let mut current = path_bytes.to_vec();
        loop {
            let parent = parent_path_bytes(&current);
            if parent.is_empty() || parent == current {
                break;
            }
            let size = self.state.entry(parent.clone()).or_insert(0);
            *size += delta;
            // Stop propagating once we've updated the scan root
            let root_bytes = path_to_bytes(&self.scan_root);
            if parent == root_bytes {
                break;
            }
            current = parent;
        }
    }

    /// Current size of a path according to in-memory state.
    pub fn current_size(&self, path: &[u8]) -> i64 {
        self.state.get(path).copied().unwrap_or(0)
    }

    /// Top N largest directories by current size.
    pub fn top_dirs(&self, n: usize) -> Vec<(Vec<u8>, i64)> {
        let mut entries: Vec<(Vec<u8>, i64)> =
            self.state.iter().map(|(k, &v)| (k.clone(), v)).collect();
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
