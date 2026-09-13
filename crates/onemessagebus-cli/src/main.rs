//! The `onemessagebus` binary. Everything it does is in the crate's library,
//! so the clap tree can be walked by `tests/capability.rs` and the journeys can
//! name the same entry point a user runs.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    onemessagebus_cli::run(std::env::args_os())
}
