//! Every library entry the capability manifest names, exercised.
//!
//! A capability says which entry point a Rust consumer calls instead of
//! spawning the binary. This holds the two together in both directions: each
//! named entry is called here, through the path the manifest spells, and an
//! entry called here that the manifest does not name fails too — so the
//! manifest cannot name a function that does not exist, or drift away from
//! the library it describes.

use std::collections::BTreeSet;

use onemessagebus::sdk_schema::{self, Lang};
use onemessagebus::{
    Emitter, Merge, Message, Open, Reader, Reading, Redactor, Registry, SchemaId, Source,
    CAPABILITIES,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Ping {
    n: u64,
}

impl Message for Ping {
    const SCHEMA: SchemaId = SchemaId::literal("test", "ping", 1);
}

/// One library entry as the manifest spells it, and the call that exercises it.
type Exercise = (&'static str, Box<dyn Fn()>);

/// Each entry the manifest may name, and the call that exercises it.
fn exercised() -> Vec<Exercise> {
    vec![
        (
            "onemessagebus::Registry::ids",
            Box::new(|| {
                let mut registry = Registry::new();
                registry.register::<Ping>().expect("registers");
                assert_eq!(registry.ids(), vec![Ping::SCHEMA]);
            }),
        ),
        (
            "onemessagebus::Registry::check",
            Box::new(|| {
                let mut registry = Registry::new();
                registry.register::<Ping>().expect("registers");
                registry
                    .check(&Ping::SCHEMA, &json!({ "n": 1 }))
                    .expect("conforms");
                registry
                    .check(&Ping::SCHEMA, &json!({ "n": "one" }))
                    .expect_err("violates");
            }),
        ),
        (
            "onemessagebus::sdk_schema::generate",
            Box::new(|| {
                let document = schemars::schema_for!(Ping).to_value();
                let rendered =
                    sdk_schema::generate(Lang::Rust, &Ping::SCHEMA, &document).expect("renders");
                assert!(rendered.contains("pub struct Ping"));
                assert!(rendered.contains("pub n: u64,"));
            }),
        ),
        (
            "onemessagebus::Registry::register_schema",
            Box::new(|| {
                let mut registry = Registry::new();
                registry
                    .register_schema(Ping::SCHEMA, json!({ "type": "object" }))
                    .expect("registers");
                registry
                    .register_schema(Ping::SCHEMA, json!({ "type": "string" }))
                    .expect_err("a different document is refused");
            }),
        ),
        (
            "onemessagebus::Merge::open",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let path = dir.path().join("s.ndjson");
                Emitter::<Open>::shared("s", Source::from("cli"), &path)
                    .with_redactor(Redactor::new())
                    .emit("tick", Map::new());
                let merged = Merge::<Open>::open([&path]).expect("merges");
                assert_eq!(merged.records().len(), 1);
            }),
        ),
        (
            "onemessagebus::Emitter::shared",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let path = dir.path().join("s.ndjson");
                let emitter = Emitter::<Open>::shared("s", Source::from("cli"), &path)
                    .with_redactor(Redactor::new());
                assert_eq!(emitter.emit("tick", Map::new()).seq, 1);
                assert_eq!(emitter.emit("tock", Map::new()).seq, 2);
                let read: Vec<Reading<Open>> = Reader::open(&path).expect("opens").collect();
                assert_eq!(read.len(), 2);
            }),
        ),
    ]
}

#[test]
fn every_named_library_entry_is_exercised_and_every_exercised_entry_is_named() {
    let named: BTreeSet<&str> = CAPABILITIES.iter().map(|c| c.library_entry).collect();
    let exercised = exercised();
    let covered: BTreeSet<&str> = exercised.iter().map(|(entry, _)| *entry).collect();
    assert_eq!(
        named, covered,
        "the manifest's library entries and the exercised entries differ"
    );
    for (entry, exercise) in exercised {
        exercise();
        assert!(
            entry.starts_with("onemessagebus::"),
            "{entry} is not a path into the core"
        );
    }
}
