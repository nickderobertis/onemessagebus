# onemessagebus

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
- **`onemessagebus`**, the binary — `schema list|check|gen|register` over the
  registry, `schemas` with `schemas clear|fetch` over the cache of schema
  bundles a configuration links by URL, `events merge|emit` over streams with
  `--profile open` (the default) — the one vocabulary it links — `deliver` and `inbox carried` over the inbox —
  a typed channel into a running process whose sender learns what the receiver
  did with each message — and `send`, `next`, `reply`, `subscribe` and `status`
  over durable queues kept on a transport a configuration names — under a
  layout a linked schema bundle declares as data, or one a program links —
  with
  `transports` listing the kinds a transport can be, `ask` raising a question and
  waiting for the one reply that echoes its correlation, `validate` judging a
  record by a queue's validators before anything is sent, and `serve` answering a
  a configured member protocol's frames. The local transport keeps
  each queue as plain files in one directory, and a distributed one is a plugin
  rather than a consumer change.

## Install the command line

```bash
pip install onemessagebus-cli          # PyPI wheel, no toolchain needed
npm install -g onemessagebus-cli       # npm launcher, no toolchain needed
cargo install --git https://github.com/nickderobertis/onemessagebus onemessagebus-cli --locked
```

```bash
$ echo '{"note":"hello"}' | onemessagebus events emit run.ndjson --kind note-left --stream s-1 --source billing --label tenant=acme
{"v":1,"ts":"2026-09-13T06:16:27.838Z","stream":"s-1","seq":1,"source":"billing","kind":"note-left","labels":{"tenant":"acme"},"payload":{"note":"hello"},"artifacts":[]}
$ onemessagebus events merge run.ndjson other.ndjson --filter '{"include":[{"source":"billing"}]}' --format text
$ onemessagebus schema check onemessagebus.transport-hello@1 --file hello.json
```

<!-- llmlint: ignore-block[no_redundant_instruction_pointers] this README is also the PyPI page of the `onemessagebus-cli` wheel (pyproject.toml's `readme`) and the repository's front page, read by someone who has installed or found the tool and never opens AGENTS.md; these links are how that reader reaches the command line, the wire and the contract. -->
[`docs/cli.md`](docs/cli.md) is the whole command line; [`docs/wire.md`](docs/wire.md)
is the wire; [`docs/contract.md`](docs/contract.md) is the approved contract the
consumers restate from.
<!-- llmlint: ignore-end[no_redundant_instruction_pointers] -->

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
