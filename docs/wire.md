# The wire

What travels on a onemessagebus stream, how streams are ordered and merged, how
payloads are bounded and redacted, and how the schema registry versions the
shapes — the approved contract, said in this repository's own voice.

## One line, one envelope

A stream is NDJSON: one JSON object per line, appended in the order the
producer wrote them. Every line is an envelope:

| key | type | meaning |
| --- | --- | --- |
| `v` | `u32` | The envelope schema version the producer wrote against. Per producer, not per bus: a relayed envelope keeps its producer's number. |
| `ts` | string | RFC 3339, millisecond precision, UTC — `2026-09-13T05:43:18.700Z`. |
| `stream` | string | A unique id per producing process. |
| `seq` | `u64` | Monotonic per `stream`, from 1. A gap is a lost event. |
| `source` | string | What produced the event, in the vocabulary's words. |
| `kind` | string | What happened, kebab-case. Open on the wire: a relay carries a sibling's kinds without interpreting them. |
| *dimensions* | | The vocabulary's reserved top-level dimensions, carried as named fields here and omitted when absent. The agent profile has one, `phase`. |
| `labels` | object | The vocabulary's reserved keys plus free-form extras, carried untouched. Absent rather than empty when unknown. |
| `payload` | object | Kind-specific detail, bounded as below. |
| `artifacts` | array | `{id, kind, bytes}` references to evidence too large for a payload, stored by the producing library and read back through it. |

Keys are written in that order, and the recorded streams under
`crates/onemessagebus-agent/tests/recorded/` prove the bytes: a stream each
producer wrote round-trips through the reader and the serializer unchanged.

Reading refuses, by name: an unknown top-level key, a `seq` that is not an
unsigned integer, a source word the vocabulary does not admit, a missing
required field.

## Vocabularies

The core knows the shape and none of the words. A **vocabulary** supplies them as
types — the source word (closed enum or open string), the dimensions, the label
set, and what a matcher may ask — plus the data the command line and the SDK
manifest read: which label keys are reserved and what each admits.

`onemessagebus_agent::Agent` is the agent stack's: sources `agentgraph`, `vcs`,
`pipeline`; the dimension `phase` over `development`, `integrate`, `review`,
`release`; the reserved labels `run_id`, `round` (integer), `node`, `step`,
`member`, `persona`. `onemessagebus::Open` reserves nothing and admits any
source word. A vocabulary of your own is proven the way those two are: through
`onemessagebus::conformance::drive`, the one table every vocabulary runs.

## Order and merge

Within a stream, `seq` is the order. Across streams, the merge order is
`(ts, stream, seq)`, and that is the whole promise: a consumer detects loss as a
per-stream gap, and nothing beyond the timestamps orders one stream against
another.

`Reader` reads a file from a byte position at a record boundary and yields each
envelope with the position after it; a later reader opened at that position
yields only what followed. A final line no newline has ended is reported as a
`Torn` reading naming where it starts — never an error that ends the read — and
the resume position stays after the last whole record, so a writer that finishes
the line is read whole next time. A whole line that is not an envelope is
reported as `Refused`, with its position, rather than skipped. `Merge` folds
several readers into `(ts, stream, seq)` order and carries the torn tails and
refused lines beside the records.

## Emitting

`Emitter::new(stream, source, sink)` counts `seq` in memory: one writer, one
stream. `Emitter::shared(stream, source, path)` is for a file several processes
append to at once: each envelope is numbered from what the file already holds,
under an exclusive lock on the file, and written inside that same turn, so the
file carries one gapless series however many processes write it.

An emitter stamps the version its vocabulary says its source writes against
(`pipeline` writes 2; `agentgraph` and `vcs` write 1), its default labels
(`with_labels`, where the derived emitter's own stamp wins and the base fills
in), its default dimensions (`with_dimensions`), and admits only what its filter
does (`with_filter`). `emit` returns the envelope whether or not the filter let it
reach the sink — a producer acts on what it did not emit — and a suppressed
envelope carries the number the next admitted one takes, because `seq` numbers
what the stream carries: a filtered stream has no gaps.

## Bounds

Two constants, `MAX_PAYLOAD_TEXT_BYTES = 4096` and `MAX_ACTIVITY_DETAIL_CHARS =
160`, and three rules over them:

- `bound_payload(map)` — **the emitter's rule**, applied to the payload before
  the envelope is stamped: every top-level text value keeps its first 4096
  bytes, cut back to a character boundary, and the payload carries
  `"truncated": true` when anything was cut. Non-text values and nested
  objects are carried untouched; a payload already inside the bound passes
  unchanged and unstamped.
- `bound_text(text) -> (text, cut)` — a producer's rule over a field of its own
  typed payload: the **last** 4096 bytes, moved forward to a character
  boundary, with the cut reported for the producer to record. What names a
  failure is the tail of a process's output.
- `bound_detail(text) -> (text, cut)` — the rule over a one-line summary: runs
  of whitespace collapse to one space, and the first 160 **characters** are kept.

Every cut leaves valid UTF-8 inside the bound.

## Redaction

Redaction happens before an envelope leaves the producing library. A `Redactor`
replaces two kinds of value with `[redacted]`: every value this process's
environment carries under a credential-shaped name — one containing `TOKEN`,
`SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `APIKEY`, `API_KEY` or
`PRIVATE_KEY` — and every word carrying a credential prefix (`ghp_`, `gho_`,
`ghs_`, `ghu_`, `ghr_`, `github_pat_`, `AKIA`) with a token's worth of
characters after it. An emitter redacts with `Redactor::from_env()` unless given
another; the redaction walks every string in the payload, however nested.

## The registry

A schema id is `<namespace>.<name>@<version>`: `agent.event-envelope@2`. The
namespace ends at the first dot, and the name may itself be dot-joined parts —
`example.frame.notice@3` is the name `frame.notice` in `example`. A
*family* is the id without its version. The registry records a JSON Schema
under an id — a Rust type's generated one through `register::<M>()`, or a
document handed in through `register_schema` — and refuses a second, different
document under an id it holds.

`check(id, payload)` validates against the registered document and names the id
and the JSON pointer of the first violation. `read_set(family)` answers the
versions this build reads, newest first; `writes(family)` the newest of them;
and `read_at(family, declared)` how a document declaring a version is read:
**forward-carry** — a version in the read set reads as the version this build
writes, since every bump within a set is additive — and `Read::Unknown`
otherwise, naming the declared version and the set for the caller to refuse
with.

The agent profile registers `agent.event-envelope` at `[2, 1]`,
`agent.artifact-ref@1`, `agent.event-filter@1`, `agent.labels@1`, and
`agent.note@1` — the agent note contract's message, the first family carried
over the inbox. The transport plugin protocol's three shapes —
`onemessagebus.transport-hello@1`, `onemessagebus.transport-request@1` and
`onemessagebus.transport-reply@1`, stated in docs/transport.md — are registered
beside them, so a client in another language validates against the documents
this build reads and writes. A protocol another program owns is not registered
here: its records ride in the schema bundle that program publishes and a
configuration links (docs/schema-links.md).
