//! The clap tree the binary actually builds, walked the same way by every
//! suite that holds something else to it — the capability manifest, the
//! documentation.

use std::collections::BTreeSet;

use clap::CommandFactory;
use onemessagebus_cli::Cli;

/// Flags clap generates rather than the CLI declaring them.
const CLAP_BUILTINS: &[&str] = &["help", "version"];

/// Every verb clap exposes, as its argv path, with the command: each leaf, and
/// each command that runs on its own as well as through its subcommands
/// (`schemas`, beside `schemas clear`).
pub fn clap_verbs() -> Vec<(Vec<String>, clap::Command)> {
    let mut found = Vec::new();
    walk(&Cli::command(), &[], &mut found);
    found
}

fn walk(command: &clap::Command, path: &[String], out: &mut Vec<(Vec<String>, clap::Command)>) {
    let mut children = command.get_subcommands().peekable();
    let runs_alone = children.peek().is_none() || !command.is_subcommand_required_set();
    if runs_alone && !path.is_empty() {
        out.push((path.to_vec(), command.clone()));
    }
    for child in children {
        if child.get_name() == "help" {
            continue;
        }
        let mut child_path = path.to_vec();
        child_path.push(child.get_name().to_string());
        walk(child, &child_path, out);
    }
}

/// The long flags a verb declares, minus clap's own.
pub fn long_flags(command: &clap::Command) -> BTreeSet<String> {
    command
        .get_arguments()
        .filter_map(|arg| arg.get_long())
        .filter(|long| !CLAP_BUILTINS.contains(long))
        .map(|long| format!("--{long}"))
        .collect()
}
