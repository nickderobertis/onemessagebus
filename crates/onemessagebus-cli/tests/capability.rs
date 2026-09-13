//! The capability manifest, reconciled against the real clap surface.
//!
//! `onemessagebus::CAPABILITIES` is what the parity gate measures every
//! consumer surface against, so the manifest going stale would quietly hollow
//! the gate out. This walks the command tree clap actually builds and holds
//! the manifest to it in both directions: every verb has a capability, every
//! capability names a verb, and every long flag on a verb is either bound to
//! an SDK option or declared uncovered with a reason. There is no third option
//! and no default.

use std::collections::BTreeSet;

use clap::CommandFactory;
use onemessagebus::{Capability, FlagKind, CAPABILITIES};
use onemessagebus_cli::Cli;

/// Flags clap generates rather than the CLI declaring them.
const CLAP_BUILTINS: &[&str] = &["help", "version"];

/// Every leaf verb clap exposes, as its argv path, with the command.
fn clap_verbs() -> Vec<(Vec<String>, clap::Command)> {
    let mut found = Vec::new();
    walk(&Cli::command(), &[], &mut found);
    found
}

fn walk(command: &clap::Command, path: &[String], out: &mut Vec<(Vec<String>, clap::Command)>) {
    let mut children = command.get_subcommands().peekable();
    if children.peek().is_none() {
        if !path.is_empty() {
            out.push((path.to_vec(), command.clone()));
        }
        return;
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
fn long_flags(command: &clap::Command) -> BTreeSet<String> {
    command
        .get_arguments()
        .filter_map(|arg| arg.get_long())
        .filter(|long| !CLAP_BUILTINS.contains(long))
        .map(|long| format!("--{long}"))
        .collect()
}

/// The positional arguments a verb declares.
fn positionals(command: &clap::Command) -> usize {
    command
        .get_arguments()
        .filter(|arg| arg.is_positional())
        .count()
}

/// Every flag spelling a capability's bindings render or decline.
fn decided_flags(capability: &Capability) -> BTreeSet<String> {
    capability
        .bindings
        .iter()
        .filter_map(|binding| binding.kind.flag())
        .map(str::to_owned)
        .chain(capability.uncovered.iter().map(|flag| flag.flag.to_owned()))
        .collect()
}

#[test]
fn every_verb_is_a_capability() {
    for (path, _) in clap_verbs() {
        assert!(
            CAPABILITIES.iter().any(|c| c.verb == path.as_slice()),
            "`onemessagebus {}` has no capability in onemessagebus::CAPABILITIES; add one with \
             its SDK method, option contract and library entry, or a verb goes missing from \
             every SDK unnoticed",
            path.join(" ")
        );
    }
}

#[test]
fn every_capability_names_a_verb_clap_actually_has() {
    let verbs: Vec<Vec<String>> = clap_verbs().into_iter().map(|(path, _)| path).collect();
    for capability in CAPABILITIES {
        let path: Vec<String> = capability.verb.iter().map(|s| (*s).to_owned()).collect();
        assert!(
            verbs.contains(&path),
            "capability `{}` invokes `onemessagebus {}`, which clap does not expose",
            capability.method,
            path.join(" ")
        );
    }
    let mut methods: Vec<&str> = CAPABILITIES.iter().map(|c| c.method).collect();
    methods.sort_unstable();
    methods.dedup();
    assert_eq!(
        methods.len(),
        CAPABILITIES.len(),
        "two capabilities share a method name"
    );
}

#[test]
fn every_flag_is_bound_or_declined_with_a_reason_and_every_binding_is_a_real_flag() {
    for (path, command) in clap_verbs() {
        let joined = path.join(" ");
        let Some(capability) = CAPABILITIES.iter().find(|c| c.verb == path.as_slice()) else {
            continue; // reported by `every_verb_is_a_capability`
        };
        let real = long_flags(&command);
        let decided = decided_flags(capability);
        let undecided: Vec<&String> = real.difference(&decided).collect();
        assert!(
            undecided.is_empty(),
            "`onemessagebus {joined}` has flags no SDK option renders and no reason excludes: \
             {undecided:?}. Bind each in CAPABILITIES, or list it as uncovered with a reason."
        );
        let phantom: Vec<&String> = decided.difference(&real).collect();
        assert!(
            phantom.is_empty(),
            "capability `{}` renders flags `onemessagebus {joined}` does not have: {phantom:?}",
            capability.method
        );
        let bound_positionals = capability
            .bindings
            .iter()
            .filter(|binding| binding.kind == FlagKind::Positional)
            .count();
        assert_eq!(
            bound_positionals,
            positionals(&command),
            "`onemessagebus {joined}` and capability `{}` disagree on positional arguments",
            capability.method
        );
        for uncovered in capability.uncovered {
            assert!(
                !uncovered.reason.trim().is_empty(),
                "{} declines {} with no reason",
                capability.method,
                uncovered.flag
            );
        }
    }
}

/// Payloads never travel as a positional: the only positionals in the tree
/// are ids and paths, so a verb that takes a payload takes it on stdin or
/// `--file` and a document passed as an argument is a usage error.
#[test]
fn no_verb_reads_a_payload_from_a_positional() {
    for (path, command) in clap_verbs() {
        let positional_names: Vec<String> = command
            .get_arguments()
            .filter(|arg| arg.is_positional())
            .map(|arg| arg.get_id().to_string())
            .collect();
        for name in &positional_names {
            assert!(
                matches!(name.as_str(), "id" | "files" | "path"),
                "`onemessagebus {}` has a positional `{name}` that is not an id or a path",
                path.join(" ")
            );
        }
        let capability = CAPABILITIES
            .iter()
            .find(|c| c.verb == path.as_slice())
            .expect("a capability");
        if capability.stdin {
            assert!(
                long_flags(&command).contains("--file"),
                "`onemessagebus {}` takes a payload on stdin but offers no --file",
                path.join(" ")
            );
        }
    }
}
