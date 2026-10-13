# onemessagebus-repo (the repository's own configuration)

Tests only, over files at the root rather than over any crate: the release
declaration, and the pins restated outside their sources. It is its own project so
a release or toolchain change runs these and not the journeys, and it is the one
wrapper `deny.toml` lets name `onevcs`. It depends on no workspace crate; keep it
that way, which is why its `test` target's `inputs` name the root files these tests
read — Nx cannot see through an `include_str!`.

`tests/visual_docs_scripts.rs` sits here because a
Rust integration test must sit in a crate, but it belongs to
`onemessagebus-visual-docs`, which runs it and declares the code it reaches into.
Nextest filters in the justfile are what split the two runs apart.
