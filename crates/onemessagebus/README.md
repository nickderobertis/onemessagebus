# onemessagebus

A typed NDJSON message bus, generic over the vocabulary you declare: one
envelope, one filter grammar, payload bounds and redaction, a schema registry
with version read-sets, and an emitter and reader over streams.

Nothing here names an agent. Which source words exist, which label keys are
reserved and what each admits, and which top-level dimensions an envelope
carries are a `Vocabulary`'s to declare — `Open` reserves nothing, and a
program declares its own vocabulary in its own crate, never in the bus. A
vocabulary of your own is proven the way `Open` is: `onemessagebus::conformance::drive` runs the one
table every vocabulary is held to.

```rust
use onemessagebus::{Emitter, Filter, Labels, Matcher, Open, Source};
use serde_json::Map;

let emitter = Emitter::<Open>::new("billing-1", Source::from("billing"), Box::new(std::io::stdout()))
    .with_labels(Labels::new().with("tenant", "acme"))
    .with_filter(Filter {
        include: Vec::new(),
        exclude: vec![Matcher::new().kind("heartbeat")],
    });
let written = emitter.emit("invoice-issued", Map::new());
assert_eq!(written.seq, 1);
```

<!-- llmlint: ignore-block[no_redundant_instruction_pointers] this README is the crate's crates.io and docs.rs page (`readme = "README.md"`), read by a consumer who has only the packaged crate and never this repository's AGENTS.md; these links are how that reader reaches the wire and the contract at all. -->
The wire, the grammar and the registry rules are stated in the repository's
[`docs/wire.md`](https://github.com/nickderobertis/onemessagebus/blob/main/docs/wire.md)
and held by
[`docs/contract.md`](https://github.com/nickderobertis/onemessagebus/blob/main/docs/contract.md).
<!-- llmlint: ignore-end[no_redundant_instruction_pointers] -->
