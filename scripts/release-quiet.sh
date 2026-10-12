#!/usr/bin/env bash
# Preserve exact failing diagnostics; one invocation owns this temporary log.
release_quiet() {
  local log status
  log="$(mktemp "${ONEPIPELINE_NODE_SCRATCH_DIR:-${TMPDIR:-/tmp}}/release-quiet.XXXXXX")" || {
    echo "release-quiet: cannot create a diagnostic log; repair ONEPIPELINE_NODE_SCRATCH_DIR/TMPDIR or select a writable directory and rerun" >&2
    return 1
  }
  if "$@" >"$log" 2>&1; then
    rm -f "$log"
    return 0
  else
    status=$?
    cat "$log" >&2
    rm -f "$log"
    return "$status"
  fi
}
