# DiskTracker

DiskTracker is a fast, cross-platform CLI for storage observability. It stores directory snapshots, diffs them, and can watch the filesystem in real time to explain where disk growth comes from.

This is not a cleaner or a treemap viewer. It is a historical and real-time change tracker.

---

## Features

- Snapshot scanning with a pathless arena for low allocation overhead
- Diff and growth reports across snapshots with cached diff materialization
- Real-time watch mode with debounce, dirty queue dedup, and incremental rescan
- Explain mode with lightweight path heuristics (no ML)
- Timeline history per directory (from snapshots or watch deltas)
- Prune old snapshots with preview mode
- JSON output for every command that reports data

Default database path: `~/.disktracker/data.db`

---

## Install

### npm (all platforms)

```bash
npm i -g disktracker
```

### Linux/MacOS (curl)

```bash
curl -fsSL https://raw.githubusercontent.com/pratham15541/disktracker/main/scripts/install.sh | bash
```

### Windows (winget)

```powershell
winget install --id pratham15541.disktracker
```

### Build from source

```bash
cd disktracker
cargo build --release
# Binary: target/release/disktracker
```

---

## Quickstart

```bash
# Scan a directory and store a snapshot
disktracker scan /home/user --db ~/data.db

# List all stored snapshots
disktracker list --db ~/data.db

# Diff the two most recent snapshots
disktracker diff --db ~/data.db

# Diff from 7 days ago to now
disktracker diff --from 7d --db ~/data.db

# Growth report for the last 7 days
disktracker report --last 7d --db ~/data.db

# Watch / in real time (blocks until Ctrl+C)
disktracker watch /

# Explain what caused growth over the last 7 days
disktracker explain --last 7d

# Show the size history of a specific directory
disktracker timeline ~/Downloads

# Validate watcher state and repair drift
disktracker reconcile

# Full rescan reconcile (detects and persists drift)
disktracker reconcile --full

# Preview and prune snapshots
disktracker prune --older-than 90d --dry-run
disktracker prune --older-than 90d
```

---

## Testing

Run all tests:

```bash
cargo test
```

Run a specific crate:

```bash
cargo test -p disktracker-cli
cargo test -p disktracker-core
cargo test -p disktracker-db
cargo test -p disktracker-watch
cargo test -p disktracker-events
```

CLI end-to-end tests run the `disktracker` binary via `assert_cmd` and use temporary directories and databases.

---

## Snapshot references

Many commands accept snapshot references for `--from`, `--to`, and `--last`:

- Numeric snapshot id (for example `42`)
- Relative duration: `7d`, `2w`, `1m`
- Date: `YYYY-MM-DD`

---

## Command reference

All commands support `--db <PATH>` to set the SQLite database, and most commands support `--json` for structured output.

### scan

```text
disktracker scan [PATH] [OPTIONS]

Options:
  --max-depth <N>       Limit directory depth
  --skip <NAME>         Skip directory name (repeatable)
  --one-filesystem      Do not cross filesystem boundaries
  --db <PATH>           SQLite database path
  --quiet               Suppress progress output
  --json                JSON output
```

### diff

```text
disktracker diff [OPTIONS]

Options:
  --from <REF>          Snapshot reference (id, duration, or date)
  --to <REF>            Snapshot reference (default: latest)
  --top <N>             Number of entries (default: 20)
  --min-delta <BYTES>   Minimum delta to show (default: 1048576)
  --db <PATH>           SQLite database path
  --json                JSON output
```

### report

```text
disktracker report [OPTIONS]

Options:
  --last <DURATION>     Time window (default: 7d)
  --top <N>             Number of entries (default: 15)
  --depth <N>           Max path depth (default: 4)
  --db <PATH>           SQLite database path
  --json                JSON output
```

### list

```text
disktracker list [OPTIONS]

Options:
  --db <PATH>           SQLite database path
  --json                JSON output
```

### watch

```text
disktracker watch [PATH] [OPTIONS]

Options:
  --db <PATH>           SQLite database path
  --quiet               Suppress per-event output
  --one-filesystem      Do not cross filesystem boundaries
  --skip <NAME>         Skip directory name (repeatable)
  --debounce-ms <N>     Debounce window in milliseconds (default: 500)
  --flush-secs <N>      Flush state to DB every N seconds (default: 3600)
```

### explain

```text
disktracker explain [OPTIONS]

Options:
  --last <DURATION>     Time window (default: 7d)
  --top <N>             Number of entries (default: 15)
  --db <PATH>           SQLite database path
  --json                JSON output
```

### timeline

```text
disktracker timeline <PATH> [OPTIONS]

Options:
  --db <PATH>           SQLite database path
  --json                JSON output
```

### reconcile

```text
disktracker reconcile [OPTIONS]

Options:
  --full                Perform a full scan to detect and fix drift
  --db <PATH>           SQLite database path
  --json                JSON output
```

### prune

```text
disktracker prune [OPTIONS]

Options:
  --keep-last <N>       Keep the N most recent snapshots
  --older-than <DUR>    Delete snapshots older than this window (90d, 12w, 6m)
  --dry-run             Preview deletions without making changes
  --db <PATH>           SQLite database path
  --json                JSON output
```

---

## System design

### Crate layout

```
disktracker/
├── Cargo.toml                    workspace root
└── crates/
    ├── disktracker-core/         pathless arena scanner (no DB, no CLI)
    ├── disktracker-events/       FsEvent types + DirtyQueue
    ├── disktracker-watch/        notify watcher + incremental rescan engine
    ├── disktracker-db/           SQLite schema and queries
    └── disktracker-cli/          CLI entry point
```

### Scan pipeline

1. Build a pathless arena: intern raw filename bytes into a flat byte pool.
2. Recursively scan using platform-specific APIs.
3. Store one row per directory (path bytes + optional UTF-8) in `dir_snapshots`.

Notes:

- Linux uses `statx` where available; macOS uses `fstatat`; symlinks are never followed.
- `--one-filesystem` is enforced by device id on Unix and volume serial on Windows.

### Watch pipeline

1. Initial full scan and snapshot insertion.
2. Start a `notify` watcher and ingest events.
3. Debounce events and mark directories dirty in `DirtyQueue`.
4. Rescan only dirty subtrees; compute deltas and propagate net change up the parent chain in memory.
5. Persist `dir_deltas` and raw `fs_events` for recent activity.
6. Periodically flush a full snapshot of in-memory state.

If the watcher overflows, the dirty queue forces a full reconcile of the root.

### Diff and report

- Diffs are cached in `diff_cache` per snapshot pair.
- The first diff populates the cache; subsequent queries reuse it.
- Reports are filtered diffs with optional depth limits.

### Explain

Explain uses path heuristics (for example `node_modules`, `overlay2`, `.cache/pip`) to attribute growth and aggregate by label.

### Timeline

- Prefer `dir_snapshots` for full history.
- Fall back to `dir_deltas` when watch mode has sparse snapshots.

### Reconcile and prune

- Reconcile validates watcher state and, with `--full`, writes a new snapshot to detect drift.
- Prune deletes snapshot rows and related data, with optional preview mode.

---

## Database schema

Key tables and purposes:

- `snapshots`: summary of each scan or flush
- `dir_snapshots`: one row per directory, per snapshot
- `diff_cache`: cached diffs between snapshot pairs
- `dir_deltas`: incremental deltas from watch mode
- `fs_events`: raw event log (optional for future use)
- `watch_state`: last watch root and timestamps

SQLite is configured in WAL mode with a 64 MB page cache, 256 MB mmap, memory temp store, and foreign keys enabled.
