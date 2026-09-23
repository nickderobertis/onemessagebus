#!/usr/bin/env bash
# Write the screenshot capture screencomp gates: $SHOTS_OUT/captures.json and one
# SVG per scene, copied to docs/screenshots/ for the README. What the scenes are,
# why each is here and what the capture is pinned to: screenshots/AGENTS.md.
#
# Needs `freeze` on PATH (bash screenshots/install-freeze.sh) and builds the
# release binary it drives.
set -euo pipefail

# Byte-determinism starts with the environment. The queue verbs read
# ONEMESSAGEBUS_CONFIG, ONEMESSAGEBUS_TRANSPORT_DIR, ONEMESSAGEBUS_REGISTRY and
# the schema-cache settings ahead of this capture's own flags, so a shell that
# exports one would steer a scene away from the fixture and drift its hash
# against a baseline captured in a clean shell. Clear every one of them, exactly
# as tests/e2e/support.rs clears them for the journeys.
for _var in $(compgen -e); do
  case "$_var" in
  ONEMESSAGEBUS_*) unset "$_var" ;;
  esac
done
unset _var

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# This host's capture lane (shots/current/<arch>), named the same way the
# pre-push guard and `just screenshots-bless` name it. CI overrides SHOTS_OUT.
arch="$(bash "$repo_root/screenshots/host-arch.sh")"
SHOTS_OUT="${SHOTS_OUT:-shots/current/$arch}"
if [ -e "$SHOTS_OUT" ] && [ ! -d "$SHOTS_OUT" ]; then
  echo "screenshots: SHOTS_OUT must name a directory to capture into;" >&2
  echo "             $SHOTS_OUT is not one. Unset it or point it elsewhere." >&2
  exit 1
fi

font="$repo_root/screenshots/fonts/JetBrainsMono-Regular.ttf"
fixture="$repo_root/screenshots/fixture"
desk="$repo_root/crates/onemessagebus-e2e/tests/layouts/desk.json"
docs_dir="$repo_root/docs/screenshots"

if ! command -v freeze >/dev/null 2>&1; then
  echo "screenshots: 'freeze' not on PATH. Install the pinned version with:" >&2
  echo "             just screenshots-tools" >&2
  exit 1
fi

# The binary the scenes drive: release, the way a user runs it.
bus="${ONEMESSAGEBUS_BIN:-$repo_root/target/release/onemessagebus}"
if [ -z "${SCREENSHOTS_NO_BUILD:-}" ] || [ ! -x "$bus" ]; then
  cargo build --release --locked -p onemessagebus-cli >&2
fi
if [ ! -x "$bus" ]; then
  echo "screenshots: no onemessagebus binary at $bus" >&2
  echo "             Build it: cargo build --release -p onemessagebus-cli" >&2
  exit 1
