#!/usr/bin/env bash
# Re-captures spikes/saphyr-diff/corpus/captured: every configuration, event
# filter and predicate text the core, CLI and e2e suites hand to
# Config::parse, Filter::parse and Predicate::read. Applies capture.patch (a
# hook that writes each text it is given, named by its digest, under
# $SPIKE_YAML_CAPTURE), runs the suites, and reverts the patch whatever happens.
# The cross-language journey is left out: it needs both SDKs built and drives
# the same verbs the e2e suite does.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
here="$root/spikes/saphyr-diff"
out="$here/corpus/captured"
cd "$root"
git apply "$here/capture.patch"
trap 'git apply -R "$here/capture.patch"' EXIT
rm -rf "$out" && mkdir -p "$out"
cargo build -q -p onemessagebus-cli -p onemessagebus-e2e --bins
# Two runs: a `--test` filter applies to every package it is given with, so the
# core and CLI suites (every target) run apart from the e2e crate's two journeys.
# A failing test still captured what it parsed; the status is reported, not fatal.
{
  SPIKE_YAML_CAPTURE="$out" cargo test -p onemessagebus -p onemessagebus-cli --no-fail-fast \
    || echo "capture: core/CLI tests failed" >&2
  SPIKE_YAML_CAPTURE="$out" cargo test -p onemessagebus-e2e --test e2e --test plugin_transport --no-fail-fast \
    || echo "capture: e2e tests failed" >&2
} >"$here/capture.log" 2>&1
grep -E '^test result:' "$here/capture.log" | awk '{p+=$4; f+=$6} END {printf "capture: %d tests passed, %d failed\n", p, f}'

# The texts carry this checkout's path, each run's temporary directories and
# its loopback ports;
# write both as fixed names, and name each file by its digest after that, so a
# text two runs differ in only by those is kept once.
find "$out" -type f -exec sed -i -E "s|$root|/repo|g; s|/tmp/\.tmp[A-Za-z0-9]+|/tmp/scratch|g; s|127\.0\.0\.1:[0-9]+|127.0.0.1:8443|g" {} +
for file in "$out"/*/*.yaml; do
  digest=$(sha256sum "$file" | cut -c1-16)
  [ "$file" = "${file%/*}/$digest.yaml" ] || mv -f "$file" "${file%/*}/$digest.yaml"
done
for kind in config filter predicate; do
  printf '%s: %s distinct texts\n' "$kind" "$(find "$out/$kind" -type f 2>/dev/null | wc -l)"
done
