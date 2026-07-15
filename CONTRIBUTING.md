# Contributing to DiskTracker

Thank you for your interest in contributing to **DiskTracker** — a high-performance, real-time Windows file-system change tracking utility written in Rust.

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Project Overview](#project-overview)
- [Getting Started](#getting-started)
  - [Prerequisites](#prerequisites)
  - [Cloning & Building](#cloning--building)
- [Workspace Structure](#workspace-structure)
- [Development Workflow](#development-workflow)
  - [Branching Strategy](#branching-strategy)
  - [Making Changes](#making-changes)
  - [Commit Style](#commit-style)
- [Running Tests](#running-tests)
- [Architecture Primer](#architecture-primer)
- [Submitting a Pull Request](#submitting-a-pull-request)
- [Reporting Issues](#reporting-issues)
- [License](#license)

---

## Code of Conduct

Be respectful, constructive, and welcoming. Harassment or discrimination of any kind will not be tolerated. When in doubt, be kind.

---

## Project Overview

DiskTracker tracks every file-system change on a Windows volume in real time using the **NTFS USN Journal**. It uses a **Log-Structured State Machine** model to decouple:

- A **fast append-only write path** (raw USN events → `mutation_log` SQLite table)
- A **slow state-reduction path** (Drain Engine → `facts` table)

This design avoids database contention and allows zero-overhead event capture even under heavy I/O.

The project targets **Windows (x86_64)** as its primary platform. Linux/WSL builds compile but use mock implementations for platform-specific code (useful for CI and development).

---

## Getting Started

### Prerequisites

| Tool | Version | Notes |
|------|---------|-------|
| [Rust](https://rustup.rs/) | stable (latest) | Install via `rustup` |
| `x86_64-pc-windows-gnu` target | — | For cross-compiling on Linux |
| `x86_64-pc-windows-msvc` target | — | Required for release builds |
| Git | any | — |

On Linux/WSL (development only):

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt-get install gcc-mingw-w64
```

On Windows (native / CI):

```sh
rustup target add x86_64-pc-windows-msvc
```

### Cloning & Building

```sh
git clone https://github.com/pratham15541/disktracker.git
cd disktracker

# Native Linux/WSL build (mock platform layer — compiles only, no Windows APIs)
cargo build

# Cross-compile for Windows from Linux
cargo build --target x86_64-pc-windows-gnu

# Release build on Windows (used in CI)
cargo build --release -p disktracker-cli --bin disktracker --target x86_64-pc-windows-msvc
```

> **Note:** Always verify the build compiles before starting development. If `PROGRESS.md` claims a feature is complete, cross-check it against the actual crate source — never trust docs blindly.

---

## Workspace Structure

This is a Cargo workspace. Each crate has a single, well-defined responsibility:

```
disktracker/
├── apps/
│   └── cli/                  # CLI frontend: init, status, doctor, uninstall + daemon launcher
└── crates/
    ├── core/                 # Shared types: Fact, Mutation, RawEvent, progress registry
    ├── storage/              # SQLite connection pool, WAL PRAGMAs, table schemas
    ├── scanner/              # Directory walker (Win32 batch API on Windows, mock on Linux)
    ├── pipeline/             # Coordination pipeline between scanner, watcher, and drain
    ├── api/                  # Named pipe IPC server, JSON-RPC handler, Drain Engine
    ├── agent/                # AI agent integration layer
    ├── telemetry/            # Progress tracking and telemetry emission
    ├── config/               # Configuration loading and management
    ├── common/               # Shared utilities across crates
    ├── platform/
    │   ├── traits/           # Platform abstraction interfaces (IpcListener, etc.)
    │   └── windows/          # Win32 bindings: USN journal, OpenFileById, elevation checks
    └── rust-langgraph/       # Vendored LangGraph runtime (patched via [patch.crates-io])
```

### Key invariants

- **No circular crate dependencies.** `core` and `common` are leaf crates — they must never depend on other workspace crates.
- **Platform code is isolated.** Only `crates/platform/windows` should contain `#[cfg(target_os = "windows")]` Win32 API calls. All other crates use the `traits` abstraction.
- **`apps/cli` is the only binary.** All library logic belongs in `crates/`.

---

## Development Workflow

### Branching Strategy

Work is organized into **numbered loops** (see `docs/AI_MASTER_PLAN.md`). Each loop targets a self-contained feature or subsystem. Follow this pattern:

```sh
# Create a branch for your work
git checkout -b loop-N-short-description

# Work, test, iterate...

# Merge only after Windows manual verification
git checkout main
git merge loop-N-short-description
git tag loop-N-verified
git push --tags
```

> **Never commit directly to `main`.** PRs must be merged only after manual Windows testing has been confirmed.

If an agent or automated change makes a mess, roll back cleanly:

```sh
git reset --hard loop-N-verified
```

### Making Changes

1. **Read the architecture first.** Read `docs/ARCHITECTURE.md` and the relevant crate's `Cargo.toml` / source before writing code.
2. **Keep crate boundaries clean.** If your change introduces a new dependency between crates, justify it.
3. **Platform stubs.** If adding a new Win32 call, add a stub/mock implementation behind a `#[cfg(not(target_os = "windows"))]` guard so Linux builds continue to compile.
4. **No agent commits to main.** AI-assisted code must be reviewed and manually tested on native Windows before merging.

### Commit Style

Use conventional commit prefixes:

```
feat: add USN journal replay on startup
fix: drain engine skipping deleted directory entries
refactor: extract OpenFileById into platform-windows crate
docs: update ARCHITECTURE.md with drain state table schema
chore: bump rust-langgraph patch version
```

Keep commits atomic — one logical change per commit.

---

## Running Tests

```sh
# Run all tests (Linux-compatible; platform tests are stubbed)
cargo test

# Run tests for a specific crate
cargo test -p disktracker-core

# Run with output visible
cargo test -- --nocapture
```

> Windows-specific integration tests (USN journal, Named Pipe IPC) must be run on a native Windows machine or Windows CI runner. They will be skipped automatically on Linux.

---

## Architecture Primer

DiskTracker's three core components per volume:

| Component | Role | Write Target |
|-----------|------|-------------|
| **Watcher** | Streams NTFS USN journal events in real time | `mutation_log` (append-only) |
| **Scanner** | Crawls the volume at startup for a baseline | `facts` (bulk insert) |
| **Drain Engine** | Polls `mutation_log`, resolves file sizes, merges into `facts` | `facts` (UPSERT/DELETE) |

**Lifecycle gating:** The Drain Engine is blocked in `BaselineScanning` state until the Scanner completes, then replays the overlap window in `Reconciling` state, and finally enters `Live` (polling every 500ms).

SQLite is configured with **WAL mode** for lock-free concurrent reads and writes.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full design with diagrams and schema definitions.

---

## Submitting a Pull Request

1. **Fork** the repository and create your branch from `main`.
2. **Build and test** your changes locally:
   ```sh
   cargo build
   cargo test
   ```
3. **Cross-compile** to verify Windows compatibility:
   ```sh
   cargo build --target x86_64-pc-windows-gnu
   ```
4. **Open a PR** against `main` with a clear description of:
   - What problem this solves or what feature it adds
   - Which loop or area of the architecture it touches
   - Whether Windows manual testing was performed (and the result)
5. **Link any related issues** in the PR description.

PRs that break the Linux cross-compilation build or introduce circular crate dependencies will be rejected.

---

## Reporting Issues

Use [GitHub Issues](https://github.com/pratham15541/disktracker/issues) to report bugs or request features.

When reporting a bug, please include:

- **OS and version** (Windows edition, build number)
- **DiskTracker version or commit SHA**
- **Steps to reproduce**
- **Expected vs actual behavior**
- **Relevant logs** (run with `RUST_LOG=debug disktracker` and attach the output)

---

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).

---

*Maintainer: [Pratham Parikh](https://github.com/pratham15541)*
