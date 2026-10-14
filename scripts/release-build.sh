#!/usr/bin/env bash
# One compile; archive, wheel and npm all carry the wheel's exact binary bytes.
set -euo pipefail
# Argument/staging refusals are 2; operational tool failures are 1.
trap 'echo "release-build.sh: command failed at line $LINENO; repair the pinned toolchain or output permissions, then rerun scripts/release-build.sh with the same arguments" >&2; exit 1' ERR
if [[ $# == 2 && $1 == x86_64-apple-darwin ]]; then
  echo 'release-build: Intel macOS (x86_64-apple-darwin) is no longer released; build aarch64-apple-darwin for macOS' >&2
  exit 2
fi
if [[ $# != 2 || ! $1 =~ ^(aarch64-apple-darwin|(x86_64|aarch64)-unknown-linux-gnu)$ ]]; then
  echo 'release-build: expected TARGET OUTPUT_DIRECTORY; use a declared Darwin or Linux target' >&2
  exit 2
fi
source scripts/release-quiet.sh
target="$1"
out="$(mkdir -p "$2" && cd "$2" && pwd)"
entries="$(ls -A -- "$out")"
if [[ -n $entries ]]; then
  echo 'release-build: output must be an empty staging directory; choose a new directory and rerun' >&2
  exit 2
fi
stage_inputs() {
  local status
  if python3 scripts/release-stage.py "$@"; then
    return 0
  else
    status=$?
    if [[ $status == 2 ]]; then exit 2; fi
    return 1
  fi
}
rustup target add "$target"
just _wheel-build "$target" "$out" true
stage_inputs --target "$target" --directory "$out"
pkg="$(node scripts/npm-build.mjs platform --target "$target" --binary "$out/onemessagebus" --out "$out/npm")"
(cd "$pkg" && release_quiet npm pack --pack-destination "$out")
stage_inputs --target "$target" --directory "$out" --verify-npm
echo "staged $target from one compiled binary"
