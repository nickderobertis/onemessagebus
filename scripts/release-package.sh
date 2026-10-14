#!/usr/bin/env bash
# Everything a release does after its one compile, shared by release-build.sh
# and the artifact-contract tests: stage the archive from the compiled wheel's
# binary, pack the npm platform package around those bytes, and verify npm
# carries them. Run from the repository root.
#
#   release-package.sh --check TARGET   refuse a target no release builds
#   release-package.sh TARGET STAGE     STAGE holds exactly the compiled wheel
set -euo pipefail
# Argument/staging refusals are 2; operational tool failures are 1.
trap 'echo "release-package.sh: command failed at line $LINENO; repair the staged wheel or output permissions, then rerun scripts/release-build.sh with the same arguments" >&2; exit 1' ERR
check_target() {
  if [[ $1 == x86_64-apple-darwin ]]; then
    echo 'release-package: Intel macOS (x86_64-apple-darwin) is no longer released; build aarch64-apple-darwin for macOS' >&2
    exit 2
  fi
  if [[ ! $1 =~ ^(aarch64-apple-darwin|(x86_64|aarch64)-unknown-linux-gnu)$ ]]; then
    echo "release-package: unsupported target '$1'; use a declared Darwin or Linux target" >&2
    exit 2
  fi
}
if [[ $# == 2 && $1 == --check ]]; then
  check_target "$2"
  exit 0
fi
if [[ $# != 2 || ! -d $2 ]]; then
  echo 'release-package: expected TARGET STAGE, where STAGE is the directory holding the compiled wheel' >&2
  exit 2
fi
check_target "$1"
source scripts/release-quiet.sh
target="$1"
out="$(cd "$2" && pwd)"
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
stage_inputs --target "$target" --directory "$out"
pkg="$(node scripts/npm-build.mjs platform --target "$target" --binary "$out/onemessagebus" --out "$out/npm")"
(cd "$pkg" && release_quiet npm pack --pack-destination "$out")
stage_inputs --target "$target" --directory "$out" --verify-npm
