//! Contract R in the core: `SchemaId`'s grammar, the registry's refusals, and
//! forward-carry over a family of this test's own.

use onemessagebus::{CheckError, Message, Read, Registry, RegistryError, SchemaId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[test]
fn a_well_formed_id_parses_to_its_family_and_version() {
    let finding: SchemaId = "agent.finding@1".parse().expect("parses");
    assert_eq!(finding.namespace(), "agent");
    assert_eq!(finding.name(), "finding");
    assert_eq!(finding.version(), 1);
    assert_eq!(finding.family(), "agent.finding");
    assert_eq!(finding.to_string(), "agent.finding@1");
    let envelope: SchemaId = "agent.event-envelope@2".parse().expect("parses");
    assert_eq!(envelope.family(), "agent.event-envelope");
    assert_eq!(envelope.version(), 2);
    assert_eq!(envelope.at(1).to_string(), "agent.event-envelope@1");
    assert_eq!(SchemaId::literal("agent", "finding", 1), finding);
    assert_eq!(
        SchemaId::new("agent", "finding", 1).expect("builds"),
        finding
    );
    let json = serde_json::to_string(&finding).expect("serializes");
    assert_eq!(json, "\"agent.finding@1\"");
    assert_eq!(
        serde_json::from_str::<SchemaId>(&json).expect("reads"),
        finding
    );
}

#[test]
fn a_malformed_id_is_refused_naming_the_fault() {
    let refusals = [
        ("agent.finding", "names no version"),
        ("agent.finding@0", "not a positive integer"),
        ("agent.finding@x", "not a positive integer"),
        (".finding@1", "namespace is empty"),
        ("agent.@1", "name is empty"),
        ("@1", "names no namespace"),
        ("agent.fin ding@1", "not letters, digits"),
    ];
    for (text, names) in refusals {
        let refusal = text.parse::<SchemaId>().expect_err(text);
        assert!(refusal.to_string().contains(names), "{text}: {refusal}");
        assert!(refusal.to_string().contains(text), "{text}: {refusal}");
        assert!(
            SchemaId::new("agent", "finding", 0).is_err(),
            "a zero version is refused however it is built"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Finding {
    severity: String,
    line: u64,
}

impl Message for Finding {
    const SCHEMA: SchemaId = SchemaId::literal("test", "finding", 1);
}

#[test]
fn registering_two_different_documents_under_one_id_is_refused_naming_the_id() {
    let mut registry = Registry::new();
    registry.register::<Finding>().expect("registers");
    registry
        .register::<Finding>()
        .expect("the same document again is fine");
    let other = json!({ "type": "object", "properties": { "different": { "type": "string" } } });
    match registry.register_schema(Finding::SCHEMA, other) {
        Err(RegistryError::Conflict { id }) => assert_eq!(id, Finding::SCHEMA),
        other => panic!("{other:?}"),
    }
    let not_a_schema = json!({ "type": "nonsense" });
    match registry.register_schema("test.broken@1".parse().expect("id"), not_a_schema) {
        Err(RegistryError::NotASchema { id, .. }) => assert_eq!(id.to_string(), "test.broken@1"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_violating_payload_is_refused_naming_the_id_and_the_pointer() {
    let mut registry = Registry::new();
    registry.register::<Finding>().expect("registers");
    registry
        .check(&Finding::SCHEMA, &json!({ "severity": "high", "line": 3 }))
        .expect("a conforming payload passes");
    match registry.check(
        &Finding::SCHEMA,
        &json!({ "severity": "high", "line": "three" }),
    ) {
        Err(CheckError::Violation(violation)) => {
            assert_eq!(violation.id, Finding::SCHEMA);
            assert_eq!(violation.pointer, "/line");
            assert!(violation
                .to_string()
                .starts_with("test.finding@1: at /line:"));
        }
        other => panic!("{other:?}"),
    }
    match registry.check(&Finding::SCHEMA, &json!({ "severity": "high" })) {
        Err(CheckError::Violation(violation)) => assert_eq!(violation.pointer, ""),
        other => panic!("{other:?}"),
    }
    match registry.check(&"test.nothing@1".parse().expect("id"), &json!({})) {
        Err(CheckError::Registry(RegistryError::Unknown { id, known })) => {
            assert_eq!(id.to_string(), "test.nothing@1");
            assert!(known.contains("test.finding@1"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_family_reads_every_registered_version_and_carries_each_forward() {
    let mut registry = Registry::new();
    let document = |version: u32| json!({ "type": "object", "properties": { "version": { "const": version } } });
    for version in [1, 3, 2] {
        registry
            .register_schema(
                SchemaId::literal("test", "thing", version),
                document(version),
            )
            .expect("registers");
    }
    assert_eq!(registry.read_set("test.thing"), vec![3, 2, 1]);
    assert_eq!(registry.writes("test.thing"), Some(3));
    for declared in [1, 2, 3] {
        assert_eq!(registry.read_at("test.thing", declared), Read::At(3));
    }
    match registry.read_at("test.thing", 4) {
        Read::Unknown(unknown) => {
            assert_eq!(unknown.declared, 4);
            assert_eq!(unknown.read_set, vec![3, 2, 1]);
            assert_eq!(
                unknown.to_string(),
                "test.thing@4 is not a version this build reads; it reads [3, 2, 1]"
            );
        }
        other => panic!("{other:?}"),
    }
    match registry.read_at("test.other", 1) {
        Read::Unknown(unknown) => assert!(unknown.to_string().contains("no version of it")),
        other => panic!("{other:?}"),
    }
    assert_eq!(registry.ids().len(), 3);
    assert_eq!(
        registry.schema(&SchemaId::literal("test", "thing", 2)),
        Some(&document(2))
    );
}
