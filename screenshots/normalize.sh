#!/usr/bin/env bash
# Rewrite the per-run values the bus mints to fixed placeholders, reading stdin
# and writing stdout.
#
# The one place they are written. Both the hash-gated stills (capture.sh) and the
# animated hero (subscribe-gif.py) pass their captured text through this, so a
# value normalised in one is normalised the same way in the other and the two
# read as one session.
#
# There is no clock-override and no fixed-id switch on this CLI, and the
# visual-docs adoption did not add one: a flag is a capability-manifest change
# first, then a method in both SDK clients and a parity audit. Normalising after
# the verb has run is the answer (screenshots/AGENTS.md).
set -euo pipefail

sed \
  -e 's/c-[0-9a-f]\{32\}/c-4f3c1d92a08b47e6b1d5c0a7e93f2b18/g' \
  -e 's/"raised_at":[0-9]\{13\}/"raised_at":1789300000000/g' \
  -e 's/"at":[0-9]\{13\}/"at":1789300000000/g'
