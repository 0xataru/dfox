#!/usr/bin/env bash
#
# Step 1 of a release: cuts a release branch from an up-to-date main, bumps the
# version, commits, pushes and opens the release pull request.
#
# main is protected, so the bump has to be reviewed and merged like any other
# change. Once it is merged, run `make publish` to tag it.
#
#   ./scripts/release.sh 0.2.0

set -euo pipefail

version="${1:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
branch="release/v$version"

die() {
	echo "release: $*" >&2
	exit 1
}

[ -n "$version" ] || die "usage: ./scripts/release.sh X.Y.Z"
command -v gh >/dev/null || die "the GitHub CLI (gh) is required"

cd "$root"

[ "$(git branch --show-current)" = "main" ] || die "run this from main"
[ -z "$(git status --porcelain)" ] || die "working tree is not clean"

git fetch -q origin main --tags
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "main is not in sync with origin/main — git pull first"
git rev-parse -q --verify "refs/heads/$branch" >/dev/null && die "branch $branch already exists"

"$root/scripts/check.sh"

git switch -q -c "$branch"
"$root/scripts/bump.sh" "$version"

git add Cargo.toml Cargo.lock CHANGELOG.md README.md
git commit -q -m "chore: release v$version"
git push -q -u origin "$branch"

gh pr create --base main --head "$branch" \
	--title "chore: release v$version" \
	--body "$("$root/scripts/release-notes.sh" "$version")"

echo
echo "Once the pull request is merged:"
echo "  git switch main && git pull && make publish"
