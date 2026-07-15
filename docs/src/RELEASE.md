# DiskTracker Release and Publishing Guide

This document outlines the step-by-step release workflow for DiskTracker on Windows x64.

---

## 1. WinGet Versioning Explanation

**Where are the WinGet files?**
There are no local WinGet files in the repository.

**How does WinGet versioning work?**
WinGet publishing is handled automatically by the `.github/workflows/release.yml` workflow using the [WinGet Releaser](https://github.com/vedantmgoyal9/winget-releaser) GitHub Action.
When you push a tag (e.g. `v2.0.0`), the action:
1. Detects the new release tag name.
2. Automatically generates the WinGet YAML manifests dynamically.
3. Automatically opens a Pull Request on the official Microsoft community repository ([microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)) to register or update version `pratham15541.disktracker` using the newly compiled GitHub release ZIP archive.

You do **not** need to maintain any local manifests for WinGet.

---

## 2. Step-by-Step Release Flow

Follow these steps to release a new version of DiskTracker.

### Step A: Run Cargo Release Locally
To release a version bump (e.g., `patch`, `minor`, or `major`), run the `cargo release` command.
For example, to release a patch version:
```bash
cargo release patch --execute
```
*(You will need `cargo-release` installed: `cargo install cargo-release`)*.

**What happens behind the scenes?**
1. `cargo-release` bumps the version of all 13 workspace crates in their respective `Cargo.toml` files.
2. `cargo-release` executes the `pre-release-hook` defined in the root `Cargo.toml`:
   ```bash
   node scripts/update-versions.js
   ```
3. The script `update-versions.js` automatically reads the newly bumped version from `apps/cli/Cargo.toml` and writes it to:
   - `npm/package.json` (`version` property)
   - `chocolatey/disktracker.nuspec` (`<version>` tag)

### Step B: Commit and Push to GitHub
After running `cargo release`, verify your git status, commit the updated manifests, and push the branch and the new release tag.
```bash
# Add updated manifest files
git add Cargo.toml Cargo.lock apps/cli/Cargo.toml crates/*/Cargo.toml npm/package.json chocolatey/disktracker.nuspec

# Commit
git commit -m "chore: bump version to 2.0.0"

# Push main branch
git push origin main

# Push the tag (which triggers the Release workflow)
git push origin v2.0.0
```

### Step C: Automated GitHub Actions Actions
Once you push the `v*` tag, the `.github/workflows/release.yml` workflow starts:

1. **Build Job**: Builds the Windows static-CRT binary (`disktracker.exe`) for target `x86_64-pc-windows-msvc`, packs it into `disktracker-v2.0.0-windows-x64.zip`, and uploads it.
2. **Release Job**: Downloads the ZIP artifact and creates a GitHub Release under the pushed tag, uploading the ZIP asset.
3. **Publish NPM Job**: Checkouts the repository, verifies if version `2.0.0` is already on NPM. If not, it publishes the `npm` folder to NPM registry. (When users run `npm install -g disktracker`, npm downloads the binary ZIP from the GitHub Release).
4. **Publish Chocolatey Job**: Downloads the release ZIP, calculates the SHA256 checksum, prepares the nuspec and `chocolateyinstall.ps1`, checks if version `2.0.0` is already published on Chocolatey, packs, and pushes the package to Chocolatey.
5. **WinGet Job**: Submits the new release package to the Microsoft WinGet repository.

---

## 3. Safe Reruns and Recovery

If the release workflow crashes midway (e.g. because of NPM token expiration, Chocolatey API downtime, or WinGet API rate limits), you can safely **rerun the failed jobs** in the GitHub Actions UI:

- **GitHub Release**: Uses `softprops/action-gh-release@v2` which updates existing releases instead of failing if they already exist.
- **NPM Publishing**: Checks `npm view disktracker@$VERSION` and skips publication if it's already on NPM.
- **Chocolatey Publishing**: Requests the Chocolatey API for version existence and skips pushing if it's already on Chocolatey.
