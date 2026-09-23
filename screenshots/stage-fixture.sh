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
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
desk="$here/../crates/onemessagebus-e2e/tests/layouts/desk.json"

mkdir -p "$root"
cat >"$root/bus.yaml" <<YAML
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
printf '%s\n' "$root/bus.yaml"
