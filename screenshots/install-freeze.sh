#!/usr/bin/env bash
# Install the pinned `freeze` — the renderer that turns each captured scene into
# a deterministic SVG — from its prebuilt release, choosing the build that
# matches this machine's architecture.
#
# `freeze_version` below is the ONE place the renderer's version is stated: both
# `just screenshots-tools` and the Visual-docs workflow's capture step run this
# script, so there is no second pin to keep in step. Bumping it reflows every
# shot; re-bless afterwards (screenshots/AGENTS.md).
set -euo pipefail

freeze_version="0.2.2"

# freeze's Linux release assets are named for the arch the way screencomp names
# its lanes (x86_64 / arm64); map explicitly anyway, since that is a coincidence
# of two vocabularies rather than one shared name.
host="$(uname -m)"
case "$host" in
x86_64 | amd64) asset_arch="x86_64" ;;
arm64 | aarch64) asset_arch="arm64" ;;
*)
  echo "install-freeze: no pinned freeze build for this architecture: $host" >&2
  echo "                freeze v$freeze_version ships Linux x86_64 and arm64 only." >&2
  echo "                Capture on one of those, or build freeze from source:" >&2
  echo "                go install github.com/charmbracelet/freeze@v$freeze_version" >&2
  exit 1
  ;;
esac

# Overridable so a test can drive this script against a stand-in release tree
# instead of the network. `-` rather than `:-`: an override that is SET but empty
# is a misconfigured caller, which the checks below reject rather than silently
# replace with a default nobody asked for.
base_url="${FREEZE_BASE_URL-https://github.com/charmbracelet/freeze/releases/download}"
install_dir="${FREEZE_INSTALL_DIR-$HOME/.local/bin}"
sums_file="${FREEZE_SHA256_FILE-$(dirname "$0")/freeze.sha256}"

case "$base_url" in
https://* | file://*) ;;
*)
  echo "install-freeze: FREEZE_BASE_URL must be an https:// or file:// URL" >&2
  echo "                got: ${base_url:-<empty>}" >&2
  echo "                Unset it to use the default release base." >&2
  exit 1
  ;;
esac
case "$install_dir" in
"" | -*)
  echo "install-freeze: FREEZE_INSTALL_DIR must name a directory to install into," >&2
  echo "                not something \`install\` would read as an option." >&2
  echo "                Got: ${install_dir:-<empty>}; unset it for ~/.local/bin." >&2
  exit 1
  ;;
esac
if [ ! -r "$sums_file" ]; then
  echo "install-freeze: no readable digest pin file at $sums_file" >&2
  echo "                Restore screenshots/freeze.sha256, or point" >&2
  echo "                FREEZE_SHA256_FILE at a copy of it." >&2
  exit 1
fi

stem="freeze_${freeze_version}_Linux_${asset_arch}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

curl -fsSL -o "$tmp/freeze.tar.gz" "$base_url/v${freeze_version}/${stem}.tar.gz" || {
  echo "install-freeze: could not download ${stem}.tar.gz (curl's error is above)." >&2
  echo "                Check network reach to $base_url, or install freeze from" >&2
  echo "                source: go install github.com/charmbracelet/freeze@v$freeze_version" >&2
  exit 1
}

# Validate the archive before unpacking it. The expected digest is pinned in THIS
# repository rather than fetched beside the archive: a checksum served from the
# download's own origin vouches for nothing.
expected="$(awk -v want="${stem}.tar.gz" '$2 == want { print $1 }' "$sums_file")"
if [ -z "$expected" ]; then
  echo "install-freeze: no pinned sha256 for ${stem}.tar.gz in $sums_file" >&2
  echo "                Add its line from" >&2
  echo "                $base_url/v${freeze_version}/checksums.txt" >&2
  exit 1
fi
actual="$(sha256 "$tmp/freeze.tar.gz")"
if [ "$actual" != "$expected" ]; then
  echo "install-freeze: sha256 mismatch for ${stem}.tar.gz — NOT installing" >&2
  echo "                expected $expected (pinned in $sums_file)" >&2
  echo "                got      $actual" >&2
  echo "                If the pin is stale, refresh $sums_file from" >&2
  echo "                $base_url/v${freeze_version}/checksums.txt; otherwise treat the" >&2
  echo "                download as untrusted and do not retry blindly." >&2
  exit 1
fi

tar -xzf "$tmp/freeze.tar.gz" -C "$tmp" || {
  echo "install-freeze: ${stem}.tar.gz matched its pinned digest but did not" >&2
  echo "                unpack (tar's error is above). Re-run; if it repeats, the" >&2
  echo "                pinned digest in screenshots/freeze.sha256 names an" >&2
  echo "                archive this tar cannot read." >&2
  exit 1
}
if [ ! -f "$tmp/$stem/freeze" ]; then
  echo "install-freeze: ${stem}.tar.gz matched its pinned digest but holds no" >&2
  echo "                $stem/freeze — upstream changed the archive layout." >&2
  echo "                Re-pin freeze_version in this script against the new one." >&2
  exit 1
fi

install -d "$install_dir" && install "$tmp/$stem/freeze" "$install_dir/freeze" || {
  echo "install-freeze: could not install into $install_dir (the error is above)." >&2
  echo "                Point FREEZE_INSTALL_DIR at a directory you can write." >&2
  exit 1
}
echo "install-freeze: installed freeze v$freeze_version ($asset_arch) to $install_dir" >&2
