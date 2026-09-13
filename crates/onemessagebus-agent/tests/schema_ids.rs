//! The schema-id grammar was widened to admit a dot-joined name
//! (`agent.onejudge-frame.judge@6`), and nothing that parsed before may parse
//! differently now: every id the profile registers, and every id-shaped string
//! in the recorded and golden fixtures, is held here to the parse the narrower
//! grammar gave it — namespace, name and version alike.

use std::path::{Path, PathBuf};

use onemessagebus::SchemaId;
use serde_json::Value;

/// The grammar before the widening: the first `.` ends the namespace, and both
/// halves are one run of ASCII letters, digits, `-` and `_`.
fn previous_parse(text: &str) -> Option<(String, String, u32)> {
    let part = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    };
    let (family, version) = text.rsplit_once('@')?;
    let (namespace, name) = family.split_once('.')?;
    let version: u32 = version.parse().ok().filter(|version| *version > 0)?;
    (part(namespace) && part(name)).then(|| (namespace.to_owned(), name.to_owned(), version))
}

fn strings_in(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings_in(item, out)),
        Value::Object(fields) => fields.iter().for_each(|(key, item)| {
            out.push(key.clone());
            strings_in(item, out);
        }),
        _ => {}
    }
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a fixture directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Every string in every JSON document and JSON line under the fixtures.
fn fixture_strings() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files = Vec::new();
    files_under(&root.join("recorded"), &mut files);
    files_under(&root.join("golden"), &mut files);
    let mut strings = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(document) => strings_in(&document, &mut strings),
            Err(_) => text
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .for_each(|line| strings_in(&line, &mut strings)),
        }
    }
    strings
}

#[test]
fn every_id_that_parsed_before_the_widening_parses_to_the_same_parts() {
    let registered: Vec<String> = onemessagebus_agent::registry()
        .ids()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(!registered.is_empty(), "the profile registers nothing");
    let mut held = 0;
    for text in registered.iter().cloned().chain(fixture_strings()) {
        let Some((namespace, name, version)) = previous_parse(&text) else {
            continue;
        };
        let now: SchemaId = text
            .parse()
            .unwrap_or_else(|failure| panic!("{text} parsed before and is refused now: {failure}"));
        assert_eq!(
            (now.namespace(), now.name(), now.version()),
            (namespace.as_str(), name.as_str(), version),
            "{text} parses to different parts than it did"
        );
        held += 1;
    }
    let previously_parsed = registered
        .iter()
        .filter(|text| previous_parse(text).is_some())
        .count();
    assert!(
        held >= previously_parsed && previously_parsed > 0,
        "the registered ids that parsed before were not all held: {held} of {previously_parsed}"
    );
    for text in &registered {
        if previous_parse(text).is_none() {
            let id: SchemaId = text.parse().expect("a registered id parses");
            assert!(
                id.name().contains('.'),
                "{text} is registered, did not parse before, and is not a dot-joined name"
            );
        }
    }
}
