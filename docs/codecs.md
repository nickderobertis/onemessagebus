# Codecs

A codec reads the frames of a member's protocol and answers each with one
response object, over a bus. `onemessagebus serve <queue> --codec <name>` runs
one. This is the approved contract's Contract K, said in this repository's own
voice; `docs/contract.md` is the text both restate.

## The loop every codec shares

`Bus::serve(queue, codec, options, input, output)` reads frames one line at a
time, hands each to the codec, and writes the response it answers as one line of
JSON, flushed before the next frame is read. Blank lines are passed over. The
frame stream is read on a thread of its own, so a session bound is a real
deadline rather than something noticed between frames; the bound is asked before
a frame is read and never during an exchange, so a member is never left waiting
on a response the session decided not to write.

A codec reaches the bus through its `ServeSession`: `raise` puts a record on the
served queue, stamped with the session's asker and what it is about; `ask` asks a
question there (`docs/ask.md`) and waits the reply window for its answer, and
the session keeps the question. **How the session ends decides what becomes of
the questions it asked**, which is `onepipeline`'s `Served` distinction, kept:

| ending | what the member is | what the session's unanswered questions become |
| --- | --- | --- |
| the frame stream ends | gone: nothing is listening for those answers now | abandoned — kept, readable and still answerable, for a later listener of the asker to take back |
| the session reaches its bound with the stream still open | still there, and still owed every answer | counted, as they stand |

A frame the codec refuses ends the session with exit 2, and a member the codec
reports failed ends it with exit 1; neither marks anything, because the member is
still there to be told.

## The `codecs` block

```yaml
codecs:
  onejudge: {queue: surfaces, reply_window_seconds: 3000, session_env: ONEPIPELINE_SERVE_SESSION_SECONDS,
             asker_env: ONEPIPELINE_CHANNEL_ASKER, run_env: ONEPIPELINE_RUN_ID, about_env: ORCHESTRATOR_ASK_MANAGER_NODE}
```

| key | meaning | when absent |
| --- | --- | --- |
| `queue` | the queue the codec raises and asks on; `serve <queue>` must name the same one | any queue `serve` names |
| `reply_window_seconds` | how long a question the codec asks waits for its ruling, a whole number greater than zero | 30 seconds |
| `session_env` | the variable the session bound is read from when `--session-seconds` is not given | the codec's default |
| `asker_env` | the variable the asker is read from when `--asker` is not given | the codec's default |
| `run_env` | the variable the run is read from when a frame does not name it | the codec's default |
| `about_env` | the variable what the member's questions are about is read from | nothing is read |

The block is generic: every key names a constant a host configures, and no
protocol's word is in it. Which codec names there are is the binary's to say —
`serve` refuses one it does not link, naming the ones it does — and every key of
a codec's block is refused by name when unknown, at `Config::load`. The flag wins
over the variable, and a variable set to a value the key does not take is refused
naming the variable, before the first frame is read.

## The onejudge codec

`onejudge` spawns its judge-side command once per operation, writes one request
frame to its stdin, and reads one response object from its stdout. The onejudge
codec is that command. Its frames are protocol v6 of `onejudge`'s
`docs/protocol.md`, transcribed field for field from that document and
`crates/onejudge/src/command.rs` at 0.8.1, and every fixed string it reads or
writes — the operations, the response fields, the transcript-frame shape — is
declared once in `onemessagebus_agent::codec::onejudge`. The five frames are
registered as `agent.onejudge-frame.<op>@6` (`respond`, `user`, `supervisor`,
`judge`, `assess`), so `schema list`, `schema check` and `schema gen` cover them,
and `codec::onejudge::schemas()` hands them to a crate reconciling its own frame
types.

Its defaults for the `codecs.onejudge` block are `onepipeline`'s names:
`ONEPIPELINE_SERVE_SESSION_SECONDS`, `ONEPIPELINE_CHANNEL_ASKER` and
`ONEPIPELINE_RUN_ID`.

### `supervisor`: liveness, and only liveness

- **Any assistant content** in the turn means the member took its turn. Nothing
  is raised, and the member is answered with a non-completion it can act on —
  `{"completion": false, "message": ..., "reason": ...}` telling it that a report
  reaches the planner as a `finding` op, never as the prose a turn ended in.
- **No assistant content at all**, or a last assistant message that is a machine
  transcript **proving** the turn was lost, is a failure: one bounded,
  non-blocking `monitor-failed` surface naming the cause and the harness identity
  is raised on the queue, and the process exits 1.

