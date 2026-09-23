//! `docs/cli.md` and the README, held to the clap tree the binary builds.
//!
//! Both restate the command line and nothing else reconciles them with it, so
//! every expected value here is read from the binary — its verbs, flags,
//! possible values, environment fallbacks and exit codes — or from the
//! profile it links:
//!
//! - `docs/cli.md`: a heading per verb and no heading for a verb the binary
//!   lacks; every flag of a verb under its heading, and no heading flag or
//!   value the verb does not take; each rule that names a flag on every verb
//!   of a family; the exit code table; the default profile, the default source
//!   word, the label typing and the payload bound; every sample invocation.
//! - `README.md`: every verb of each family in its verb list and no other;
//!   the profile names, and which is the default; every sample invocation; and
//!   every flag its prose names, against the verb the span names it beside.

mod clap_tree;

use std::collections::{BTreeMap, BTreeSet};

use clap::Parser as _;
use clap_tree::{clap_verbs, long_flags};
use onemessagebus::{Admits, Open, Vocabulary as _, MAX_PAYLOAD_TEXT_BYTES};
use onemessagebus_cli::{Cli, EXIT_FAILED, EXIT_INVALID, EXIT_OK};

const CLI_MD: (&str, &str) = ("docs/cli.md", include_str!("../../../docs/cli.md"));
const README: (&str, &str) = ("README.md", include_str!("../../../README.md"));

/// `text` with every run of whitespace collapsed to one space, so a statement
/// wrapped across lines reads as it does rendered.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The section under the `## ` heading `title`, up to the next one.
fn section<'a>((file, doc): (&str, &'a str), title: &str) -> &'a str {
    let heading = format!("\n## {title}\n");
    let start = doc
        .find(&heading)
        .unwrap_or_else(|| panic!("{file} has no `## {title}` section; the gate reads it there"))
        + heading.len();
    let rest = &doc[start..];
    &rest[..rest.find("\n## ").unwrap_or(rest.len())]
}

/// The text between the first `after` and the next `until`, flattened.
fn between((file, doc): (&str, &str), after: &str, until: &str) -> String {
    let doc = flat(doc);
    let start = doc
        .find(after)
        .unwrap_or_else(|| panic!("{file} no longer says `{after}`; the gate reads it there"))
        + after.len();
    let end = doc[start..]
        .find(until)
        .unwrap_or_else(|| panic!("{file}: nothing ends `{after}` with `{until}`"));
    doc[start..start + end].to_owned()
}

/// `doc` without its fenced blocks, whose three-backtick fences would pair the
/// prose's spans wrongly.
fn prose(doc: &str) -> String {
    doc.split("```").step_by(2).collect::<Vec<_>>().join(" ")
}

/// Every backticked span in `text`, in order.
fn ticked(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// Every `--flag` spelled in `text`, in order.
fn flags_in(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find("--") {
        let start = at + offset;
        let glued =
            start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'-');
        let name: String = text[start + 2..]
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .collect();
        if !glued && name.starts_with(|c: char| c.is_ascii_lowercase()) {
            found.push(format!("--{name}"));
        }
        at = start + 2 + name.len();
    }
    found
}

/// One `### ` heading of `docs/cli.md`: the verb it names, the heading's
/// synopsis, and the prose under it.
struct Heading {
    verb: Vec<String>,
    synopsis: String,
    body: String,
}

fn headings() -> Vec<Heading> {
    let (file, doc) = CLI_MD;
    let mut found = Vec::new();
    for part in doc.split("\n### ").skip(1) {
        let (line, rest) = part.split_once('\n').unwrap_or((part, ""));
        let synopsis = line
            .strip_prefix('`')
            .and_then(|l| l.strip_suffix('`'))
            .unwrap_or_else(|| panic!("{file}: the heading `### {line}` is not one `synopsis`"))
            .to_owned();
        let verb = synopsis
            .split_whitespace()
            .take_while(|word| word.chars().all(|c| c.is_ascii_lowercase()))
            .map(str::to_owned)
            .collect();
        let body = rest[..rest.find("\n## ").unwrap_or(rest.len())].to_owned();
        found.push(Heading {
            verb,
            synopsis,
            body,
        });
    }
    found
}

fn verb_names() -> BTreeSet<String> {
    clap_verbs()
        .into_iter()
        .map(|(path, _)| path.join(" "))
        .collect()
}

