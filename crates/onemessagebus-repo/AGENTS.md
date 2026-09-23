# onemessagebus-repo (the repository's own configuration)

Tests only, over files at the root rather than over any crate: the release
declaration, the toolchain and workflow pins restated outside their sources, and
the committed `.githooks/pre-push` and the scripts around it, driven for real
with the third-party tools they shell out to stood in for. `screenshots/capture.sh`
is deliberately absent — driving it renders screenshots, which belong to no gate
(`screenshots/AGENTS.md`) — and its output is gated by the committed digest
baseline instead.

It is its own project so a release, toolchain or visual-docs change runs these
and not the journeys, and it is the one wrapper `deny.toml` lets name `onevcs`.
Its `test` target's `inputs` name every root file these tests read, because Nx
cannot see through an `include_str!`. It depends on no workspace crate; keep it
that way.
