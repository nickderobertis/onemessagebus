#!/usr/bin/env bash
# Print this host's screencomp capture lane: the normalized CPU architecture that
# names shots/current/<arch>/ and shots/baseline/<arch>.json.
#
# One place, so the three consumers can never disagree about what a lane is
# called: the capture (screenshots/capture.sh), the local pre-push guard
# (.githooks/pre-push), and `just screenshots-bless`. The names match
# [capture].arches in screencomp.toml, which is what screencomp fans CI out over.
set -euo pipefail

arch="$(uname -m)" || {
  echo "host-arch: 'uname -m' did not answer, so this host's capture lane cannot" >&2
  echo "           be named. Run it by hand to see why; every screenshot command" >&2
  echo "           needs a lane." >&2
  exit 1
}
case "$arch" in
x86_64 | amd64) arch="x86_64" ;;
arm64 | aarch64) arch="arm64" ;;
esac
# A lane names a directory and a committed baseline file, so an architecture
# nobody here has mapped still has to be a plain path component.
case "$arch" in
*[!A-Za-z0-9_-]* | "")
  echo "host-arch: 'uname -m' answered \"$arch\", which cannot name a capture" >&2
  echo "           lane — a lane is a directory and a shots/baseline/<arch>.json." >&2
  echo "           Map it in this script if this host should have one." >&2
  exit 1
  ;;
esac
printf '%s\n' "$arch"
