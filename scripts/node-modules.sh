#!/usr/bin/env bash
# The workspace's locked Node install, healed if it is missing.
#
# A fresh clone has no `node_modules`, and more than one entry point needs it:
# `scripts/nx` cannot find the orchestrator without it, and
# `scripts/required-contexts.mjs` cannot find the YAML reader it parses ci.yml
# with. Both heal through here rather than each hand-rolling an install, so a
# recipe can never fail with "cannot find package" while another quietly
# repaired it — and so CI installs exactly what a clean clone does.
#
# Quiet on success and idempotent: with the install already in place this is a
# test and a return. Everything it does say goes to stderr, because a caller in
# show-output mode reads stdout for its command's answer and installer chatter
# is not that.
set -euo pipefail

if ! ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; then
  echo "node-modules: cannot resolve the repository root from ${BASH_SOURCE[0]}" >&2
  echo "ACTION: run scripts/node-modules.sh by its path inside a checkout whose directories are readable" >&2
  exit 1
fi
cd "$ROOT" || {
  echo "node-modules: cannot enter the repository root $ROOT" >&2
  echo "ACTION: run this from a checkout whose directories are readable" >&2
  exit 1
}

# The npm-written shim rather than a path inside a package: a dependency can move
# its bin entry between releases, and the shim is the one name that cannot.
if [ -e node_modules/.bin/nx ] || [ -e node_modules/.bin/nx.cmd ]; then
  exit 0
fi

if ! command -v npm >/dev/null 2>&1; then
  echo "node-modules: npm not found; cannot install the workspace's locked dependencies" >&2
  echo "ACTION: install Node.js 20+ (https://nodejs.org/) and re-run 'just bootstrap'" >&2
  exit 1
fi
if ! npm ci --silent --no-audit --no-fund >&2; then
  echo "node-modules: 'npm ci' failed in $ROOT" >&2
  echo "ACTION: check network access to the npm registry, then re-run 'just bootstrap'" >&2
  exit 1
fi