#[test]
fn cli_md_has_a_heading_for_every_verb_and_none_for_a_verb_the_binary_lacks() {
    let (file, _) = CLI_MD;
    let documented: BTreeSet<String> = headings().iter().map(|h| h.verb.join(" ")).collect();
    let real = verb_names();
    let missing: Vec<&String> = real.difference(&documented).collect();
    let extra: Vec<&String> = documented.difference(&real).collect();
    assert!(
        missing.is_empty(),
        "{file} has no `### ` heading for {missing:?}; document each verb the binary has"
    );
    assert!(
        extra.is_empty(),
        "{file} has a heading for {extra:?}, which the binary does not have; remove or rename it"
    );
}

#[test]
fn every_flag_is_documented_under_its_verb_and_every_heading_flag_is_real() {
    let (file, _) = CLI_MD;
    let headings = headings();
    for (path, command) in clap_verbs() {
        let verb = path.join(" ");
        let Some(heading) = headings.iter().find(|h| h.verb == path) else {
            continue; // reported by the heading test
        };
        let documented: BTreeSet<String> = flags_in(&heading.synopsis)
            .into_iter()
            .chain(flags_in(&heading.body))
            .collect();
        let real = long_flags(&command);
        let missing: Vec<&String> = real.difference(&documented).collect();
        assert!(
            missing.is_empty(),
            "{file}: `{verb}` takes {missing:?}, but neither its heading nor its section \
             mentions them; document each flag"
        );
        let words: Vec<&str> = heading.synopsis.split_whitespace().collect();
        for (index, word) in words.iter().enumerate() {
            let Some(flag) = flags_in(word).into_iter().next() else {
                continue;
            };
            assert!(
                real.contains(&flag),
                "{file}: the heading for `{verb}` names {flag}, which `onemessagebus {verb}` \
                 does not take; the verb takes {real:?}"
            );
            let Some(values) = words.get(index + 1).map(|w| w.trim_matches(['[', ']'])) else {
                continue;
            };
            if !values.contains('|') {
                continue;
            }
            let stated: BTreeSet<&str> = values.split('|').collect();
            let arg = command
                .get_arguments()
                .find(|arg| arg.get_long() == flag.strip_prefix("--"))
                .expect("a real flag has an argument");
            let taken: Vec<String> = arg
                .get_possible_values()
                .iter()
                .filter(|value| !value.is_hide_set())
                .map(|value| value.get_name().to_owned())
                .collect();
            let taken: BTreeSet<&str> = taken.iter().map(String::as_str).collect();
            assert_eq!(
                stated, taken,
                "{file}: the heading for `{verb}` gives {flag} the values {stated:?}, but the \
                 verb takes {taken:?}"
            );
        }
    }
}

#[test]
fn each_rule_that_names_a_flag_on_a_family_holds_on_every_verb_of_it() {
    let (file, _) = CLI_MD;
    let rules = section(CLI_MD, "Rules every verb keeps");
    let verbs = clap_verbs();
    let mut held = 0;
    for bullet in rules.split("\n- ").map(flat) {
        let family = ["on every `", "on both `"]
            .iter()
            .find_map(|marker| bullet.split_once(marker))
            .and_then(|(_, rest)| rest.split_once('`'))
            .map(|(family, _)| family.to_owned());
        let (Some(family), Some(flag)) = (family, flags_in(&bullet).into_iter().next()) else {
            continue;
        };
        held += 1;
        let members: Vec<&(Vec<String>, clap::Command)> =
            verbs.iter().filter(|(path, _)| path[0] == family).collect();
        assert!(
            !members.is_empty(),
            "{file}: a rule names the `{family}` verbs, and the binary has none"
        );
        if bullet.contains("on both `") {
            assert_eq!(
                members.len(),
                2,
                "{file} says `{flag}` is on both `{family}` verbs, but `{family}` has {}",
                members.len()
            );
        }
        let env = ticked(&bullet)
            .into_iter()
            .find(|span| span.chars().all(|c| c.is_ascii_uppercase() || c == '_'));
        for (path, command) in members {
            let arg = command
                .get_arguments()
                .find(|arg| arg.get_long() == flag.strip_prefix("--"))
                .unwrap_or_else(|| {
                    panic!(
                        "{file} says {flag} is on every `{family}` verb, but `onemessagebus {}` \
                         does not take it",
                        path.join(" ")
                    )
                });
            if let Some(env) = &env {
                assert_eq!(
                    arg.get_env().and_then(|e| e.to_str()),
                    Some(env.as_str()),
                    "{file} says {flag} falls back to `{env}` on `onemessagebus {}`, but it does not",
                    path.join(" ")
                );
            }
        }
    }
    assert!(
        held > 0,
        "{file}'s rules no longer name a flag on a family's verbs; the gate found nothing to hold"
    );
}

