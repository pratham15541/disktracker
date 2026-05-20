/// Full SQL schema — all tables. Idempotent (IF NOT EXISTS everywhere).
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS snapshots (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    scan_root   TEXT    NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    total_files INTEGER NOT NULL,
    total_bytes INTEGER NOT NULL,
    error_count INTEGER NOT NULL DEFAULT 0,
    host        TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS dir_snapshots (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id     INTEGER NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
    path_blob       BLOB    NOT NULL,
    path_utf8       TEXT,
    depth           INTEGER NOT NULL,
    total_bytes     INTEGER NOT NULL,
    file_count      INTEGER NOT NULL,
    mtime           INTEGER NOT NULL,
    dev             INTEGER NOT NULL DEFAULT 0,
    ino             INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_dir_snapshot_id
    ON dir_snapshots(snapshot_id);

CREATE INDEX IF NOT EXISTS idx_dir_path_bytes
    ON dir_snapshots(path_blob, snapshot_id);

CREATE INDEX IF NOT EXISTS idx_dir_identity
    ON dir_snapshots(dev, ino, snapshot_id);

CREATE TABLE IF NOT EXISTS diff_cache (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_a      INTEGER NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
    snapshot_b      INTEGER NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
    path_blob       BLOB    NOT NULL,
    bytes_a         INTEGER,
    bytes_b         INTEGER,
    delta_bytes     INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_diff_delta
    ON diff_cache(snapshot_a, snapshot_b, delta_bytes);

-- MVP2 tables

CREATE TABLE IF NOT EXISTS fs_events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp       INTEGER NOT NULL,
    event_type      INTEGER NOT NULL,
    path_blob       BLOB    NOT NULL,
    delta_bytes     INTEGER,
    is_dir          INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_fs_events_time
    ON fs_events(timestamp);

CREATE TABLE IF NOT EXISTS dir_deltas (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id     INTEGER NOT NULL,
    path_blob       BLOB    NOT NULL,
    path_utf8       TEXT,
    previous_bytes  INTEGER,
    current_bytes   INTEGER NOT NULL,
    delta_bytes     INTEGER NOT NULL,
    recorded_at     INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_dir_deltas_snapshot
    ON dir_deltas(snapshot_id);

CREATE INDEX IF NOT EXISTS idx_dir_deltas_path
    ON dir_deltas(path_blob, recorded_at);

CREATE TABLE IF NOT EXISTS watch_state (
    id                  INTEGER PRIMARY KEY,
    watch_root          BLOB    NOT NULL,
    last_event_time     INTEGER,
    last_reconcile_time INTEGER,
    last_snapshot_id    INTEGER
);

CREATE TABLE IF NOT EXISTS mutation_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp       INTEGER NOT NULL,
    mutation_type   INTEGER NOT NULL,
    dev             INTEGER NOT NULL,
    ino             INTEGER NOT NULL,
    path_blob       BLOB    NOT NULL,
    old_size        INTEGER,
    new_size        INTEGER,
    old_path_blob   BLOB
);

CREATE INDEX IF NOT EXISTS idx_mutation_log_time
    ON mutation_log(timestamp);

CREATE INDEX IF NOT EXISTS idx_mutation_log_identity
    ON mutation_log(dev, ino);
"#;

pub const WAL_PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA cache_size   = -65536;
PRAGMA mmap_size    = 268435456;
PRAGMA temp_store   = MEMORY;
PRAGMA foreign_keys = ON;
";
