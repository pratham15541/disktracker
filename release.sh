#!/usr/bin/env bash
set -euo pipefail

dry_run=false

if [[ $# -gt 1 ]]; then
  echo "Usage: $0 [--dry-run|-n]"
  exit 1
fi

if [[ $# -eq 1 ]]; then
  if [[ "$1" == "--dry-run" || "$1" == "-n" ]]; then
    dry_run=true
  else
    echo "Unknown option: $1"
    exit 1
  fi
fi

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

if [[ -n "$(git status --porcelain)" ]]; then
  allowed_only_version=true
  while IFS= read -r line; do
    file="${line:3}"
    if [[ "$file" != "VERSION" ]]; then
      allowed_only_version=false
      break
    fi
  done <<<"$(git status --porcelain)"

  if ! $allowed_only_version; then
    echo "Working tree is not clean. Commit or stash changes before releasing."
    exit 1
  fi

  echo "Working tree has VERSION changes only; continuing."
fi

run() {
  if $dry_run; then
    echo "[dry-run] $*"
  else
    "$@"
  fi
}

if [[ ! -f VERSION ]]; then
  echo "VERSION file not found."
  exit 1
fi

version=$(tr -d ' \t\r\n' <VERSION)
if [[ -z "$version" ]]; then
  echo "VERSION file is empty."
  exit 1
fi

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid version in VERSION: $version"
  exit 1
fi

if $dry_run; then
  echo "[dry-run] Using VERSION -> $version"
else
  echo "Using VERSION -> $version"
fi

if $dry_run; then
  echo "[dry-run] Update workspace version in Cargo.toml"
else
  echo "Updating workspace version"
  python3 - Cargo.toml "$version" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
version = sys.argv[2]
text = path.read_text()

section_match = re.search(r'(?ms)^\[workspace\.package\]\s*$(.*?)(?=^\[|\Z)', text)
if not section_match:
    raise SystemExit(f"No [workspace.package] section found in {path}")

section = section_match.group(1)
new_section, count = re.subn(
    r'(?m)^version\s*=\s*"[^"]+"$',
    f'version = "{version}"',
    section,
    count=1,
)
if count == 0:
    raise SystemExit(f"No version field found in [workspace.package] in {path}")

new_text = text[:section_match.start(1)] + new_section + text[section_match.end(1):]
path.write_text(new_text)
PY
fi

if [[ -f npm/package.json ]]; then
  if $dry_run; then
    echo "[dry-run] Update npm/package.json and package-lock.json version"
  else
    echo "Updating npm package versions"

    python3 - npm/package.json "$version" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
version = sys.argv[2]

data = json.loads(path.read_text())
data["version"] = version

path.write_text(json.dumps(data, indent=2) + "\n")
PY

    if [[ -f npm/package-lock.json ]]; then
      python3 - npm/package-lock.json "$version" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
version = sys.argv[2]

data = json.loads(path.read_text())

data["version"] = version

if "packages" in data and "" in data["packages"]:
    data["packages"][""]["version"] = version

path.write_text(json.dumps(data, indent=2) + "\n")
PY
    fi
  fi
else
  echo "npm/package.json not found; skipping npm version update."
fi

if $dry_run; then
  echo "[dry-run] cargo fmt -- --check"
  echo "[dry-run] cargo clippy --workspace --all-targets --all-features --keep-going -- -D warnings"
  echo "[dry-run] cargo test --workspace --all-targets --all-features"
else
  echo "Running fmt"
  cargo fmt -- --check
  echo "Running clippy"
  cargo clippy --workspace --all-targets --all-features --keep-going -- -D warnings
  echo "Running tests"
  cargo test --workspace --all-targets --all-features
fi

echo "Committing and tagging"
run git add VERSION Cargo.toml
if [[ -f npm/package.json ]]; then
  run git add npm/package.json
fi
if [[ -f npm/package-lock.json ]]; then
  run git add npm/package-lock.json
fi
run git commit -m "release: v$version"
run git tag "v$version"

echo "Pushing"
run git push
run git push --tags

if $dry_run; then
  echo "Dry-run complete. No changes were made."
else
  echo "Release v$version pushed. CI/CD will create the GitHub Release."
fi