#[test]
fn cli_md_states_the_exit_codes_the_binary_uses() {
    let (file, _) = CLI_MD;
    let stated: BTreeSet<u8> = section(CLI_MD, "Exit codes")
        .lines()
        .filter_map(|row| row.strip_prefix("| `"))
        .map(|row| {
            let code = row.split('`').next().unwrap_or_default();
            code.parse()
                .unwrap_or_else(|_| panic!("{file}: `{code}` in the exit code table is not a code"))
        })
        .collect();
    let used = BTreeSet::from([EXIT_OK, EXIT_FAILED, EXIT_INVALID]);
    assert_eq!(
        stated, used,
        "{file} lists the exit codes {stated:?}, but the binary uses {used:?}; correct the table"
    );
    // The table says a usage error is refused input and help did what was
    // asked; both codes are clap's, so hold clap to them.
    let usage = Cli::try_parse_from(["onemessagebus", "no-such-verb"])
        .expect_err("an unknown verb is a usage error");
    assert_eq!(usage.exit_code(), i32::from(EXIT_INVALID));
    let help = Cli::try_parse_from(["onemessagebus", "--help"]).expect_err("help exits early");
    assert_eq!(help.exit_code(), i32::from(EXIT_OK));
}

#[test]
fn cli_md_states_the_defaults_and_bounds_the_binary_links() {
    let (file, _) = CLI_MD;
    let default_profile = between(CLI_MD, "and it defaults to `", "`");
    assert_eq!(
        default_profile,
        Open::NAME,
        "{file} says `--profile` defaults to `{default_profile}`, but the default is `{}`",
        Open::NAME
    );
    let default_source = between(CLI_MD, "profile's default word (`", "`");
    assert_eq!(
        default_source,
        Open::DEFAULT_SOURCE,
        "{file} says the open profile's default source is `{default_source}`, but it is `{}`",
        Open::DEFAULT_SOURCE
    );
    let label = between(CLI_MD, "(`--label ", "=");
    assert!(
        !Open::RESERVED
            .iter()
            .any(|reserved| reserved.key == label && reserved.admits == Admits::Integer),
        "{file} gives `--label {label}=…` as a text label, but the open profile types `{label}` \
         as an integer"
    );
    assert_eq!(
        flat(CLI_MD.1).contains("`open` reserves no key"),
        Open::RESERVED.is_empty(),
        "{file} says whether `open` reserves a label key, and it must agree with Open::RESERVED"
    );
    let bound = between(CLI_MD, "bounded to ", " bytes");
    assert_eq!(
        bound,
        MAX_PAYLOAD_TEXT_BYTES.to_string(),
        "{file} bounds a payload text value to {bound} bytes, but MAX_PAYLOAD_TEXT_BYTES is {}",
        MAX_PAYLOAD_TEXT_BYTES
    );
}

#[test]
fn the_readme_lists_every_verb_and_only_the_binarys() {
    let (file, doc) = README;
    let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, _) in clap_verbs() {
        declared
            .entry(path[0].clone())
            .or_default()
            .insert(path[1..].join(" "));
    }
    let mut stated: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for span in ticked(&flat(&prose(doc))) {
        // A verb with no family (`deliver`), or a family that runs on its own
        // as well as through its verbs (`schemas`), is stated by its own name.
        let (family, verbs) = match span.split_once(' ') {
            Some(split) => split,
            None if declared
                .get(&span)
                .is_some_and(|verbs| verbs.contains(&String::new())) =>
            {
                (span.as_str(), "")
            }
            None => continue,
        };
        let is_list = verbs.chars().all(|c| c.is_ascii_lowercase() || c == '|');
        if declared.contains_key(family) && is_list {
            stated
                .entry(family.to_owned())
                .or_default()
                .extend(verbs.split('|').map(str::to_owned));
        }
    }
    assert_eq!(
        stated, declared,
        "{file}'s verb list (`family a|b|…`) states {stated:?}, but the binary has {declared:?}; \
         correct the list"
    );
    let profiles: Vec<String> = ticked(&flat(&prose(doc)))
        .into_iter()
        .filter_map(|span| span.strip_prefix("--profile ").map(str::to_owned))
        .collect();
    let linked = vec![Open::NAME.to_owned()];
    assert_eq!(
        profiles, linked,
        "{file} names the profiles {profiles:?}, but the binary links {linked:?}, default first"
    );
    let default = between(README, "`--profile ", "` (the default)");
    assert_eq!(
        default,
        Open::NAME,
        "{file} calls `{default}` the default profile, but it is `{}`",
        Open::NAME
    );
}

