# onemessagebus-repo (the repository's own configuration)

Tests only, over files at the root rather than over any crate:
`tests/release_declaration.rs` holds `release-targets.toml` to its schema
through `onevcs`'s reader, `tests/toolchain_pins.rs` holds `clippy.toml` and
`rustfmt.toml` to `Cargo.toml`, `tests/visual_docs_pins.rs` holds the two
versions `.github/workflows/visual-docs.yml` restates to their sources, and
`tests/visual_docs_guard.rs` drives the committed `.githooks/pre-push` the way
git does, over a throwaway repository with screencomp, freeze and the capture
stood in for — the guard's decisions belong to a gate, its screenshots do not.
It is its own project so a release, toolchain or
visual-docs change runs these and not the journeys, and it is the one wrapper
`deny.toml` lets name `onevcs`. It depends on no workspace crate; keep it that
way.
