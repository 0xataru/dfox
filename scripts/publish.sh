#!/usr/bin/env bash
#
# Step 2 of a release: tags the merged release commit on main and pushes the tag.
# Pushing the tag is the release — the release workflow builds the binaries and
# publishes the GitHub release with the CHANGELOG.md section as its notes.
#
# Release tags are protected: once pushed they cannot be moved or deleted, so this
# only ever tags a clean main that matches origin and passes the checks.
#
#   ./scripts/publish.sh

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

die() {
	echo "publish: $*" >&2
	exit 1
}

cd "$root"

version="$("$root/scripts/version.sh")"
tag="v$version"

[ "$(git branch --show-current)" = "main" ] || die "run this from main"
[ -z "$(git status --porcelain)" ] || die "working tree is not clean"

git fetch -q origin main --tags
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "main is not in sync with origin/main — git pull first"

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
	die "$tag already exists — bump first with make release VERSION=X.Y.Z"
fi

"$root/scripts/release-notes.sh" "$version" >/dev/null ||
	die "CHANGELOG.md has no [$version] section — was the release pull request merged?"

"$root/scripts/check.sh"

git tag -a "$tag" -m "$tag"
git push -q origin "$tag"

echo
echo "Pushed $tag at $(git rev-parse --short HEAD). The release workflow is building it:"
echo "  gh run watch \$(gh run list --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
