<!-- llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] Contract B designates this document as the single normative source for configured codec semantics; core configuration tests and compiled-binary journeys reconcile its fixture, action behavior, refusal ordering, and session outcomes with the implementation. -->
# Configured codecs

`onemessagebus serve <queue> --codec <name>` interprets a member protocol from
the configuration. There are no built-in codecs or defaults: `<name>` must be a
key in `codecs`, otherwise `serve` exits 2 and names the declared keys.

<!-- fixture: codecs-config -->
```yaml
codecs:
  example:
    queue: prompts
    reply_window_seconds: 3000
    session_env: EXAMPLE_SESSION
    asker_env: EXAMPLE_ASKER
    about_env: EXAMPLE_ABOUT
    select: op
    frames:
      hello:
        schema: example.frame.hello@3
        bindings:
          - when: {field: mood, equals: lost}
            do: raise
            record: {kind: example-lost, blocking: false, source: proposal, message: "lost: {frame.cause}"}
            fail: "the member lost its turn: {frame.cause}"
          - when: {field: kind, equals: numeric}
            do: refuse
            message: "a numeric frame is not served"
          - do: ask
            record: {kind: example-question, source: proposal, message: "rule on: {frame.criterion}"}
            blocking: false
            response:
              value: {from: reply.completion}
              reason: {from: [reply.reason, reply.message], default: "ruled without a reason"}
            unanswered: {value: false, reason: "no ruling arrived, so this is scored false"}
      notice:
        schema: example.frame.notice@3
        bindings:
          - do: answer
            response: {completion: false, message: noted, reason: taken}
```

`queue` is optional and, when set, must equal the positional queue.
`reply_window_seconds` defaults to 30. `session_env`, `asker_env`, and
`about_env` are optional and have no defaults. `select` is required. `frames`
and every entry's ordered `bindings` are non-empty; each entry names a
registered `schema`.

## Paths, conditions, and templates

Paths are dot-separated object keys, with no array indexing. An unaddressed
path is absent. `when` holds only when its field is present and JSON-equal to
its scalar `equals`; absence never matches. A binding without `when` always
holds. The first holding binding runs. If none holds, the frame is refused.

Every string in `record`, `response`, `unanswered`, `message`, and `fail` is a
template. `{frame.path}` addresses the frame. Only an ask response may also use
`{reply.path}`. A string consisting of one placeholder preserves the addressed
JSON type. In longer text strings render directly, other values as compact
JSON, and absence as empty text. `{{` and `}}` are literal braces.
`{reply.path}` addresses the ruling envelope's `reply` object when present,
otherwise the ruling record itself.

## Actions

- `answer` requires `response`, writes it, and touches no queue.
- `refuse` requires `message`, writes nothing, and ends with exit 2.
- `raise` requires `record` and exactly one of `response` or `fail`. It raises
  through `ServeSession::raise`, stamping the session asker and about. A
  response continues; a failure ends with exit 1. A queue refusal ends with
  exit 1 naming it.
- `ask` requires `record`, `response`, and `unanswered`; `blocking` defaults
  false. It asks through `ServeSession::ask`. Response fields may be templates
  or mappings whose keys are exactly `from` and optional `default`. `from` is
  one path or an ordered list under `reply`; the first present non-empty value
  wins. An unresolved mapping without a default, timeout, or abandonment writes
  `unanswered`. A refused answer ends with exit 1.

## Refusals and sessions

Configuration loading refuses unknown keys at every level, malformed schema
ids, empty required maps or lists, malformed conditions or placeholders, and
actions with missing or inapplicable keys. It names the codec, frame entry, and
binding index where applicable. Before reading a frame, `serve` refuses an
entry schema absent from the linked and directory registries.

A non-object line is refused. Next, `select` must address a string naming a
declared entry. The frame must validate against that entry's schema. These and
a missing matching binding end with exit 2 and mark nothing.

The bound is checked before every frame. End of stream marks every still
unanswered question abandoned. Reaching the session bound leaves those
questions counted and reports that fact on stderr.
<!-- llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate] End of the normative configured-codec contract. -->
