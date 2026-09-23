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
#
# SHOTS_OUT comes from outside and the capture starts by emptying it, so bound it
# first: a path inside this checkout, named without walking out of it. Everything
# that sets it — the guard, the reusable workflow, this default — already does.
arch="$(bash "$repo_root/screenshots/host-arch.sh")"
SHOTS_OUT="${SHOTS_OUT:-shots/current/$arch}"
case "$SHOTS_OUT" in
"" | /* | *..*)
  echo "screenshots: SHOTS_OUT must be a path inside this checkout, written" >&2
  echo "             relative to its root and without '..'; the capture empties" >&2
  echo "             it before it writes. Got: ${SHOTS_OUT:-<empty>}" >&2
  echo "             Unset it for the default, shots/current/$arch." >&2
  exit 1
  ;;
esac
if [ -e "$SHOTS_OUT" ] && [ ! -d "$SHOTS_OUT" ]; then
  echo "screenshots: SHOTS_OUT must name a directory to capture into;" >&2
  echo "             $SHOTS_OUT is not one. Unset it or point it elsewhere." >&2
  exit 1
fi

font="$repo_root/screenshots/fonts/JetBrainsMono-Regular.ttf"
fixture="$repo_root/screenshots/fixture"
docs_dir="$repo_root/docs/screenshots"

if ! command -v freeze >/dev/null 2>&1; then
  echo "screenshots: 'freeze' not on PATH. Install the pinned version with:" >&2
  echo "             just screenshots-tools" >&2
  exit 1
fi

# The binary the scenes drive: release, the way a user runs it.
bus="${ONEMESSAGEBUS_BIN:-$repo_root/target/release/onemessagebus}"
if [ -z "${SCREENSHOTS_NO_BUILD:-}" ]; then
  cargo build --release --locked -p onemessagebus-cli >&2
fi
if [ ! -x "$bus" ]; then
  echo "screenshots: no onemessagebus binary at $bus" >&2
  if [ -n "${SCREENSHOTS_NO_BUILD:-}" ]; then
    echo "             SCREENSHOTS_NO_BUILD is set, so nothing built it. Unset it," >&2
    echo "             or point ONEMESSAGEBUS_BIN at a binary you already have." >&2
  else
    echo "             Build it: cargo build --release -p onemessagebus-cli" >&2
  fi
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

config="$(bash "$repo_root/screenshots/stage-fixture.sh" "$work")"
[ "$config" = "$work/bus.yaml" ] || {
  echo "screenshots: the fixture was staged somewhere the scenes do not read;" >&2
  echo "             screenshots/stage-fixture.sh answered $config, not" >&2
  echo "             $work/bus.yaml. Reconcile the two." >&2
  exit 1
}

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

# Every scene's prompt line is DERIVED from the argv that was run, never written
# beside it: `show` is what both prints and runs, so a transcript cannot come to
# quote a flag the capture did not pass. It folds an over-wide command the way a
# person would, with a trailing `\` and a four-space continuation.
scene=""
emit() { scene+="$1"$'\n'; }

# One argv word as a shell would need it typed. Deriving the prompt from the argv
# is only honest if what it prints would run: a `--filter` document is a word to
# `run` and a brace expansion to a reader who pastes it unquoted.
shell_quote() {
  case "$1" in
  "") printf "''" ;;
  *[!A-Za-z0-9_@%+=:,./-]*) printf "'%s'" "${1//\'/\'\\\'\'}" ;;
  *) printf '%s' "$1" ;;
  esac
}

# Emit already-quoted words under `prefix`, folding an over-wide command the way
# a person would: a trailing `\` and a four-space continuation.
fold_words() {
  local prefix="$1"
  shift
  local budget=$((100 - ${#prefix})) out="" word
  for word in "$@"; do
    if [ -n "$out" ] && [ $((${#out} + ${#word} + 1)) -gt "$budget" ]; then
      emit "$prefix$out \\"
      prefix="    "
      budget=96
      out="$word"
    else
      out="${out:+$out }$word"
    fi
  done
  emit "$prefix$out"
}

# The `$ ` line for `onemessagebus <argv>`, optionally piped a document.
prompt() {
  local input="$1"
  shift
  local words=("onemessagebus") word
  for word in "$@"; do words+=("$(shell_quote "$word")"); done
  if [ -n "$input" ]; then
    emit "\$ echo $(shell_quote "$input") |"
    fold_words '    ' "${words[@]}"
  else
    fold_words '$ ' "${words[@]}"
  fi
}

# Print the command and append what running it said. `$SHOW_INPUT` is piped to it
# and shown above it; `$SHOW_STATUS` also appends the exit code a shell would
# report, which is how the refusal scenes show the `0`/`1`/`2` table.
show() {
  local input="${SHOW_INPUT:-}" out status
  prompt "$input" "$@"
  set +e
  if [ -n "$input" ]; then
    out="$(printf '%s' "$input" | run "$@" 2>&1)"
  else
    out="$(run "$@" 2>&1)"
  fi
  status=$?
  set -e
  emit "$out"
  if [ -n "${SHOW_STATUS:-}" ]; then
    emit '$ echo $?'
    emit "$status"
  fi
}

# Render the scene buffer to an SVG, hash it, record it, and copy the committed
# README/gallery copy beside it.
render() {
  local name="$1" image="$2"
  local src="$work/$name.txt"
  printf '%s' "$scene" >"$src"
  if [ ! -s "$src" ]; then
    echo "screenshots: scene '$name' produced no output — cannot render." >&2
    echo "             Run that scene's commands by hand over a staged fixture" >&2
    echo "             (bash screenshots/stage-fixture.sh \"\$(mktemp -d)\") and" >&2
    echo "             see what the verb says." >&2
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

# The two stream fixtures and the malformed record, beside the configuration, so
# every scene's argv names them the short way a reader would type.
cp "$fixture/checkout.ndjson" "$fixture/fulfilment.ndjson" "$work/"
cp "$fixture/draft-question.json" "$work/draft.json"

# The question is raised by an `ask` left running with piped stdio — the way
# tests/e2e/ask.rs holds one open — and answered from a second invocation over
# the same transport directory, which is what the two-shell transcript shows.
question='{"kind":"question","message":"which base do I fork from?","source":"proposal"}'
verdict='{"version":3,"completion":true,"reason":"main; the release branch is closed"}'
asking_argv=(ask questions --blocking --asker worker-1 --timeout 60 --config bus.yaml)
(
  cd "$work" \
    && printf '%s' "$question" | "$bus" "${asking_argv[@]}" \
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
  {
    echo "screenshots: the ask never printed a correlation, so the question never"
    echo "             reached the queue and there is nothing to capture. What it"
    echo "             said:"
    sed 's/^/             /' "$work/ask.err"
    echo "             Raise the same question by hand over a staged fixture"
    echo "             (bash screenshots/stage-fixture.sh \"\$(mktemp -d)\")."
  } >&2
  exit 1
fi
reply_argv=(reply questions --correlation "$correlation" --config bus.yaml)
printf '%s' "$verdict" | run "${reply_argv[@]}" >/dev/null
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

prompt "$question" "${asking_argv[@]}"
emit "$(cat "$work/ask.err")"
emit ""
emit "# ...waiting. Meanwhile, in another shell, the lead answers that correlation:"
emit "#   echo '$verdict' |"
shown_reply=()
for word in "${reply_argv[@]}"; do shown_reply+=("$(shell_quote "$word")"); done
emit "#     onemessagebus ${shown_reply[*]}"
emit ""
emit "$(cat "$work/ask.out")"
render ask ask.svg

# `next` claims before `status` reads, so a cursor in that view has moved.
note='{"kind":"note","message":"the nightly sweep is green","source":"sweep","blocking":false}'
SHOW_INPUT="$note" show send questions --config bus.yaml
show next answers --format text --config bus.yaml
show status --format text --config bus.yaml
render queues queues.svg

show events merge checkout.ndjson fulfilment.ndjson --format text
emit ""
show events merge checkout.ndjson fulfilment.ndjson --format text \
  --filter '{"include":[{"source":"fulfilment"}]}'
render events-merge events-merge.svg

show schema list --format text --config bus.yaml
emit ""
SHOW_STATUS=1 show schema check desk.question@1 --file draft.json --config bus.yaml
render schema schema.svg

frame='{"op":"quote","sku":"KB-118","units":2}'
SHOW_INPUT="$frame" show serve questions --codec checkout --config bus.yaml
render serve serve.svg

SHOW_STATUS=1 show ask questions --timeout soon --config bus.yaml
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
