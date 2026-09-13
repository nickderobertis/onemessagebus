//! The compiled-binary journeys: every verb Contract C names, driven the way a
//! user runs it — the real `onemessagebus` binary as a subprocess, real files
//! in a temporary directory, exit codes, stdout and stderr asserted.
//!
//! One test binary rather than one per file, so the fixture is a module the
//! journeys share rather than a file each of them compiles its own copy of.

mod events;
mod rich;
mod schema;
mod support;
