#!/usr/bin/env bash
set -euo pipefail
# The one actionlint pin. Where Go is present it is built with `go install`;
# where it is not — a hosted macOS runner carries no Go — the same release's
# prebuilt binary is fetched and checked against that release's checksums.
actionlint_version=1.7.12
npm ci --ignore-scripts --quiet
if ! command -v actionlint >/dev/null; then
  mkdir -p .github/.tools
  if command -v go >/dev/null; then
    GOBIN="$PWD/.github/.tools" go install "github.com/rhysd/actionlint/cmd/actionlint@v$actionlint_version"
  else
    case "$(uname -s)/$(uname -m)" in
      Darwin/arm64) platform=darwin_arm64 ;;
      Linux/x86_64) platform=linux_amd64 ;;
      Linux/aarch64) platform=linux_arm64 ;;
      *)
        echo "bootstrap: no Go and no prebuilt actionlint for $(uname -s)/$(uname -m) — install Go or actionlint $actionlint_version, then rerun 'just bootstrap'" >&2
        exit 1
        ;;
    esac
    release="https://github.com/rhysd/actionlint/releases/download/v$actionlint_version"
    archive="actionlint_${actionlint_version}_$platform.tar.gz"
    checksums="actionlint_${actionlint_version}_checksums.txt"
    download=$(mktemp -d)
    trap 'rm -rf "$download"' EXIT
    curl -fsSL -o "$download/$archive" "$release/$archive"
    curl -fsSL -o "$download/$checksums" "$release/$checksums"
    if ! (cd "$download" && grep " $archive\$" "$checksums" | shasum -a 256 -c - >/dev/null); then
      echo "bootstrap: $archive does not match $release/$checksums — refusing to install it" >&2
      exit 1
    fi
    tar -xzf "$download/$archive" -C .github/.tools actionlint
  fi
fi
