//! The compiled-binary journeys: every verb Contract C names, driven the way a
//! user runs it — the real `onemessagebus` binary as a subprocess, real files
//! in a temporary directory, exit codes, stdout and stderr asserted.
//!
//! One test binary rather than one per file, so the fixture is a module the
//! journeys share rather than a file each of them compiles its own copy of.

mod ask;
mod events;
mod inbox;
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] the onepipeline 0.28.2 byte-compatibility journey fetches one exact pinned wheel (onepipeline-cli==0.28.2, the anchored engine release the compatibility promise is made against) anonymously from PyPI, so no credential is involved, and uv caches it after the first run; its edges are exactly this project's three — the core's LocalTransport, the agent crate's channel, and the CLI binary — so a project of its own would run on the identical change set and add only Nx and CI structure, and the task placed this journey in the gate because byte compatibility with that release must gate every pull request.
mod onepipeline;
mod queues;
mod rich;
mod schema;
mod support;
mod validators;
