# onemessagebus-e2e (the journeys)

Only journeys live here. They spawn the binary Cargo built beside them
(`ONEMESSAGEBUS_BIN` overrides), so this project's `test` target depends on the
CLI crate's `build`; run it with `just test-e2e`.

`tests/generated/` is what `schema gen --lang rust` prints, committed and
compiled: `artifact_ref.rs` for a profile schema and `rich.rs` for
`tests/e2e/rich.rs`, the hand-written document exercising every construct the
renderer covers. Regenerate a file with that command when the renderer or its
schema moves; the journeys hold each to the binary's current output and to the
document it regenerates.

A test about the repository's configuration rather than the binary (the release
declaration, the toolchain pins) belongs in `crates/onemessagebus-repo`, so it
does not pay for this suite.

A journey that needs a receiver to die without closing runs this test binary
as its child and kills that `Child` by the handle that started it — never a
process found by name, which on a shared host is somebody else's.

`tests/plugin_transport.rs` is the third transport, written outside the core,
and `src/bin/onemessagebus-transport-dirfiles.rs` compiles that same file into
the plugin executable the binary finds on `PATH`: the queue table and the binary
run over one implementation, so change the transport there and nowhere else.

The journeys' layout is the bus's own `tests/layouts/desk.json`, linked by a
configuration; `src/bin/onemessagebus-with-desk.rs` is the program that links a
compiled-in `desk`.

## The resident core

A resident core a journey starts is stopped by removing its socket, which ends
it with a normal exit. Killing one instead loses its coverage profile — the
resident's lines then count as missed — and leaves a stale socket for the next
start to take over. The one journey that kills a resident does so to make that
stale socket.
