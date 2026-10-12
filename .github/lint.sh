#!/usr/bin/env bash
set -euo pipefail
node .github/check-syntax.mjs
node node_modules/@biomejs/biome/bin/biome lint --error-on-warnings .github
if command -v actionlint >/dev/null; then
  actionlint .github/workflows/*.yml
else
  .github/.tools/actionlint .github/workflows/*.yml
fi
