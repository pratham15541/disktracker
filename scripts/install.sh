#!/usr/bin/env bash
set -euo pipefail

REPO_DEFAULT="pratham15541/disktracker"
REPO="${DISKTRACKER_REPO:-$REPO_DEFAULT}"

uname_s="$(uname -s)"
case "$uname_s" in
	Linux)
		os="linux"
		;;
	Darwin)
		os="macos"
		;;
	*)
		echo "Unsupported OS: $uname_s" >&2
		exit 1
		;;
esac

uname_m="$(uname -m)"
case "$uname_m" in
	x86_64|amd64)
		arch="x64"
		;;
	aarch64|arm64)
		arch="arm64"
		;;
	*)
		echo "Unsupported architecture: $uname_m" >&2
		exit 1
		;;
esac

version="${DISKTRACKER_VERSION:-}"
if [ -z "$version" ]; then
	api_url="https://api.github.com/repos/$REPO/releases/latest"
	version="$(
		curl -fsSL "$api_url" \
			| grep -m1 '"tag_name":' \
			| sed -E 's/.*"([^"]+)".*/\1/'
	)"
fi

if [ -z "$version" ]; then
	echo "Failed to resolve latest version. Set DISKTRACKER_VERSION to a tag like v1.2.3." >&2
	exit 1
fi

asset="disktracker-${version}-${os}-${arch}.tar.gz"
url="https://github.com/$REPO/releases/download/$version/$asset"

tmp_dir="$(mktemp -d)"
cleanup() {
	rm -rf "$tmp_dir"
}
trap cleanup EXIT

download() {
	if command -v curl >/dev/null 2>&1; then
		curl -fSL "$1" -o "$2"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO "$2" "$1"
	else
		echo "Missing download tool: install curl or wget." >&2
		exit 1
	fi
}

archive_path="$tmp_dir/$asset"
download "$url" "$archive_path"
tar -xzf "$archive_path" -C "$tmp_dir"

install_dir="${DISKTRACKER_INSTALL_DIR:-}"
if [ -z "$install_dir" ]; then
	if [ -w "/usr/local/bin" ]; then
		install_dir="/usr/local/bin"
	else
		install_dir="$HOME/.local/bin"
	fi
fi

mkdir -p "$install_dir"
bin_src="$tmp_dir/disktracker"
bin_dst="$install_dir/disktracker"

if [ ! -f "$bin_src" ]; then
	echo "Downloaded archive does not contain disktracker binary." >&2
	exit 1
fi

if [ -w "$install_dir" ]; then
	cp "$bin_src" "$bin_dst"
	chmod +x "$bin_dst"
else
	if command -v sudo >/dev/null 2>&1; then
		sudo cp "$bin_src" "$bin_dst"
		sudo chmod +x "$bin_dst"
	else
		echo "No write access to $install_dir and sudo not available." >&2
		exit 1
	fi
fi

echo "Installed disktracker $version to $bin_dst"
if ! command -v disktracker >/dev/null 2>&1; then
	echo "Note: $install_dir is not on your PATH." >&2
fi
