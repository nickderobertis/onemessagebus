#!/usr/bin/env bash
# Smoke-test an `onemessagebus` that is already on PATH, and name the install
# that broke when it does not behave.
#
# One script, one set of assertions. `release.yml`'s verify jobs and
# `published-smoke.yml` run this over a binary they installed from PyPI or npm;
# CI's `install` job runs the identical file over the binary this repo just
# compiled. That is what stops a workflow's idea of "it works" from drifting from
# what actually ships — assertions inlined in a workflow keep passing after the
# surface around them changes.
#
# Deliberately toolchain-free: bash and the installed binary. The published smoke
# runs this on every OS, for both registries, each time a release completes, and
# anything it had to install first would be a second thing that can rot.
#
# What a published artifact is held to here is what it can prove *alone*: it
# reports its version, prints the documented verbs, lists the schemas the
# profile registers (so it can read its own registry), refuses a payload it
# cannot read with exit 2 and nothing on stdout, and merges a stream it wrote.
# The e2e suite drives every verb for real; this proves the artifact that ships
# is the one the suite tested.
set -euo pipefail

expect_version=""
label="installed onemessagebus"

fail() {
  echo "::error::$label: $1" >&2
  echo "ACTION: $2" >&2
  exit 1
}

# Every option takes a value, so a missing one is an argument error rather than
# a silently empty setting.
need_value() {
  if [ "$#" -lt 2 ]; then
    echo "$1 needs a value" >&2
    echo "ACTION: pass it as '$1 <value>'" >&2
    exit 2
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --expect-version) need_value "$@"; expect_version="$2"; shift 2 ;;
    # What installed the binary, so a red matrix leg names the platform and the
    # registry rather than only the assertion that failed.
    --label) need_value "$@"; label="$2"; shift 2 ;;
    *)
      echo "unknown option $1" >&2
      echo "ACTION: run 'smoke-published.sh [--expect-version V] [--label TEXT]'" >&2
      exit 2
      ;;
  esac
done

# One scratch file for a probe's stderr, so a failure report can carry the
# binary's own diagnostic rather than only the assertion that tripped.
probe_stderr="$(mktemp)"
trap 'rm -f "$probe_stderr"' EXIT

command -v onemessagebus >/dev/null 2>&1 || fail "no 'onemessagebus' on PATH" \
  "install it first — 'pip install onemessagebus-cli' or 'npm install -g onemessagebus-cli'"

# Windows ships the same bytes with CRLF once anything touches them, so strip CR
# rather than let a line ending decide the verdict.
#
# Each probe carries its own `|| fail`: under `set -e` an install that cannot run
# at all would otherwise end the script on the binary's exit status, with no
# cause and no next action — the report a broken artifact most needs.
reported="$(onemessagebus --version 2>"$probe_stderr" | tr -d '\r')" || fail \
  "'--version' failed: $(cat "$probe_stderr")" \
  "the installed binary cannot run at all — reinstall it, and check the platform package matches this machine"
if [ -n "$expect_version" ] && [ "$reported" != "onemessagebus $expect_version" ]; then
  fail "reports '$reported', not 'onemessagebus $expect_version'" \
    "the install resolved a different version than the one just published — wait for the registry to serve $expect_version, reinstall it by exact version, and re-run"
fi

help="$(onemessagebus --help 2>"$probe_stderr" | tr -d '\r')" || fail \
  "'--help' failed: $(cat "$probe_stderr")" \
  "the installed binary runs but cannot print its own surface — reinstall this version and re-run"
for command in schema events; do
  case "$help" in
    *"$command"*) ;;
    *) fail "'--help' does not list the '$command' command" \
         "the installed binary does not carry the documented command surface — check 'command -v onemessagebus' is this install rather than an older one earlier on PATH, then reinstall" ;;
  esac
done

# The profile's registry travels inside the binary, and listing it is what
# proves the artifact can read a schema rather than only parse argv.
listed="$(onemessagebus schema list --format text 2>"$probe_stderr" | tr -d '\r')" || fail \
  "'schema list' failed: $(cat "$probe_stderr")" \
  "the installed binary cannot construct its own registry — reinstall this version and re-run"
case "$listed" in
  *"agent.event-envelope@2"*) ;;
  *) fail "'schema list' does not name agent.event-envelope@2" \
       "the installed binary does not carry the agent profile's registry — check 'command -v onemessagebus' is this install, then rebuild from crates/onemessagebus-cli, which links the profile" ;;
esac

# A payload file that is not there is exit 2, and nothing on stdout: a caller
# reads a line on stdout as a document, so a refusal must not produce one.
code=0
out="$(onemessagebus schema check agent.labels@1 --file no-such-payload.json 2>"$probe_stderr")" || code=$?
why="$(cat "$probe_stderr")"
if [ "$code" -ne 2 ]; then
  fail "'schema check' on a missing payload exited $code, not 2: $why" \
    "reinstall this version and re-run; if it still does, the published artifact is not the revision CI gated — re-cut the release"
fi
if [ -n "$out" ]; then
  fail "'schema check' wrote to stdout while refusing a missing payload" \
    "reinstall this version and re-run; if it still does, revert the change that made a refusal write to stdout"
fi

# `events emit` then `events merge` needs nothing else installed, so it is what
# proves the artifact can write and read a stream rather than only its registry.
work="$(mktemp -d)"
trap 'rm -rf "$work" "$probe_stderr"' EXIT
if ! why="$(printf '{"smoke":true}' | onemessagebus events emit "$work/smoke.ndjson" --kind smoke --stream smoke --source pipeline 2>&1 >/dev/null)"; then
  fail "'events emit' refused a payload it should write: $why" \
    "fix what that refusal names, or reinstall — an install that cannot append a stream is truncated or from the wrong revision"
fi
merged="$(onemessagebus events merge "$work/smoke.ndjson" 2>"$probe_stderr" | tr -d '\r')" || fail \
  "'events merge' failed over a stream this binary wrote: $(cat "$probe_stderr")" \
  "reinstall this version and re-run; a binary that cannot read what it wrote is not the revision CI gated"
case "$merged" in
  *'"kind":"smoke"'*'"seq":1'*|*'"seq":1'*'"kind":"smoke"'*) ;;
  *) fail "'events merge' did not print the envelope 'events emit' wrote: $merged" \
       "reinstall this version and re-run" ;;
esac

echo "$label: surface smoke test passed${expect_version:+ for $expect_version}"
