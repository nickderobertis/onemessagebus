# Recorded streams

One stream from each producer of the agent stack, copied byte for byte from the
file its producer wrote on a real host, and held by `tests/recorded.rs` to
round-trip through `Reader` and `serde_json::to_string` with no byte changed.
Nothing in these files was edited; a fixture that had been touched would prove
the touch rather than the producer.

- `oneagentgraph-run.ndjson` — the whole `events.jsonl` of the `oneagentgraph`
  run `review-bar-probe-1789277572872-1933894` (9 envelopes, `v: 1`, source
  `agentgraph`, the `session` extra label on the turn kinds).
- `onevcs-session.ndjson` — the whole stream of the `onevcs` publication session
  `publish-branch-onevcs-s-b5c195333f94` (7 envelopes, `v: 1`, source `vcs`,
  `phase` stamped on every line).
- `onepipeline-events.jsonl` — lines 2–5 and 59–64 of the `onepipeline` run
  `otg-issue-repo-design`'s `events.jsonl`, selected by line number and
  otherwise untouched: the run's own `v: 2` `pipeline` envelopes beside the
  `v: 1` `agentgraph` and `vcs` envelopes it relayed, so one file carries every
  source and both envelope versions. The lines left out are the same shapes
  with long turn payloads — and the three below.
- `onepipeline-relayed-drift.jsonl` — lines 6, 8 and 9 of that same file: three
  `agentgraph` envelopes `onepipeline` relayed whose labels it wrote as
  `run_id, node, persona, member, …`. That order is the drift this crate
  resolves rather than a shape to preserve: `onepipeline`'s copy of `Labels`
  had no `member` field, so a relayed `member` fell among the extras and came
  back out after `persona`, while `oneagentgraph` — the producer — writes
  `member` before `persona`, as the contract lists the reserved keys.
  `tests/recorded.rs` holds these three to reading whole and re-serializing in
  the contract's order, which is what `onepipeline` writes once it adopts this
  crate. They are kept apart from the file above so the byte-identity test stays
  exact rather than carrying an exception list.
- `onejudge/lost-turn.jsonl` — a monitor turn a real producer really lost: the
  13 JSON-RPC frames `codex app-server` (codex-cli 0.153.4) wrote when driven by
  `oneharness run --stream --control` (oneharness 0.12.1) against a model
  endpoint that refuses every turn, as `ai-orchestrator`'s
  `tests/lost_turn_producer.py` (at `2fc711e`) captures it — its `capture()`,
  run on this host on 2026-09-13 with a scratch codex home. It ends in an
  `error` frame and a `turn/completed` whose `status` is `failed`, which is the
  last assistant message a lost turn leaves in place of an answer and what the
  onejudge codec reads the cause (`codexErrorInfo`) and identity (`codexHome`)
  out of. The endpoint and the codex home are the scratch ones the producer
  stood up; nothing else in the file was touched.
