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
# A failing test still captured what it parsed; the run's status is reported, not fatal.
SPIKE_YAML_CAPTURE="$out" cargo test -q -p onemessagebus -p onemessagebus-cli \
  -p onemessagebus-e2e --test e2e --test plugin_transport --no-fail-fast >"$here/capture.log" 2>&1 \
  || echo "capture: some tests failed; see $here/capture.log" >&2
# The texts carry this checkout's path; keep the committed corpus free of it.
find "$out" -type f -exec sed -i "s|$root|/repo|g" {} +
for kind in config filter predicate; do
  printf '%s: %s distinct texts\n' "$kind" "$(find "$out/$kind" -type f 2>/dev/null | wc -l)"
done
