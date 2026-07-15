# 🔍 DiskTracker (NPM Wrapper)

Created by **[Pratham Parikh](https://github.com/pratham15541)**

[![GitHub stars](https://img.shields.io/github/stars/pratham15541/disktracker.svg?style=social)](https://github.com/pratham15541/disktracker)

[![Platform: Windows x64](https://img.shields.io/badge/platform-windows--x64-blue.svg)](https://github.com/pratham15541/disktracker)
[![NPM version](https://img.shields.io/npm/v/disktracker.svg)](https://www.npmjs.com/package/disktracker)

**DiskTracker** is an NPM wrapper that installs the high-performance, real-time Windows file-system observation daemon and AI-driven command-line tool.

---

## ⚡ Installation

Install DiskTracker globally through `npm`:

```bash
npm install -g disktracker
```

> [!NOTE]
> **Compatibility**: DiskTracker is strictly supported on **Windows x64** platforms. Installation on Linux or macOS is blocked to prevent incompatibility.
> The NPM package runs a lightweight `postinstall` script to download the version-matched precompiled native Windows binary from GitHub Releases and sets up a global symlink.

---

## 🚀 Commands Quick Reference

Once installed, you can use the `disktracker` command directly in your command line:

```bash
# Initialize and crawl C: drive (starts daemon service)
disktracker init

# Check background tracking status
disktracker status

# Ask AI questions about disk usage
disktracker ask "Find all log files in C:\Temp larger than 50MB and summarize them"

# Search for files (substring search by default; use --advanced for fuzzy search)
disktracker search "log" --min-size 10485760

# View file mutation history (Created, Deleted, Modified)
disktracker history C:\Projects

# Stop and uninstall the service
disktracker uninstall
```

---

## 💡 Why use DiskTracker?

- **Real-Time Log Decoupling**: Instantly tracks file system operations by streaming NTFS USN Journals, writing them to a local SQLite WAL-mode log, and merging them in the background.
- **AI-Driven LangGraph Agent**: By configuring your OpenAI/OpenRouter API key, you can talk to your disk in natural language (`disktracker ask`), allowing the LLM to inspect files, query sizes, and perform safe, human-approved cleanup actions.
- **Developer Friendly**: Easily execute disk analysis scripts, track file updates, and integrate diagnostics directly into your Node projects.

---

## 🎯 Next Goals

We plan to expand DiskTracker along these key vectors:
1. **Testing Suite Expansion**: Build full workspace integration and Mock USN tests.
2. **Cross-Platform Support**: Develop macOS `FSEvents` and Linux `inotify` platforms.
3. **GUI Desktop Client**: Build an Electron/Tauri frontend visualizer for real-time charting.
