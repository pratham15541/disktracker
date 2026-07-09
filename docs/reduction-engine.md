Viewed PROGRESS.md:115-118

Here is a detailed breakdown of how the **Reducer Engine** (referred to in the code as the **Drain Engine**) works, from polling to state reduction.

---

### 1. The Core Architecture: Log-Structured State
Instead of writing file system changes directly to the active files list (which could lead to locks and database corruption), DiskTracker uses a **log-structured state machine**:
1. **Write (Watcher):** The USN Watcher appends change events to `mutation_log` (very fast, append-only).
2. **Reduce (Drain Engine):** The Drain Engine periodically processes the log and merges the changes into the `facts` table.

---

### 2. Step-by-Step Flow of the Reducer

Every **500 milliseconds** for each volume, the Drain Engine performs the following steps:

#### Step A: Read the Last Cursor position
It queries the `drain_state` table to see the sequence number it processed last:
```sql
SELECT last_sequence FROM drain_state WHERE volume = 'C:';
```
*(If no row exists, it defaults to `0`)*.

#### Step B: Fetch the Pending Batch
It queries the next **500 raw changes** from `mutation_log` that occurred after the cursor:
```sql
SELECT sequence, file_id, parent_file_id, name, kind, is_directory, size_delta, at
FROM mutation_log
WHERE volume = 'C:' AND sequence > :last_sequence
ORDER BY sequence ASC
LIMIT 500;
```

#### Step C: Reduce inside a Database Transaction
If any mutations are found, it opens a **single database transaction** and processes each event in chronological order. The state reducer applies these rules based on the event `kind`:

##### Rule 1: `Created` or `Modified` events
It performs an **UPSERT** (Insert or Update on Conflict) on the `facts` table:
```sql
INSERT INTO facts (volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at)
VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(volume, file_id) DO UPDATE SET
    parent_file_id = excluded.parent_file_id,
    name = excluded.name,
    is_directory = excluded.is_directory,
    size = excluded.size,
    modified_at = excluded.modified_at;
```
* **Why it's a reduction:** If a file was created and then modified 3 times, the `facts` table only cares about the **latest** state of the file. The final upsert overwrites previous states, keeping the table clean and up-to-date.

##### Rule 2: `Deleted` events
It removes the file from the `facts` table entirely:
```sql
DELETE FROM facts WHERE volume = ? AND file_id = ?;
```
* **Why it's a reduction:** Even if the database previously recorded modifications to this file, the delete operation collapses all previous history down to "this file no longer exists".

##### Rule 3: `Renamed` events
It updates the name and location (parent directory) of the file:
```sql
UPDATE facts 
SET name = ?, parent_file_id = ? 
WHERE volume = ? AND file_id = ?;
```
* **Why it's a reduction:** This moves the file record to its new path location without deleting and recreating it, maintaining path relationships.

#### Step D: Save Cursor and Commit
At the end of the batch, the engine saves the highest sequence number processed in this batch back to the cursor table:
```sql
INSERT INTO drain_state (volume, last_sequence)
VALUES ('C:', :new_last_sequence)
ON CONFLICT(volume) DO UPDATE SET last_sequence = excluded.last_sequence;
```
It then calls **`tx.commit()`**. 

Because SQLite runs in **WAL (Write-Ahead Log) Mode**, the transaction write is extremely fast and lock-free, allowing the CLI status queries to run concurrently without blocking.