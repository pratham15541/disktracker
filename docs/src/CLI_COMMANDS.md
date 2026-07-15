# 🚀 DiskTracker CLI Subcommand Reference Card

This reference card details every subcommand, parameter, flag, and usage example in DiskTracker.

---

## 1. Core Daemon Lifecycle Management

### `disktracker init`
Starts or registers the DiskTracker background daemon as a Windows Service and initiates baseline scanning.
* **Usage**: `disktracker init`
* **Admin privileges required**: Yes.

### `disktracker status`
Queries the running background daemon's state and monitored volumes.
* **Usage**: `disktracker status`
* **Parameters**: 
  * `--json`: Returns raw JSON (useful for tooling/scripts) instead of human-readable text.

### `disktracker doctor`
Runs health diagnostics on the local installation, including checks for Admin permissions, NTFS Journal access, SQLite integrity, and database WAL-mode config.
* **Usage**: `disktracker doctor`
* **Admin privileges required**: Yes.

### `disktracker uninstall`
Stops the running daemon, terminates the service registry, removes the named pipe, and cleans up database files.
* **Usage**: `disktracker uninstall [flags]`
* **Admin privileges required**: Yes.
* **Flags**:
  * `--delete-snapshot`: Deletes all historical snapshot index files as well (default keeping snapshots).
  * `-y`, `--yes`: Auto-approves the confirmation prompt.

---

## 2. Windows Service Control

### `disktracker service <subcommand>`
Direct Win32 service wrapper operations.
* **Admin privileges required**: Yes.
* **Subcommands**:
  * `register`: Registers DiskTracker as a Windows Service named `DiskTracker`.
  * `unregister`: Stops and unregisters the `DiskTracker` service.
  * `start`: Starts the registered Windows Service.
  * `stop`: Stops the registered Windows Service.

---

## 3. Database Search and Statistics

### `disktracker search [query]`
Performs ultra-fast substring searches on indexed filenames. By default, it runs an exact case-insensitive substring search.
* **Usage**: `disktracker search "log" [options]`
* **Options**:
  * `--advanced` (alias `--advance`): Enable multi-tier scoring (phrase, prefix, ngram, fuzzy matching).
  * `--fuzzy`: Enable fuzzy matching scoring tier.
  * `--path <path>`: Filter files by folder path prefix (e.g. `C:\Users`).
  * `--ext <extension>`: Filter by exact file extension (e.g. `pdf`).
  * `--volume <volume>`: Filter by volume designation (e.g. `C:`).
  * `--min-size <bytes>`: Filter files larger than `N` bytes.
  * `--max-size <bytes>`: Filter files smaller than `N` bytes.
  * `--modified-after <duration/datetime>`: Filter files modified after a relative duration (e.g., `2d`, `12h`) or absolute UTC datetime (RFC3339).
  * `--modified-before <duration/datetime>`: Filter files modified before a relative duration or UTC datetime.
  * `--hidden`: Filter hidden files.
  * `--system`: Filter system files.
  * `--limit <limit>`: Maximum results to return (default: `100`).
  * `--json`: Outputs raw JSON results.

### `disktracker history [path]`
Displays the mutation timeline of a folder or file.
* **Usage**: `disktracker history [path] [options]`
* **Options**:
  * `--since <duration/datetime>`: Filter events after a relative time (e.g., `2d`, `12h`) or UTC datetime.
  * `--until <duration/datetime>`: Filter events before a relative time or UTC datetime.
  * `--kind <Created/Modified/Deleted/Renamed>`: Filter by change action.
  * `--collapse`: Combines consecutive entries of the same change action.
  * `--limit <limit>`: Max entries to return (default: `100`).
  * `--json`: Outputs raw JSON results.

### `disktracker top`
Ranks files and directories by size, growth, or modifications.
* **Usage**: `disktracker top [options]`
* **Options**:
  * `--path <path>`: Restricts ranking to a specific folder path.
  * `--volume <volume>`: Restricts ranking to a specific volume (e.g. `C:`).
  * `--folders`: Group results by folder rollup sizes (conflicts with `--files`).
  * `--files`: Flat listing of largest files (conflicts with `--folders`).
  * `--since <duration/datetime>`: Filter growth or churn since a relative duration.
  * `--between <SNAP_A> <SNAP_B>`: Compare growth/churn delta between two snapshots.
  * `--growth`: Rank results by size delta (default for interval mode).
  * `--churn`: Rank results by modification frequency.
  * `--limit <limit>`: Max items to display (default: `20`).

---

## 4. Snapshot Management

### `disktracker snapshot <subcommand>`
Manage and inspect volume snapshots.
* **Subcommands**:
  * `create [--label <name>] [path]`: Takes a snapshot index of the specified volume or folder with a custom label (auto-generated if omitted).
  * `list [--volume <vol>]`: Lists all saved snapshots in the system.
  * `diff <snapshot_a> <snapshot_b> [--path <prefix>]`: Computes the net mutations and size differences between two snapshots.

---

## 5. Conversational AI Agent

### `disktracker ai <subcommand>`
Configure the AI model bindings.
* **Subcommands**:
  * `config [options]`:
    * `--base-url <url>`: Set the LLM endpoint base URL.
    * `--api-key <token>`: Commit the LLM API token to the Windows Credential Manager.
    * `--model <name>`: Define the chat model name (e.g., `gpt-4o`, `meta-llama/llama-3-70b`).
    * `--chat-session-store <true/false>`: Choose whether to save AI conversation histories.
    * `--websearch-provider <name>`: Configure web search provider (e.g. duckduckgo, google).
    * `--websearch-key <key>`: Commit the Web Search API key to Credential Manager.
  * `test`: Runs a structural ping handshake test with the configured LLM API.
  * `session <list/show>`: Manages saved conversation session history.

### `disktracker ask "<question>"`
Invokes the LangGraph agent to inspect the disk state using natural language.
* **Usage**: `disktracker ask "Which folders in my AppData grew the most yesterday?"`
* **Options**:
  * `-i`, `--interactive`: Enable interactive Action mode. Allows the AI agent to propose file system mutations (like deleting caches or temporary log files) under human-in-the-loop (HITL) approval gates.
  * `--session <id>`: Resume a previous conversation session.

---

## 6. Self-Update Utility

### `disktracker update`
Checks the GitHub repository for the latest release, downloads the appropriate target archive, stops any running background daemons/services, performs a zero-downtime binary swap, restarts the daemon, and cleans up old temporary binaries.
* **Usage**: `disktracker update`
* **Admin privileges required**: Yes (on Windows if installed in program files or registered as a Windows Service).
