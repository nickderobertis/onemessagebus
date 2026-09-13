# onemessagebus-agent

The agent profile over the `onemessagebus` core: the vocabulary the agent stack
shares, declared once here and re-exported by every consumer.

- `Source::{Agentgraph, Vcs, Pipeline}` — the three producers, and the envelope
  version each writes against (`pipeline` 2, the others 1).
- `Phase::{Development, Integrate, Review, Release}` — the reserved top-level
  dimension, omitted from the wire when absent.
- `Labels` — `run_id`, `round`, `node`, `step`, `member`, `persona`, plus a
  flattened map of free-form extras.
- `Envelope`, `EventFilter`, `Matcher`, `Emitter`, `Reader`, `Merge` — the core's
  generic types over this vocabulary, serializing to the bytes `oneagentgraph`,
  `onevcs` and `onepipeline` write today.
- `registry()` — every schema the stack registers: `agent.event-envelope` at
  `[2, 1]`, `agent.reply-envelope` at `[3, 2]`, and the artifact reference, the
  filter and the labels at 1.

```rust
use onemessagebus_agent::{Emitter, Labels, Phase, Source};
use serde_json::Map;

let emitter = Emitter::new("s-1", Source::Vcs, Box::new(std::io::stdout()))
    .with_labels(Labels { run_id: Some("R".into()), ..Labels::default() })
    .with_dimensions(Phase::Development.into());
let envelope = emitter.emit("push", Map::new());
assert_eq!(envelope.v, 1);
```

<!-- llmlint: ignore-block[no_redundant_instruction_pointers] this README is the crate's crates.io and docs.rs page (`readme = "README.md"`), read by a consumer who has only the packaged crate and never this repository's AGENTS.md; this link is how that reader reaches the contract at all. -->
The contract every consumer restates from is
[`docs/contract.md`](https://github.com/nickderobertis/onemessagebus/blob/main/docs/contract.md).
<!-- llmlint: ignore-end[no_redundant_instruction_pointers] -->
