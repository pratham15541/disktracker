# DiskTracker — Epoch 2 Detailed Spec

> Referenced by `AI_MASTER_PLAN_EPOCH2.md` §§3–9. Section numbers here are kept in lockstep
> with the master plan's cross-references. `why`/`ask` are deliberately absent — different
> epoch, not discussed here.

## 0.1 Size Grammar

`<number>[.<number>]<unit>`, case-insensitive, no space, unit ∈ `B|KB|MB|GB|TB`, base-1024
(matching Windows Explorer / `Get-ChildItem` conventions, not SI decimal). A bare integer
with no unit is raw bytes. No other bare-number form is accepted — this is deliberate per
`AI_MASTER_PLAN_EPOCH2.md` §1 principle 7 (ambiguity resolved by the tool, not guessed at).

Examples: `100MB`, `1.5GB`, `500KB`, `4096` (bytes).

**Note on future-proofing:** `facts.size` is indexed as the raw exact byte count (`u64`).
This grammar is a CLI-input parsing step only, converting `"100MB"` to a byte threshold at
query time — changing the grammar's base later (1024 vs. 1000) is a one-line parser change,
not an index rebuild.

## 0.2 Duration / Date Grammar

Relative duration: `<integer><unit>`, unit ∈ `h|d|w|mo|y` (hours, days, weeks, 30-day
months, 365-day years — `mo` not `m`, deliberately, to avoid a minutes/months clash).
Absolute: ISO-8601 date (`2026-07-01`) or datetime (`2026-07-01T00:00:00`).

- `--since` (`history`, `top`): accepts **either** relative duration or absolute date/datetime.
- `--modified-after` / `--modified-before` / `--until`: accept **absolute date/datetime
  only** — an end-bound expressed as a relative duration is ambiguous (relative to now, or
  to `--since`?) and is rejected with `E_INVALID_PARAMS` rather than guessed at.

Examples: `7d`, `24h`, `2w`, `2026-06-01`.

## 0.3 Cursor Format

