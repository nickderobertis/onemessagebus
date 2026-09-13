# onemessagebus contract

The approved contract for this repository, committed verbatim below. It is the
one source of the wire envelope, the filter grammar, the schema registry rules,
the emitter and reader rules, the inbox, the agent note contract, the transport
seam, queues with their policies, subscriptions and authors, the configuration
file, and the command line: the public types are written
to match this text, and the contract tests — `crates/onemessagebus/tests/contract.rs`
for the core and `crates/onemessagebus-agent/tests/contract.rs` for the profile —
drive every fenced block below through those types so the two cannot drift. A
consumer's `docs/contract.md` that carries a copy of a section says it is a copy
and names this file. Changing the interface is a proposal to the owner of this
contract, never a unilateral edit here.

<!-- llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate] this file IS the
one source: the consumers' copies name it, and the contract tests in both crates drive
the fenced blocks below through the public types, so a restatement that drifted from
this text fails a gate rather than surviving quietly. -->

---

### Contract W — the wire envelope

One NDJSON line per event, byte-identical to what `oneagentgraph`, `onevcs` and
`onepipeline` write today:

<!-- fixture: envelope -->
```json
{"v": 1, "ts": "<RFC 3339, millisecond precision, UTC>", "stream": "<unique id per producing process>",
 "seq": 42, "source": "agentgraph|vcs|pipeline", "kind": "<kebab-case event kind>",
 "phase": "development|integrate|review|release",
 "labels": {"run_id": "R", "round": 2, "node": "service", "step": "implement", "member": "worker", "persona": "engineer", "extra": "carried"},
 "payload": {}, "artifacts": [{"id": "a-91", "kind": "log", "bytes": 21400}]}
```

- `v` is a **`u32`** (the `u8` in `oneagentgraph` was the drift; `u32` is what
  `onevcs` and `onepipeline` hold, and a `u8` reads every value the others
  write). It is the **envelope schema version the producer wrote against**, per
  producer: `agentgraph` and `vcs` write `1`, `pipeline` writes `2`; a relayed
  envelope keeps its producer's number. The registry (Contract R) records the
  read-set `[2, 1]` for the family `agent.event-envelope`.
- `seq` is a `u64`, monotonic per `stream`; merge order across streams is
  `(ts, stream, seq)`; a consumer detects loss as a per-stream gap; no
  cross-stream promise beyond timestamps.
- `kind` is **open on the wire and a string newtype in the core**
  (`onemessagebus::Kind`), because a relay carries a sibling's kinds without
  interpreting them; each producing library keeps its own closed enum and
  converts into `Kind` (`From`).
- `source` is a **closed enum in the profile**
  (`onemessagebus_agent::Source::{Agentgraph, Vcs, Pipeline}`, serialized
  lowercase) over an open core `onemessagebus::Source(String)`; the profile is
  what makes it closed.
- `phase` is a **reserved top-level dimension declared by the profile**:
  `onemessagebus_agent::Phase::{Development, Integrate, Review, Release}`
  (kebab-case), optional, omitted from the wire when absent, exactly as
  `onepipeline` relays it today and `onevcs` stamps it. **The core has no
  `Phase`**: it carries profile-declared dimensions as named extension fields
  the vocabulary admits. The profile owns the closed enum, `onevcs` re-exports
  it and keeps `Phase::of(kind)`, `onepipeline` drops its copy.
- `labels` is the reserved keys the profile declares plus free-form extras
  carried untouched: `run_id` (string), `round` (u64), `node`, `step`, `member`,
  `persona` (strings); absent rather than empty when unknown; producers stamp
  what they know, enrichers never rewrite. The core's `Labels` is an open
  ordered map with a `Vocabulary` saying which keys are reserved and what each
  admits; `onemessagebus_agent::Labels` is the typed struct with those six
  optional fields and a flattened `extra` map, serializing to the same bytes the
  three crates' structs do today.
