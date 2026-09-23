#!/usr/bin/env bash
# Refresh THIS host's committed screencomp baseline from the capture in
# shots/current — the one place that decides which lane gets rewritten and where.
#
# Shared by `just screenshots-bless` (an intended output change) and the pre-push
# guard's drift path (.githooks/pre-push), so the two can never disagree about
# the lane or the manifest path. $SHOTS_CURRENT overrides the capture root.
set -euo pipefail

# What the caller asked for is checked before this host is: a wrong
# $SHOTS_CURRENT is wrong on every machine, while a missing screencomp is only
# true of this one, and a lane without the renderer is owed the refusal it
# actually earned rather than an install link it would hit again afterwards.
current="${SHOTS_CURRENT:-shots/current}"
case "$current" in
-*)
  echo "bless-baseline: SHOTS_CURRENT must name the capture to bless, not" >&2
  echo "                something screencomp would read as an option: $current" >&2
  exit 1
  ;;
esac
if [ ! -d "$current" ]; then
  echo "bless-baseline: no capture to bless at $current" >&2
  echo "                Capture one first: just screenshots" >&2
  exit 1
fi

if ! command -v screencomp >/dev/null 2>&1; then
  echo "bless-baseline: screencomp is not installed, so the baseline cannot be" >&2
  echo "                refreshed. Install it and retry:" >&2
  echo "                https://github.com/nickderobertis/screencomp#install" >&2
  exit 1
fi

lane="$(bash "$(dirname "$0")/host-arch.sh")" || {
  echo "bless-baseline: this host's lane could not be named (the error is above)," >&2
  echo "                so there is no baseline to refresh." >&2
  exit 1
}
screencomp manifest --input "$current" --arch "$lane" --output "shots/baseline/${lane}.json" || {
  echo "bless-baseline: screencomp could not write the $lane baseline from" >&2
  echo "                $current (its error is above). Recapture first:" >&2
  echo "                just screenshots" >&2
  exit 1
}