Opaque, base64-encoded wrapper around the last-seen ULID of the page. Because ULIDs sort
lexicographically by creation time (invariant #10), pagination is `WHERE id > cursor_id
ORDER BY id LIMIT n` — no offset drift as `facts`/`mutation_log` mutate underneath a
paginated query in flight. Treat the encoding as opaque from the CLI/consumer side.

---

## 3. Mutation Log Retention & Config — Loop 7

**Retention algorithm:** see `AI_MASTER_PLAN_EPOCH2.md` §3 for the full sequence (03:00
local trigger, per-volume mid-replay skip, `facts` untouched, every run logged).

### 3.1 `config` — Exact CLI and Storage

```
disktracker config get retention-days
disktracker config set retention-days <N>
```

| Parameter | Type | Required | Description |
|---|---|---|---|
| `key` | enum (`retention-days` only) | Yes | Config key |
| `value` (set only) | int | Yes (for `set`) | New retention window, in days, `1..=3650` |

**Storage:** `config.toml` at the AppData path already resolved for the DB in Epoch 1 Loop
2. Backed by the `config` crate:

```toml
[dependencies]
config = { version = "0.15", default-features = false, features = ["toml"] }
```

`default-features = false` + `features = ["toml"]` drops JSON/YAML/INI/RON support this
doesn't need.

**Examples:**
```
disktracker config get retention-days
disktracker config set retention-days 45
```

**On `set`, the CLI prints a note about reload timing** rather than leaving the user to
wonder why nothing changed immediately:
```
Retention window updated to 45 days. This takes effect on the next scheduled cleanup
(runs daily at 03:00) — not retroactively.
```

**Errors:** `E_INVALID_PARAMS` — unknown key, non-integer value, or out of `1..=3650`; human
text states the actual bound, e.g. `Retention must be between 1 and 3650 days.`

---

## 4. `search`

Purpose: find files/directories matching criteria over `facts`.

**CLI:** `disktracker search <query>`

| Parameter | Type | Required | Description |
|---|---|---|---|
| `query` | string | Yes | Search text |
| `--path` | path | No | Restrict search to a folder |
| `--ext` | string | No | File extension |
| `--volume` | string | No | `C:`, `D:`, etc. |
| `--min-size` | size (§0.1) | No | Minimum size |
| `--max-size` | size (§0.1) | No | Maximum size |
| `--modified-after` | date (§0.2) | No | Modified after |
| `--modified-before` | date (§0.2) | No | Modified before |
| `--hidden` | bool | No | Include hidden files (default: false) |
| `--system` | bool | No | Include system files (default: false) |
| `--limit` | int | No | Number of results (default: 50) |
| `--cursor` | string | No | Pagination (§0.3) |
| `--json` | flag | No | Raw JSON output |
| `--verbose` | flag | No | Extra columns |

**Examples:**
```
disktracker search invoice
disktracker search report --ext pdf
disktracker search cache --path C:\Users
disktracker search chrome --min-size 100MB
```

**First-run index build (see master plan §4.1) — exact console behavior:**
```
Building search index: 42,300 files indexed...
Building search index: 118,900 files indexed...
Search index ready (312,204 files).
```
Throttled the same way Epoch 1 §11.2 throttles scan progress — a ticker, not per-file prints.

**Mid-scan warning — exact output when a queried volume isn't `Live` yet:**
```
⚠ C: is still building its initial index — results from this volume may be incomplete.

[results table follows as normal]
```
`--json` adds `"volumes_incomplete": ["C:"]` to the result object instead of the prose line.

**Errors:** `E_INVALID_PARAMS` (malformed size/date), `E_SEARCH_INDEX_STALE`.

### 4.2 — `facts` schema migration

```sql
ALTER TABLE facts ADD COLUMN attributes INTEGER NOT NULL DEFAULT 0;
```

Populated by the Scanner/Watcher at write time going forward (bitmask: hidden, system,
readonly, reparse point — reuse the existing Win32 attribute constants, don't invent a new
encoding). Existing rows read as `0` ("unknown") until next touched — the CLI help text for
`--hidden`/`--system` states this plainly:
```
--hidden <true|false>   Filter by hidden attribute. Note: files not modified since the
                        Epoch 2 upgrade may not have this recorded yet.
```

Indexed by Tantivy alongside name, derived path, extension, volume, size, modified date, and
directory flag.

---

## 5. `history`

Purpose: show everything that happened to one file/folder.

**CLI:** `disktracker history <path>`

| Parameter | Type | Required | Description |
|---|---|---|---|
| `path` | path | Yes | File or folder |
| `--since` | duration/date (§0.2) | No | Only recent history |
| `--until` | date (§0.2) | No | End time |
| `--kind` | enum | No | `created`/`modified`/`deleted`/`renamed` |
| `--collapse` | flag | No | Collapse consecutive same-kind events (server-side) |
| `--limit` | int | No | Results (default: 50) |
| `--cursor` | string | No | Pagination (§0.3) |
| `--json` | flag | No | Raw JSON output |
| `--verbose` | flag | No | Extra columns |

**Examples:**
```
disktracker history notes.txt
disktracker history Downloads --since 7d
disktracker history project.zip --kind renamed
```

**Collapse algorithm (server-side, exact):** group consecutive rows (by `sequence`, on the
fetched page only, not globally) sharing the same `kind` with no other kind interrupting the
run, into `{kind, count, total_size_delta, earliest_at, latest_at}`.

**Truncation warning — exact output:**
```
⚠ History may be incomplete — DiskTracker keeps 30 days of detail, and this query goes
further back.

[results table follows]
```
`--json`: `"truncated": true` in the result object, no prose.

**Errors:** `E_NOT_FOUND` (path never seen), `E_INVALID_PARAMS`.

---

## 6. `snapshot`

### Create

**CLI:** `disktracker snapshot create [--label <name>]`

Async — returns a `job_id` (ULID) immediately, completion via `job.completed`. Debug
metadata (daemon/schema version, retention setting, facts count) captured at creation time,
informational only.

**Label uniqueness — exact check, before the async job is even queued:**
```rust
fn check_label_collision(label: &str, volume: &str, conn: &Connection) -> Option<SnapshotRow> {
    query_snapshot_by_label(label, volume, conn)
}
```
If a collision exists, refuse immediately (synchronously, not as a failed async job) with:
```
A snapshot named "before-update" already exists (created 2 hours ago, id snap_01HXYZ...).
Choose a different label, or omit --label to get an auto-generated one (e.g. "snap-0714").
```
No `--label` given: auto-generate one as `snap-<MMDD>` with a numeric suffix if that collides
too (`snap-0714-2`), so every snapshot always has a human-readable label, not just a ULID.

**Example:** `disktracker snapshot create --label before-update`

### List (new)

**CLI:** `disktracker snapshot list [--volume <v>] [--limit N] [--cursor C] [--json] [--verbose]`

| Default columns | `--verbose` adds |
|---|---|
| Label, Created (relative), Volume, Age | Snapshot ID, sequence_ref, daemon_version, schema_version, retention_days at creation, facts_count |

**Example output (default):**
```
LABEL           CREATED        VOLUME  AGE
before-update   2 hours ago    C:      2h
after-cleanup   10 minutes ago C:      10m
```

### Diff

**CLI:** `disktracker snapshot diff <a> <b>`

| Parameter | Type | Required | Description |
|---|---|---|---|
| `a` | snapshot ID or label | Yes | Start point |
| `b` | snapshot ID or label | Yes | End point |
| `--path` | path | No | Restrict diff to subtree |
| `--limit` | int | No | Results |
| `--json` / `--verbose` | flag | No | Same as above |

Accepts either a label or a full `snapshot_id` for `a`/`b` — resolves label → id internally
via the same lookup used for collision-checking above. If a label is ambiguous (shouldn't
happen given uniqueness enforcement, but defensively) or unknown, `E_NOT_FOUND` with the
literal input echoed back: `Couldn't find a snapshot named "before-updte". Did you mean
"before-update"?` (simple edit-distance suggestion, not fuzzy search — just a helpful nudge).

`snapshot.diff` replays `mutation_log` between the two sequence numbers through the Drain
Engine's own reducer — same code path, not a parallel implementation.

**Errors:** `E_NOT_FOUND` (unknown snapshot), `E_SNAPSHOT_DATA_EXPIRED` (range outside
retention window), `E_INVALID_PARAMS`.

---

## 7. `top`

Purpose: rank files/folders. Two modes.

### Mode A — current size (ungated, zero-flag default)

**CLI:** `disktracker top`

| Parameter | Description |
|---|---|
| `--path` | Search inside folder |
| `--volume` | Limit to volume |
| `--folders` | Folder-only rollup |
| `--files` | File-only, no rollup |
| `--limit` | Top N (default: 20) |
| `--json` / `--verbose` | Same conventions as above |

`--folders`/`--files` mutually exclusive; default (neither) is `--files` for Mode A.

**Example:** `disktracker top --folders`

### Mode B/C — growth over an interval (gated, invariant #11)

**CLI:** `disktracker top --since <duration>` or `disktracker top --between <a> <b>`

| Parameter | Description |
|---|---|
| `--since` | Relative duration (§0.2) — mutually exclusive with `--between` |
| `--between` | Two snapshot IDs/labels (resolved the same way as `snapshot diff`) |
| `--growth` | Rank by size delta (default) |
| `--churn` | Rank by modification count — mutually exclusive with `--growth` |
| `--folders` / `--files` | Default `--folders` for this mode |
| `--limit`, `--json`, `--verbose` | Same conventions |

**Examples:**
```
disktracker top --since 7d
disktracker top --between before-update after-cleanup
```


**Errors:** `E_INSUFFICIENT_HISTORY`, `E_NOT_FOUND` (unknown snapshot in `--between`),
`E_INVALID_PARAMS` (both `--since`/`--between`, or both `--growth`/`--churn`, given together).

---

## 8. Output Format Conventions — Per-Command Column Sets

| Command | Default columns | `--verbose` adds |
|---|---|---|
| `search` | Name, Path, Size, Modified, Type | Extension, Volume, Attributes, Exact Bytes, Fact ID |
| `history` | Timestamp (relative), Kind, Path, Size Δ | Full timestamp, Mutation ID, Source, Sequence #, Pre-change path (renames) |
| `top` (Mode A) | Rank, Name, Size, Type | Exact bytes, Volume, Full path, Item count (folders) |
| `top` (Mode B/C) | Rank, Name, Δ (size or churn), Type | Exact byte delta, Window/snapshot bounds, Item count |
| `snapshot list` | Label, Created, Volume, Age | Snapshot ID, sequence_ref, versions, facts_count |
| `snapshot diff` | Change (Added/Removed/Modified), Path, Size Δ | Full timestamp, Mutation ID(s), Exact byte delta |

`--json` always returns the full underlying RPC result regardless of `--verbose`.