//! The compiled-binary journeys: every verb Contract C names, driven the way a
//! user runs it — the real `onemessagebus` binary as a subprocess, real files
//! in a temporary directory, exit codes, stdout and stderr asserted.
//!
//! One test binary rather than one per file, so the fixture is a module the
//! journeys share rather than a file each of them compiles its own copy of.

mod ask;
mod events;
mod inbox;
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] these two journeys wait out the connect and read bounds Contract L fixes (5 s and 15 s) against loopback listeners they start, with no network and no credential; their edges are exactly `links`' — the core's resolver and the CLI binary this project already depends on — so a project of their own would be affected by the identical change set, and nextest runs them in parallel with the rest of the suite, whose wall time they add about 15 s to. The node that introduced them is required to measure both bounds in this suite.
mod link_timeouts;
mod links;
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] the onepipeline 0.28.2 byte-compatibility journey fetches one exact pinned wheel (onepipeline-cli==0.28.2, the anchored engine release the compatibility promise is made against) anonymously from PyPI, so no credential is involved, and uv caches it after the first run; its edges are exactly this project's three — the core's LocalTransport, the agent crate's channel, and the CLI binary — so a project of its own would run on the identical change set and add only Nx and CI structure, and the task placed this journey in the gate because byte compatibility with that release must gate every pull request.
mod onepipeline;
mod queues;
// The resident core listens on a unix socket, which only a unix build has.
#[cfg(unix)]
mod resident;
// Where there is no unix socket, the resident core is refused by name.
#[cfg(not(unix))]
mod resident_unavailable;
mod rich;
mod schema;
mod serve;
mod support;
mod validators;
