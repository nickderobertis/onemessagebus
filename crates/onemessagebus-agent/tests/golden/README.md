# Golden documents

Copied from `onepipeline`'s `tests/golden/` unchanged: the event envelope at
versions 1 and 2. `tests/registry.rs` validates each against the schema the
profile registers under its family and version, and reads each at the version
this build writes.

## `onejudge-0.8.1-note.json`

Not one of `onepipeline`'s documents: the bytes and refusal words of the note
shapes as `onejudge` released them. Captured by compiling
`crates/onejudge/src/note.rs` at tag `v0.8.1` (commit
`729bd43e3b9ff5c7a63415ebd755dd70ab0ce5aa`) unchanged in a scratch crate beside
this workspace's lockfile, and serializing, refusing and rendering the values
that module's own unit tests use; its `provenance` field says the same.
`tests/note.rs` builds each value with `onemessagebus_agent::note` and compares.
Re-capture it only against a new `onejudge` release, as a new file.
