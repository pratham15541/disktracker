# DiskTracker — AI Master Plan (Epoch 2)

> Companion to `AI_MASTER_PLAN.md` (Epoch 1). All Epoch 1 invariants, domain schemas
> (`Fact`, `Mutation`, `RawEvent`), and the append-only/dated-amendment rule for this file
> still apply. This version supersedes all prior Epoch 2 drafts — see §11 for what changed
> and why, including a correction to unverified content that appeared in a prior session.

## 0. Scope

Epoch 2 is **pure deterministic data retrieval** on top of what Epoch 1 already collects:
`search`, `history`, `snapshot`, `top`. Nothing in Epoch 2 reasons, correlates, or narrates —
it only returns facts about what's on disk and what happened to it. `why`, `ask`, app-registry
correlation, and any AI orchestration are a different epoch and are not discussed here at all.

**Still out of scope:** a GUI, remote/network access to the daemon, Linux/macOS support.

## 1. UX & Product Principles (read this before implementing anything)

DiskTracker's CLI is the entire product surface for Epoch 2 — there's no GUI to fall back on
for polish. Treat these as binding requirements, not aspirations:

1. **Hide the machinery, always.** Nothing in default output or error text ever names an
   internal engine (Tantivy, "the reducer," "the drain engine," "USN cursor"). A user sees
   files, folders, sizes, and dates — never the plumbing. This continues Epoch 1's iceberg
   model into every Epoch 2 surface.
2. **Sensible defaults, zero required flags for the common case.** `disktracker top` with no
   arguments must produce something immediately useful (current top-20 folders by size) —
   never an empty result or a request for more flags. Every command's zero-flag behavior is
   specified explicitly below, not left as "whatever falls out of the code."
3. **Errors are human sentences by default; machine codes are an opt-in, not the default.**
   `E_INSUFFICIENT_HISTORY` printed as JSON is correct for `--json`; printed as the *default*
   experience it's a bug. See §9 for the exact human-readable template per error code.
4. **Never go silent, never go quiet-wrong.** If a volume hasn't finished its Epoch 1
   baseline scan yet, `search`/`history`/`top` against it must say so visibly (§4.1) rather
   than return a result that looks complete but isn't. If a first-time search index build is
   in progress, say so with real running counts (§5.1) — the same "no fabricated
   percentages" rule from Epoch 1 §11.2 applies here too.
5. **Progressive disclosure, not information overload.** Default table output is scannable —
   a handful of columns, human units, relative timestamps. `--verbose` is where precision
   lives (exact bytes, full timestamps, IDs). `--json` is where machine-readability lives.
   Three tiers, each with an obvious reason to reach for it.
6. **Discoverability over cleverness.** Every stateful thing the user creates must be
   listable. `snapshot create` without `snapshot list` is a trap — added in §6.4. If a future
   command creates anything with an identity, it gets a `list` counterpart at the same time
   it's designed, not as an afterthought.
7. **Ambiguity is resolved by the tool, never by the user guessing.** Carried over from
   Epoch 2's original size/duration grammar work: no bare numbers, explicit units always,
   hard parse errors with a corrective suggestion rather than a silent best-guess.
8. **Destructive or identity-colliding actions get a clear, named refusal, not silent
   overwrite.** A duplicate snapshot label is refused at creation time with a specific
   suggestion (§6.3), not silently allowed to create ambiguity for later.

## 2. Architecture Invariants (in addition to Epoch 1's)

9. **List-shaped responses use cursor-based pagination, never offset-based.** `facts` and
   `mutation_log` mutate continuously; an offset drifts as soon as anything changes between
   requests. Every list response (`search`, `history`, `top`, `snapshot list`) takes
   `limit`/`cursor` and returns `next_cursor: Option<String>`.
10. **IDs are ULIDs**, not UUIDv4 or auto-increment ints, everywhere a cross-service
    reference is needed (`snapshot_id`, `job_id`). Sortable by creation time, which is what
    makes cursor pagination and replay-based diffing trivial.
