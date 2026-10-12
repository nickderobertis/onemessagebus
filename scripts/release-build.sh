#!/usr/bin/env bash
# One compile; archive, wheel and npm all carry the wheel's exact binary bytes.
set -euo pipefail
# Input refusals stay 2; all other unsuccessful tool statuses become 1.
trap 'status=$?; echo "release-build.sh: command failed at line $LINENO; repair the pinned toolchain or output permissions, then rerun scripts/release-build.sh with the same arguments" >&2; if [[ $status == 2 ]]; then exit 2; else exit 1; fi' ERR
if [[ $# != 2 || ! $1 =~ ^(x86_64|aarch64)-(apple-darwin|unknown-linux-gnu)$ ]]; then
  echo 'release-build: expected TARGET OUTPUT_DIRECTORY; use a declared Darwin or Linux target' >&2
  exit 2
fi
source scripts/release-quiet.sh
target="$1"
out="$(mkdir -p "$2" && cd "$2" && pwd)"
rustup target add "$target"
just _wheel-build "$target" "$out" true
python3 scripts/release-stage.py --target "$target" --directory "$out"
pkg="$(node scripts/npm-build.mjs platform --target "$target" --binary "$out/onemessagebus" --out "$out/npm")"
(cd "$pkg" && release_quiet npm pack --pack-destination "$out")
python3 scripts/release-stage.py --target "$target" --directory "$out" --verify-npm
echo "staged $target from one compiled binary"
