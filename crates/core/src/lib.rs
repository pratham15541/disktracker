use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RawEventReason {
    Created,
    Modified,
    RenamedOld,
    RenamedNew,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    pub volume: String,
    pub file_id: u64,
    pub parent_file_id: u64,
    pub usn: Option<u64>,       // Some from Watcher, None from Scanner
    pub name: String,           // file/dir name only, not a path
    pub reason: RawEventReason, // Created, Modified, RenamedOld, RenamedNew, Deleted
    pub attributes: u32,
    pub is_directory: bool,
    pub size: u64,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MutationKind {
    Created,
    Modified,
    Renamed,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MutationSource {
    Watcher,
    Scanner,
    Recovery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mutation {
    pub mutation_id: String, // ULID
    pub volume: String,
    pub file_id: u64,
    pub parent_file_id: u64,
    pub name: String,
    pub kind: MutationKind, // Created | Modified | Renamed | Deleted
    pub is_directory: bool,
    pub size_delta: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub source: MutationSource, // Watcher | Scanner | Recovery
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub volume: String,      // Part of PRIMARY KEY
    pub file_id: u64,        // Part of PRIMARY KEY
    pub parent_file_id: u64, // FOREIGN KEY -> facts.(volume, file_id) (nullable for volume root, e.g. 0 or self)
    pub name: String,        // just this segment, e.g. "invoice.pdf"
    pub is_directory: bool,
    pub size: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub modified_at: chrono::DateTime<chrono::Utc>,
    pub attributes: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DaemonState {
    Starting,
    BaselineScanning,
    Reconciling,
    Live,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerProgress {
    pub dirs_scanned: u64,
    pub files_scanned: u64,
    pub current_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherProgress {
    pub usn_start: u64,
    pub events_buffered: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainProgress {
    pub replaying: bool,
    pub mutations_replayed: u64,
    pub mutations_total: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeProgress {
    pub state: DaemonState,
    pub scanner: ScannerProgress,
    pub watcher: WatcherProgress,
    pub drain: DrainProgress,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressSnapshot {
    pub daemon_pid: u32,
    pub db_path: std::path::PathBuf,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub state: DaemonState, // Live when all volumes are Live
    pub volumes: std::collections::HashMap<String, VolumeProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_db_modified: Option<chrono::DateTime<chrono::Utc>>,
}

use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicU64, AtomicBool};

pub struct VolumeProgressTracker {
    pub state: Mutex<DaemonState>,
    pub dirs_scanned: AtomicU64,
    pub files_scanned: AtomicU64,
    pub current_path: Mutex<Option<String>>,
    pub usn_start: AtomicU64,
    pub events_buffered: AtomicU64,
    pub replaying: AtomicBool,
    pub mutations_replayed: AtomicU64,
    pub mutations_total: AtomicU64,
    pub has_mutations_total: AtomicBool,
}

static PROGRESS_REGISTRY: OnceLock<Mutex<std::collections::HashMap<String, Arc<VolumeProgressTracker>>>> = OnceLock::new();

pub fn get_volume_tracker(volume: &str) -> Arc<VolumeProgressTracker> {
    let registry = PROGRESS_REGISTRY.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut map = registry.lock().unwrap();
    map.entry(volume.to_string())
        .or_insert_with(|| {
            Arc::new(VolumeProgressTracker {
                state: Mutex::new(DaemonState::Starting),
                dirs_scanned: AtomicU64::new(0),
                files_scanned: AtomicU64::new(0),
                current_path: Mutex::new(None),
                usn_start: AtomicU64::new(0),
                events_buffered: AtomicU64::new(0),
                replaying: AtomicBool::new(false),
                mutations_replayed: AtomicU64::new(0),
                mutations_total: AtomicU64::new(0),
                has_mutations_total: AtomicBool::new(false),
            })
        })
        .clone()
}

pub fn get_registered_volumes() -> Vec<String> {
    let registry = PROGRESS_REGISTRY.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let map = registry.lock().unwrap();
    map.keys().cloned().collect()
}
