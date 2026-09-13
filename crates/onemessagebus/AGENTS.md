# onemessagebus (the core)

No agent word may appear in this crate: the reserved keys of the agent
vocabulary live in the profile, and `tests/vocabulary.rs` proves the core works
over a vocabulary with none of them. A new wire fact belongs on the
`Vocabulary` trait as a type or a constant, never as a word in here.

Envelopes and matchers are read by hand (`envelope.rs`, `filter.rs`) because
serde's `flatten` hands a flattened field only the keys it declares and drops
the rest — the derive would swallow an unknown top-level key instead of
refusing it by name. Keep `Serialize` derived, so the wire order stays the
field order.

`conformance.rs` is test support published for profile crates; it is excluded
from the coverage floor and exercised by its callers.
