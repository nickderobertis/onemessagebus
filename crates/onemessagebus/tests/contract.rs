//! The committed contract drives the core's types: what `docs/contract.md`
//! says of the wire shape, the bounds, the redaction tables, the glob dialect
//! and the verbs, checked here against the crate that has no agent word in it.
//! The profile's `tests/contract.rs` drives the same document through the
//! agent types.

use onemessagebus::sdk_schema::{self, Lang};
use onemessagebus::{
    glob, Envelope, Filter, Open, Registry, Source, CAPABILITIES, CREDENTIAL_PREFIXES,
    CREDENTIAL_WORDS, MAX_ACTIVITY_DETAIL_CHARS, MAX_PAYLOAD_TEXT_BYTES, REDACTED,
};
use serde_json::{json, Value};

const CONTRACT: &str = include_str!("../../../docs/contract.md");

/// The fenced block tagged `<!-- fixture: name -->`.
fn fixture(name: &str) -> String {
    let tag = format!("<!-- fixture: {name} -->");
    let mut lines = CONTRACT.lines();
    lines
        .by_ref()
        .find(|line| line.trim() == tag)
        .unwrap_or_else(|| panic!("docs/contract.md has no fixture tagged {name:?}"));
    assert!(lines.next().is_some_and(|line| line.starts_with("```")));
    lines
        .take_while(|line| !line.starts_with("```"))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Every fixture a contract test drives: `envelope`, `filter` and `verbs` here
/// and in the profile's `tests/contract.rs`, `read-sets` there alone. A fixture
/// added to the document is added here beside the test that drives it.
const DRIVEN_FIXTURES: &[&str] = &["envelope", "filter", "read-sets", "verbs"];

fn fixture_tag(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("<!-- fixture: ")
        .and_then(|rest| rest.strip_suffix(" -->"))
}

/// A fenced block nobody drives is a shape the document states and no test
/// holds the crates to, so every block carries a tag, every tag sits on a
/// block, and every name is one a contract test reads.
#[test]
fn every_fenced_block_in_the_contract_is_a_fixture_a_contract_test_drives() {
    let lines: Vec<&str> = CONTRACT.lines().collect();
    let mut fenced = Vec::new();
    let mut inside = false;
    for (at, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with("```") {
            continue;
        }
        inside = !inside;
        if !inside {
            continue;
        }
        let name = at
            .checked_sub(1)
            .and_then(|previous| fixture_tag(lines[previous]))
            .unwrap_or_else(|| {
                panic!(
                    "the fenced block at docs/contract.md:{} has no `<!-- fixture: name -->` tag \
                     on the line before it; tag it and drive it from a contract test",
                    at + 1
                )
            });
        assert!(
            DRIVEN_FIXTURES.contains(&name),
            "docs/contract.md:{} tags a fixture {name:?} no contract test drives; drive it and \
             add it to DRIVEN_FIXTURES",
            at + 1
        );
        fenced.push(name);
    }
    assert!(!inside, "a fence in docs/contract.md never closes");
    let tagged: Vec<&str> = lines.iter().filter_map(|line| fixture_tag(line)).collect();
    assert_eq!(
        tagged, fenced,
        "a fixture tag is not on the line before a fence"
    );
    let mut names = fenced.clone();
    names.sort_unstable();
    assert_eq!(
        names, DRIVEN_FIXTURES,
        "each driven fixture is tagged exactly once"
    );
    for name in DRIVEN_FIXTURES {
        assert!(
            !fixture(name).trim().is_empty(),
            "the {name} fixture is empty"
        );
    }
}

/// Every `` `backticked` `` token in the contract.
fn backticked() -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = CONTRACT;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else { break };
        out.push(rest[..close].to_owned());
        rest = &rest[close + 1..];
    }
    out
}

/// The documented envelope, with its placeholders made concrete and its
/// profile-declared `phase` removed — the core has no phase, and over the open
/// vocabulary a top-level key nobody declared is refused rather than carried.
fn open_envelope_example() -> Value {
    let mut example: Value = serde_json::from_str(&fixture("envelope")).expect("JSON");
    example["ts"] = json!("2026-08-07T12:34:56.789Z");
    example["stream"] = json!("billing-4f2a");
    example["source"] = json!("billing");
    example["kind"] = json!("invoice-issued");
    example.as_object_mut().expect("an object").remove("phase");
    example
}

#[test]
fn the_documented_envelope_reads_over_the_open_vocabulary_with_labels_as_an_open_map() {
    let example = open_envelope_example();
    let envelope: Envelope<Open> = serde_json::from_value(example.clone()).expect("parses");
    assert_eq!(envelope.v, 1);
    assert_eq!(envelope.seq, 42);
    assert_eq!(envelope.source, Source::from("billing"));
    assert_eq!(envelope.kind.as_str(), "invoice-issued");
    assert_eq!(envelope.labels.get_str("run_id"), Some("R"));
    assert_eq!(envelope.labels.0["round"], json!(2));
    assert_eq!(envelope.labels.get_str("extra"), Some("carried"));
    assert_eq!(
        serde_json::to_value(&envelope).expect("serializes"),
        example
    );

    let mut with_phase: Value = serde_json::from_str(&fixture("envelope")).expect("JSON");
    with_phase["source"] = json!("billing");
    let refusal = serde_json::from_value::<Envelope<Open>>(with_phase)
        .expect_err("the core has no phase: an undeclared top-level key is refused");
    assert!(refusal.to_string().contains("phase"), "{refusal}");
}

