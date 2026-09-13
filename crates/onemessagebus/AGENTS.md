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

The inbox (`inbox.rs`, `spool.rs`, `carry.rs`) never makes up a disposition: a
sender is answered, told the inbox closed, or told the backend lost the answer.
Any new path that could leave a sender blocked must end in one of those three —
`Reply` answers on drop for exactly that reason. The spool's file names and
documents are held by the profile's `tests/contract.rs` against
`docs/contract.md` and `docs/inbox.md`; a change to either is a contract change.

The queues (`queue.rs`) fold a log whose bytes other programs read, so a
record's field order is part of what is written: never take a key out of a
record's `serde_json::Map` with `remove`, which under `preserve_order` swaps the
last key into the hole and reorders the record under a different seal — rebuild
the map (`without_key`). The transport seam's contract (`docs/transport.md`) and
the queues' (`docs/queues.md`) are held by `tests/contract.rs` against
`docs/contract.md`, and by the conformance module's queue table, which every
transport — the local, the memory, and `crates/onemessagebus-e2e`'s plugin — runs.
