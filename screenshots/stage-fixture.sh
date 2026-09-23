#!/usr/bin/env bash
# Stage the fixture every screenshot runs over, into $1, and print the
# configuration's path.
#
# The one place it is written: the stills (capture.sh) and the animated hero
# (subscribe-gif.py) both call this, so a change to the layout the pictures are
# taken over moves both rather than one. It is what `desk_config` stages for the
# end-to-end journeys — a `local` transport under $1 and the bus's own `desk`
# bundle linked at `@1` — plus the codec the `serve` scene answers and its frame
# bundle reached by a `file://` link, so even the link machinery runs with no
# HTTP.
set -euo pipefail

root="${1:?stage-fixture: name the directory to stage the fixture in}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || {
  echo "stage-fixture: cannot resolve this script's own directory, so the layout" >&2
  echo "               and frame bundles it links cannot be found. Run it from a" >&2
  echo "               checkout rather than a copied file." >&2
  exit 1
}
desk="$here/../crates/onemessagebus-e2e/tests/layouts/desk.json"

# `$root` is interpolated into a double-quoted YAML scalar and into a `file://`
# link, so a path carrying a quote, a backslash or a newline would produce a
# configuration that is not the one asked for. Refuse it here, named.
unsafe=$'"\\\n\t\r\v\f\a\b\e'
case "$root" in
-*)
  echo "stage-fixture: the fixture directory must be a path, not something that" >&2
  echo "               reads as an option to the commands it is handed to: $root" >&2
  exit 1
  ;;
*["$unsafe"]*)
  echo "stage-fixture: the fixture directory's path carries a quote, a backslash" >&2
  echo "               or a control character, none of which can go into the" >&2
  echo "               configuration this writes: $root" >&2
  echo "               Stage the fixture somewhere plainer." >&2
  exit 1
  ;;
esac

mkdir -p "$root" || {
  echo "stage-fixture: could not make the fixture directory (the error is above)." >&2
  echo "               Name a directory this user can create: $root" >&2
  exit 1
}
if ! cat >"$root/bus.yaml" <<YAML
version: 1
transport: {kind: local, dir: "$root/bus"}
profile: desk
schemas:
  - "$desk@1"
  - "file://$here/fixture/frames.json@1"
codecs:
  checkout:
    queue: questions
    reply_window_seconds: 1
    select: op
    frames:
      quote:
        schema: checkout.frame.quote@1
        bindings:
          - do: answer
            response:
              sku: "{frame.sku}"
              units: "{frame.units}"
              ships_from: eu-2
YAML
then
  echo "stage-fixture: could not write the configuration into $root (the error is" >&2
  echo "               above). Name a directory this user can write." >&2
  exit 1
fi
printf '%s\n' "$root/bus.yaml"
