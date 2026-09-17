//! The documents that restate the agent profile, held to the types that
//! declare it.
//!
//! Three documents say which words the profile has, and nothing else checks
//! them against the code, so each is held here to exactly what it restates —
//! every expected value is read from the types, the vocabulary's data or the
//! registry, never written out a second time:
//!
//! - `docs/wire.md`: the envelope's keys in wire order, the source words, the
//!   dimension and its phase words, the reserved labels and which admit an
//!   integer, each source's write version, both bound constants and every
//!   count its bounds section gives, both redaction tables and the
//!   replacement, and every registered family with its read set.
//! - `README.md`: the source words, the dimension, the reserved labels, and the
//!   read set of each family registered at more than one version (the only
//!   families it names).
//! - `crates/onemessagebus-agent/README.md`: the `Source` and `Phase`
//!   variants, each source's write version, the reserved labels, and every
//!   registered family with its read set.

use std::collections::{BTreeMap, BTreeSet};

use onemessagebus::{
    Admits, Vocabulary as _, CREDENTIAL_PREFIXES, CREDENTIAL_WORDS, MAX_ACTIVITY_DETAIL_CHARS,
    MAX_PAYLOAD_TEXT_BYTES, REDACTED,
};
use onemessagebus_agent::{registry, Agent, Envelope, Phase, Source};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

const WIRE: (&str, &str) = ("docs/wire.md", include_str!("../../../docs/wire.md"));
const README: (&str, &str) = ("README.md", include_str!("../../../README.md"));
const CRATE_README: (&str, &str) = (
    "crates/onemessagebus-agent/README.md",
    include_str!("../README.md"),
);
const CONTRACT: &str = include_str!("../../../docs/contract.md");
const QUEUES: &str = include_str!("../../../docs/queues.md");

fn planner_config(document: &str) -> Value {
    document
        .split("```yaml")
        .skip(1)
        .filter_map(|rest| rest.split_once("```").map(|(block, _)| block))
        .find(|block| block.contains("profile: planner-channel") && block.contains("authors:"))
        .map(|block| serde_norway::from_str(block).expect("the documented configuration is YAML"))
        .expect("a planner-channel configuration block")
}

#[test]
fn queues_author_configuration_is_the_contracts_shape() {
    assert_eq!(
        planner_config(QUEUES)["authors"],
        planner_config(CONTRACT)["authors"],
        "docs/queues.md author configuration drifted from Contract A"
    );
}

/// `text` with every run of whitespace collapsed to one space, so a statement
/// wrapped across lines reads as it does rendered.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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

/// The paragraph that starts with `opening`, flattened.
fn paragraph((file, doc): (&str, &str), opening: &str) -> String {
    let start = doc
        .find(opening)
        .unwrap_or_else(|| panic!("{file} no longer says `{opening}`; the gate reads it there"));
    let rest = &doc[start..];
    flat(&rest[..rest.find("\n\n").unwrap_or(rest.len())])
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

/// Every backticked span in `text`, in order.
fn ticked(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

fn assert_same(file: &str, what: &str, stated: &[String], declared: &[String]) {
    let stated_set: BTreeSet<&String> = stated.iter().collect();
    let declared_set: BTreeSet<&String> = declared.iter().collect();
    let missing: Vec<&&String> = declared_set.difference(&stated_set).collect();
    let extra: Vec<&&String> = stated_set.difference(&declared_set).collect();
    assert!(
        missing.is_empty() && extra.is_empty() && stated.len() == declared.len(),
        "{file} states {what} as {stated:?}, but the profile declares {declared:?}: \
         missing {missing:?}, not declared {extra:?}; correct the document"
    );
}

/// Every word a unit enum's schema admits, in declaration order — whether
/// schemars spelled it as one `enum` or as a `const` per documented variant.
fn words_of<T: JsonSchema>() -> Vec<String> {
    fn collect(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    match (key.as_str(), inner) {
                        ("enum", Value::Array(words)) => {
                            out.extend(words.iter().filter_map(Value::as_str).map(str::to_owned));
                        }
                        ("const", Value::String(word)) => out.push(word.clone()),
                        _ => collect(inner, out),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, out)),
            _ => {}
        }
    }
    let mut words = Vec::new();
    collect(&schemars::schema_for!(T).to_value(), &mut words);
    words
}

