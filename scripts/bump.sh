#!/usr/bin/env bash
#
# Release version bump: syncs the workspace version in Cargo.toml (and Cargo.lock),
# CHANGELOG.md and the README version badge.
#
# It deliberately stops there. Committing, opening the release pull request and
# tagging are separate steps (`make release`, then `make publish` once merged).
#
#   ./scripts/bump.sh 0.2.0

set -euo pipefail

version="${1:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

die() {
	echo "bump: $*" >&2
	exit 1
}

[ -n "$version" ] || die "usage: ./scripts/bump.sh X.Y.Z"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "'$version' is not X.Y.Z"

cd "$root"

for f in Cargo.toml CHANGELOG.md README.md; do
	[ -f "$f" ] || die "missing $f"
done

current="$("$root/scripts/version.sh")"

# The current version may legitimately equal the new one when it has never been
# released (the very first release). What must never happen is reusing a tag.
if git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
	die "v$version is already tagged"
fi
lowest="$(printf '%s\n%s\n' "$current" "$version" | sort -V | head -n1)"
[ "$lowest" = "$current" ] || die "v$version is lower than the current v$current"

# The changelog entry is the point of the release, so refuse to cut one without
# it rather than silently producing an empty section.
body="$(awk '
	/^## \[?Unreleased\]?[[:space:]]*$/ { found = 1; next }
	found && /^## / { exit }
	found { print }
' CHANGELOG.md)"

grep -q '^[[:space:]]*[-*]' <<<"$body" || die "CHANGELOG.md: [Unreleased] has no entries yet"

date="$(date -u +%Y-%m-%d)"

# Leave [Unreleased] in place and open a dated section right below it: everything
# that was unreleased becomes the body of the new version.
awk -v ver="$version" -v date="$date" '
	state == "" {
		print
		if ($0 ~ /^## \[?Unreleased\]?[[:space:]]*$/) {
			state = "moved"
			print ""
			print "## [" ver "] - " date
		}
		next
	}
	{ print }
' CHANGELOG.md >CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md

# Only the version line inside [workspace.package]; the members inherit it.
awk -v ver="$version" '
	/^\[/ { section = $0 }
	section == "[workspace.package]" && /^version[[:space:]]*=/ && !done {
		print "version = \"" ver "\""
		done = 1
		next
	}
	{ print }
	END { if (!done) exit 1 }
' Cargo.toml >Cargo.toml.tmp || {
	rm -f Cargo.toml.tmp
	die "Cargo.toml: no version under [workspace.package]"
}
mv Cargo.toml.tmp Cargo.toml

# Rewrites only the workspace members' entries in the lockfile.
cargo update --workspace --offline -q 2>/dev/null || cargo update --workspace -q

# sed -i is spelled differently on BSD and GNU, so write through a temp file.
sed -E "s#(img\.shields\.io/badge/version-)[^-\"']+(-blue)#\1${version}\2#" README.md >README.md.tmp
if cmp -s README.md README.md.tmp && ! grep -q "badge/version-${version}-blue" README.md; then
	rm -f README.md.tmp
	die "README.md: version badge not found"
fi
mv README.md.tmp README.md

echo "Bumped $current → $version"
echo "  Cargo.toml, Cargo.lock"
echo "  CHANGELOG.md"
echo "  README.md (badge)"
