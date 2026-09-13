# Golden documents

Copied from `onepipeline`'s `tests/golden/` unchanged: the event envelope at
versions 1 and 2, and the reply envelope at versions 2 and 3. `tests/registry.rs`
validates each against the schema the profile registers under its family and
version, and reads each at the version this build writes.
