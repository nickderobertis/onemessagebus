//! The `dirfiles` transport plugin: the directory-of-files transport the plugin
//! journeys run the queue table over, served over the plugin protocol so the
//! `onemessagebus` binary opens it by kind with no change of its own.
//!
//! The transport is the one `tests/plugin_transport.rs` declares, compiled here
//! rather than copied, so the executable the binary spawns and the transport the
//! in-process table runs over are one implementation.

use std::process::ExitCode;

// llmlint: ignore[shared_internals_are_a_project_not_a_reach_in] the task places the third transport's implementation in tests/plugin_transport.rs and requires the binary to reach it as a plugin executable; this crate is that proof and nothing else, so including the one file keeps a single implementation where a copy would be a second one free to drift.
#[path = "../../tests/plugin_transport.rs"]
mod plugin_transport;

fn main() -> ExitCode {
    let served = onemessagebus::transport::serve(
        plugin_transport::DirFiles::from_config,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    );
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!(
                "onemessagebus-transport-{}: {failure}",
                plugin_transport::KIND
            );
            ExitCode::FAILURE
        }
    }
}