#[test]
fn every_sample_invocation_names_a_real_verb_and_its_real_flags() {
    let verbs = clap_verbs();
    let mut samples = 0;
    for (file, doc) in [CLI_MD, README] {
        for line in doc.lines() {
            let line = line.trim().trim_start_matches("$ ");
            let Some(args) = line
                .rsplit("| ")
                .next()
                .and_then(|command| command.strip_prefix("onemessagebus "))
            else {
                continue;
            };
            samples += 1;
            let words: Vec<&str> = args.split_whitespace().collect();
            // The longest verb the words begin with: `schemas fetch`, not the
            // `schemas` it is a verb of.
            let Some((path, command)) = verbs
                .iter()
                .filter(|(path, _)| {
                    words.starts_with(&path.iter().map(String::as_str).collect::<Vec<_>>())
                })
                .max_by_key(|(path, _)| path.len())
            else {
                panic!("{file}: `onemessagebus {args}` names no verb the binary has");
            };
            let real = long_flags(command);
            for flag in words.iter().filter(|w| w.starts_with("--")) {
                let flag = flag.split('=').next().unwrap_or_default();
                assert!(
                    real.contains(flag),
                    "{file}: `onemessagebus {args}` passes {flag}, which `onemessagebus {}` \
                     does not take",
                    path.join(" ")
                );
            }
        }
    }
    assert!(samples > 0, "no sample invocation was found to hold");
}

#[test]
fn every_flag_the_readme_names_is_one_that_verb_takes() {
    // The README's sections introduce the command line rather than restating
    // it — `docs/cli.md` is the reference — but the flags they name are claims
    // about the surface, and a renamed, retired or MOVED one would go on being
    // advertised on the crates.io and PyPI front page with nothing to notice.
    // Every flag is held to the flags of ONE command, indexed by its whole argv
    // path, so that a flag moving between two verbs of a family — `events emit`
    // to `events merge` — fails here rather than being found under `events`.
    let (file, doc) = README;
    let by_path: BTreeMap<Vec<String>, BTreeSet<String>> = clap_verbs()
        .into_iter()
        .map(|(path, command)| (path, long_flags(&command)))
        .collect();
    assert!(
        by_path.values().any(|flags| !flags.is_empty()),
        "the clap tree declares no long flags at all; this gate reads nothing"
    );
    // The longest declared path `words` begins with: `events merge`, not the
    // `events` it is a verb of.
    let named = |words: &[&str]| -> Option<Vec<String>> {
        by_path
            .keys()
            .filter(|path| words.starts_with(&path.iter().map(String::as_str).collect::<Vec<_>>()))
            .max_by_key(|path| path.len())
            .cloned()
    };

    // The command the prose is explaining, carried across spans: a bare
    // `--filter` under `## Streams` is a claim about the `events merge` the
    // sentence just named, not about the binary at large.
    let mut about: Option<Vec<String>> = None;
    let mut checked = 0;
    for span in ticked(&flat(&prose(doc))) {
        let words: Vec<&str> = span.split_whitespace().collect();
        // `onemessagebus ask` and `ask` are the same claim about the same verb.
        let words = words.strip_prefix(&["onemessagebus"]).unwrap_or(&words);
        let verb = match named(words) {
            Some(path) => {
                about = Some(path.clone());
                Some(path)
            }
            // A bare flag belongs to the command the prose last named.
            None if words.first().is_some_and(|word| word.starts_with('-')) => about.clone(),
            None => continue,
        };
        let flags = flags_in(&span);
        if flags.is_empty() {
            continue;
        }
        let Some(verb) = verb else {
            panic!(
                "{file}: `{span}` names a flag before any verb, so there is no \
                 command to hold it to; name the verb it belongs to"
            );
        };
        let allowed = &by_path[&verb];
        let verb = verb.join(" ");
        for flag in flags {
            checked += 1;
            assert!(
                allowed.contains(&flag),
                "{file}: `{span}` names {flag}, which `{verb}` does not take; \
                 it takes {allowed:?}"
            );
        }
    }
    assert!(
        checked > 0,
        "{file} names no flag beside a verb; this gate reads nothing"
    );
}
