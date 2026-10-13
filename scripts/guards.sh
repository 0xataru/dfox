#!/usr/bin/env bash
#
# Rules the compiler can't check:
#   - colours come from the theme: no `Color::` outside crates/dfox-tui/src/theme/
#   - drawing and key handling never wait: no `.await` in components or widgets
#
#   ./scripts/guards.sh

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

failed=0

check() {
	local message="$1"
	shift
	local hits
	hits="$(grep -rn "$@" || true)"
	if [ -n "$hits" ]; then
		echo "guards: $message" >&2
		echo "$hits" >&2
		failed=1
	fi
}

check "colours must come from the theme (crates/dfox-tui/src/theme/)" \
	--include='*.rs' --exclude-dir=theme 'Color::' crates/dfox-tui/src
check "components and widgets must not wait on anything" \
	--include='*.rs' '\.await' crates/dfox-tui/src/components crates/dfox-tui/src/widgets

exit "$failed"
