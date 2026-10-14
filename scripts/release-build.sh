#!/usr/bin/env bash
# One compile; archive, wheel and npm all carry the wheel's exact binary bytes.
set -euo pipefail
# Argument/staging refusals are 2; operational tool failures are 1.
trap 'echo "release-build.sh: command failed at line $LINENO; repair the pinned toolchain or output permissions, then rerun scripts/release-build.sh with the same arguments" >&2; exit 1' ERR
if [[ $# != 2 ]]; then
  echo 'release-build: expected TARGET OUTPUT_DIRECTORY; use a declared Darwin or Linux target' >&2
  exit 2
fi
# release-package.sh's own status (2 refused, 1 failed) is this script's.
package() {
  local status=0
  bash scripts/release-package.sh "$@" || status=$?
  if [[ $status != 0 ]]; then exit "$status"; fi
}
package --check "$1"
target="$1"
out="$(mkdir -p "$2" && cd "$2" && pwd)"
entries="$(ls -A -- "$out")"
if [[ -n $entries ]]; then
  echo 'release-build: output must be an empty staging directory; choose a new directory and rerun' >&2
  exit 2
fi
rustup target add "$target"
just _wheel-build "$target" "$out" true
package "$target" "$out"
echo "staged $target from one compiled binary"