/// Each word of `T` as the value it deserializes to.
fn values_of<T: JsonSchema + DeserializeOwned>() -> Vec<(String, T)> {
    words_of::<T>()
        .into_iter()
        .map(|word| {
            let value = serde_json::from_value(Value::String(word.clone()))
                .unwrap_or_else(|failure| panic!("`{word}` is in the schema: {failure}"));
            (word, value)
        })
        .collect()
}

/// The Rust variant names of `T`, as its `Debug` spells them.
fn variants_of<T: JsonSchema + DeserializeOwned + std::fmt::Debug>() -> Vec<String> {
    values_of::<T>()
        .into_iter()
        .map(|(_, value)| format!("{value:?}"))
        .collect()
}

fn source_words() -> Vec<String> {
    words_of::<Source>()
}

fn label_keys() -> Vec<String> {
    Agent::RESERVED
        .iter()
        .map(|reserved| reserved.key.to_owned())
        .collect()
}

fn dimension_keys() -> Vec<String> {
    Agent::DIMENSIONS
        .iter()
        .map(|reserved| reserved.key.to_owned())
        .collect()
}

fn declared_write_versions() -> BTreeMap<String, u32> {
    values_of::<Source>()
        .into_iter()
        .map(|(word, source)| (word, Agent::write_version(&source)))
        .collect()
}

/// Every family the registry holds, with its read set.
fn declared_read_sets() -> BTreeMap<String, Vec<u32>> {
    let registry = registry();
    registry
        .ids()
        .iter()
        .map(|id| (id.family(), registry.read_set(&id.family())))
        .collect()
}

/// The write versions a parenthetical states: clauses of backticked source
/// words and a number, split on `;` or `,`, where `the others` stands for
/// every source the other clauses do not name.
fn stated_write_versions(file: &str, clause: &str) -> BTreeMap<String, u32> {
    let mut stated = BTreeMap::new();
    let mut others = None;
    for part in clause.split([';', ',']) {
        let version: u32 = part
            .split_whitespace()
            .last()
            .and_then(|word| word.parse().ok())
            .unwrap_or_else(|| panic!("{file}: `{part}` states no write version"));
        if part.contains("the others") {
            others = Some(version);
        }
        for word in ticked(part) {
            stated.insert(word, version);
        }
    }
    if let Some(version) = others {
        for word in source_words() {
            stated.entry(word).or_insert(version);
        }
    }
    stated
}

/// The read sets a statement gives: `` `family` at `[a, b]` ``, `` `family@n` ``,
/// or families listed and then `at n`.
fn stated_read_sets(clause: &str) -> BTreeMap<String, Vec<u32>> {
    let mut stated = BTreeMap::new();
    let mut pending: Vec<String> = Vec::new();
    for (index, part) in clause.split('`').enumerate() {
        if index % 2 == 1 {
            if let Some(list) = part.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
                let versions: Vec<u32> = list
                    .split(',')
                    .filter_map(|v| v.trim().parse().ok())
                    .collect();
                for family in pending.drain(..) {
                    stated.insert(family, versions.clone());
                }
            } else if let Some((family, version)) = part.split_once('@') {
                if let Ok(version) = version.parse() {
                    stated.insert(family.to_owned(), vec![version]);
                }
            } else if part.contains('.') {
                pending.push(part.to_owned());
            }
        } else {
            let words: Vec<&str> = part.split_whitespace().collect();
            for pair in words.windows(2) {
                let number = pair[1].trim_end_matches(|c: char| !c.is_ascii_digit());
                if let (true, Ok(version)) = (pair[0] == "at", number.parse::<u32>()) {
                    for family in pending.drain(..) {
                        stated.insert(family, vec![version]);
                    }
                }
            }
        }
    }
    stated
}