- `payload` is a JSON object whose text fields are bounded at **4096 bytes**
  (`MAX_PAYLOAD_TEXT_BYTES`) and whose summary fields are bounded at **160
  characters** (`MAX_ACTIVITY_DETAIL_CHARS`). The core offers **two cutting
  rules** over the first constant: `bound_text(&str) -> (String, bool)` keeps
  the **last** 4096 bytes, moved forward to a character boundary, and reports
  the cut for the producer to record in its typed payload (`oneagentgraph`'s
  field rule); `bound_payload(Map) -> Map` keeps the **first** 4096 bytes of
  every top-level text value, moved back to a character boundary, and stamps
  `"truncated": true` on the payload when any value was cut (`onevcs::stream`'s
  and `onepipeline::journal`'s payload rule); `bound_detail(&str) -> (String,
  bool)` collapses runs of whitespace to one space and keeps the first 160
  **characters**. Every cut leaves valid UTF-8 inside the bound. **The
  emitter's rule is `bound_payload`**: `Emitter::emit` — and so the binary's
  `events emit` — applies it to the payload map before the envelope is stamped;
  `bound_text` and `bound_detail` are a producer's rules over fields of its own
  typed payload before the map reaches the emitter, and a payload already inside
  the bound passes the emitter unchanged with no `truncated` stamp. Larger
  evidence is an `ArtifactRef {id, kind, bytes}` stored by the producing library
  and read back through that library.
- Redaction happens **before an envelope or artifact leaves the producing
  library**: a `Redactor` with the credential-word and credential-prefix tables
  `onevcs::stream` holds today (`TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`,
  `CREDENTIAL`, `APIKEY`, `API_KEY`, `PRIVATE_KEY`; `ghp_`, `gho_`, `ghs_`,
  `ghu_`, `ghr_`, `github_pat_`, `AKIA`) and the replacement `[redacted]`, so a
  consumer that redacts through the bus redacts exactly what it redacts now.
- Type names the consumers import: `onemessagebus_agent::event::{Envelope,
  Labels, Source, Phase, ArtifactRef, EventFilter, Matcher}` and
  `onemessagebus::{Kind, Emitter, Reader, Merge, bound_text, bound_payload,
  bound_detail, Redactor, MAX_PAYLOAD_TEXT_BYTES, MAX_ACTIVITY_DETAIL_CHARS}`.
  `Envelope` in the profile is the concrete type over the agent vocabulary; the
  core's is generic over a `Vocabulary`. Deserialization refuses an unknown
  top-level field, a non-`u64` `seq`, an unknown `source` word, a missing
  required field.

### Contract F — the filter grammar

<!-- fixture: filter -->
```json
{"include": [{"kind": "member-*"}, {"member": "worker", "persona": "engineer"}],
 "exclude": [{"kind": "turn-activity"}, {"source": "vcs", "phase": "release"}]}