11. **Growth-mode `top` is gated behind actual accumulated history** (§7) — a data-
    sufficiency check, not a correlation/reasoning gate, which is why it stays in Epoch 2.
12. **`mutation_log` has a retention policy** (§3) — not kept forever. Anything reading
    history must assume a rolling window, not the full lifetime of the install.
13. **Output format is defined once (§8) and applied identically across every command** —
    default table, `--json`, `--verbose` — never redecided per command.

## 3. Mutation Log Retention (Loop 7)

**Algorithm:**
1. A scheduled job runs once every 24 hours, triggered at **03:00 local time** (a firm
   default, not a placeholder — chosen as a low-activity window; not user-configurable in
   Epoch 2, only the retention *length* is, via §3.1).
2. For each known volume: if that volume's Drain Engine is currently mid-replay (per its
   in-memory phase state — reuse the existing `DaemonState`/`VolumeProgress` from Epoch 1
   §11, don't build a second state tracker), **skip** pruning for that volume this run —
   deferred to the next scheduled run, visible via `doctor`, never silently dropped.
3. Otherwise: `cutoff = now - retention_days`; `DELETE FROM mutation_log WHERE volume = ?
   AND at < cutoff`.
4. `facts` is never touched by this job.
5. Every run, including skips, is logged for observability.

### 3.1 Retention Config

`disktracker config get [retention|retention-days]` / `disktracker config set [retention|retention-days] <Value>` —
deliberately the *only* config key in Epoch 2, not a general config subsystem. Any other key
is `E_INVALID_PARAMS` with the valid key list in `details`. Both `retention` and `retention-days` are accepted as aliases.

- **Value Format**: Supports numeric values (defaulting to days) or duration strings with units:
  - `h` / `hour` / `hours`: Hours
  - `d` / `day` / `days`: Days
  - `m` / `month` / `months`: Months (treated as 30 days)
  - `y` / `year` / `years`: Years (treated as 365 days)
- **Bounds:** Minimum `1h` (1 hour) up to maximum `3650d` (10 years). Out of range → `E_INVALID_PARAMS` with
  a human message stating the actual bounds, not just "invalid value."
- **Storage:** a `config.toml` file at the same AppData path resolved for the DB. Backed by the `config` crate.
- **Reload:** the pruning job reads `retention-days` fresh at the start of each run — a
  `config set` takes effect on the *next* scheduled run, not immediately, and this is stated
  plainly in the command's own output so the user isn't left wondering why nothing changed
  right away.

## 4. Search (Loop 8)

Depends only on `facts` — no dependency on retention, safe to build immediately.

- **Engine:** Tantivy, indexing name, derived path, extension, volume, size, modified date,
  directory flag, and file attributes.
- **`facts` schema migration required:** add an `attributes INTEGER NOT NULL DEFAULT 0`
  column (bitmask: hidden/system/readonly/reparse point) — Epoch 1's `Fact` never stored
  this. Existing rows read as "unknown" (`0`) until next touched by a scan or mutation; this
  is stated plainly in `--hidden`/`--system` help text, not silently assumed accurate.
- **Index maintenance:** incremental, hooked into the Drain Engine's existing UPSERT/DELETE
  path — one Tantivy commit per `facts` write, no batching in Epoch 2.
- **Index versioning:** `index_meta.json` stores `schema_version`/`last_synced_sequence`. A
  mismatch (missing file, version bump, corruption) triggers an automatic full rebuild — the
  daemon does this without asking, since serving stale/incompatible results silently is
  worse than a one-time delay. `search.query` returns `E_SEARCH_INDEX_STALE` while a rebuild
  is in progress.
- **CLI:** `disktracker search <query> [--path <p>] [--ext <e>] [--volume <v>]
  [--min-size <size>] [--max-size <size>] [--modified-after <when>]
  [--modified-before <when>] [--hidden true|false] [--system true|false] [--limit N]
  [--cursor C] [--json] [--verbose]`. Full grammar and examples in detailed spec §3.

