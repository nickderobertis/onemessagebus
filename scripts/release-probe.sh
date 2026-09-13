#!/usr/bin/env bash
# What a public registry currently serves for one release target.
#
# The one interface a consumer sequences work on: an orchestrator that landed a
# fix here and needs to know whether the artifact carrying it is downloadable
# yet runs this and reads the answer. Three answers, and they are the whole
# contract:
#
#   exit 0, one line on stdout   the version that registry serves right now
#   exit 0, nothing on stdout    the registry has no release of it yet
#   any non-zero exit            NOT ANSWERED — the reason is on stderr
#
# "Not answered" and "no release yet" are different answers and stay different
# all the way out. A consumer holds indefinitely on the first and must never read
# it as evidence that a release has not happened, so every ambiguity here — an
# identifier this does not recognise, a registry that answered something other
# than "here it is" or "no such thing", a document whose version this cannot
# pick out unambiguously — is a non-zero exit rather than empty output. Empty
# output is only ever produced by a registry saying, in as many words, that the
# artifact does not exist.
#
# Called as a direct subprocess with no shell interposed, from the repository
# root, with an environment carrying only PATH and HOME (and their two Windows
# equivalents). It therefore takes no credential and reads no variable of its
# own: every artifact this repository publishes is on a public registry, so an
# unauthenticated read is all it needs and all it may need. Nor can anything in
# that environment change an answer: curl runs with `--disable`, so a `.curlrc`
# under HOME cannot turn a "no release yet" into a "not answered". It must answer
# well inside sixty seconds, which is what the timeouts below are sized for.
#
# Identifiers are registry-qualified — `crate:<name>`, `pypi:<name>`,
# `npm:<name>` — because the qualification is load-bearing rather than
# decorative: sibling repositories publish one name to two registries on two
# cadences, so an unqualified name is two different artifacts and a consumer
# waiting on it cannot say which one it got.
#
# The registry is what is recognised, not the name: this answers for any name a
# registry serves, so a consumer never has to know which repository publishes
# what. `release-targets.toml` is where *this* repository declares which
# identifiers are its own, and npm/test/release-targets.test.mjs holds that
# declaration against the release configuration.
#
# Scoped npm names (`@scope/name`) are deliberately not recognised: the packages
# here are unscoped on purpose — a `@scope/` name needs an npm organization a
# publish token cannot create (see scripts/npm-build.mjs) — so supporting them
# would add a URL-encoding path nothing in this repository can exercise.
set -euo pipefail

readonly NOT_ANSWERED=3
readonly USER_AGENT="onemessagebus-release-probe (https://github.com/nickderobertis/onemessagebus)"
# One attempt, well inside the sixty seconds the contract allows even when it
# burns its whole budget. Deliberately not retried in here: what calls this is a
# consumer waiting for a release, so it is already a loop — a second attempt
# inside would only make each of its polls slower, and hide a registry that is
# down behind a longer silence.
readonly MAX_TIME=15

# The third answer. Every exit through here is non-zero and carries its reason
# and a next action on stderr; nothing is written to stdout, because a caller
# reads stdout as the answer.
not_answered() {
  echo "release-probe: $1" >&2
  echo "ACTION: $2" >&2
  exit "$NOT_ANSWERED"
}

if [ "$#" -ne 1 ]; then
  not_answered "takes exactly one argument, got $#" \
    "run 'release-probe.sh <registry>:<name>', e.g. 'release-probe.sh crate:onemessagebus'"
fi

identifier="$1"
registry="${identifier%%:*}"
name="${identifier#*:}"

case "$identifier" in
  *:*) ;;
  *) not_answered "'$identifier' is not registry-qualified" \
       "qualify it: 'crate:$identifier', 'pypi:$identifier', or 'npm:$identifier'" ;;
esac

# The charset every name this repository publishes is drawn from, on all three
# registries. Anything else is a name this cannot build a URL for without
# guessing at an encoding, which is not answered rather than probed.
if ! printf '%s' "$name" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$'; then
  not_answered "'$name' is not a registry package name this recognises" \
    "pass a name of letters, digits, '.', '_' and '-' — scoped npm names are not supported"
fi

command -v curl >/dev/null 2>&1 || not_answered "no 'curl' on PATH" \
  "install curl — this reads the registries over HTTPS and has no other transport"

no_scratch() {
  not_answered "cannot create a scratch file in ${TMPDIR:-/tmp}" \
    "free space there or point TMPDIR at a writable directory, then re-run; this is NOT evidence that nothing is published"
}
body="$(mktemp)" || no_scratch
curl_err="$(mktemp)" || { rm -f "$body"; no_scratch; }
trap 'rm -f "$body" "$curl_err"' EXIT

# GET `$1`, leaving the response body in `$body` and printing the HTTP status.
#
# `--disable` first, so the home directory's `.curlrc` cannot change an answer: a
# caller who happens to have `--fail` there would otherwise turn every 404 into a
# transport error, and "no release yet" into "not answered". curl is left
# un-`--fail`ed here for the same reason — a 404 is an *answer*, and --fail would
# collapse it into the exit a dead network produces.
fetch() {
  local url="$1" status
  if status="$(curl --disable --silent --show-error --location \
    --user-agent "$USER_AGENT" \
    --connect-timeout 5 --max-time "$MAX_TIME" \
    --output "$body" --write-out '%{http_code}' \
    "$url" 2>"$curl_err")"; then
    printf '%s' "$status"
    return 0
  fi
  not_answered "could not reach $url: $(tr -d '\r\n' <"$curl_err")" \
    "check network access to the registry, then re-run; this is NOT evidence that nothing is published"
}

