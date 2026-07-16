# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 2.x     | :white_check_mark: |
| < 2.0   | :x:                |

Please report issues against the latest release on [GitHub Releases](https://github.com/pratham15541/disktracker/releases).

## Reporting a Vulnerability

**Do not report security vulnerabilities through public GitHub issues, discussions, or pull requests.**

### Preferred: GitHub Private Vulnerability Reporting

1. Open the [Security advisories](https://github.com/pratham15541/disktracker/security/advisories) page.
2. Click **Report a vulnerability**.
3. Fill in the form with as much detail as possible.

This creates a private discussion visible only to maintainers.

### Alternative

If private reporting is unavailable, contact the maintainer via GitHub: [@pratham15541](https://github.com/pratham15541) (use a private channel such as email listed on the profile, if available). Mark the subject clearly as `[SECURITY]`.

### What to include

- Affected version, tag, or commit SHA
- Description of the issue and why it is security-sensitive
- Steps to reproduce or a minimal proof of concept
- Potential impact (e.g. privilege escalation, data loss, local IPC abuse, secret exposure)
- Suggested mitigations or fixes, if known

### Response expectations

- **Acknowledgment:** within 3 business days
- **Status update:** within 7 days of acknowledgment
- **Fix / disclosure:** coordinated with the reporter; typically within 90 days depending on severity

Valid reports may be credited in release notes and advisories unless you prefer anonymity. Verified issues may receive a GitHub Security Advisory (and CVE when appropriate).

## Scope

In-scope examples for DiskTracker include:

- Privilege escalation or unintended elevation / UAC bypass
- Abuse of the named-pipe JSON-RPC IPC surface (`\\.\pipe\disktracker`)
- Path traversal, unauthorized file deletion, or unsafe AI-agent tool execution bypassing human-in-the-loop (HITL) approval
- Exposure or insecure storage of API keys / credentials (e.g. Credential Manager mishandling)
- Tampering with the SQLite index / mutation log that leads to incorrect destructive actions
- Supply-chain issues in official installers (GitHub Releases, npm, Chocolatey, WinGet) or the self-update path

Out of scope (unless they demonstrate a concrete DiskTracker vulnerability):

- Social engineering or phishing
- Denial of service that requires local Administrator access already
- Issues solely in third-party LLM providers (OpenAI, OpenRouter, etc.)
- Reports against unsupported / outdated versions without a path to the current release

## Security notes for users

- DiskTracker’s daemon and USN Journal access typically require **Administrator** privileges on Windows. Run only from trusted machines and accounts.
- The AI assistant (`disktracker ask`) can propose mutating actions (e.g. deletes). **Never approve** HITL prompts you do not understand.
- Store LLM API keys via `disktracker ai config` rather than committing them to config files or shell history.
- Prefer official distribution channels and verify release checksums when published.