A transcript is recognised as every non-blank line being a JSON object, and a
loss is proven only by what a transcript ends in: an `error` frame, or a
`turn/completed` whose turn `status` is `failed`. The cause is the error's
`codexErrorInfo`, else its `code`, else its `message`, collapsed to one line and
bounded to 80 characters; the identity is `codex:alternate` when the stream's
`codexHome` is the host's alternate codex home (`ORCHESTRATOR_CODEX_ALT_HOME`),
`codex` when it names another home, and an unidentified harness when it names
none. A transcript nothing can be proven inside is content like any other: the
member took its turn.

### `judge`: the planner's score

The criterion is raised as its own **non-blocking** question on the queue — kind
`monitor-completion` — and the ruling that answers it is the score: its
`completion` becomes the boolean `value`, and its `reason` (else its `message`)
becomes the score's `reason`, which is the field `onejudge` reads. A wait that
elapses, or a question abandoned before anyone ruled, is scored the conservative
`unsatisfied` — `{"value": false, "reason": ...}` — and **never a fabricated
pass**.

A reply that is a live edit — commands, and no boolean `completion` — never
reaches the codec, because the reply router sends it to the command path alone
(`docs/ask.md`). If one arrives anyway, as only a regressed transport would
deliver it, it is recognised, the member is answered with a non-completion
naming the edits, and nothing is re-applied: the edit reached the engine when it
was sent, and sending it again would apply it twice.

### Refused by name

`assess`, a `numeric` `judge`, `respond` and `user` are refused with exit 2,
naming the operation or the kind: a planner rules with a `completion` boolean,
which is no free-text judgement and no score on a scale.

### The run, and what the member is about

The run a frame belongs to is read from its `task`'s opening line where
`onejudge` writes one — ``onepipeline run `<run>`.`` — and from `run_env`
otherwise; `judge` frames carry no task, so theirs is always the environment's. A
frame with no run, or a run that is not one word of letters, digits, `_`, `.`
and `-`, is refused. What the member's questions are about is read from
`about_env`, and a linking consumer holds it to a check of its own
(`Onejudge::with_about_check`) — `onepipeline` supplies "the run's graph has this
node".

## What the host's filter measured

These accounts come from `channel-serve.py`, the filter this codec replaces, and
are why each rule is what it is.

- **A handed-over frame killed the monitor.** `onepipeline channel serve` refused
  `onejudge`'s frame by name — `unknown field 'op'` — and the member died with
  `provider produced no output` on its first turn while the run carried on
  unwatched. One process reading the frames `onejudge` writes removes the
  reconciliation step entirely.
- **Raising prose duplicated findings.** While a turn's prose was raised
  automatically, a monitor with a finding filed the `finding` op and prose
  beside it: of one run's 54 surfaces, 19 were findings and 8 were prose, all 8
  duplicating the finding before them. Removing the path removed the choice.
- **A monitor that looked and found nothing is not a monitor that failed.** A
  frame with no assistant content was once refused as a protocol failure, and a
  run lost its observer five minutes in and ran two hours with nothing watching.
  Liveness is the whole question now, with no sentinel string for a monitor to
  get wrong.
- **A lost turn does not always arrive empty.** A harness writes its own stream
  into the last assistant message instead — measured at fifteen JSON-RPC frames
  and 21,531 characters, ending in `method: error` and a failed `turn/completed`.
  Reading that as content would let a failing monitor look healthy, so a
  transcript that proves the loss is raised as a bounded surface, and its
  transcript is never repeated in it. The fixture the journeys use,
  `crates/onemessagebus-agent/tests/recorded/onejudge/lost-turn.jsonl`, is such a
  transcript, produced by a real harness losing a real turn.
- **The scoring op killed every watched run's monitor at its end.** `judge` was
  refused, `onejudge` always asks it of a member with a `done_when`, and the
  member died after every run. It is served now, as the planner's own score.
- **A manager's live edit killed the watcher it was supervising.** Through
  `onepipeline` 0.8.x a reply went to whichever reader arrived first, and forty
  recorded runs died on a graph edit handed to this reader. The edit is routed
  by its halves now; the recognition here is what stands between a run and that
  death if a release regresses.
- **The score's prose went to the wrong field.** `channel-serve.py` wrote the
  ruling's prose as `rationale`, and `onejudge` 0.8.1 reads a score's
  justification from `reason`, so every score it relayed arrived unexplained.
  This codec writes `reason`.
