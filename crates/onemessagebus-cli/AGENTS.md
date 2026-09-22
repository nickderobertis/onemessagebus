# onemessagebus-cli (the binary)

Unpublished, and its own crate so the core stays a library with no command-line
dependency in it. It links one vocabulary, the core's `Open`, which `--profile`
defaults to, and registers the bus's own schemas and no product's. Every verb is a `Capability` in the core's `capability.rs`;
`tests/capability.rs` walks the clap tree and refuses a flag with no binding and
no declared reason, so a new flag is a manifest change first.

Payloads arrive on stdin or `--file` — and `deliver`'s message also through the
named `--message`, from exactly one of the three — never as a positional; refusals go to
stderr with exit 2, a well-formed no with exit 1.
