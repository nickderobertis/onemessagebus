#!/usr/bin/env bash
set -euo pipefail
npm ci --prefix .github --ignore-scripts --quiet
if ! command -v actionlint >/dev/null; then
  mkdir -p .github/.tools
  GOBIN="$PWD/.github/.tools" go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12
fi