# llmlint: ignore-block[boundary_inputs_validated] the registry document is read with grep and sed because the probe is a contract consumers run with bash and curl alone — the suite spawns it with nothing but a search path and a home directory — so no JSON parser can be assumed on their hosts. The input is still validated before it becomes an answer: each reader takes a key only when it occurs exactly once or the probe answers "not answered", and whatever it extracts must match VERSION_SHAPE below before it is printed.
# The value of a `"key": "value"` pair that occurs exactly once in `$2`.
#
# JSON escapes an embedded quote as `\"`, so this byte sequence cannot occur
# inside a string *value* — only as a real key. Exactly one occurrence or a
# failure: a document where the key appears twice, or not at all, is one this
# cannot read unambiguously, and every caller turns that into "not answered"
# rather than into an empty answer. Exactly one is also what keeps the answer to
# one line, which is the whole of what a caller parses.
json_string() {
  local key="$1" matches
  matches="$(printf '%s' "$2" | grep -Eo "\"$key\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" || true)"
  [ "$(printf '%s' "$matches" | grep -c .)" = "1" ] || return 1
  printf '%s' "$matches" | sed -E 's/^.*:[[:space:]]*"(.*)"$/\1/'
}

# The one `"key": { ... }` object in `$2`, on the same exactly-one terms — a
# brace-counting parser is not worth writing for a flat tag map, but reading the
# *first* of several would be guessing, and guessing is what this file does not do.
json_object() {
  local key="$1" matches
  matches="$(printf '%s' "$2" | grep -Eo "\"$key\"[[:space:]]*:[[:space:]]*\{[^}]*\}" || true)"
  [ "$(printf '%s' "$matches" | grep -c .)" = "1" ] || return 1
  printf '%s' "$matches"
}
# llmlint: ignore-end[boundary_inputs_validated]

case "$registry" in
  # crates.io's own summary of the crate, which is where "what a `cargo add`
  # resolves" is stated rather than inferred: the sparse index lists every
  # version in publish order, which is not the same question.
  crate) url="https://crates.io/api/v1/crates/$name" ;;
  pypi) url="https://pypi.org/pypi/$name/json" ;;
  npm) url="https://registry.npmjs.org/$name" ;;
  *)
    not_answered "unknown registry '$registry' in '$identifier'" \
      "qualify the name with one of: crate, pypi, npm"
    ;;
esac

status="$(fetch "$url")"
case "$status" in
  200) ;;
  # The registry said, in as many words, that there is no such artifact. This is
  # the only route to an empty answer, and it is the one status all three
  # registries answer an unpublished name with — nothing else is accepted as
  # meaning "no release", because guessing which other statuses might mean it is
  # how a released artifact ends up reading as unreleased.
  404) exit 0 ;;
  *)
    not_answered "$url answered HTTP $status" \
      "re-run in a moment — a rate limit or an outage is not evidence that nothing is published"
    ;;
esac

document="$(cat "$body")"
case "$registry" in
  crate)
    # What `cargo add` resolves is the newest non-prerelease; a crate whose every
    # release is a prerelease has no such version and falls back to the newest.
    version="$(json_string max_stable_version "$document" || true)"
    [ -n "$version" ] || version="$(json_string newest_version "$document" || true)"
    ;;
  pypi)
    # `info.version` — the release the project page and a bare `pip install`
    # resolve to.
    version="$(json_string version "$document" || true)"
    ;;
  npm)
    # `dist-tags.latest` — what a bare `npm install <name>` resolves to. Read out
    # of the dist-tags object rather than the packument at large, which need not
    # carry only this tag. A packument with no dist-tags at all — npm serves one
    # for a name whose every version was unpublished — leaves both of these empty
    # and falls to the guard below, rather than out through `set -e` with nothing
    # said.
    tags="$(json_object dist-tags "$document" || true)"
    version="$(json_string latest "$tags" || true)"
    ;;
esac

if [ -z "${version:-}" ]; then
  not_answered "$url answered, but no version could be read from it unambiguously" \
    "the registry's response shape changed — fix the reader in this script; a released artifact must never read as unreleased"
fi

# What came out of the document is still registry-supplied text, and a caller
# parses stdout as the answer. So the answer is only ever one version value —
# never a JSON escape, a tag name, or a second line — and anything else is not
# answered rather than passed through for a consumer to misread. Bash's own regex
# rather than `grep`, which matches per line and would accept a value with one
# good line in it. The shape is SemVer 2.0.0's: numbers without leading zeros,
# then an optional prerelease and an optional build, each a dot-separated run of
# non-empty identifiers — so `1.2.3-.`, `1.2.3-a..b` and `1.2.3-01` are not
# versions, however much of them looks like one.
readonly NUMBER='(0|[1-9][0-9]*)'
readonly PRERELEASE_ID='(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
readonly BUILD_ID='[0-9A-Za-z-]+'
readonly VERSION_SHAPE="^$NUMBER\\.$NUMBER\\.$NUMBER(-$PRERELEASE_ID(\\.$PRERELEASE_ID)*)?(\\+$BUILD_ID(\\.$BUILD_ID)*)?\$"
if ! [[ $version =~ $VERSION_SHAPE ]]; then
  not_answered "$url answered with '$version', which is not a version" \
    "the registry served something this cannot read as one version — fix the reader in this script; a released artifact must never read as unreleased"
fi

printf '%s\n' "$version"