```

- A matcher's fields are all optional and **conjoin**: `source` (exact),
  `phase` (exact), `kind` (a glob over the kebab-case wire string where `*` is
  any run of characters including none and every other character is itself),
  and the reserved labels `run_id`, `node`, `step`, `member`, `persona` (exact;
  a matcher naming a label the envelope did not stamp does not match it).
  `stream` and payload fields are deliberately not matchable.
- An absent or empty `include` admits everything; a match in `exclude` rejects
  whatever `include` said.
- **Validation at the trust boundary**: a matcher naming no field, or naming an
  empty field, is refused with a message naming the list, the index and the
  matcher; an unknown field is refused by serde.
- The core's `Filter`/`Matcher` are generic over the vocabulary (which keys
  exist); `onemessagebus_agent::event::{EventFilter, Matcher}` are the concrete
  types with exactly the fields above, with `EventFilter::validate() ->
  Result<(), String>`, `EventFilter::parse(spec)` and `EventFilter::read(spec)`
  for the `--event-filter` spelling the three CLIs accept today (inline JSON, or
  a path to a YAML document), and `Matcher::parse(spec)` for one matcher.
- Filtering decides what is **emitted**, never what a producer acts on; `seq`
  numbers what the stream carries, so a filtered stream has no gaps.

### Contract R — the schema registry

- `onemessagebus::SchemaId` is `<namespace>.<name>@<version>` —
  `agent.finding@1`, `agent.event-envelope@2` — parsed and refused at the
  boundary (empty parts, a version that is not a positive integer).
- `trait Message: Serialize + DeserializeOwned + JsonSchema { const SCHEMA:
  SchemaId; }` — a Rust message type says which schema it is;
  `Registry::register::<M>()` records the type's generated JSON Schema under its
  id, and `Registry::register_schema(id, schema)` records one handed in as JSON.
  Registering two different documents under one id is refused.
- `Registry::check(id, &Value)` validates a payload against the registered
  document, naming the id and the JSON pointer of the first violation.
- **Versions**: a `family` is the id without its version;
  `Registry::read_set(family) -> Vec<u32>` (newest first) and
  `Registry::writes(family)`. `Registry::read_at(family, declared) -> Read`
  answers `Read::At(writes)` for a declared version in the read set
  (**forward-carry**: an envelope written against an older version in the set
  is read as the version this build writes, since every bump in the set is
  additive), and `Read::Unknown(..)` otherwise — the caller refuses, naming the
  declared version and the set.
- The profile registers on construction: `agent.event-envelope` reads `[2, 1]`,
  writes `2` for `pipeline` and `1` for `agentgraph` and `vcs` (per-source write
  version is a profile fact); `agent.reply-envelope` reads `[3, 2]`, writes `3`
  — the shape is `onepipeline::channel::Reply` (`version`, `author`,
  `completion`, `message`, `reason`, `commands`), registered here as JSON
  Schema so the profile owns the wire shape while `onepipeline` keeps owning
  the `Command` variants' meaning.

<!-- fixture: read-sets -->
```json
{"agent.event-envelope": [2, 1], "agent.reply-envelope": [3, 2]}
```

### Contract E — emitting, reading and merging

- `Emitter::new(stream, source, sink: Box<dyn Write + Send>)` writes one
  envelope per line, stamping `ts`, `stream`, `seq` (monotonic from 1, counted
  in memory for a single-writer stream and **from the file** under a lock for a
  shared one — `Emitter::shared(stream, source, path)`), the producer's write
  version, its default labels (`with_labels`), and its filter (`with_filter`);
  `emit(kind, payload) -> Envelope` returns the envelope whether or not the
  filter let it reach the sink, which is what lets a producer act on what it
  did not emit.
- `Reader` reads a stream from a byte position at a record boundary, yielding
  envelopes and the position after each — a position a later `Reader` resumes
  from, yielding only the records after it — and tolerates a torn final line:
  the whole records before it are yielded, the torn bytes are reported (a `Torn`
  reading naming the position they start at, never an error that ends the
  read), and the resume position is the one after the last whole record, so a
  writer that completes the line is read whole on the next resume; `events
  merge` over such a file prints the whole records and reports the torn tail on
  stderr without failing. `Merge` folds several readers in `(ts, stream, seq)`
  order.
- Byte-for-byte fidelity is proven, not asserted: a recorded stream from each
  of the three repositories is checked in under
  `crates/onemessagebus-agent/tests/recorded/` and round-trips through `Reader`
  and `serde_json::to_string` with no byte changed.

### Contract I — the inbox

A typed channel into a running process whose sender learns what the receiver
did with the message. `docs/inbox.md` restates this section in the repository's
own voice.

- `trait Disposition: Serialize + DeserializeOwned + Send + 'static {}` — what a
  receiver may answer a delivered message with: the consumer's own type, which
  the core requires only to serialize, so a disposition crosses a spool or a
  socket. `trait Carried: Disposition { fn carried() -> Self; }` is the
  disposition a carried message's sender is answered with.
- `Inbox<M: Message, D: Disposition>` is the receiving end and
  `Sender<M: Message, D: Disposition>` the sending end (`Clone`).
  `Sender::send(&self, message: M) -> Result<D, Undelivered>` blocks until the
  receiver has taken the message **and** answered it, or the inbox is closed,
  and never returns a fabricated disposition. `Inbox::take(&self) ->
  Option<Delivered<M, D>>` is the next delivered message if one is waiting,
  non-blocking; `Inbox::take_within(&self, timeout)` blocks up to `timeout` for
  one; `Inbox::answered(&self) -> Vec<Answered<M, D>>` is every message answered
  so far with its disposition, oldest first; `Inbox::close(&self, reason:
  Closed)` gives every blocked sender, and every later one,
  `Undelivered::Closed(reason)`. `Delivered::message(&self) -> &M` reads a taken
  message and `Delivered::answer(self, disposition: D)` hands the disposition
  back to the sender blocked in `send`. `enum Undelivered { Closed(Closed),
  Backend(BackendError) }`; `struct Closed { pub reason: String }` is the
  closer's words, carried to the sender verbatim.
- **Backends**, each an implementation of the one `trait InboxBackend<M, D>` a
  `Sender` is built over, so a consumer writes against `Sender`/`Inbox`
  whichever backend carries the message: `InProcess` (one process);
  `Spool` (a directory the receiver's process binds, where a sender in another
  process writes one file per message, a courier thread on the receiver's side
  moves each into the in-process inbox and writes the disposition back beside
  it, and the sender's `send` blocks reading that answer — `Spool::address()` is
  the path a consumer records for a sender elsewhere to find it); and `Carry`
  (for a receiver that is not running now: `send` appends to a durable carry
  store and answers `D::carried()`, and the receiver's next session drains the
  store on open through `Inbox::adopt_carried()`).
