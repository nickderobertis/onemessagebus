#!/usr/bin/env bash
# Durable output logs for the wrappers that capture their own output.
#
# Sourced, never executed. A script that is quiet on success has to put the
# output somewhere, and a `mktemp` file an EXIT trap removes takes the evidence
# with it exactly when a failed run is being diagnosed. A log at a deterministic
# path is readable *while* the command runs (`tail -f .logs/nx.log`) and still
# readable after it exits.
#
# One log per label, truncated per run: the newest run is the one worth keeping,
# and unbounded history inside a working tree is its own problem.
#
# "Per run" has to mean per *invocation*, though. `just check` runs the e2e suite
# through Nx, and those journeys spawn `scripts/nx.sh` again — so a nested
# invocation resolves the same `.logs/nx.log` the still-running outer one is
# writing and would truncate it, making the running gate uninspectable exactly
# when a reader needs it. Every invocation therefore records the absolute path it
# is writing in an exported claim list that descendants inherit, and one that
# finds its path already claimed takes a distinct destination rather than erasing
# evidence.
#
# No `set -euo pipefail` here, unlike every other script in this tree: this file
# is sourced, so those options would land on whatever shell sourced it, and a
# library must not reach into its caller's. Every caller runs strict already
# (`scripts/nx.sh`, and the justfile via `set shell`), and every function below
# checks and reports its own failures rather than relying on errexit.
#
# shellcheck shell=bash

#: Absolute paths of the logs that enclosing invocations are still writing, one
#: per line. Exported, so descendants inherit the set without a caller passing it.
export ONEMESSAGEBUS_PRESERVED_LOGS="${ONEMESSAGEBUS_PRESERVED_LOGS-}"

# Open (create and truncate) the log for one labelled command and set
# `PRESERVED_LOG` to its absolute path. Owner-only from creation: preserved
# evidence outlives the terminal that would otherwise have been its only reader.
#
# The path comes back in a variable rather than on stdout on purpose. A caller
# writing `log=$(preserved_log_open ...)` would run this in a subshell, and the
# claim it records would die with that subshell — leaving the next nested
# invocation free to truncate the log this one is about to write.
preserved_log_open() {
  local root=$1 label=$2 dir path
  if ! printf '%s' "$label" | grep -Eq '^[a-z][a-z0-9-]*$'; then
    echo "preserved-log: invalid log label '$label'; use lowercase words and dashes" >&2
    return 1
  fi
  dir="$root/.logs"
  # Canonicalized, because the claim is compared as a string and callers spell
  # one root several ways. Two spellings of the same file have to be recognized
  # as the same file, or a nested run truncates it after all.
  #
  # `pwd -W` first because on Windows this runs under Git Bash, whose `pwd -P`
  # answers `/d/a/...` — a spelling only MSYS resolves. Both messages below print
  # this path for someone to open, and a path that Explorer, PowerShell, and
  # every native tool reject is not one they can. `-W` gives `D:/a/...`, which
  # Git Bash reads too; it does not exist off Windows, hence the fallback.
  if ! { mkdir -p "$dir" && chmod 700 "$dir" && dir=$(cd -- "$dir" && { pwd -W 2>/dev/null || pwd -P; }); }; then
    echo "preserved-log: cannot prepare '$root/.logs'; repair its parent permissions and retry" >&2
    return 1
  fi
  path="$dir/$label.log"
  if _preserved_log_claimed "$path"; then
    # An enclosing invocation is still writing this exact log, so truncating it
    # would erase a run that has not finished producing its evidence.
    path="$dir/$label.$$.log"
  else
    # This invocation owns the stable path, which makes any diverted logs beside
    # it leftovers from nested runs of a previous one.
    rm -f "$dir/$label".[0-9]*.log
  fi
  if ! { : >"$path" && chmod 600 "$path"; }; then
    echo "preserved-log: cannot open '$path'; repair its permissions and retry" >&2
    return 1
  fi
  ONEMESSAGEBUS_PRESERVED_LOGS="${ONEMESSAGEBUS_PRESERVED_LOGS:+${ONEMESSAGEBUS_PRESERVED_LOGS}
}$path"
  export ONEMESSAGEBUS_PRESERVED_LOGS
  # shellcheck disable=SC2034 # this variable is the function's return value;
  # every caller reads it in the shell that sourced this file.
  PRESERVED_LOG=$path
}

# Whether some enclosing invocation is already writing exactly this log.
_preserved_log_claimed() {
  local candidate=$1 held
  while IFS= read -r held; do
    [ "$held" = "$candidate" ] && return 0
  done <<<"$ONEMESSAGEBUS_PRESERVED_LOGS"
  return 1
}

# The credential-name grammar. A value is only worth hiding when its *name* says
# it is a credential; masking on shape alone would corrupt the evidence this
# exists to preserve.
_PRESERVED_LOG_SECRET_NAME='(^|_)(TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|API_?KEY|ACCESS_?KEY|PRIVATE_?KEY|SESSION_?KEY|AUTH)S?$'

# Print `length<TAB>name` for every credential-shaped environment variable worth
# hiding, longest value first so a token containing a shorter token's value is
# replaced before the shorter one can split it.
_preserved_log_secret_names() {
  local name value restore_nocasematch=0
  # Matched case-insensitively through `nocasematch` rather than by upshifting
  # the name with `${name^^}`: that is bash 4, macOS ships bash 3.2, and there it
  # fails at *expansion* time — every candidate is skipped, so nothing is
  # redacted and the failure is silent — so keep `${name^^}` out of this tree.
  shopt -q nocasematch || restore_nocasematch=1
  shopt -s nocasematch
  while IFS= read -r name; do
    value=${!name-}
    # Shorter values collide with ordinary words and would corrupt the very
    # evidence this preserves; a credential that short is not one worth
    # protecting.
    [ "${#value}" -ge 8 ] || continue
    case $value in
    *[$'\n\r']*) continue ;;
    esac
    [[ $name =~ $_PRESERVED_LOG_SECRET_NAME ]] || continue
    printf '%s\t%s\n' "${#value}" "$name"
  done < <(compgen -e) | sort -k1,1nr -k2,2
  [ "$restore_nocasematch" -eq 0 ] || shopt -u nocasematch
}

# Filter stdin to stdout, replacing every credential value in the environment
# with `<redacted:NAME>`.
#
# A terminal forgets; a preserved log does not. One command that echoed its
# environment inside a failing run would record a live token on disk, and this is
# the only place the value is known well enough to remove it. Pure Bash on
# purpose: it sits in front of `just check`, so it cannot depend on an
# interpreter `just bootstrap` has not installed yet.
redact_secrets() {
  local -a names=() values=()
  local name line index
  while IFS=$'\t' read -r _ name; do
    names+=("$name")
    values+=("${!name}")
  done < <(_preserved_log_secret_names)
  # `|| [ -n "$line" ]` also emits a final chunk that arrived without a trailing
  # newline; the log gets one appended, which no reader can tell apart from the
  # command having written it.
  while IFS= read -r line || [ -n "$line" ]; do
    for index in "${!values[@]}"; do
      line=${line//"${values[index]}"/"<redacted:${names[index]}>"}
    done
    printf '%s\n' "$line"
    line=""
  done
}
