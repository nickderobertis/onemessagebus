//! The core is usable with no agent concept in it: a vocabulary of this test's
//! own — reserved labels `tenant` and `order` over sources `billing` and
//! `shipping` — driven through the public API with the profile crate not
//! linked, over the same conformance table the profile runs over the agent
//! vocabulary.

use onemessagebus::conformance::{drive, Fixture, Sample};
use onemessagebus::{
    Emitter, Filter, Matcher, Message, NoDimensions, Reader, Reading, Redactor, Registry, Reserved,
    SchemaId, Vocabulary,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// A commerce vocabulary: nothing in it names an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Commerce;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Department {
    Billing,
    Shipping,
}

impl std::fmt::Display for Department {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Department::Billing => "billing",
            Department::Shipping => "shipping",
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
struct CommerceLabels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommerceMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<String>,
}

impl Vocabulary for Commerce {
    type Source = Department;
    type Dimensions = NoDimensions;
    type Labels = CommerceLabels;
    type Fields = CommerceMatch;

    const NAME: &'static str = "commerce";
    const RESERVED: &'static [Reserved] = &[Reserved::text("tenant"), Reserved::text("order")];
    const DIMENSIONS: &'static [Reserved] = &[];
    const DEFAULT_SOURCE: &'static str = "billing";

    fn write_version(_: &Self::Source) -> u32 {
        1
    }
}

/// A message of the commerce namespace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Invoice {
    tenant: String,
    total_cents: u64,
}

impl Message for Invoice {
    const SCHEMA: SchemaId = SchemaId::literal("commerce", "invoice", 1);
}

fn labels(tenant: &str, order: &str) -> CommerceLabels {
    CommerceLabels {
        tenant: Some(tenant.to_owned()),
        order: Some(order.to_owned()),
        extra: Map::new(),
    }
}

#[test]
fn a_vocabulary_with_no_agent_word_holds_the_conformance_table() {
    let fixture = Fixture::<Commerce> {
        namespace: "commerce",
        sources: [Department::Billing, Department::Shipping],
        labels: labels("acme", "o-1"),
        other_labels: labels("globex", "o-2"),
        matching: CommerceMatch {
            tenant: Some("acme".to_owned()),
            order: None,
        },
        dimensions: NoDimensions {},
        unknown_key: "phase",
    };
    let sample = Sample::<Invoice> {
        conforming: Invoice {
            tenant: "acme".to_owned(),
            total_cents: 1999,
        },
        violating: (
            json!({ "tenant": "acme", "total_cents": -1 }),
            "/total_cents",
        ),
    };
    drive(&fixture, &sample);
}

/// The same journey spelled out over the public API, for the reader who wants
/// to see what the table does: envelopes, filters, emitting, reading, merging
/// and the registry, with the crate's words and nobody else's.
#[test]
fn the_commerce_bus_end_to_end() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let billing = dir.path().join("billing.ndjson");
    let shipping = dir.path().join("shipping.ndjson");

    let billing_emitter = Emitter::<Commerce>::shared("billing-1", Department::Billing, &billing)
        .with_redactor(Redactor::new())
        .with_labels(labels("acme", "o-1"));
    let shipping_emitter =
        Emitter::<Commerce>::shared("shipping-1", Department::Shipping, &shipping)
            .with_redactor(Redactor::new())
            .with_labels(labels("acme", "o-1"));
    let invoiced = billing_emitter.emit(
        "invoice-issued",
        Map::from_iter([("total_cents".to_owned(), json!(1999))]),
    );
    assert_eq!(invoiced.seq, 1);
    assert_eq!(invoiced.source, Department::Billing);
    assert_eq!(invoiced.labels.tenant.as_deref(), Some("acme"));
    shipping_emitter.emit("parcel-shipped", Map::new());
    billing_emitter.emit("invoice-paid", Map::new());

    let read: Vec<Reading<Commerce>> = Reader::open(&billing).expect("opens").collect();
    assert_eq!(read.len(), 2);

    let only_shipping = Filter::<Commerce> {
        include: vec![Matcher::new().source(Department::Shipping)],
        exclude: Vec::new(),
    };
    let merged = onemessagebus::Merge::<Commerce>::open([&billing, &shipping]).expect("merges");
    let kinds: Vec<&str> = merged
        .records()
        .iter()
        .filter(|envelope| only_shipping.matches(envelope))
        .map(|envelope| envelope.kind.as_str())
        .collect();
    assert_eq!(kinds, ["parcel-shipped"]);

    let by_tenant = Filter::<Commerce>::parse(r#"{"include": [{"tenant": "acme"}]}"#)
        .expect("a matcher on a reserved key of this vocabulary");
    assert_eq!(
        merged
            .records()
            .iter()
            .filter(|envelope| by_tenant.matches(envelope))
            .count(),
        3
    );
    let refused = Filter::<Commerce>::parse(r#"{"include": [{"member": "worker"}]}"#)
        .expect_err("an agent key is not one this vocabulary admits");
    assert!(refused.to_string().contains("member"), "{refused}");

    let mut registry = Registry::new();
    registry.register::<Invoice>().expect("registers");
    assert_eq!(registry.ids()[0].namespace(), "commerce");
    registry
        .check(&Invoice::SCHEMA, &invoiced.payload.clone().into())
        .expect_err("a payload missing the tenant is not an invoice");
}
