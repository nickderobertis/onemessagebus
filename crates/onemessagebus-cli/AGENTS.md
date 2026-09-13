# onemessagebus-cli (the binary)

Unpublished: it links the agent profile so `--profile` defaults to it, and a
binary links only its own crate's dependencies, so it can live in neither
library. Every verb is a `Capability` in the core's `capability.rs`;
`tests/capability.rs` walks the clap tree and refuses a flag with no binding and
no declared reason, so a new flag is a manifest change first.

Payloads arrive on stdin or `--file`, never as a positional; refusals go to
stderr with exit 2, a well-formed no with exit 1.
