#!/usr/bin/env bash
# One compile; archive, wheel and npm all carry the wheel's exact binary bytes.
set -euo pipefail
if [[ $# != 2 || ! $1 =~ ^(x86_64|aarch64)-(apple-darwin|unknown-linux-gnu)$ ]]; then
  echo 'release-build: expected TARGET OUTPUT_DIRECTORY; use a declared Darwin or Linux target' >&2
  exit 2
fi
target="$1"
out="$(mkdir -p "$2" && cd "$2" && pwd)"
rustup target add "$target"
uvx --from maturin==1.14.1 maturin build --release --locked --strip true --target "$target" --out "$out"
python3 scripts/release-stage.py --target "$target" --directory "$out"
pkg="$(node scripts/npm-build.mjs platform --target "$target" --binary "$out/onemessagebus" --out "$out/npm")"
(cd "$pkg" && npm pack --silent --pack-destination "$out" >/dev/null)
python3 scripts/release-stage.py --target "$target" --directory "$out" --verify-npm
echo "staged $target from one compiled binary"
