# onemessagebus

A typed NDJSON message bus for agent communication — and for anything else
that speaks in events. One envelope, one filter grammar, payload bounds and
redaction, a schema registry with version read-sets, and an emitter and reader
over streams, generic over the **vocabulary** a consumer declares.

Three artifacts:

- **`onemessagebus`** — the core library. It knows the shape of an envelope and
  none of the words: which sources exist, which label keys are reserved and
  which top-level dimensions an envelope carries are a `Vocabulary`'s to
  declare. `Open` reserves nothing; a vocabulary of your own is proven through
  the same conformance table the agent one is.
- **`onemessagebus-agent`** — the agent profile: the sources `agentgraph`,
  `vcs`, `pipeline`; the `phase` dimension; the reserved labels `run_id`,
  `round`, `node`, `step`, `member`, `persona`; and the schema families the
  stack registers (`agent.event-envelope` at `[2, 1]`, `agent.reply-envelope`
  at `[3, 2]`). Its types serialize to the bytes `oneagentgraph`, `onevcs` and
  `onepipeline` write today, which the recorded streams under
  `crates/onemessagebus-agent/tests/recorded/` prove byte for byte.
- **`onemessagebus`**, the binary — `schema list|check|gen|register` over the
  registry and `events merge|emit` over streams, with `--profile agent` (the
  default) or `--profile open`.

## Install the command line

```bash
pip install onemessagebus-cli          # PyPI wheel, no toolchain needed
npm install -g onemessagebus-cli       # npm launcher, no toolchain needed
cargo install --git https://github.com/nickderobertis/onemessagebus onemessagebus-cli --locked
```

```bash
$ echo '{"note":"hello"}' | onemessagebus events emit run.ndjson --kind note-left --stream s-1 --source vcs --label run_id=R
{"v":1,"ts":"2026-09-13T06:16:27.838Z","stream":"s-1","seq":1,"source":"vcs","kind":"note-left","labels":{"run_id":"R"},"payload":{"note":"hello"},"artifacts":[]}
$ onemessagebus events merge run.ndjson other.ndjson --filter '{"include":[{"source":"vcs"}]}' --format text
$ onemessagebus schema check agent.event-envelope@2 --file envelope.json
```

<!-- llmlint: ignore-block[no_redundant_instruction_pointers] this README is also the PyPI page of the `onemessagebus-cli` wheel (pyproject.toml's `readme`) and the repository's front page, read by someone who has installed or found the tool and never opens AGENTS.md; these links are how that reader reaches the command line, the wire and the contract. -->
[`docs/cli.md`](docs/cli.md) is the whole command line; [`docs/wire.md`](docs/wire.md)
is the wire; [`docs/contract.md`](docs/contract.md) is the approved contract the
consumers restate from.
<!-- llmlint: ignore-end[no_redundant_instruction_pointers] -->

## Use the library

```rust
use onemessagebus_agent::{Emitter, EventFilter, Labels, Source};
use serde_json::Map;

let emitter = Emitter::new("run-1", Source::Vcs, Box::new(std::io::stdout()))
    .with_labels(Labels { run_id: Some("R".into()), ..Labels::default() })
    .with_filter(EventFilter::parse(r#"{"exclude":[{"kind":"heartbeat"}]}"#)?);
let envelope = emitter.emit("push", Map::new());
assert_eq!(envelope.seq, 1);
```

Every wire value is a Rust type with `serde` and `schemars` derivations, and
the public API is stated in [`docs/contract.md`](docs/contract.md).

## Develop

```bash
just bootstrap   # from a clean clone
just check       # the deterministic gate: format, lint, tests incl. the binary journeys, docs, coverage
just gate        # check plus the diff-scoped LLM-judge tier; the bar before a push
```

`AGENTS.md` is the instruction layer; `just --list` is the command surface.