#[test]
fn the_documented_filter_reads_over_the_open_vocabulary_as_label_asks() {
    let filter: Filter<Open> =
        serde_json::from_str(&fixture("filter")).expect("the documented filter parses");
    filter.validate().expect("valid");
    assert_eq!(filter.include.len(), 2);
    assert_eq!(filter.include[0].kind.as_deref(), Some("member-*"));
    assert_eq!(filter.include[1].fields.0["member"], json!("worker"));
    assert_eq!(filter.exclude[1].source, Some(Source::from("vcs")));
    assert_eq!(filter.exclude[1].fields.0["phase"], json!("release"));
    assert_eq!(
        serde_json::to_value(&filter).expect("serializes"),
        serde_json::from_str::<Value>(&fixture("filter")).expect("JSON")
    );
}

#[test]
fn the_documented_bounds_are_the_constants() {
    let tokens = backticked();
    assert!(tokens.contains(&"MAX_PAYLOAD_TEXT_BYTES".to_owned()));
    assert!(tokens.contains(&"MAX_ACTIVITY_DETAIL_CHARS".to_owned()));
    assert!(CONTRACT.contains(&format!("**{MAX_PAYLOAD_TEXT_BYTES} bytes**")));
    assert!(CONTRACT.contains(&format!("**{MAX_ACTIVITY_DETAIL_CHARS}\n  characters**")));
}

#[test]
fn the_documented_redaction_tables_are_the_crates() {
    let tokens = backticked();
    for word in CREDENTIAL_WORDS.iter().chain(CREDENTIAL_PREFIXES) {
        assert!(
            tokens.contains(&(*word).to_owned()),
            "{word} is not in the contract"
        );
    }
    assert!(tokens.contains(&REDACTED.to_owned()));
    // And nothing the contract lists as a table entry is missing from the crate.
    let listed =
        "TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `APIKEY`, `API_KEY`, `PRIVATE_KEY";
    let words: Vec<&str> = listed.split("`, `").collect();
    assert_eq!(words, CREDENTIAL_WORDS);
}

#[test]
fn the_glob_dialect_is_star_alone() {
    assert!(glob("member-*", "member-started"));
    assert!(glob("member-*", "member-"));
    assert!(!glob("member-*", "turn-started"));
    assert!(glob("*", "anything"));
    assert!(glob("*-started", "member-started"));
    assert!(glob("*a*b*", "xaxbx"));
    assert!(!glob("*a*b*", "xbxax"));
    assert!(
        !glob("member-?", "member-x"),
        "`?` is itself, not a wildcard"
    );
    assert!(!glob("[a-z]", "a"), "a class is itself, not a class");
    assert!(glob("", ""));
    assert!(!glob("", "x"));
}

#[test]
fn the_documented_verbs_are_exactly_the_capabilities() {
    let documented: Vec<String> = serde_json::from_str(&fixture("verbs")).expect("JSON");
    let declared: Vec<String> = CAPABILITIES.iter().map(|c| c.verb.join(" ")).collect();
    assert_eq!(documented, declared);
}

#[test]
fn the_bundle_emits_the_manifest_and_every_documented_root() {
    let bundle = sdk_schema::bundle::<Open>(&Registry::new());
    let document: Value = serde_json::from_str(&bundle.to_json()).expect("the bundle is JSON");
    for root in [
        "envelope",
        "filter",
        "schema_id",
        "registry_document",
        "capabilities",
    ] {
        assert!(document.get(root).is_some(), "the bundle has no {root}");
    }
    assert_eq!(
        document["capabilities"].as_array().map(Vec::len),
        Some(CAPABILITIES.len())
    );
    assert_eq!(document["vocabulary"]["name"], json!("open"));
    for capability in CAPABILITIES {
        if let Some(root) = capability.options {
            assert!(
                document["options"].get(root).is_some(),
                "{root} has no schema"
            );
        }
        assert_eq!(
            capability.python_method(),
            capability
                .method
                .chars()
                .fold(String::new(), |mut out, ch| {
                    if ch.is_ascii_uppercase() {
                        out.push('_');
                        out.push(ch.to_ascii_lowercase());
                    } else {
                        out.push(ch);
                    }
                    out
                })
        );
    }
    let entries: Vec<&Value> = document["capabilities"]
        .as_array()
        .expect("array")
        .iter()
        .collect();
    for entry in entries {
        for binding in entry["bindings"].as_array().expect("bindings") {
            assert!(binding.get("option").is_some() && binding.get("flag").is_some());
        }
    }
}

#[test]
fn an_unsupported_language_is_refused_by_name() {
    let id = "test.thing@1".parse().expect("id");
    for lang in [Lang::Python, Lang::Typescript] {
        let refusal = sdk_schema::generate(lang, &id, &json!({})).expect_err("refused");
        assert!(refusal.to_string().contains(lang.as_str()), "{refusal}");
        assert!(refusal.to_string().contains("test.thing@1"), "{refusal}");
    }
    let json = sdk_schema::generate(Lang::Json, &id, &json!({ "type": "object" })).expect("json");
    assert_eq!(json, "{\n  \"type\": \"object\"\n}\n");
    let untitled = sdk_schema::generate(Lang::Rust, &id, &json!({ "type": "object" }))
        .expect_err("a document with no title cannot name a type");
    assert!(untitled.to_string().contains("title"), "{untitled}");
}
