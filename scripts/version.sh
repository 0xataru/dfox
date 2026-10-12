#!/usr/bin/env bash
#
# Prints the workspace version from Cargo.toml — the single source of truth.
#
#   ./scripts/version.sh

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

version="$(awk '
	/^\[/ { section = $0 }
	section == "[workspace.package]" && /^version[[:space:]]*=/ {
		gsub(/^version[[:space:]]*=[[:space:]]*"|".*$/, "")
		print
		exit
	}
' "$root/Cargo.toml")"

[ -n "$version" ] || {
	echo "version: no version under [workspace.package] in Cargo.toml" >&2
	exit 1
}
echo "$version"