- The **routing** of a delivered message — which party of a conversation is
  live, whether a decision is re-taken — is the consumer's, never the inbox's:
  the inbox promises only that a message reaches `take` or the sender learns why
  not, and that a disposition reaches exactly the sender that asked.
- A sender never receives a disposition it was not answered with: no timeout
  synthesizes a `D`. Three states are distinct. **Pending** — the receiver is
  bound and has neither answered nor closed, however long that takes, and
  `send` stays blocked. **Closed** — the receiver closed the inbox, before or
  after the message arrived, and `send` returns `Undelivered::Closed` with the
  closer's reason; the spool records the close, so a sender arriving later is
  refused rather than left waiting. **Lost** — the answer cannot be had: the
  answer document is present but unreadable, or the spool's bounded wait
  (`SPOOL_WAIT`) elapses with the message never taken — and `send` returns
  `Undelivered::Backend` naming the spool path and the file or the elapsed wait,
  and withdraws the offered message so a receiver waking later does not deliver
  what its sender was told was lost. The in-process backend has no bounded wait.

The documents a spool holds, one per file (`<id>.offer.json`,
`<id>.answer.json`, `spool.json`, `closed.json`), and a carry store's header and
record lines:

<!-- fixture: spool-documents -->
```json
{"spool.json": {"schema_version": 1, "schema": "agent.note@1"},
 "offer": {"schema_version": 1, "schema": "agent.note@1", "message": {"addressee": "worker", "text": "look again at the migration"}},
 "answer": {"schema_version": 1, "answer": {"disposition": {"interrupted": {"party": "worker"}}}},
 "answer-closed": {"schema_version": 1, "answer": {"closed": {"reason": "the conversation ended"}}},
 "answer-refused": {"schema_version": 1, "answer": {"refused": {"reason": "<why the receiver could not read the offer>"}}},
 "closed.json": {"schema_version": 1, "reason": "the conversation ended"}}
```

<!-- fixture: carry-store -->
```json
{"header": {"schema_version": 1, "kind": "onemessagebus-carry-store"},
 "record": {"ts": "<RFC 3339, millisecond precision, UTC>", "schema": "agent.note@1", "message": {"addressee": "both", "text": "the ruling applies to both of you"}}}
```

### Contract N — the agent note contract

`onemessagebus_agent::note` declares, with the same names, the same serde shapes
and the same refusals as `onejudge::note` at release 0.8.1: `Addressee::{Worker,
Supervisor, Both}` (lowercase on the wire), `Party`, `Criterion` (the newtype and
its refusals), `CriterionRefused`, `NoteText`, `Note` (`new`, `to`, `binding`,
`binds`), `NoteRefused`, `DeliveredNote`, `Accepted::{Queued, Interrupted {
party }, JudgedWith}`, `Undelivered` (the note contract's own variants, re-exported
at the crate root as `NoteUndelivered`), `Criteria::{compose, rendered, bound}`,
and `supervisor_block`. `Note: Message` with schema `agent.note@1`; `Accepted:
Disposition`, with `Carried` answering `Accepted::Queued`. `type Notes =
Sender<Note, Accepted>` and `type NoteInbox = Inbox<Note, Accepted>`, with
`Notes::channel()` building the in-process pair. What a conversation does with a
delivered note stays in `onejudge`.

<!-- fixture: note -->
```json
{"addressee": "both", "text": "the bar moved", "criterion": "the flag defaults to off"}
```

<!-- fixture: accepted -->
```json
["queued", {"interrupted": {"party": "worker"}}, {"interrupted": {"party": "supervisor"}}, {"judged_with": {"completion_reason": "passed with the note in hand"}}]
```

<!-- fixture: note-undelivered -->
```json
[{"conversation_completed": {"completion_reason": "the work is done"}}, {"member_settled": {"outcome": "the conversation ended"}}, {"no_conversation": {"reason": "nothing ever read this channel"}}]
```

**Departures, ruled by the manager over the ask seam** and recorded here so the
adopting nodes read them where they read the contract:

