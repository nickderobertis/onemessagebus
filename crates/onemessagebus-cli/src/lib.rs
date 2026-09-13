//! The `onemessagebus` command line: the schema registry verbs and the stream
//! verbs, over the agent profile by default and the open vocabulary on request.
//!
//! Every verb is a `Capability` in [`onemessagebus::CAPABILITIES`];
//! `tests/capability.rs` walks the clap tree [`Cli`] declares and holds the two
//! to each other. `docs/cli.md` is the user's account of the same verbs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cli;
mod profile;
mod registry_dir;

pub use cli::{run, Cli};

/// The exit code of a verb that did what it was asked.
pub const EXIT_OK: u8 = 0;
/// The exit code of a verb whose input was well-formed and whose answer is no:
/// a payload that violates its schema, a stream that could not be written.
pub const EXIT_FAILED: u8 = 1;
/// The exit code of input the verb refuses: a malformed id or filter, an
/// unknown profile, an unsupported language, an unregistered id.
pub const EXIT_INVALID: u8 = 2;
