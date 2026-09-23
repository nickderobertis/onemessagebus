# onemessagebus

![A terminal tailing a queue: three backlogged records land at once, then new ones append a line at a time as another process writes them — each an id, a kind, a message and the instant the bus stamped it](docs/screenshots/subscribe.gif)

A typed NDJSON message bus for anything that speaks in events. One envelope, one filter grammar, payload bounds and
redaction, a schema registry with version read-sets, and an emitter and reader
over streams, generic over the **vocabulary** a consumer declares.

Two artifacts:

- **`onemessagebus`** — the core library. It knows the shape of an envelope and
  none of the words: which sources exist, which label keys are reserved and
  which top-level dimensions an envelope carries are a `Vocabulary`'s to
  declare. `Open` reserves nothing; a vocabulary of your own — declared in the
  program that owns it, never in the bus — is proven through the same
  conformance table.
- **`onemessagebus`**, the binary — the command line the sections below show,
  over the registry, the schema cache, NDJSON streams, the inbox and durable
  queues kept on a transport a configuration names. The local transport keeps
  each queue as plain files in one directory, and a distributed one is a plugin
  rather than a consumer change.

## Install the command line

```bash
pip install onemessagebus-cli          # PyPI wheel, no toolchain needed
npm install -g onemessagebus-cli       # npm launcher, no toolchain needed
cargo install --git https://github.com/nickderobertis/onemessagebus onemessagebus-cli --locked
```

<!-- llmlint: ignore-block[no_redundant_instruction_pointers] this README is also the PyPI page of the `onemessagebus-cli` wheel (pyproject.toml's `readme`) and the repository's front page, read by someone who has installed or found the tool and never opens AGENTS.md; these links are how that reader reaches the command line, the wire and the contract. -->
[`docs/cli.md`](docs/cli.md) is the whole command line; [`docs/wire.md`](docs/wire.md)
is the wire; [`docs/contract.md`](docs/contract.md) is the approved contract the
consumers restate from.
<!-- llmlint: ignore-end[no_redundant_instruction_pointers] -->

<!-- llmlint: ignore-block[no_redundant_instruction_pointers] this README is the PyPI page of the `onemessagebus-cli` wheel and the repository's front page, read by someone who never opens an AGENTS.md; a reader who wonders whether these pictures are real has nowhere else to look. -->
Every picture below is a capture of that binary run against a fixture of the
kind the end-to-end journeys stage, gated on its content hash so it cannot drift
away from what the tool prints ([`screenshots/AGENTS.md`](screenshots/AGENTS.md)).
The prose around them is an introduction: `docs/cli.md` is the reference, and
`crates/onemessagebus-cli/tests/docs.rs` holds this page's verbs, profiles, flags
and sample commands to the binary itself.
<!-- llmlint: ignore-end[no_redundant_instruction_pointers] -->

## Queues

A queue is a log a transport keeps, under a layout that says how its records are
shaped, who may write them and what happens to one that is claimed. `send`
appends, `next` claims, `subscribe` tails as it grows — the GIF above is
`subscribe` on this queue — `status` reports, `validate` judges without appending
and `transports` lists the kinds this build can open. `docs/queues.md`,
`docs/validators.md` and `docs/transport.md` are those stories.

![A send printing the queue, byte position and id its record landed at; a claim printing that record back; then one counts line per queue — records, waiting, pending, abandoned, unread — each with its consumers' cursors indented beneath it](docs/screenshots/queues.svg)

## Ask, and the answer that echoes the correlation

`ask` raises a question and waits for the one reply that carries its
correlation; `reply` is how another process gives it. What the picture shows is
the shape of that wait: a correlation the moment the question is queued, then
nothing at all, then the answer. `docs/ask.md` is the rest.

![An ask printing `correlation: c-…` and then, after a lead answers from another shell, one JSON line whose answer is `reply` and whose reply record carries that same correlation](docs/screenshots/ask.svg)

## Streams

`events emit` appends one envelope to a stream file, `events merge` reads any
number of them back as one ordered stream, and `--filter` narrows what comes out.
Which source words an envelope may carry, and how its labels are typed, come from
the vocabulary `--profile` names — `--profile open` (the default) is the one this
binary links. `docs/wire.md` is the envelope.

![Two NDJSON streams merged into one timestamp-ordered listing — a checkout's and a warehouse's envelopes interleaved, with labels, payload and an artifact reference — then the same merge narrowed by a filter to one source](docs/screenshots/events-merge.svg)

```bash
$ echo '{"note":"hello"}' | onemessagebus events emit run.ndjson --kind note-left --stream s-1 --source billing --label tenant=acme
{"v":1,"ts":"2026-09-13T06:16:27.838Z","stream":"s-1","seq":1,"source":"billing","kind":"note-left","labels":{"tenant":"acme"},"payload":{"note":"hello"},"artifacts":[]}
```

## Schemas

`schema list` is every id this build knows, `schema check` judges a payload
against one, `schema register` records a document of your own and `schema gen`
renders one back as JSON or as Rust. Under them, `schemas` and
`schemas clear|fetch` are the cache a configuration's links resolve through.
`docs/schema-links.md` is how a link and its pin work.

![A listing of registered schema ids — the bus's own transport and resident protocols beside a linked bundle's desk and frame schemas — then a check refusing a record at `/blocking` and exiting 1](docs/screenshots/schema.svg)

## Serve a member protocol

`serve` answers a configured member protocol — `serve --codec`, one frame in and
one response out — and `serve --resident --socket` instead holds the transport open
and answers every capability over a unix socket, which is how both SDKs subscribe
without spawning a process per call. `deliver` and `inbox carried` are the other
side of that, for a receiver that is running and one that is not.
`docs/codecs.md` and `docs/inbox.md` are those two.

![A quote frame piped into a codec session and the response document it answered with, built from the frame's own fields](docs/screenshots/serve.svg)

## Exit codes are a contract

Three of them, and the table that assigns them is `docs/cli.md`'s. What the
picture shows is the shape a refusal takes when you meet one: a single line
naming the verb, what was wrong with it, and where to read more.

![A one-line refusal — the verb, the invalid `--timeout` value and a pointer to that verb's help — and the exit code 2 the shell then reports](docs/screenshots/refusal.svg)

## Use the library

```rust
use onemessagebus::{Emitter, Filter, Labels, Matcher, Open, Source};
use serde_json::Map;

let emitter = Emitter::<Open>::new("billing-1", Source::from("billing"), Box::new(std::io::stdout()))
    .with_labels(Labels::new().with("tenant", "acme"))
    .with_filter(Filter {
        include: Vec::new(),
        exclude: vec![Matcher::new().kind("heartbeat")],
    });
let envelope = emitter.emit("invoice-issued", Map::new());
assert_eq!(envelope.seq, 1);
```

Every wire value is a Rust type with `serde` and `schemars` derivations, and
the public API is stated in [`docs/contract.md`](docs/contract.md).

## Use the SDKs

```bash
pip install onemessagebus              # Python: import onemessagebus
npm install @onemessagebus/sdk         # TypeScript
```

Both are typed clients over the binary — one method per verb, message types
declared as a Pydantic model or with Zod's `defineMessage`, subscriptions over a
resident core — so a message defined in Python is validated by the Rust core and
read typed in TypeScript. [`docs/sdk.md`](docs/sdk.md) is the whole story.

## Develop

```bash
just bootstrap   # from a clean clone
just check       # the deterministic gate: format, lint, tests incl. the binary journeys, docs, coverage
just gate        # check plus the diff-scoped LLM-judge tier; the bar before a push
```

`AGENTS.md` is the instruction layer; `just --list` is the command surface.
