#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
REF_DIR=${RAIL_REF_DIR:-"$REPO_DIR/../pi-rail-ui-ref-r1"}
EXPECTED_COMMIT=1d0dd1611a4d9546c64fe9f5b5c966253fb88eba

head=$(git -C "$REF_DIR" rev-parse --verify HEAD 2>/dev/null || true)
if [ "$head" != "$EXPECTED_COMMIT" ]; then
	printf 'reference HEAD %s does not match pinned %s\n' "${head:-<unavailable>}" "$EXPECTED_COMMIT" >&2
	exit 1
fi

tracked=$(git -C "$REF_DIR" status --porcelain --untracked-files=no)
if [ -n "$tracked" ]; then
	printf 'reference checkout has tracked modifications:\n%s\n' "$tracked" >&2
	exit 1
fi

# Both installs are lockfile-driven. The generator performs the final
# fail-closed validation, including every @earendil-works/pi-* package and
# every Rail import, before it writes a fixture.
npm ci --prefix "$REF_DIR" --ignore-scripts --no-audit --no-fund
npm ci --prefix "$SCRIPT_DIR" --ignore-scripts --no-audit --no-fund
RAIL_REF_DIR="$REF_DIR" npm --prefix "$SCRIPT_DIR" run generate