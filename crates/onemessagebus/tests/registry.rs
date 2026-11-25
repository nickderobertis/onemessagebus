//! Contract R in the core: `SchemaId`'s grammar, the registry's refusals, and
//! forward-carry over a family of this test's own.

use std::num::NonZeroU32;

use onemessagebus::{CheckError, Message, Read, Registry, RegistryError, SchemaId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const ONE: NonZeroU32 = NonZeroU32::MIN;

#[test]
fn a_well_formed_id_parses_to_its_family_and_version() {
    let order: SchemaId = "shop.order@1".parse().expect("parses");
    assert_eq!(order.namespace(), "shop");
    assert_eq!(order.name(), "order");
    assert_eq!(order.version(), 1);
    assert_eq!(order.family(), "shop.order");
    assert_eq!(order.to_string(), "shop.order@1");
    let invoice: SchemaId = "shop.invoice@2".parse().expect("parses");
    assert_eq!(invoice.family(), "shop.invoice");
    assert_eq!(invoice.version(), 2);
    assert_eq!(invoice.at(ONE).to_string(), "shop.invoice@1");
    assert_eq!(SchemaId::literal("shop", "order", 1), order);
    assert_eq!(SchemaId::new("shop", "order", 1).expect("builds"), order);
    let json = serde_json::to_string(&order).expect("serializes");
    assert_eq!(json, "\"shop.order@1\"");
    assert_eq!(
        serde_json::from_str::<SchemaId>(&json).expect("reads"),
        order
    );
}

#[test]
fn a_name_may_be_dot_joined_and_the_first_dot_still_ends_the_namespace() {
    let frame: SchemaId = "shop.fulfilment-frame.pick@6".parse().expect("parses");
    assert_eq!(frame.namespace(), "shop");
    assert_eq!(frame.name(), "fulfilment-frame.pick");
    assert_eq!(frame.version(), 6);
    assert_eq!(frame.family(), "shop.fulfilment-frame.pick");
    assert_eq!(frame.to_string(), "shop.fulfilment-frame.pick@6");
    assert_eq!(SchemaId::literal("shop", "fulfilment-frame.pick", 6), frame);
    assert_eq!(
        SchemaId::new("shop", "fulfilment-frame.pick", 6).expect("builds"),
        frame
    );
    let deep: SchemaId = "a.b.c.d@1".parse().expect("parses");
    assert_eq!((deep.namespace(), deep.name()), ("a", "b.c.d"));
}

#[test]
fn a_malformed_id_is_refused_naming_the_fault() {
    let refusals = [
        ("shop.order", "names no version"),
        ("shop.order@0", "not a positive integer"),
        ("shop.order@x", "not a positive integer"),
        (".order@1", "namespace is empty"),
        ("shop.@1", "name is empty"),
        ("@1", "names no namespace"),
        ("shop.or der@1", "not letters, digits"),
        ("shop.or..der@1", "dot-joined parts is empty"),
        ("shop..order@1", "dot-joined parts is empty"),
        ("shop.order.@1", "dot-joined parts is empty"),
        ("shop.fulfilment-frame.pi ck@6", "not letters, digits"),
    ];
    for (text, names) in refusals {
        let refusal = text.parse::<SchemaId>().expect_err(text);
        assert!(refusal.to_string().contains(names), "{text}: {refusal}");
        assert!(refusal.to_string().contains(text), "{text}: {refusal}");
        assert!(
            SchemaId::new("shop", "order", 0).is_err(),
            "a zero version is refused however it is built"
        );
    }
}

// `black_box` keeps each call out of const evaluation, so these prove the
// runtime refusal a `const` turns into a compile error.

#[test]
#[should_panic(expected = "SchemaId::literal: the namespace is not")]
fn a_literal_with_an_empty_namespace_panics() {
    let _ = SchemaId::literal(std::hint::black_box(""), "order", 1);
}

#[test]
#[should_panic(expected = "SchemaId::literal: the name is not")]
fn a_literal_whose_name_has_an_empty_dot_joined_part_panics() {
    let _ = SchemaId::literal("shop", std::hint::black_box("or..der"), 1);
}

#[test]
#[should_panic(expected = "SchemaId::literal: the namespace is not")]
fn a_literal_whose_namespace_carries_a_dot_panics() {
    let _ = SchemaId::literal(std::hint::black_box("sh.op"), "order", 1);
}

#[test]
#[should_panic(expected = "SchemaId::literal: the version is not a positive integer")]
fn a_literal_at_version_zero_panics() {
    let _ = SchemaId::literal("shop", "order", std::hint::black_box(0));
}

#[test]
fn an_id_moved_to_a_version_keeps_its_family_and_takes_that_version() {
    let order = SchemaId::literal("shop", "order", 1);
    for version in [2, 7, u32::MAX] {
        let moved = order.at(NonZeroU32::new(version).expect("non-zero"));
        assert_eq!(moved.family(), "shop.order");
        assert_eq!(moved.version(), version);
        assert_eq!(moved.to_string(), format!("shop.order@{version}"));
    }
    assert_eq!(order.at(ONE), order, "moving to its own version is itself");
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

/// Documents compiling a validator refuses, each beside why — the meta-schema
/// alone accepts every one marked *compile*.
fn refused_documents() -> Vec<(&'static str, serde_json::Value)> {
    let draft_07 = "http://json-schema.org/draft-07/schema#";
    vec![
        ("meta: an unknown type", json!({ "type": "nonsense" })),
        ("meta: not an object", json!(3)),
        ("meta: a negative bound", json!({ "minLength": -1 })),
        (
            "meta: an unknown dialect",
            json!({ "$schema": "https://example.com/dialect" }),
        ),
        (
            "meta: a bad definition",
            json!({ "definitions": { "a": { "type": "nonsense" } } }),
        ),
        (
            "compile: an invalid pattern",
            json!({ "type": "string", "pattern": "(" }),
        ),
        (
            "compile: an empty range",
            json!({ "properties": { "a": { "pattern": "[z-a]" } } }),
        ),
        (
            "compile: an invalid key pattern",
            json!({ "patternProperties": { "(": {} } }),
        ),
        (
            "compile: a referenced bad pattern",
            json!({ "$ref": "#/$defs/a", "$defs": { "a": { "pattern": "(" } } }),
        ),
        (
            "compile: an unresolvable pointer",
            json!({ "$ref": "#/$defs/missing" }),
        ),
        (
            "compile: a nested unresolvable pointer",
            json!({ "not": { "$ref": "#/nope" } }),
        ),
        ("compile: an unknown anchor", json!({ "$ref": "#nope" })),
        (
            "compile: an unknown dynamic anchor",
            json!({ "$dynamicRef": "#nope" }),
        ),
        (
            "compile: an http reference",
            json!({ "$ref": "http://example.com/x" }),
        ),
        (
            "compile: a relative reference",
            json!({ "$ref": "other.json" }),
        ),
        (
            "compile: a reference under an id",
            json!({ "$id": "http://x.example/y", "$ref": "other" }),
        ),
        (
            "compile: a draft-07 $defs target",
            json!({ "$schema": draft_07, "$defs": { "a": { "type": "nonsense" } }, "$ref": "#/$defs/a" }),
        ),
        (
            "compile: a meta-schema reference",
            json!({ "$ref": draft_07 }),
        ),
    ]
}

#[test]
fn registration_refuses_every_document_compilation_refuses_with_its_reason() {
    for (why, document) in refused_documents() {
        let compiled = jsonschema::validator_for(&document)
            .err()
            .map(|failure| failure.to_string())
            .unwrap_or_else(|| panic!("{why}: compilation refuses {document}"));
        if why.starts_with("compile") {
            assert!(
                jsonschema::meta::validate(&document).is_ok(),
                "{why}: only compiling refuses {document}"
            );
        }
        let mut registry = Registry::new();
        match registry.register_schema(SchemaId::literal("test", "refused", 1), document.clone()) {
            Err(RegistryError::NotASchema { id, why: said }) => {
                assert_eq!(id.to_string(), "test.refused@1", "{why}");
                assert_eq!(said, compiled, "{why}: {document}");
            }
            other => panic!("{why}: {document} gave {other:?}"),
        }
        assert!(registry.ids().is_empty(), "{why}: nothing is held");
    }
}

#[test]
fn registration_accepts_every_document_compilation_accepts_and_checks_against_it() {
    let draft_04 = "http://json-schema.org/draft-04/schema#";
    let draft_07 = "http://json-schema.org/draft-07/schema#";
    let accepted = [
        json!(true),
        json!({}),
        json!({ "$ref": "#", "properties": { "next": { "$ref": "#" } } }),
        json!({ "$ref": "#/$defs/a", "$defs": { "a": { "$ref": "#/$defs/a" } } }),
        json!({ "$ref": "#/definitions/a", "definitions": { "a": { "type": "string" } } }),
        json!({ "$schema": draft_07, "$ref": "#/definitions/a", "definitions": { "a": {} } }),
        json!({ "$schema": draft_04, "id": "http://x.example/s", "maxLength": 3 }),
        json!({ "$defs": { "unused": { "pattern": "(" } } }),
        json!({ "$defs": { "unused": { "$ref": "#/nope" } } }),
        json!({ "x-unknown": { "pattern": "(", "$ref": "#/nope" } }),
        json!({ "if": { "pattern": "(" } }),
        json!({ "format": "nonsense", "contentMediaType": "application/x-nope" }),
        json!({ "$anchor": "a", "items": { "$ref": "#a" } }),
        json!({ "$id": "urn:example:s", "$defs": { "a/b": {} }, "$ref": "#/$defs/a~1b" }),
        json!({ "dependencies": { "a": ["b"], "c": { "required": ["d"] } }, "prefixItems": [true] }),
    ];
    for document in accepted {
        assert!(
            jsonschema::validator_for(&document).is_ok(),
            "compilation accepts {document}"
        );
        let mut registry = Registry::new();
        let id = SchemaId::literal("test", "accepted", 1);
        registry
            .register_schema(id.clone(), document.clone())
            .unwrap_or_else(|failure| panic!("{document}: {failure}"));
        match registry.check(&id, &json!({})) {
            Ok(()) | Err(CheckError::Violation(_)) => {}
            Err(CheckError::Registry(failure)) => panic!("{document}: {failure}"),
        }
    }
}
