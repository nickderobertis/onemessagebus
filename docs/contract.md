# onemessagebus contract

The approved contract for this repository, committed verbatim below. It is the
one source of the wire envelope, the filter grammar, the schema registry rules,
the emitter and reader rules, and the command line: the public types are written
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
- `crates/onemessagebus/src/capability.rs`: `CAPABILITIES: &[Capability {
  method, verb, options, output, bindings, library_entry }]`, one per verb,
  with `FlagKind` bindings as `oneharness-core` declares them; the binary
  crate's `tests/capability.rs` walks the real clap tree so a flag with no
  binding and no declared exclusion fails the build; `tests/library_surface.rs`
  exercises every named library entry. `sdk_schema::bundle()` emits the
  capability manifest beside the schema roots (`envelope`, `filter`,
  `schema_id`, `registry_document`, and every message the profile registers).
- `--format json|text` on every reading verb; text is a deterministic rendering
  of the same events.

<!-- fixture: verbs -->
```json
["schema list", "schema check", "schema gen", "schema register", "events merge", "events emit"]
```
