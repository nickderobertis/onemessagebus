# The crates

Four crates, one workspace version, one lockfile. The dependency runs one way:
`onemessagebus-agent` depends on `onemessagebus`, `onemessagebus-cli` depends on
both, `onemessagebus-e2e` dev-depends on all three. `deny.toml` refuses an edge
from a published crate to a sibling of the stack, and `cargo metadata` is what
shows the direction.

- **`onemessagebus`** — the core. No agent word may appear in it; the reserved
  keys of the agent vocabulary live in the profile, and
  `tests/vocabulary.rs` proves the core works over a vocabulary with none of
  them. Envelopes and matchers are read by hand (`envelope.rs`, `filter.rs`)
  because serde's `flatten` hands a flattened field only the keys it declares
  and drops the rest — the derive would swallow an unknown top-level key
  instead of refusing it by name.
- **`onemessagebus-agent`** — the profile. `tests/recorded/` are real streams
  each producer wrote, byte-identical; never edit one, and a fixture that
  needs a different shape is a new file with its provenance in the README.
  `tests/golden/` are `onepipeline`'s golden documents, copied unchanged.
- **`onemessagebus-cli`** — the binary, `publish = false`. Every verb is a
  `Capability` in the core's `capability.rs`; `tests/capability.rs` walks the
  clap tree and refuses a flag with no binding and no declared reason.
- **`onemessagebus-e2e`** — the journeys, and nothing else. They spawn the
  binary Cargo built beside them (`ONEMESSAGEBUS_BIN` overrides), so the
  project's `test` target depends on the CLI crate's `build`.
  `tests/generated/artifact_ref.rs` is what `schema gen --lang rust` prints,
  committed and compiled; regenerate it with that command when the generator
  or the schema moves.
