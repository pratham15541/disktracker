# DiskTracker — Progress Tracker

> This is the **only** file that changes every session. Do not edit `AI_MASTER_PLAN.md` to
> reflect progress. If something here contradicts the master plan, stop and flag it under
> "Open Issues" instead of silently resolving it either direction.

## Current Active Loop

**None (All loops complete)**

## Next Action

1. All loops in `AI_MASTER_PLAN.md` are fully implemented, optimized, and verified on native Windows. No further loop actions are required.

---

## Loop Status

| Loop | Status | Verified on Windows? | Model(s) used | Date |
|---|---|---|---|---|
| 1 — CLI & IPC | Completed | yes | Gemini 3.5 Flash | 2026-07-08 |
| 2 — Storage schemas | Completed | yes | Gemini 3.5 Flash | 2026-07-08 |
| 3 — Scanner + Watcher | Completed | yes | Gemini 3.5 Flash / Gemini 3.5 Flash (High) | 2026-07-09 |
| 4 — Pipeline & Drain | Completed | yes | Gemini 3.5 Flash / Gemini 3.5 Flash (High) | 2026-07-09 |
| 5 — Diagnostics | Completed | yes | Gemini 3.5 Flash (High) | 2026-07-09 |
| 6 — Uninstall | Completed | yes | Gemini 3.5 Flash (High) | 2026-07-09 |

_Update this table every time a loop is verified. "Verified on Windows?" must be an actual
yes/no based on a real manual test on native Windows, not on the code compiling in WSL._


## Known Deviations from AI_MASTER_PLAN.md

_(list anything the actual code does differently from the plan, and why — e.g. a struct
field renamed, a crate merged with another. If this list is non-empty, the next agent must
read it before touching related code.)_

- **In-Memory Progress Tracking & Crate Layout (Deviation from §11.2):**
  - **Crate Ownership:** The `core-types` crate (located in `crates/core/src/lib.rs`) owns the `ProgressSnapshot`, `VolumeProgress`, `ScannerProgress`, `WatcherProgress`, and `DrainProgress` structures. It also hosts the global static registry (`VolumeProgressTracker` and `get_volume_tracker`) so that the `scanner` crate (which does not depend on the `api` crate to prevent circular dependencies) and the `platform-windows` crate can directly update progress counters in-memory.
  - **Structural Drift:** Section 11.2 originally specified a single, flat progress snapshot at the root. The actual implementation instead structures progress per volume inside a `volumes: HashMap<String, VolumeProgress>` field to handle parallel monitoring across multiple drives (`C:`, `D:`, `E:`). The root-level `state` is dynamically computed as an aggregate: it remains `DaemonState::BaselineScanning` until all monitored volumes transition to `DaemonState::Live`.
- **Persisted Drain Cursor (`drain_state` table):** Section 5 originally described an in-memory cursor for the Drain Engine's replay. The implementation introduces a database table `drain_state (volume TEXT PRIMARY KEY, last_sequence INTEGER)` to persist the last-drained sequence per volume. This guarantees that if the daemon is stopped or crashes during operation, it can resume replaying the mutation log exactly where it left off, preventing data loss or redundant replays upon restart.

## Open Issues / Things to Check Next Session

- None.

## Session Log

