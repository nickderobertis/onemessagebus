#!/usr/bin/env bash
# All inputs are staged files; registries only provision third-party dependencies.
set -euo pipefail
trap 'echo "release-local-install.sh: command failed at line $LINENO; repair the staged products or selected interpreters, then rerun scripts/release-local-install.sh with the same arguments" >&2' ERR
if [[ $# != 4 ]]; then
  echo 'release-local-install: expected STAGE SDK_DIRECTORY PYTHON NODE' >&2
  exit 2
fi
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
"$work/venv/bin/python" -m pip install --quiet "$stage"/*.whl "$sdks"/onemessagebus-*.whl
PATH="$work/venv/bin:$PATH" bash scripts/smoke-published.sh --expect-version "$version" --label 'staged CLI wheel'
(cd "$work" && PATH="$work/venv/bin:$PATH" "$work/venv/bin/python" "$root/sdk-install/smoke.py" "$version")
launcher="$(node scripts/npm-build.mjs launcher --out "$work/npm")"
(cd "$launcher" && npm pack --silent --pack-destination "$work" >/dev/null)
mkdir "$work/app"
(cd "$work/app" && npm init -y >/dev/null && npm install --silent --no-audit --no-fund "$stage"/*.tgz "$work"/*.tgz "$sdks"/*.tgz)
PATH="$work/app/node_modules/.bin:$PATH" bash scripts/smoke-published.sh --expect-version "$version" --label 'staged npm CLI'
cp sdk-install/smoke.mjs "$work/app/smoke.mjs"
(cd "$work/app" && "$node" smoke.mjs "$version")
