#!/usr/bin/env bash
#
# After `cargo bench -- --baseline main`, fails if any benchmark got clearly
# slower: by more than THRESHOLD (default 10%) even at the low end of
# criterion's confidence interval, so ordinary noise doesn't fail a build.
#
#   ./scripts/bench-compare.sh [target/criterion]

set -euo pipefail

dir="${1:-target/criterion}"
threshold="${THRESHOLD:-0.10}"

[ -d "$dir" ] || {
	echo "bench-compare: no $dir; run the benchmarks first" >&2
	exit 1
}

failed=0
found=0
while IFS= read -r file; do
	found=1
	name="${file#"$dir"/}"
	name="${name%/change/estimates.json}"
	read -r point lower < <(python3 -c '
import json, sys
mean = json.load(open(sys.argv[1]))["mean"]
print(mean["point_estimate"], mean["confidence_interval"]["lower_bound"])
' "$file")
	verdict=$(python3 -c "print('slower' if $lower > $threshold else 'ok')")
	printf '%-40s %+7.1f%%  (at least %+.1f%%)  %s\n' "$name" \
		"$(python3 -c "print($point * 100)")" "$(python3 -c "print($lower * 100)")" "$verdict"
	[ "$verdict" = ok ] || failed=1
done < <(find "$dir" -path '*/change/estimates.json' | sort)

[ "$found" = 1 ] || echo "bench-compare: nothing to compare (no baseline yet)"
[ "$failed" = 0 ] || {
	echo "bench-compare: a benchmark is more than $(python3 -c "print(int($threshold * 100))")% slower than main" >&2
	exit 1
}
