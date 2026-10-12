#!/usr/bin/env bash
#
# Prints the CHANGELOG.md section for one version, without its heading. Used as the
# body of the GitHub release, so the changelog is the only place notes are written.
#
#   ./scripts/release-notes.sh 0.2.0

set -euo pipefail

version="${1:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

[ -n "$version" ] || {
	echo "usage: ./scripts/release-notes.sh X.Y.Z" >&2
	exit 1
}

notes="$(awk -v ver="$version" '
	index($0, "## [" ver "]") == 1 { found = 1; next }
	found && /^## / { exit }
	found { print }
' "$root/CHANGELOG.md" | sed -e '/./,$!d')"

[ -n "$notes" ] || {
	echo "release-notes: CHANGELOG.md has no section for $version" >&2
	exit 1
}
printf '%s\n' "$notes"