fi

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Deterministic freeze flags. The vendored font (embedded into each SVG as
# base64) is what makes the output reproducible across machines; the rest is
# fixed window styling. A FIXED window width keeps every card's text at the same
# size in the gallery and the README, which a per-scene auto-width does not:
# `--wrap 104` folds the few genuinely over-wide lines (a merged envelope
# carrying artifacts, the `ask` answer document) at the same column budget.
# 936px = 30+30 padding + 104 * ~8.42px per character.
freeze_flags=(
  # Force ANSI/terminal mode. freeze's content sniffing intermittently reads
  # plain CLI output as a source file and then ignores --font.file and reaches
  # for a font over the network; `--language ansi` is unconditional and offline.
  --language ansi
  --font.file "$font"
  --font.family "JetBrains Mono"
  --font.size 14
  --window
  --background "#0d1117"
  --padding "20,30"
  --margin 0
  --border.radius 8
  --width 936
  --wrap 104
)

rm -rf "$SHOTS_OUT"
mkdir -p "$SHOTS_OUT" "$docs_dir"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# captures.json identity is `name + JSON.stringify(toggles)`; entries collect one
# "name|toggles|hash|image" record per rendered scene, sorted at the end.
entries=()

# What `desk_config` stages for the journeys, plus the codec the `serve` scene
# answers and its frame bundle reached by a `file://` link — so even the link
# machinery runs with no HTTP.
queues="$work/bus"
cat >"$work/bus.yaml" <<YAML
version: 1
transport: {kind: local, dir: "$queues"}
profile: desk
schemas:
  - "$desk@1"
  - "file://$fixture/frames.json@1"
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

# Every scene runs from $work, so the paths a transcript quotes are the short
# relative ones a reader would type rather than this machine's temp directory.
run() { (cd "$work" && "$bus" "$@"); }

# Per-run values the bus mints, rewritten to fixed placeholders so the bytes are
# identical on every machine: the 32-hex correlation `ask` mints, and the
# epoch-millisecond instants the desk layout stamps onto a question and an
# answer. There is no clock-override or fixed-id switch on this CLI and this
# capture does not add one — a flag is a capability-manifest change first.
normalize() {
  sed -i \
    -e 's/c-[0-9a-f]\{32\}/c-4f3c1d92a08b47e6b1d5c0a7e93f2b18/g' \
    -e 's/"raised_at":[0-9]\{13\}/"raised_at":1789300000000/g' \
    -e 's/"at":[0-9]\{13\}/"at":1789300000000/g' \
    "$1"
}

# Append a command to the scene buffer as a shell transcript would show it: the
# first line under a `$ ` prompt, any continuation lines indented beneath it. The
# prompt lines are the argv this capture ran, verbatim; every line `emit` adds is
# the binary's own bytes.
scene=""
say() {
  scene+="\$ $1"$'\n'
  shift
  local line
  for line in "$@"; do scene+="    $line"$'\n'; done
}
emit() { scene+="$1"$'\n'; }

# Render the scene buffer to an SVG, hash it, record it, and copy the committed
# README/gallery copy beside it.
render() {
  local name="$1" image="$2"
  local src="$work/$name.txt"
  printf '%s' "$scene" >"$src"
  if [ ! -s "$src" ]; then
    echo "screenshots: scene '$name' produced no output — cannot render." >&2
    exit 1
  fi
  normalize "$src"
  # `< /dev/null`: freeze reads stdin whenever it is not a character device, so
  # under CI's piped stdin it would ignore the file argument and render nothing.
  freeze "$src" "${freeze_flags[@]}" -o "$SHOTS_OUT/$image" </dev/null >&2
  local hash
  hash="$(sha256 "$SHOTS_OUT/$image")"
  entries+=("$name|{}|$hash|$image")
  cp "$SHOTS_OUT/$image" "$docs_dir/$image"
  scene=""
}

# Run a verb that is expected to refuse, and append its output and the exit code
# a shell would then report — the `0`/`1`/`2` table docs/cli.md states.
emit_with_status() {
  local out status
  set +e
  out="$(run "$@" 2>&1)"
  status=$?
  set -e
  emit "$out"
  say 'echo $?'
  emit "$status"
}

# The question is raised by an `ask` left running with piped stdio — the way
# tests/e2e/ask.rs holds one open — and answered from a second invocation over
# the same transport directory, which is what the two-shell transcript shows.
question='{"kind":"question","message":"which base do I fork from?","source":"proposal"}'
verdict='{"version":3,"completion":true,"reason":"main; the release branch is closed"}'
(
  cd "$work" \
    && printf '%s' "$question" \
      | "$bus" ask questions --blocking --asker worker-1 --timeout 60 --config bus.yaml \
        >"$work/ask.out" 2>"$work/ask.err"
) &
asking=$!
# Wait for the correlation line rather than sleeping a guessed interval: it is
# printed the moment the question is on the queue, which is the event the reply
# below needs to have happened.
correlation=""
for _ in $(seq 1 600); do
  correlation="$(sed -n 's/^correlation: //p' "$work/ask.err" 2>/dev/null | head -n1)"
  [ -n "$correlation" ] && break
  sleep 0.1
done
if [ -z "$correlation" ]; then
  kill "$asking" 2>/dev/null || true
  echo "screenshots: the ask never printed a correlation; nothing to capture" >&2
  exit 1
fi
printf '%s' "$verdict" | run reply questions --correlation "$correlation" --config bus.yaml >/dev/null
# An `ask` that did not exit 0 answered something other than the reply — a
# timeout, an abandonment, a refusal — and rendering that as the resolved scene
# would publish a picture of a failure as the documented happy path.
if ! wait "$asking"; then
  {
    echo "screenshots: the ask did not resolve to its reply, so the 'ask' scene"
    echo "             would show the wrong answer. What it said:"
    sed 's/^/             /' "$work/ask.err" "$work/ask.out"
    echo "             Re-run the capture; if it repeats, the reply no longer"
    echo "             binds to the correlation the ask minted."
  } >&2
  exit 1
fi

say "echo '$question' |" \
  "onemessagebus ask questions --blocking --asker worker-1 --timeout 60 --config bus.yaml"
emit "$(cat "$work/ask.err")"
emit ""
emit "# ...waiting. Meanwhile, in another shell, the lead answers that correlation:"
emit "#   echo '$verdict' |"
emit "#     onemessagebus reply questions --correlation $correlation --config bus.yaml"
emit ""
emit "$(cat "$work/ask.out")"
render ask ask.svg

# `next` claims before `status` reads, so a cursor in that view has moved.
note='{"kind":"note","message":"the nightly sweep is green","source":"sweep","blocking":false}'
say "echo '$note' |" \
  "onemessagebus send questions --config bus.yaml"
emit "$(printf '%s' "$note" | run send questions --config bus.yaml)"
say "onemessagebus next answers --format text --config bus.yaml"
emit "$(run next answers --format text --config bus.yaml)"
say "onemessagebus status --format text --config bus.yaml"
emit "$(run status --format text --config bus.yaml)"
render queues queues.svg

say "onemessagebus events merge checkout.ndjson fulfilment.ndjson --format text"
emit "$("$bus" events merge "$fixture/checkout.ndjson" "$fixture/fulfilment.ndjson" --format text)"
emit ""
say "onemessagebus events merge checkout.ndjson fulfilment.ndjson --format text \\" \
  "--filter '{\"include\":[{\"source\":\"fulfilment\"}]}'"
emit "$("$bus" events merge "$fixture/checkout.ndjson" "$fixture/fulfilment.ndjson" \
  --filter '{"include":[{"source":"fulfilment"}]}' --format text)"
render events-merge events-merge.svg

say "onemessagebus schema list --format text --config bus.yaml"
emit "$(run schema list --format text --config bus.yaml)"
emit ""
say "onemessagebus schema check desk.question@1 --file draft.json --config bus.yaml"
emit_with_status schema check desk.question@1 --file "$fixture/draft-question.json" --config bus.yaml
render schema schema.svg

frame='{"op":"quote","sku":"KB-118","units":2}'
say "echo '$frame' |" \
  "onemessagebus serve questions --codec checkout --config bus.yaml"
emit "$(printf '%s' "$frame" | run serve questions --codec checkout --config bus.yaml)"
render serve serve.svg

say "onemessagebus ask questions --timeout soon --config bus.yaml"
emit_with_status ask questions --timeout soon --config bus.yaml
render refusal refusal.svg

# Write captures.json — shots sorted by identity, schema 1, trailing newline: the
# exact shape screencomp's classify/manifest/gallery read. Every field is safe
# ASCII (scene names, hex digests, file names), so plain printf is sound.
{
  printf '{\n  "schema": 1,\n  "shots": [\n'
  IFS=$'\n' sorted=($(printf '%s\n' "${entries[@]}" | sort))
  unset IFS
  last=$((${#sorted[@]} - 1))
  for i in "${!sorted[@]}"; do
    IFS='|' read -r name toggles hash image <<<"${sorted[$i]}"
    comma=","
    [ "$i" -eq "$last" ] && comma=""
    printf '    {\n      "name": "%s",\n      "toggles": %s,\n      "hash": "%s",\n      "image": "%s"\n    }%s\n' \
      "$name" "$toggles" "$hash" "$image" "$comma"
  done
  printf '  ]\n}\n'
} >"$SHOTS_OUT/captures.json"

echo "screenshots: wrote ${#entries[@]} shots to $SHOTS_OUT and docs/screenshots/" >&2
