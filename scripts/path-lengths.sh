#!/usr/bin/env bash
# Every tracked path is short enough for a Windows `cargo install --git`.
#
# cargo checks the repository out with libgit2 under
#   C:/Users/<user>/.cargo/git/checkouts/onemessagebus-<16 hex>/<7-char rev>/
# — 81 characters for the runner's `runneradmin` — and libgit2 refuses any path
# past Windows' default MAX_PATH of 260 (259 characters and the terminator) with
# `error: path too long`, failing the README's install for every Windows user.
# A tracked path of at most LIMIT characters leaves 259 - 81 - 150 = 28 more for
# a longer user name than the runner's.
#
# Usage: scripts/path-lengths.sh   (from anywhere inside the checkout)
# Quiet on success; on failure names each path, its length, and the limit.
set -euo pipefail

LIMIT=150

root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  echo "path-lengths: not inside a git checkout" >&2
  echo "ACTION: run this from the repository" >&2
  exit 2
}
cd "$root"

too_long="$(git ls-files -z | tr '\0' '\n' | awk -v limit="$LIMIT" 'length($0) > limit { print length($0) "\t" $0 }')"
if [ -n "$too_long" ]; then
  echo "path-lengths: tracked paths longer than $LIMIT characters fail a Windows cargo install --git checkout:" >&2
  printf '%s\n' "$too_long" >&2
  echo "ACTION: shorten each path named above (a fixture whose name is long can be stored short and renamed at run time)" >&2
  exit 1
fi
