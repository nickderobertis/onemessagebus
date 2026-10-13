//! Real documents captured on dc345f3 before replacing the YAML reader. The
//! captured texts come from the spike's suite corpus (356af85), the five docs
//! named below, and ai-orchestrator's production config at 219568e3c890.
//! Expected values are the base's serialized typed results, not new-parser output.

use onemessagebus::{Config, Filter, Open, Predicate};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Case {
    kind: String,
    label: String,
    text: String,
    expected: Value,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("fixtures/yaml-base.json")).expect("the captured corpus")
}

#[test]
fn real_configurations_load_with_the_base_meaning() {
    let directory = tempfile::tempdir().expect("scratch directory");
    let path = directory.path().join("onemessagebus.yaml");
    for case in cases().into_iter().filter(|case| case.kind == "config") {
        std::fs::write(&path, &case.text).expect("write real configuration");
        let actual = Config::load(&path).unwrap_or_else(|error| panic!("{}: {error}", case.label));
        let mut expected: Config = serde_json::from_value(case.expected).expect("base Config");
        expected.schemas = expected
            .schemas
            .into_iter()
            .map(|link| link.rebased(directory.path()))
            .collect();
        assert_eq!(actual, expected, "{}", case.label);
    }
}

#[test]
fn documentation_and_production_fixture_are_in_the_corpus() {
    let cases = cases();
    for (name, document) in [
        ("queues", include_str!("../../../docs/queues.md")),
        ("contract", include_str!("../../../docs/contract.md")),
        (
            "schema-links",
            include_str!("../../../docs/schema-links.md"),
        ),
        ("codecs", include_str!("../../../docs/codecs.md")),
        ("validators", include_str!("../../../docs/validators.md")),
    ] {
        for block in document.split("```yaml\n").skip(1) {
            let block = block.split("```").next().expect("closing fence");
            let text = if block.contains("version:") {
                block.to_owned()
            } else {
                format!("version: 1\ntransport: {{kind: local, dir: runs/r1/channel}}\n{block}")
            };
            assert!(
                cases
                    .iter()
                    .any(|case| case.kind == "config" && case.text == text),
                "docs/{name}.md block missing from base capture"
            );
        }
    }
    assert!(cases
        .iter()
        .any(|case| case.kind == "config"
            && case.text == include_str!("fixtures/ai-orchestrator.yaml")));
}

#[test]
fn yaml_and_sdk_json_filters_and_predicates_keep_the_base_meaning() {
    let directory = tempfile::tempdir().expect("scratch directory");
    let path = directory.path().join("spec.yaml");
    for case in cases().into_iter().filter(|case| case.kind != "config") {
        std::fs::write(&path, &case.text).expect("write real spec");
        if case.kind == "filter" {
            let expected: Filter<Open> =
                serde_json::from_value(case.expected).expect("base Filter");
            assert_eq!(
                Filter::<Open>::parse(&case.text).expect("parse filter"),
                expected,
                "{}",
                case.label
            );
            let from_file =
                Filter::<Open>::read(path.to_str().expect("path")).expect("read filter file");
            assert_eq!(from_file, expected, "{}", case.label);
            if case.text.trim_start().starts_with('{') {
                let inline = Filter::<Open>::read(&case.text).expect("inline SDK filter");
                assert_eq!(inline, expected, "{}", case.label);
            }
        } else {
            let expected: Predicate =
                serde_json::from_value(case.expected).expect("base Predicate");
            assert_eq!(
                Predicate::read(path.to_str().expect("path")).expect("predicate file"),
                expected,
                "{}",
                case.label
            );
            if case.text.trim_start().starts_with('{') {
                assert_eq!(
                    Predicate::read(&case.text).expect("inline SDK predicate"),
                    expected,
                    "{}",
                    case.label
                );
            }
        }
    }
}

#[test]
fn python_surrogate_pair_is_read_as_json() {
    let text = r#"{"field": "text", "equals": "\ud83d\ude00"}"#;
    let expected: Predicate = serde_json::from_str(text).expect("JSON surrogate pair");
    assert_eq!(Predicate::read(text).expect("Python predicate"), expected);
    let text = r#"{"include": [{"kind": "\ud83d\ude00"}]}"#;
    let expected: Filter<Open> = serde_json::from_str(text).expect("JSON surrogate pair");
    assert_eq!(
        Filter::<Open>::parse(text).expect("Python filter"),
        expected
    );
}

#[test]
fn malformed_documents_name_the_file_and_fault_with_a_location() {
    let directory = tempfile::tempdir().expect("scratch directory");
    let path = directory.path().join("broken.yaml");
    for (kind, text, fault) in [
        (
            "config",
            "version: 1\ntransport: {kind: memory}\nqueus: {}\n",
            "unknown field `queus`",
        ),
        (
            "config",
            "version: 1\ntransport: {kind: memory}\nauthors: {bot: {}}\n",
            "missing field `capabilities`",
        ),
        ("config", "[", "unclosed bracket"),
        ("filter", "only: []\n", "unknown field `only`"),
        (
            "predicate",
            "field: value\nequal: true\n",
            "unknown field `equal`",
        ),
    ] {
        std::fs::write(&path, text).expect("write malformed document");
        let spec = path.to_str().expect("path");
        let failure = match kind {
            "config" => Config::load(&path)
                .expect_err("malformed config")
                .to_string(),
            "filter" => Filter::<Open>::read(spec)
                .expect_err("malformed filter")
                .to_string(),
            _ => Predicate::read(spec).expect_err("malformed predicate"),
        };
        assert!(failure.contains(spec), "{failure}");
        assert!(failure.contains(fault), "{failure}");
        assert!(
            failure.contains("line ") && failure.contains("column "),
            "{failure}"
        );
        assert_eq!(failure.lines().count(), 1, "snippets disabled: {failure}");
    }
}