#[test]
fn wire_md_lists_the_envelope_keys_in_the_order_they_are_written() {
    let (file, _) = WIRE;
    let table = section(WIRE, "One line, one envelope");
    let mut stated = Vec::new();
    for row in table.lines().filter(|line| line.starts_with("| ")) {
        let cell = row.split('|').nth(1).unwrap_or_default().trim();
        if cell == "*dimensions*" {
            stated.extend(dimension_keys());
        } else if let Some(key) = cell.strip_prefix('`').and_then(|c| c.strip_suffix('`')) {
            stated.push(key.to_owned());
        }
    }
    // Every optional part populated, so nothing is left off the line; the
    // order written is the serializer's, not this literal's.
    let every_part = json!({
        "artifacts": [{"id": "a-1", "kind": "log", "bytes": 1}],
        "payload": {"note": "n"},
        "labels": {"run_id": "R"},
        "phase": Phase::Development,
        "kind": "push",
        "source": source_words()[0],
        "seq": 1,
        "stream": "s-1",
        "ts": "2026-09-13T05:43:18.700Z",
        "v": 1
    });
    let envelope: Envelope = serde_json::from_value(every_part).expect("a whole envelope");
    let written: Vec<String> = match serde_json::to_value(&envelope).expect("it serializes") {
        Value::Object(map) => map.keys().cloned().collect(),
        other => panic!("an envelope serializes to an object, not {other}"),
    };
    assert_eq!(
        stated, written,
        "{file}'s envelope table lists the keys as {stated:?}, but an envelope is written \
         as {written:?}; correct the table's rows and their order"
    );
}

#[test]
fn every_document_states_the_source_words_the_profile_declares() {
    let declared = source_words();
    for (doc, after) in [
        (WIRE, "is the agent stack's: sources"),
        (README, "the agent profile: the sources"),
    ] {
        let stated = ticked(&between(doc, after, ";"));
        assert_same(doc.0, "the source words", &stated, &declared);
    }
    let stated: Vec<String> = between(CRATE_README, "`Source::{", "}")
        .split(',')
        .map(|variant| variant.trim().to_owned())
        .collect();
    assert_same(
        CRATE_README.0,
        "the Source variants",
        &stated,
        &variants_of::<Source>(),
    );
}

#[test]
fn every_document_states_the_dimension_and_its_phase_words() {
    let dimensions = dimension_keys();
    let dimension_and_words = between(WIRE, "the dimension ", ";");
    let (dimension, words) = dimension_and_words
        .split_once(" over ")
        .unwrap_or_else(|| panic!("{}: `the dimension ... over ...` is gone", WIRE.0));
    assert_same(WIRE.0, "the dimensions", &ticked(dimension), &dimensions);
    assert_same(
        WIRE.0,
        "the phase words",
        &ticked(words),
        &words_of::<Phase>(),
    );
    assert_same(
        WIRE.0,
        "the profile's dimensions",
        &ticked(&between(WIRE, "The agent profile has ", ".")),
        &dimensions,
    );
    assert_same(
        README.0,
        "the dimensions",
        &ticked(&between(README, "`pipeline`; the ", " dimension")),
        &dimensions,
    );
    let stated: Vec<String> = between(CRATE_README, "`Phase::{", "}")
        .split(',')
        .map(|variant| variant.trim().to_owned())
        .collect();
    assert_same(
        CRATE_README.0,
        "the Phase variants",
        &stated,
        &variants_of::<Phase>(),
    );
}

#[test]
fn every_document_states_the_reserved_labels_and_which_admit_an_integer() {
    let declared = label_keys();
    for (doc, after, until) in [
        (WIRE, "the reserved labels", "."),
        (README, "the reserved labels", ";"),
        (CRATE_README, "`Labels` — ", "plus"),
    ] {
        let stated = ticked(&between(doc, after, until));
        assert_same(doc.0, "the reserved labels", &stated, &declared);
    }
    let clause = between(WIRE, "the reserved labels", ".");
    let stated_integer: Vec<String> = declared
        .iter()
        .filter(|key| clause.contains(&format!("`{key}` (integer)")))
        .cloned()
        .collect();
    let integer: Vec<String> = Agent::RESERVED
        .iter()
        .filter(|reserved| reserved.admits == Admits::Integer)
        .map(|reserved| reserved.key.to_owned())
        .collect();
    assert_same(
        WIRE.0,
        "the labels marked (integer)",
        &stated_integer,
        &integer,
    );
}

