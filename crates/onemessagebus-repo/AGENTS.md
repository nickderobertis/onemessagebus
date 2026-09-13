# onemessagebus-repo (the repository's own configuration)

Tests only, over files at the root rather than over any crate:
`tests/release_declaration.rs` holds `release-targets.toml` to its schema
through `onevcs`'s reader, and `tests/toolchain_pins.rs` holds `clippy.toml` and
`rustfmt.toml` to `Cargo.toml`. It is its own project so a release or toolchain
change runs these and not the journeys, and it is the one wrapper `deny.toml`
lets name `onevcs`. It depends on no workspace crate; keep it that way.