1. `Notes::send` is the core's `Sender::send` through the alias, so it answers
   `onemessagebus::Undelivered` rather than `note::Undelivered`: a crate cannot
   add a method to the core's type. `note::Undelivered:
   From<onemessagebus::Undelivered>` reads a note refusal carried in a close
   (`Closed::from(&note::Undelivered)`) back into the same variant, a close in
   anyone else's words into `MemberSettled`, and a backend failure into
   `NoConversation`, so `onejudge` adapts with one `.map_err(Into::into)`.
   `Notes::channel()` keeps its signature through the core's `Sender::channel()`.
2. `NoteInbox::delivered()` is `note::NoteInboxExt::delivered`, re-exported by
   `note::prelude`. It lists the notes answered `Interrupted` (reaching the party
   named) and `JudgedWith` (reaching the supervisor); a `Queued` note has reached
   no party yet.
3. `onejudge`'s seven note shape tests moved verbatim. Its five channel tests
   drive the phase machine that stays in `onejudge`, and the core's inbox tests
   prove the inbox-level equivalents: a dropped inbox answers every blocked
   sender, and a closed inbox stays closed with its first reason.
4. `worker_block` is public, the one item beyond the list above, because
   `onejudge`'s engine renders a worker's turn with it and a copy left there
   drifts.
5. `deliver` takes `--wait <SECONDS>` (default 30), a named option bound in
   `CAPABILITIES`, so the lost state is observable through the binary;
   `Spool::connect_within` is the library's way.

### Contract T — the transport plugin seam

`docs/transport.md` restates this section in the repository's own voice, with the
NATS JetStream mapping.

- `trait Transport: Send + Sync + 'static` is the one seam every queue is kept on,
  **object-safe** (a transport is chosen at runtime from configuration and held
  as `Arc<dyn Transport>`) and **implementable outside the core** (every type it
  names is public and constructible). Its methods and their responsibilities:
  `append` (one record, total order per queue, answering the position after it),
  `read` (records after a position, oldest first, at most `limit`, each with the
  position after it, a torn trailing record reported in `Batch::torn`, never
  dropped and never fatal), `cursor` / `commit` (a named consumer's position),
  `exclusive` (a section over one queue, its body handed the transport to use
  inside it), `fingerprint` / `wait_for_change` (a cheap change token and a bounded
  wait for it to move), `document` / `replace_document` (a small named document,
  read whole and replaced atomically).
- `Position` and `Fingerprint` are opaque to consumers, serializable, and built
  only through a transport (`Position::from_token`, `Fingerprint::from_parts`).
- `LocalTransport::open(dir)` lays a queue out as below; a position is the byte
  offset at a record boundary; a fingerprint is each file's length and
  modification time; an append heals a torn tail back to its record boundary and
  records the loss in `<queue>.jsonl.torn`. `MemoryTransport::new()` keeps the
  same promises in memory.

<!-- fixture: transport-layout -->
```json
{"records": "<queue>.jsonl", "default cursor": "<queue>-cursor.json", "consumer cursor": "<queue>-cursor.<consumer>.json",
 "document": "<name>", "exclusive section": ".lock/<queue>.lock"}
```

- A transport kind resolves **built-in first** (`local`, `memory`), **then
  registered in-process** (`TransportKinds::register`), **then a plugin
  executable on `PATH`** named `onemessagebus-transport-<kind>`; a kind none of
  them serves is refused naming every kind there is.

<!-- fixture: transport-kinds -->
```json
{"built-in": ["local", "memory"], "order": ["builtin", "registered", "plugin"], "plugin executable": "onemessagebus-transport-<kind>"}
```

- A plugin serves one transport over its stdin and stdout, one JSON object per
  line in each direction, at protocol version 1: the client's first line is a
  hello naming the protocol, its version and the `transport` block; every later
  line is a request discriminated by `op`, answered `{"ok": ...}` or
  `{"error": {kind, message, ...}}`; `exclusive` is `begin_exclusive` …
  `end_exclusive`. `transport::serve` is a plugin's whole `main`, and
  `ProcessTransport` the core's client. The three shapes are registered as
  `onemessagebus.transport-hello@1`, `onemessagebus.transport-request@1` and
  `onemessagebus.transport-reply@1`.

<!-- fixture: plugin-protocol -->
```json
{"hello": {"protocol": "onemessagebus-transport", "version": 1, "config": {"kind": "nats", "dir": "runs/r1/channel", "url": "nats://h:4222"}},
 "hello-answer": {"ok": {"hello": {"protocol": "onemessagebus-transport", "version": 1}}},
 "requests": [{"op": "append", "queue": "surfaces", "record": "{\"id\":0}"},
              {"op": "read", "queue": "surfaces", "from": 9, "limit": 100},
              {"op": "begin_exclusive", "queue": "surfaces"},
              {"op": "end_exclusive", "queue": "surfaces", "failed": false}],
 "replies": [{"ok": {"position": 9}},
             {"ok": {"batch": {"records": [{"record": "{\"id\":0}", "after": 9}]}}},
             {"ok": "done"},
             {"error": {"kind": "past_end", "message": "surfaces: position 12 is past the end of the queue, which ends at 9; the log was replaced or truncated", "queue": "surfaces", "position": 12, "end": 9}}]}
```

### Contract Q — queues, policies, subscriptions, authors

`docs/queues.md` restates this section in the repository's own voice, with the
projection's `accounted` and `seal` account.

- `Queue<M: Message>` over a `Transport` with a `Policy` (`delivery`, `ordering`,
  `supersede_on: Option<Supersede { key: FieldPath, when: Option<Predicate> }>`,
  `hold_pending`, `blocking_first`, `retention`, `projection:
  Option<DocumentName>`): `push(record) -> Pushed`, `claim(consumer) ->
  Option<Claimed<M>>`, `answer(claimed, reply_position)`, `pending(consumer) ->
  Option<Claimed<M>>`, `waiting() -> Vec<M>`, `unread_count()`. A record pushed onto
  a typed queue is validated against `M::SCHEMA` before it is appended.
  `RawQueue` is the same queue over JSON records, as a configuration declares one.
  A policy asking for none of `hold_pending`, `blocking_first`, `supersede_on` and
  `projection` is a plain queue, read through consumer cursors; any of them makes
  an event queue, whose log records every state a record reaches.

<!-- fixture: policy -->
```json
{"delivery": "at-least-once", "ordering": "per-queue", "hold_pending": false, "blocking_first": false, "retention": "keep"}
```

- The projection document, when configured, is the fold of the log with
  `accounted` (the log bytes it accounts for) and a `seal` (FNV-1a 128 over its
  waiting records, pending record and `next_id`, then over `accounted`), written
  exactly as `onepipeline::channel::Queue` writes it: a stamped document that
  does not seal reads as no document, and the log is folded whole.
- `Subscription { queue, consumer, lifetime: Lifetime::Session |
  Lifetime::Durable(Asker) }`: `abandon()` marks what the listener raised and
  claimed, and has not seen answered, as abandoned (kept, uncounted, still
  readable); a durable listener on opening `attend`s — takes back — what an
  earlier listener of the **same** asker abandoned; a session adopts nothing and
  nothing adopts what it raised. `Asker` is a non-blank Unicode word compared for
  equality, refused otherwise with `onepipeline`'s two refusals, naming where the
  value came from.
- `Author(String)` is open in the core; `Allowlist<Op: Operation>` with
  `grant(author, op)` and `allows(author, op) -> Result<(), Refusal>` refuses an op
  not granted **by omission**, naming the author, the op and the reason recorded.
  The profile's `planner-channel` layout declares the ops, the planner's and the
  monitor's grants, and each refusal's text as `channel::allows` states it, with
  `complete` granted or refused for a legacy verdict carrying `completion: true`:

<!-- fixture: planner-channel-grants -->
```json
{"planner": ["add", "drop", "reparent", "retry", "cancel", "requeue", "complete", "attest", "finding", "amend", "note", "settle"],
 "monitor": ["retry", "requeue", "cancel", "finding", "add"],
 "refused-monitor": {"complete": "whether the run is finished is the planner's verdict, not an observation",
                     "attest": "a human action is attested by the person who took it, never by a watcher",
                     "drop": "removing work from the graph is a decomposition decision the planner owns",
                     "reparent": "rewiring dependencies is a decomposition decision the planner owns",
                     "amend": "what a node is judged against is a decomposition decision the planner owns",
                     "note": "a note may bind a criterion the node's judge decides against, which is the planner's decision rather than an observation",
                     "settle": "settling a node from evidence declares an outcome this run never observed, which is the planner's decision rather than an observation"}}
```

- The `planner-channel` layout's queues, as declared — the files a directory
  `onepipeline` 0.28.2 wrote are read by this crate, and those this crate writes
  are read by 0.28.2:

<!-- fixture: planner-channel -->
```json
[{"name": "surfaces",
  "policy": {"delivery": "at-least-once", "ordering": "per-queue",
             "supersede_on": {"key": "source", "when": {"field": "source", "equals": "check-in"}},
             "hold_pending": true, "blocking_first": true, "retention": "keep", "projection": "queue.json"},
  "schema": "agent.planner-surface@1", "answers": "replies", "consumers": ["default"], "numbered": false},
 {"name": "replies",
  "policy": {"delivery": "at-least-once", "ordering": "per-queue", "hold_pending": false, "blocking_first": false, "retention": "keep"},
  "schema": "agent.queued-reply@1",
  "claims": {"not": {"all": [{"field": "reply.commands", "non_empty": true},
                             {"not": {"any": [{"field": "reply.completion", "present": true},
                                              {"field": "reply.message", "present": true},
                                              {"field": "reply.reason", "present": true}]}}]}},
  "consumers": ["default"], "numbered": true},
 {"name": "commands",
  "policy": {"delivery": "at-least-once", "ordering": "per-queue", "hold_pending": false, "blocking_first": false, "retention": "keep"},
  "schema": "agent.queued-commands@1", "consumers": ["default"], "numbered": true},
 {"name": "command-outcomes",
  "policy": {"delivery": "at-least-once", "ordering": "per-queue", "hold_pending": false, "blocking_first": false, "retention": "keep"},
  "schema": "agent.command-outcome@1", "consumers": ["default"], "numbered": false}]
```

### The configuration file — `onemessagebus.yaml`, version 1

<!-- fixture: config -->
```yaml
version: 1
transport: {kind: local, dir: runs/r1/channel}
profile: planner-channel
queues:
  findings: {policy: {hold_pending: false}}
authors:
  monitor: {capabilities: [retry, requeue, cancel, finding]}
```

- `kind` is `local`, `memory`, or a registered or plugin kind; `profile` names a
  layout a linked profile declares; `queues` adds queues or overrides a layout's
  by name; `authors` may narrow a layout author's grants and never widen them.
- **Two steps, which the types keep apart.** `onemessagebus::Config::load(path)`
  refuses what the file alone decides, naming the key: YAML that is not one
  document, an unknown key, a version other than 1, a name, schema id or
  predicate that does not parse. `Config::resolve(&layouts, &kinds)` refuses what
  only the linked layouts and transport kinds decide, naming the key: a profile no
  layout declares, a widened grant, an op that does not exist or an author the
  layout does not declare (`authors.<author>.capabilities`), a `schema` the layout
  does not register or an `answers` naming no queue (`queues.<queue>.<key>`), and
  a transport its kind refuses. A loaded `Config` opens nothing; the `Bus`
  `resolve` answers is the one type that opens a queue or authors a record.
- The binary reads it from `--config <path>` or `ONEMESSAGEBUS_CONFIG`, and
  `--transport-dir <path>` or `ONEMESSAGEBUS_TRANSPORT_DIR` replaces
  `transport.dir` for one invocation: the flag over the variable, the variable
  over the file. Its JSON Schema is the SDK bundle's `config` root.

**Departures, ruled by the manager over the ask seam** for Contracts T and Q and
recorded here so the adopting nodes read them where they read the contract:

1. The surfaces queue supersedes on `source == check-in`, not the `kind ==
   check-in` Contract Q first stated: that is what 0.28.2's `channel.rs` does, and
   byte compatibility wins over the wording. The `Supersede { key, when }` shape
   is unchanged.
2. The shipped binary reaches a plugin transport through the executable
   `onemessagebus-transport-<kind>` on `PATH`, over the versioned line-delimited
   JSON protocol above, whose hello names the version in its first line and whose
   shapes are registered schemas; resolution is built-in, registered, `PATH`.
3. A local cursor file holds the number of records before the position, as
   0.28.2 writes `replies-cursor.json` and `commands-cursor.json`; the position
   stays the byte offset, converted at the transport boundary.
4. A queue's declaration carries `schema`, `answers`, `claims`, `consumers` and
   `numbered` beside its `Policy`, as declaration keys rather than policy fields.
5. A widened grant is refused by `Config::resolve`, not `Config::load`, since
   only the linked layouts know a profile's grants; the unresolved `Config` cannot
   open a queue or author a record.
6. `answer(claimed, reply_position)` releases the slot once the reply is at
   `reply_position`, so the reply is appended first; the profile's typed
   `Channel::answer` releases first and appends after, in 0.28.2's order.
7. `onepipeline results` at 0.28.2 reads no channel file, so the journey holds
   `results` over the recorded run root to identical output with the written
   channel substituted, and compares `next` and `status` with this crate's answers.

### Contract C — the command line and the capability manifest

- `onemessagebus schema list` (no input; every registered id), `schema check
  <id>` (a payload on stdin or `--file`, exit 0 / a violation naming the
  pointer), `schema gen --lang json|rust|python|typescript <id>` (no input; the
  schema document, or the language's declaration for it — `rust` and `json`
  here, `python` and `typescript` in the SDK packages), `schema register <id>
  --file <schema.json>` (the document on `--file`, into a registry file the CLI
  keeps under `--registry <dir>` / `ONEMESSAGEBUS_REGISTRY`); `events merge
  <file>... [--filter <spec>] [--profile <name>]` (the stream files as
  positional arguments; the merged, filtered stream on stdout); `events emit
  <file> --kind <kind> --stream <id> [--source <word>] [--profile <name>]
  [--label <key>=<value>]...` (the payload on stdin or `--file`; appends one
  envelope to the stream file `<file>` through `Emitter::shared`, so `seq` is
  taken from the file under its lock and several processes may append to one
  file at once; prints the envelope it wrote). **`--profile <name>` on both
  `events` verbs defaults to the agent profile** the binary links, so a profile
  — and with it the source words and each source's write version — is chosen
  the same way on `emit` and on `merge`. **Payloads arrive on stdin or `--file`,
  never as a positional argument**: what the rule refuses is a payload the clap
  tree would read as a positional.
- `onemessagebus deliver <address> [--message <M>] [--file <path>] [--wait
  <seconds>]` sends one message to a spool address — the message read from
  stdin, from `--file`, or given inline through the named `--message` option,
  exactly one of the three, more than one given at once refused naming each
  source given and nothing written — and prints the disposition, or the
  `Undelivered` reason with a non-zero exit. `deliver` takes one positional, the
  address, and a second positional is refused; `--message` is a named option
  the payload rule does not forbid. `onemessagebus inbox carried <store>
  [--format json|text]` lists a carry store in the order its messages were
  carried, prints nothing for an empty store, and refuses a path that is no
  carry store by name. Each is a `Capability` with a library entry
  (`Spool::deliver`, `Carry::read`).
- `crates/onemessagebus/src/capability.rs`: `CAPABILITIES: &[Capability {
  method, verb, options, output, bindings, library_entry }]`, one per verb,
  with `FlagKind` bindings as `oneharness-core` declares them; the binary
  crate's `tests/capability.rs` walks the real clap tree so a flag with no
  binding and no declared exclusion fails the build; `tests/library_surface.rs`
  exercises every named library entry. `sdk_schema::bundle()` emits the
  capability manifest beside the schema roots (`envelope`, `filter`,
  `schema_id`, `registry_document`, `config`, and every message the profile
  registers).
- `onemessagebus send <queue> [--file <path>]` (a record on stdin or `--file`,
  shaped and checked by the layout, validated, appended; prints `{queue,
  position, id}` per record appended), `next <queue> [--consumer <name>]
  [--asker <word>]` (claims one, blocking-first, and prints `{queue, position,
  id, record}`; nothing to claim exits 1), `reply <queue> <position> [--file
  <path>]` (answers the record pending at the position `next` printed with the
  reply on stdin or `--file`, appended to the queue it answers on; prints
  `{answered, sent}`), `subscribe <queue> --until <predicate> [--timeout <s>]`
  (streams the log as `{position, record}` lines, ending on the first the
  predicate admits), `status [<queue>]` (a list of `{queue, events, records,
  waiting, pending, pending_position, abandoned, unread, cursors}`) and
  `transports` (every kind, built-in, registered and plugin). Each queue verb
  takes `--config` and `--transport-dir`; each is a `Capability` with a library
  entry (`Bus::send`, `RawQueue::claim`, `RawQueue::answer_at`,
  `RawQueue::wait_for_change`, `RawQueue::status`, `TransportKinds::kinds`).
- `--format json|text` on every reading verb; text is a deterministic rendering
  of the same events.

<!-- fixture: verbs -->
```json
["schema list", "schema check", "schema gen", "schema register", "events merge", "events emit", "deliver", "inbox carried", "send", "next", "reply", "subscribe", "status", "transports"]
```
