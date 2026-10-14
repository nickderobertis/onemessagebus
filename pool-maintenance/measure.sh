#!/usr/bin/env bash
# The measurement behind onemessagebus-pool-maintenance-seconds (budgets.yaml):
# one steady-state pass of the declared pool maintenance, `just pool-maintenance`,
# whose figure the pruner itself reports. Steady state is the pass after one
# ordinary run's worth of superseded builds, never a first pass over accumulated
# history, so this clone's target dir is put in that state before anything is
# timed:
#   (a) the declared maintenance, untimed, clears any backlog;
#   (b) the gate's test build runs, so that what the next build creates is only
#       what its configuration changes; the same build runs under an alternative
#       configuration giving this repository's own crates new metadata hashes,
#       and every entry that build created is backdated past the 2-day window,
#       standing in for a run's builds that have since been superseded;
#   (c) the ordinary gate build runs again, so each unit's newest build is the
#       ordinary one;
#   (d) one declared pass is timed: the only run that reports through
#       ONEBUDGETSPEC_RESULT. The backlog pass's seconds join its detail as
#       separate evidence, never the figure.
# Then the pass is held to what it was set up to remove: it removed builds, all
# of them the alternative build's, and every entry of the ordinary build
# survived. The entry listings compared, by metadata hash, are left in
# .logs/pool-maintenance/.
#
# It really prunes and builds; run it only in a checkout you own.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
evidence="$root/.logs/pool-maintenance"
fail() {
  echo "pool-maintenance/measure.sh: $1" >&2
  exit 1
}

# The crates whose `test` targets the gate builds, each built as `_crate-test`
# builds it, with the gate's own RUSTFLAGS: a different flag set would give every
# unit a new hash, and the ordinary build would not be the gate's.
crates=(onemessagebus onemessagebus-cli onemessagebus-e2e onemessagebus-repo)
RUSTFLAGS="$(just --evaluate RUSTFLAGS)"
export RUSTFLAGS CARGO_TERM_QUIET=true
# A profile override on the core crate alone: it and every crate of this
# repository built over it take new metadata hashes in the same profile
# directory, while every registry dependency keeps the ordinary build's.
alternative=(--config 'profile.dev.package.onemessagebus.debug-assertions=false')
# Past the recipe's `--keep-within 2d`.
superseded_at='3 days ago'

gate_build() {
  local crate
  for crate in "${crates[@]}"; do
    cargo llvm-cov --no-report nextest -p "$crate" --locked --no-tests=pass -E 'none()' "$@" \
      >>"$evidence/build.log" 2>&1 \
      || fail "the $crate test build failed — see $evidence/build.log"
  done
}

# Every build entry the pruner selects among — the top level of each profile
# directory's deps/, .fingerprint/, build/ and incremental/ — one path a line.
entries() {
  [ -d target ] || return 0
  local fingerprint profile dir
  find target -maxdepth 4 -type d -name .fingerprint -prune -print | while IFS= read -r fingerprint; do
    profile="${fingerprint%/.fingerprint}"
    for dir in deps .fingerprint build incremental; do
      if [ -d "$profile/$dir" ]; then
        find "$profile/$dir" -mindepth 1 -maxdepth 1
      fi
    done
  done | LC_ALL=C sort
}

seconds_since() {
  awk -v start="$1" -v end="$(date +%s.%N)" 'BEGIN { printf "%.2f", end - start }'
}

command -v cargo-prune-superseded >/dev/null || fail "cargo-prune-superseded not installed — run 'just bootstrap'"
mkdir -p "$evidence"
chmod 700 "$evidence"
: >"$evidence/build.log"

# (a)
started="$(date +%s.%N)"
env -u ONEBUDGETSPEC_RESULT just pool-maintenance >"$evidence/backlog-pass.txt" \
  || fail "the backlog pass failed — see $evidence/backlog-pass.txt"
backlog_seconds="$(seconds_since "$started")"

# (b)
gate_build
entries >"$evidence/before-alternative.txt"
gate_build "${alternative[@]}"
entries >"$evidence/after-alternative.txt"
LC_ALL=C comm -13 "$evidence/before-alternative.txt" "$evidence/after-alternative.txt" >"$evidence/alternative.txt"
[ -s "$evidence/alternative.txt" ] \
  || fail "the alternative build created nothing in target/: its builds are already there, newer than the window (an interrupted run leaves them) — delete target/ and run this again"
while IFS= read -r entry; do
  find "$entry" -exec touch -h -d "$superseded_at" {} +
done <"$evidence/alternative.txt"

# (c)
gate_build
entries >"$evidence/before-pass.txt"
LC_ALL=C comm -23 "$evidence/before-pass.txt" "$evidence/alternative.txt" >"$evidence/ordinary.txt"

# (d)
just pool-maintenance >"$evidence/timed-pass.txt" || fail "the timed pass failed — see $evidence/timed-pass.txt"
entries >"$evidence/after-pass.txt"
LC_ALL=C comm -23 "$evidence/before-pass.txt" "$evidence/after-pass.txt" >"$evidence/removed.txt"

[ -s "$evidence/removed.txt" ] \
  || fail "the timed pass removed nothing, so it timed no steady-state pass — see $evidence/timed-pass.txt"
stray="$(LC_ALL=C comm -23 "$evidence/removed.txt" "$evidence/alternative.txt")"
[ -z "$stray" ] || fail "the timed pass removed entries the alternative build did not create:
$stray"
lost="$(LC_ALL=C comm -23 "$evidence/ordinary.txt" "$evidence/after-pass.txt")"
[ -z "$lost" ] || fail "the timed pass removed entries of the ordinary build:
$lost"

if [ -n "${ONEBUDGETSPEC_RESULT:-}" ]; then
  [ -s "$ONEBUDGETSPEC_RESULT" ] || fail "the timed pass recorded no result in $ONEBUDGETSPEC_RESULT — see $evidence/timed-pass.txt"
  # The value is the pruner's and stays untouched; only the detail gains the
  # backlog pass. Single-quoted on purpose: its `${}` are JavaScript's.
  # shellcheck disable=SC2016
  node -e '
    const fs = require("node:fs");
    const [path, seconds] = process.argv.slice(1);
    const result = JSON.parse(fs.readFileSync(path, "utf8"));
    const backlog = `untimed backlog pass before it (not in the figure): ${seconds} s`;
    result.detail = result.detail ? `${result.detail}; ${backlog}` : backlog;
    fs.writeFileSync(path, JSON.stringify(result));
  ' "$ONEBUDGETSPEC_RESULT" "$backlog_seconds"
fi

cat "$evidence/timed-pass.txt" >&2
echo "pool-maintenance/measure.sh: the timed pass removed $(wc -l <"$evidence/removed.txt") of the alternative build's $(wc -l <"$evidence/alternative.txt") entries and kept all $(wc -l <"$evidence/ordinary.txt") others; untimed backlog pass ${backlog_seconds} s; listings in $evidence" >&2
