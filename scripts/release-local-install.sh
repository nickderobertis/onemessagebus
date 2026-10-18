#!/usr/bin/env bash
# All inputs are staged files; registries only provision third-party dependencies.
set -euo pipefail
# Argument refusals are 2; operational tool failures are 1.
fail() {
  echo "release-local-install.sh: command failed at line $1; repair the staged products or selected interpreters, then rerun scripts/release-local-install.sh with the same arguments" >&2
  exit 1
}
trap 'fail "$LINENO"' ERR
# bash 3.2 (macOS's /bin/bash) neither exits nor runs the ERR trap when a
# subshell fails, so each subshell names its own failure.
if [[ $# != 4 ]]; then
  echo 'release-local-install: expected STAGE SDK_DIRECTORY PYTHON NODE; rerun scripts/release-local-install.sh <staged-target-directory> <sdk-distributions-directory> <python-executable> <node-executable>' >&2
  exit 2
fi
source scripts/release-quiet.sh
stage="$(cd "$1" && pwd)"
sdks="$(cd "$2" && pwd)"
python="$3"
node="$4"
# npm and launcher shebangs must use the architecture-specific Node too.
PATH="$(dirname "$node"):$PATH"
export PATH
root="$PWD"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
"$python" -m venv "$work/venv"
release_quiet "$work/venv/bin/python" -m pip install "$stage"/*.whl "$sdks"/onemessagebus-*.whl
PATH="$work/venv/bin:$PATH" release_quiet bash scripts/smoke-published.sh --expect-version "$version" --label 'staged CLI wheel'
(cd "$work" && PATH="$work/venv/bin:$PATH" release_quiet "$work/venv/bin/python" "$root/sdk-install/smoke.py" "$version") || fail "$LINENO"
launcher="$(node scripts/npm-build.mjs launcher --out "$work/npm")"
(cd "$launcher" && release_quiet npm pack --pack-destination "$work") || fail "$LINENO"
mkdir "$work/app"
(cd "$work/app" && npm init -y >/dev/null && release_quiet npm install --no-audit --no-fund "$stage"/*.tgz "$work"/*.tgz "$sdks"/*.tgz) || fail "$LINENO"
PATH="$work/app/node_modules/.bin:$PATH" release_quiet bash scripts/smoke-published.sh --expect-version "$version" --label 'staged npm CLI'
cp sdk-install/smoke.mjs "$work/app/smoke.mjs"
(cd "$work/app" && release_quiet "$node" smoke.mjs "$version") || fail "$LINENO"
echo "staged CLI and SDK installations passed"