### 4.1 First-Run Index Build & Mid-Scan Warnings

Two distinct honesty requirements, both real per §1 principle 4:

- **First-time index build** (upgrading an existing Epoch 1 install, or a fresh install's
  first search): stream a live progress line — `Building search index: 42,300 files
  indexed...` — running counts only, no fabricated percentage, same rule as Epoch 1 §11.2.
- **Volume still baseline-scanning:** if any result's volume hasn't reached the `Live` phase
  (Epoch 1 §5), prepend a warning line to default output: `⚠ C: is still building its
  initial index — results from this volume may be incomplete.` This is informational, not
  an error — the command still returns whatever it has. `--json` includes this as a
  structured `"volumes_incomplete": ["C:"]` field rather than a prose line.

## 5. History (Loop 9)

Depends on `mutation_log` and the retention policy from §3.

- Resolves a user-given path to `(volume, file_id)` via the in-memory path cache, then
  queries mutations for that identity within an optional `[since, until]` window and an
  optional `kind` filter.
- **`--collapse` is server-side**, grouping consecutive same-kind entries on the fetched
  page — never a client-side formatting trick (keeps the CLI a dumb client, Epoch 1
  invariant #2 extended).
- **Truncation is a signal, not an error:** if the query's start falls outside the retention
  window, the response carries `"truncated": true`; the CLI renders a warning line above the
  table, not a failure.
- **CLI:** `disktracker history <path> [--since <when>] [--until <when>]
  [--kind created|modified|deleted|renamed] [--collapse] [--limit N] [--cursor C] [--json]
  [--verbose]`. Full grammar and examples in detailed spec §4.

## 6. Snapshots (Loop 10)

- **Table:** sequence bookmarks per volume, plus debug metadata (daemon/schema version,
  retention setting, facts count at creation time) — informational only, never consulted by
  diff logic.
- `snapshot.create` is async: returns a `job_id` immediately, completion via `job.completed`.
- `snapshot.diff` replays `mutation_log` between two sequence numbers through the Drain
  Engine's own reducer — same code path, not a parallel implementation — optionally
  restricted to a path prefix.

### 6.3 Label Uniqueness (new — real UX gap in the prior draft)

`--label` must be unique among all snapshots for that volume that don't already share it.
A collision at creation time is refused with a specific, actionable message:
`A snapshot named "before-update" already exists (created 2 hours ago, id snap_01H...).
Choose a different label, or omit --label to get an auto-generated one.` — never a silent
duplicate that makes `snapshot diff before-update after-update` ambiguous later.

### 6.4 `snapshot list` (new — the missing discoverability command)

Without this, `snapshot diff` is unusable in practice — there's no way to see what exists.
`disktracker snapshot list [--volume <v>] [--limit N] [--cursor C] [--json] [--verbose]`,
default columns: Label, Created, Volume, Age. `--verbose` adds Snapshot ID, sequence
reference, and the debug metadata (daemon/schema version, retention at creation, facts
count).

- **CLI (full command set):** `disktracker snapshot create [--label <name>]`,
  `disktracker snapshot list [...]`, `disktracker snapshot diff <a> <b> [--path <p>]
  [--limit N] [--json] [--verbose]`. Full detail in detailed spec §5.

## 7. Top (Loop 11)

Two modes, no narration, no correlation — purely ranking over existing data:

- **Mode A — current size:** ungated, available immediately, zero required flags.
  `disktracker top [--path <p>] [--volume <v>] [--folders | --files] [--limit N] [--json]
  [--verbose]`.
- **Mode B/C — growth over an interval:** gated behind accumulated history (invariant #11).
  `--since <duration>` or `--between <a> <b>` (mutually exclusive), ranked by `--growth`
  (size delta, default) or `--churn` (modification count, mutually exclusive with
  `--growth`), rolled up by folder by default (`--folders`/`--files` mutually exclusive,
  default `--folders` for growth mode, default `--files` for current-size mode — the two
  modes have different sensible defaults, intentionally).

Full duration/size grammar, RPC shape, and examples in detailed spec §6.

## 8. Output Format Conventions (applies to every command above)

- **Default:** a human-readable table, exact column set per command in detailed spec §7.
  Relative timestamps ("2 hours ago"), human-readable sizes ("14.2 GB"), never raw internal
  IDs unless `--verbose`.
- **`--json`:** raw pretty-printed RPC result, no table rendering. Combined with `--verbose`,
  `--verbose` has no additional effect — the JSON already contains everything.
- **`--verbose`:** adds columns — full timestamps, exact byte counts alongside human units,
  mutation/snapshot IDs, source (watcher/scanner/recovery). Exact set per command in
  detailed spec §7.
- **Errors, default mode:** a plain-language sentence (exact templates in §9), never a raw
  error code or JSON blob. **Errors, `--json` mode:** the structured `{code, message,
  details}` shape from Epoch 1's original error design.

## 9. Error Taxonomy — Machine Code and Human Template, Both Specified

| Code | Meaning | Default human-readable text |
|---|---|---|
| `E_NOT_FOUND` | referenced path/snapshot/job doesn't exist | `Couldn't find "{input}". Check the path/ID and try again.` |
| `E_INVALID_PARAMS` | malformed request params | `{specific reason, e.g. "Size must include a unit, like 100MB or 2GB."}` |
| `E_SNAPSHOT_DATA_EXPIRED` | diff range outside retention window | `This comparison goes further back than DiskTracker currently keeps ({retention_days} days). Try a more recent snapshot pair.` |
| `E_INSUFFICIENT_HISTORY` | growth-mode `top`/`history` gate not met | `DiskTracker needs a bit more history for this — you have {days_available} days, come back in {days_needed - days_available} more.` |
| `E_SEARCH_INDEX_STALE` | index rebuild in progress | `Still finishing an index update — try again in a moment.` |

`--json` mode returns `{code, message, details}` using the machine `code` column and a
structured `details` object (e.g. `{"days_needed": 7, "days_available": 2}`) instead of the
pre-formatted human string.

## 10. Loop-by-Loop Build Plan

### Loop 7 — Mutation Log Retention & Pruning + Config
Implement the nightly pruning job (03:00 local, configurable length only), the mid-replay
skip-and-log behavior, and `disktracker config get/set` scoped to `retention-days`.
**Verify:** set the window very low for testing, confirm old rows disappear from
`mutation_log` on schedule and `facts` is unaffected; confirm a run is skipped (and logged,
visible via `doctor`) if a volume is mid-replay; confirm `config set retention-days <N>`
persists across a daemon restart and is picked up by the *next* scheduled run, not
immediately; confirm any other config key returns `E_INVALID_PARAMS` with the human message
listing the one valid key.

### Loop 8 — Search
Implement Tantivy indexing hooked into the Drain Engine's UPSERT/DELETE path, the
`facts.attributes` migration, index versioning/auto-rebuild, the first-run build message and
mid-scan warning (§4.1), and the CLI command.
**Verify:** create/rename/delete real files, confirm results update within a couple seconds
with no manual re-index; confirm every filter flag narrows results correctly; confirm the
first-run index build shows live running counts (not a fabricated percentage); confirm
querying a volume still in baseline-scan shows the `⚠` warning and still returns partial
results rather than erroring; confirm cursor pagination and `--json`/`--verbose` as
specified.

### Loop 9 — History
Implement path→identity resolution, `history.get` with `since`/`until`/`kind` filters and
server-side `--collapse`, and the CLI command.
**Verify:** make real edits/renames/deletes to a test file, confirm correct filtering by
`--kind` and bounding by `--since`/`--until`; confirm `--collapse` groups consecutive
same-kind entries without merging across an interrupting different kind; confirm a query
older than retention shows the truncation warning, not silent wrongness.

### Loop 10 — Snapshots
Implement the `snapshots` table (with debug metadata), `snapshot.create`, `snapshot.list`,
`snapshot.diff` with optional `--path` restriction, and label-uniqueness enforcement.
**Verify:** create two snapshots with real file changes in between, diff them, confirm
results match reality; confirm `snapshot list` shows both with correct metadata; confirm
creating a third snapshot with a duplicate label is refused with the specific suggested-fix
message, not silently allowed; confirm `--path` restricts the diff correctly; confirm an
expired-range diff returns the human `E_SNAPSHOT_DATA_EXPIRED` message by default and the
structured code under `--json`.

### Loop 11 — Top
Implement current-size ranking (Mode A, ungated, zero-flag default) and growth ranking
(Mode B/C, gated), `--folders`/`--files` rollup, `--growth`/`--churn` selection.
**Verify:** confirm bare `disktracker top` works immediately with no flags on a fresh
install; confirm growth mode shows the human "needs more history" message before the gate
and real numbers after; confirm `--folders` aggregates correctly versus `--files` raw
entries; confirm `--between` against two real snapshots matches the equivalent `snapshot
diff` output for the same range.

### Loop 12 — Windows Service Auto-Start & Management
Implement auto-start on system restart or shutdown by running the background daemon as a Windows Service.
- By default, `disktracker init` registers and starts the daemon as a Windows Service (`DiskTracker`).
- If not running on Windows or if service registration fails, fall back to spawning the detached daemon process.
- Implement explicit service management subcommands:
  - `disktracker service register`: Registers `disktracker` executable to run as a Windows Service.
  - `disktracker service unregister`: Stops and deletes the Windows Service.
  - `disktracker service start`: Starts the registered service.
  - `disktracker service stop`: Stops the registered service.
- The daemon binary (`disktracker daemon --service`) must integrate with the Windows Service Control Manager (SCM) to handle stop/shutdown events and gracefully terminate.
- Update `disktracker uninstall` to stop and unregister the service.
**Verify:** run `init` as Administrator, confirm `DiskTracker` service is registered and running; verify daemon shuts down gracefully on service stop/shutdown; verify `uninstall` cleans up the service.

---

## 11. Amendments

_(dated entries only — do not delete or rewrite earlier entries, add new ones below them)_

- **2026-07-09:** Initial Epoch 2 plan created following Epoch 1 sign-off; later redesigned
  `top` to cover both current-size and growth ranking; cut `why`/`ask`/App Registry
  Correlation/Relationship Engine/AI tool-calling to a separate epoch entirely, since all of
  them exist only to correlate or narrate data — a categorically different kind of work from
  Epoch 2's pure retrieval scope.
- **2026-07-10:** A prior session introduced content that was checked against the actual
  repository and found to be fabricated: a claimed `async-openai` workspace dependency, and
  a crate layout (`pal`, `pal-windows`, `pal-linux`, `daemon`) that doesn't match this
  project's real structure (`platform/windows`, no Linux crate — Linux support has been out
  of scope since Epoch 1 §0). That fabricated dependency claim was also used to argue against
  an orchestration-runtime decision you'd already made explicitly (LangGraph for a future
  `why`/`ask`) — corrected by discarding that content rather than treating it as settled;
  the orchestration question itself is out of scope for this document.
- **2026-07-10 (this entry):** Full rewrite with UX/product principles promoted to a
  first-class section (§1), incorporating real gaps found on review: `snapshot list` (there
  was no way to discover existing snapshots), label-uniqueness enforcement, human-readable
  error text as the default (machine codes moved behind `--json`), explicit handling for a
  volume still baseline-scanning and for a first-run search index build, and a firm (not
  placeholder) retention job trigger time. All prior genuinely-verified design (size/duration
  grammar, cursor format, per-command flag tables, `config get/set` scoped to
  `retention-days`) is retained — see `EPOCH2_DETAILED_SPEC.md`.
- **2026-07-11 (this entry):** Added Loop 12 to support Windows Service integration for auto-start, SCM command handling, and CLI service management controls (`register`/`unregister`/`start`/`stop`).