_(one entry per session — append, don't overwrite)_

### 2026-07-08 — Master plan amendment (live status spec) — Model: Claude Sonnet 5
- What was done: No code written yet (Loop 1 not started). Amended `AI_MASTER_PLAN.md`
  per its own append-only/dated-amendment rules to add §11 "Live Progress Reporting
  (init & status)".
- What was verified on Windows (exact steps run + result): N/A — spec-only session, no
  code exists yet to verify.
- What's left / handed off: Loop 1 implementation.

### 2026-07-08 — Loop 2 Storage Schemas & Handoff — Model: Gemini 3.5 Flash
- What was done:
  - Added SQLite database support via the `rusqlite` crate (bundled feature).
  - Implemented database folder resolution (`%LOCALAPPDATA%/disktracker` on Windows, `~/.local/share/disktracker` on WSL/Linux) and table creation (facts and mutation_log in composite keys) in `crates/storage`.
  - Configured database Connections to enable WAL mode.
  - Wired database initialization to the background daemon `run_server` routine in `crates/api`.
  - Exposed the dynamic resolved database file path in the status response `ProgressSnapshot`.
- What was verified:
  - Verified local compilation and target cross-compilation successfully.
  - Verified running `init` on WSL creates `disktracker.db` under the local path, starts the daemon, and successfully enables WAL mode and creates tables.
  - Verified running `status` outputs the dynamic database path.
- What's left / handed off:
  - Verify SQLite database creation and WAL mode manually on native Windows.
  - Proceed to Loop 3 (Scanner + Watcher).

### 2026-07-08 — Loop 3 Scanner + Watcher & Handoff — Model: Gemini 3.5 Flash
- What was done:
  - Configured `platform-windows` dependency on `windows-sys` and added a private FFI `win32` module with standard Win32 signatures and constants to ensure target compilation is self-contained.
  - Implemented logical NTFS drive discovery, USN journal cursor retrieval, and a fully functional NTFS USN Journal Watcher (using `DeviceIoControl` with `FSCTL_READ_USN_JOURNAL`) in `platform-windows`.
  - Implemented a recursive directory crawler (Scanner) walk in `crates/scanner`.
  - Added robust WSL/Unix mock fallbacks for both the Scanner and Watcher.
  - Wired the daemon `run_server` routine to discover drives and launch Watcher tasks and Scanner threads in parallel per volume.
- What was verified:
  - Local compilation and target cross-compilation verified successfully.
  - Ran the daemon in the foreground on WSL. Verified that Scanner threads run concurrently and output directory crawls, and Watchers output concurrent mock change events.
- What's left / handed off:
  - Verify NTFS volume crawling and real-time USN journal logging manually on native Windows (running the daemon in the foreground).
  - Proceed to Loop 4 (Pipeline & Drain data integration).

### 2026-07-09 — Loop 3 & 4 Verification & Progress Observability — Model: Gemini 3.5 Flash (High)
- What was done:
  - Optimized Windows Scanner directory crawler using the Win32 `GetFileInformationByHandleEx` API (`FileIdBothDirectoryInfo` batch mode) to avoid the per-file handle-opening bottleneck.
  - Resolved `walk_dir_recursive` stack overflow by moving the 64KB aligned buffer from the stack to a heap-allocated `Vec<u8>`.
  - Fixed SQLite lock contention by implementing Invariant #8 (In-memory Progress Observability via atomic counters) in the `core-types` crate, completely eliminating heavy SQLite queries from the JSON-RPC `status` polling loop.
  - Cleaned up all target cross-compilation warnings.
- What was verified on Windows:
  - Manually run and verified by the user on native Windows: Scanner, Watcher, and Drain Engine are fully functional and run in parallel per volume.
  - Crawl completes in seconds instead of minutes under live progress tracking.
  - Verified system resources: extremely low footprint (~12.3% CPU and ~12.3 MB memory).
- What's left / handed off:
  - Start Loop 5 (Diagnostics: implement the `doctor` checks).

### 2026-07-09 — Loop 5 Diagnostics Verification & Handoff — Model: Gemini 3.5 Flash (High)
- What was done:
  - Implemented real diagnostics check helper functions for Admin/Root elevation and NTFS USN Cursor readability in the `platform-windows` crate.
  - Refactored `run_doctor` in `apps/cli/src/main.rs` to run real checks for Process Elevation, AppData folder permissions, IPC Named Pipe connectivity, SQLite database integrity, and NTFS volume USN accessibility.
  - Implemented automatic SQLite `drain_state` entry initialization for discovered volumes on Drain Engine startup to ensure all drives are always visible in database tables.
- What was verified on Windows:
  - Manually run and verified by the user on native Windows: `disktracker doctor` successfully runs and passes all 5 diagnostic checks when the daemon is active, and reports failures correctly if the daemon is stopped.
- What's left / handed off:
  - Start Loop 6 (Uninstall: stop daemon, delete named pipe, delete SQLite file and AppData folder).

### 2026-07-09 — Loop 6 Uninstall & Background CPU Optimization — Model: Gemini 3.5 Flash (High)
- What was done:
  - Added the CLI `uninstall` command to stop the background daemon and clean up database files.
  - Implemented `kill_process_by_pid` using native Win32 process APIs (with Unix `kill -9` fallback).
  - Prompts the user with `[y/N]` before executing database deletion to prevent data loss.
  - Optimized the Drain Engine background loop by opening and reusing a single SQLite connection instead of opening/closing a connection every 500ms, reducing idle CPU usage to ~0%.
- What was verified on Windows:
  - Manually run and verified by the user on native Windows: `disktracker uninstall` correctly terminates the background daemon, unregisters resources, and completely deletes all database files/folders.
  - Background daemon CPU usage verified to have dropped to baseline (~0%).
- What's left / handed off:
  - N/A (All master plan loops successfully completed and verified).