#[test]
fn every_document_states_each_sources_write_version() {
    let declared = declared_write_versions();
    for (doc, after) in [
        (WIRE, "its source writes against ("),
        (CRATE_README, "the envelope version each writes against ("),
    ] {
        let stated = stated_write_versions(doc.0, &between(doc, after, ")"));
        assert_eq!(
            stated, declared,
            "{} states the write versions as {stated:?}, but Agent::write_version declares \
             {declared:?}; correct the document",
            doc.0
        );
    }
}

#[test]
fn wire_md_states_the_bounds_the_constants_hold() {
    let (file, _) = WIRE;
    let bounds = flat(section(WIRE, "Bounds"));
    for (name, value) in [
        ("MAX_PAYLOAD_TEXT_BYTES", MAX_PAYLOAD_TEXT_BYTES),
        ("MAX_ACTIVITY_DETAIL_CHARS", MAX_ACTIVITY_DETAIL_CHARS),
    ] {
        let statement = format!("`{name} = {value}`");
        assert!(
            bounds.contains(&statement),
            "{file}'s Bounds section does not state {statement}, the constant's value; \
             correct the document"
        );
    }
    let words: Vec<&str> = bounds.split_whitespace().collect();
    let mut counts = 0;
    for pair in words.windows(2) {
        let Ok(count) = pair[0].parse::<usize>() else {
            continue;
        };
        let unit = pair[1].trim_matches(|c: char| !c.is_ascii_alphabetic());
        let expected = match unit {
            "bytes" => MAX_PAYLOAD_TEXT_BYTES,
            "characters" => MAX_ACTIVITY_DETAIL_CHARS,
            _ => continue,
        };
        counts += 1;
        assert_eq!(
            count, expected,
            "{file}'s Bounds section says `{} {}`, but the bound is {expected} {unit}",
            pair[0], pair[1]
        );
    }
    assert!(
        counts > 0,
        "{file}'s Bounds section gives no byte or character count"
    );
}

#[test]
fn wire_md_states_both_redaction_tables_and_the_replacement() {
    let (file, _) = WIRE;
    let words = ticked(&between(WIRE, "one containing", " — "));
    let declared: Vec<String> = CREDENTIAL_WORDS.iter().map(|w| (*w).to_owned()).collect();
    assert_same(file, "the credential words", &words, &declared);
    let prefixes = ticked(&between(WIRE, "credential prefix (", ")"));
    let declared: Vec<String> = CREDENTIAL_PREFIXES
        .iter()
        .map(|p| (*p).to_owned())
        .collect();
    assert_same(file, "the credential prefixes", &prefixes, &declared);
    assert!(
        section(WIRE, "Redaction").contains(&format!("`{REDACTED}`")),
        "{file}'s Redaction section does not name `{REDACTED}`, what a value is replaced with"
    );
}

#[test]
fn every_document_states_the_read_set_of_each_family_it_names() {
    let declared = declared_read_sets();
    for (doc, statement) in [
        (WIRE, paragraph(WIRE, "The agent profile registers")),
        (
            CRATE_README,
            paragraph(CRATE_README, "- `registry()` — every schema"),
        ),
    ] {
        let stated = stated_read_sets(&statement);
        assert_eq!(
            stated, declared,
            "{} states the registered families as {stated:?}, but registry() holds \
             {declared:?}; correct the document",
            doc.0
        );
    }
    let versioned: BTreeMap<String, Vec<u32>> = declared
        .into_iter()
        .filter(|(_, read_set)| read_set.len() > 1)
        .collect();
    let stated = stated_read_sets(&between(
        README,
        "the schema families the stack registers (",
        ")",
    ));
    assert_eq!(
        stated, versioned,
        "{} states the versioned families as {stated:?}, but registry() holds {versioned:?}; \
         correct the document",
        README.0
    );
}
