//! Contract R over the registry the profile constructs: the read set, the
//! forward-carry rule, and the two golden envelopes copied from `onepipeline`.

use onemessagebus::sdk_schema;
use onemessagebus::{CheckError, Read, SchemaId, Vocabulary as _, CAPABILITIES};
use onemessagebus_agent::{registry, Agent, EVENT_ENVELOPE_FAMILY};
use serde_json::{json, Value};

const ENVELOPE_V1: &str = include_str!("golden/envelope-v1.json");
const ENVELOPE_V2: &str = include_str!("golden/envelope-v2.json");

fn id(text: &str) -> SchemaId {
    text.parse().expect("an id")
}

#[test]
fn the_profile_reads_two_versions_of_the_event_envelope_and_writes_the_newest() {
    let registry = registry();
    assert_eq!(registry.read_set(EVENT_ENVELOPE_FAMILY), vec![2, 1]);
    assert_eq!(registry.writes(EVENT_ENVELOPE_FAMILY), Some(2));
    assert_eq!(registry.read_set("agent.reply-envelope"), Vec::<u32>::new());
    assert_eq!(registry.read_set("agent.nothing"), Vec::<u32>::new());
    assert_eq!(registry.writes("agent.nothing"), None);
}

/// Forward-carry: every supported older version reads as the version this
/// build writes, so a registry that hands a declared older version back
/// unchanged fails here.
#[test]
fn a_declared_version_in_the_read_set_reads_at_the_version_this_build_writes() {
    let registry = registry();
    assert_eq!(registry.read_at(EVENT_ENVELOPE_FAMILY, 1), Read::At(2));
    assert_eq!(registry.read_at(EVENT_ENVELOPE_FAMILY, 2), Read::At(2));
}

/// A version outside the set is unknown, naming the declared version and the
/// set — never carried, never rewritten.
#[test]
fn a_declared_version_outside_the_read_set_is_unknown_naming_the_version_and_the_set() {
    let registry = registry();
    for (family, declared, set) in [
        (EVENT_ENVELOPE_FAMILY, 0, "[2, 1]"),
        (EVENT_ENVELOPE_FAMILY, 3, "[2, 1]"),
    ] {
        match registry.read_at(family, declared) {
            Read::Unknown(unknown) => {
                assert_eq!(unknown.declared, declared);
                assert_eq!(unknown.family, family);
                let said = unknown.to_string();
                assert!(said.contains(&format!("{family}@{declared}")), "{said}");
                assert!(said.contains(set), "{said}");
            }
            Read::At(version) => panic!("{family}@{declared} was read at {version}"),
        }
    }
}

#[test]
fn the_golden_event_envelopes_validate_against_their_versions_and_read_at_two() {
    let registry = registry();
    for (document, version) in [(ENVELOPE_V1, 1), (ENVELOPE_V2, 2)] {
        let envelope: Value = serde_json::from_str(document).expect("the golden is JSON");
        assert_eq!(envelope["v"], json!(version));
        let schema = id(&format!("{EVENT_ENVELOPE_FAMILY}@{version}"));
        registry
            .check(&schema, &envelope)
            .unwrap_or_else(|e| panic!("envelope-v{version}.json does not validate: {e}"));
        assert_eq!(
            registry.read_at(EVENT_ENVELOPE_FAMILY, version),
            Read::At(2)
        );
        // It is also the profile's own type, whole.
        let typed: onemessagebus_agent::Envelope =
            serde_json::from_value(envelope.clone()).expect("the golden is an agent envelope");
        assert_eq!(serde_json::to_value(&typed).expect("serializes"), envelope);
    }
    // And each against the other version's schema is a violation at `/v`.
    let v1: Value = serde_json::from_str(ENVELOPE_V1).expect("JSON");
    match registry.check(&id("agent.event-envelope@2"), &v1) {
        Err(CheckError::Violation(violation)) => assert_eq!(violation.pointer, "/v"),
        other => panic!("a v1 envelope validated against @2: {other:?}"),
    }
}

/// The SDK bundle built from the profile's registry carries every message the
/// profile registers, each as the document it is registered with, beside the
/// shared roots and the capability manifest.
#[test]
fn the_sdk_bundle_over_the_profile_carries_every_registered_message() {
    let registry = registry();
    let bundle = sdk_schema::bundle::<Agent>(&registry);
    let document: Value = serde_json::from_str(&bundle.to_json()).expect("the bundle is JSON");
    let ids: Vec<String> = registry.ids().iter().map(ToString::to_string).collect();
    assert!(!ids.is_empty(), "the profile registers messages");
    let messages = document["messages"].as_object().expect("a messages object");
    assert_eq!(
        messages.keys().cloned().collect::<Vec<_>>(),
        ids,
        "the bundle's messages are exactly the registered ids"
    );
    for id in registry.ids() {
        assert_eq!(
            Some(&messages[&id.to_string()]),
            registry.schema(&id),
            "{id} is not carried as its registered document"
        );
    }
    for root in [
        "envelope",
        "filter",
        "matcher",
        "schema_id",
        "registry_document",
        "schema_list",
    ] {
        assert!(document[root].is_object(), "the bundle has no {root} root");
    }
    assert_eq!(document["vocabulary"]["name"], json!(Agent::NAME));
    let verbs: Vec<String> = document["capabilities"]
        .as_array()
        .expect("a capability manifest")
        .iter()
        .map(|entry| {
            entry["verb"]
                .as_array()
                .expect("a verb")
                .iter()
                .map(|word| word.as_str().expect("a word"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let declared: Vec<String> = CAPABILITIES
        .iter()
        .map(|capability| capability.verb.join(" "))
        .collect();
    assert_eq!(verbs, declared);
}

/// The agent namespace holds the profile's messages, and the core's
/// `onemessagebus` namespace the transport plugin protocol's shapes the
/// profile's registry carries for SDK clients.
#[test]
fn every_registered_id_is_listed_in_order() {
    let registry = registry();
    let ids: Vec<String> = registry.ids().iter().map(ToString::to_string).collect();
    assert_eq!(
        ids,
        [
            "agent.artifact-ref@1",
            "agent.event-envelope@1",
            "agent.event-envelope@2",
            "agent.event-filter@1",
            "agent.labels@1",
            "agent.note@1",
            "onemessagebus.transport-hello@1",
            "onemessagebus.transport-reply@1",
            "onemessagebus.transport-request@1",
        ]
    );
}
