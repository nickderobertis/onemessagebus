//! The small public surface the journeys do not reach on their own: accessors,
//! conversions, renderings, and the refusals of the renderer and the emitter.

use std::sync::{Arc, Mutex};

use onemessagebus::sdk_schema::{self, Bundle, Format, Lang};
use onemessagebus::{
    Admits, Emitter, Filter, FlagKind, Kind, LabelMatch, Labels, Matcher, Merge, Open, Reader,
    Reading, Redactor, Registry, Reserved, SchemaId, Source,
};
use serde_json::{json, Map, Value};

#[test]
fn reserved_keys_say_what_they_admit() {
    assert_eq!(Reserved::text("node").admits, Admits::Text);
    assert_eq!(Reserved::integer("round").admits, Admits::Integer);
    assert_eq!(Reserved::word("phase").admits, Admits::Word);
    assert_eq!(Reserved::text("node").key, "node");
    assert_eq!(
        serde_json::to_value(Admits::Integer).expect("serializes"),
        json!("integer")
    );
}

#[test]
fn every_flag_kind_renders_its_wire_name_and_flag() {
    let kinds = [
        (FlagKind::Positional, "positional", None),
        (FlagKind::Value("--a"), "value", Some("--a")),
        (FlagKind::Repeated("--b"), "repeated", Some("--b")),
        (FlagKind::Switch("--c"), "switch", Some("--c")),
        (FlagKind::KeyValue("--d"), "key-value", Some("--d")),
    ];
    for (kind, name, flag) in kinds {
        assert_eq!(kind.wire_name(), name);
        assert_eq!(kind.flag(), flag);
        assert_eq!(serde_json::to_value(kind).expect("serializes"), json!(name));
    }
}

#[test]
fn kinds_sources_and_labels_convert_and_render() {
    let kind = Kind::from(String::from("thing-done"));
    assert_eq!(kind.as_str(), "thing-done");
    assert_eq!(kind.to_string(), "thing-done");
    let source = Source::from(String::from("billing"));
    assert_eq!(source.as_str(), "billing");
    assert_eq!(source.to_string(), "billing");
    let mut labels = Labels::new();
    labels.insert("tenant", "acme").insert("n", 2);
    assert_eq!(labels.get_str("tenant"), Some("acme"));
    assert_eq!(labels.get_str("n"), None, "a non-text label is not text");
    assert_eq!(labels.get_str("missing"), None);
    let asks = LabelMatch::default().with("tenant", "acme");
    assert_eq!(asks.0["tenant"], json!("acme"));
}

#[test]
fn a_matcher_asking_for_null_or_an_unreadable_file_is_refused() {
    let filter = Filter::<Open> {
        include: vec![Matcher::new().fields(LabelMatch::default().with("member", Value::Null))],
        exclude: Vec::new(),
    };
    let refusal = filter.validate().expect_err("null asks for nothing");
    assert!(refusal.contains("`member` is empty"), "{refusal}");
    let missing = Filter::<Open>::read("no-such-filter.yaml").expect_err("no such file");
    assert!(missing.to_string().contains("no-such-filter.yaml"));
    let empty = Matcher::<Open>::parse("{}").expect_err("a field-less matcher is refused");
    assert!(empty.to_string().starts_with("{}: "), "{empty}");
    let not_json = Matcher::<Open>::parse("nonsense").expect_err("not a matcher");
    assert!(not_json.to_string().contains("unusable"), "{not_json}");
}

#[test]
fn the_registry_renders_itself_and_refuses_a_bad_namespace_by_name() {
    let registry = Registry::new();
    assert_eq!(format!("{registry:?}"), "Registry { ids: [] }");
    let unknown = registry
        .check(&"test.none@1".parse().expect("id"), &json!({}))
        .expect_err("nothing registered");
    assert!(
        unknown.to_string().contains("registered: nothing"),
        "{unknown}"
    );
    let bad = "agent space.name@1"
        .parse::<SchemaId>()
        .expect_err("refused");
    assert!(
        bad.to_string().contains("namespace is not letters"),
        "{bad}"
    );
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("sink").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_emitter_reports_what_it_stamps_and_takes_a_version_override() {
    let sink = Captured::default();
    let emitter = Emitter::<Open>::new("s-1", Source::from("billing"), Box::new(sink.clone()))
        .with_redactor(Redactor::new())
        .with_version(7)
        .with_labels(Labels::new().with("tenant", "acme"));
    assert_eq!(emitter.stream(), "s-1");
    assert_eq!(emitter.source().as_str(), "billing");
    assert_eq!(emitter.version(), 7);
    assert_eq!(emitter.labels().get_str("tenant"), Some("acme"));
    assert!(format!("{emitter:?}").contains("stream: \"s-1\""));
    assert_eq!(emitter.emit("tick", Map::new()).v, 7);
}

#[test]
fn a_shared_emitter_that_cannot_lock_its_file_reports_the_path() {
    let dir = tempfile::tempdir().expect("a temp dir");
    // A directory is not a file that can be opened for appending.
    let emitter = Emitter::<Open>::shared("s", Source::from("billing"), dir.path())
        .with_redactor(Redactor::new());
    let unrecorded = emitter
        .try_emit("tick", Map::new(), Vec::new())
        .expect_err("a directory cannot be appended to");
    let said = unrecorded.to_string();
    assert!(said.contains("cannot order a tick event in"), "{said}");
    assert!(std::error::Error::source(&*unrecorded).is_some());
    assert_eq!(unrecorded.envelope.seq, 0, "never numbered");
    // The infallible spelling reports and hands the envelope back.
    assert_eq!(emitter.emit("tick", Map::new()).kind.as_str(), "tick");
}

#[test]
fn a_reader_collects_its_readings_and_a_merge_hands_its_records_over() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("s.ndjson");
    let line = json!({
        "v": 1, "ts": "2026-09-13T00:00:01.000Z", "stream": "s", "seq": 1,
        "source": "billing", "kind": "tick", "labels": {}, "payload": {}, "artifacts": []
    })
    .to_string();
    let mut bytes = format!("{line}\n\nnot json\n").into_bytes();
    bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
    bytes.extend_from_slice(b"{\"v\":1");
    std::fs::write(&path, bytes).expect("written");

    let readings = Reader::<Open>::open(&path).expect("opens").collect_all();
    assert_eq!(readings.records.len(), 1);
    assert_eq!(readings.refused.len(), 3);
    assert!(readings.refused[0].reason.contains("empty line"));
    assert!(readings.refused[2].reason.contains("UTF-8"));
    assert!(readings.torn.is_some());
    assert_eq!(readings.position, readings.refused[2].position);
    assert_eq!(Reader::<Open>::open(&path).expect("opens").path(), path);

    let merged = Merge::<Open>::open([&path]).expect("merges");
    assert_eq!(merged.refused().len(), 3);
    let records = merged.into_records();
    assert_eq!(records.len(), 1);
    assert!(matches!(
        Reader::<Open>::open(&path).expect("opens").next(),
        Some(Reading::Record(_))
    ));
}

fn render(document: Value) -> Result<String, sdk_schema::GenerateError> {
    sdk_schema::generate(Lang::Rust, &"test.thing@1".parse().expect("id"), &document)
}

#[test]
fn the_rust_renderer_refuses_what_it_cannot_express_naming_where() {
    let object =
        |properties: Value| json!({ "title": "Thing", "type": "object", "properties": properties });
    let cases = [
        (
            object(
                json!({ "inline": { "type": "object", "properties": { "a": { "type": "string" } } } }),
            ),
            "Thing.inline",
            "inline object",
        ),
        (
            object(json!({ "items": { "type": "array" } })),
            "Thing.items",
            "no item schema",
        ),
        (
            object(json!({ "elsewhere": { "$ref": "https://example.com/x" } })),
            "Thing.elsewhere",
            "outside the document's $defs",
        ),
        (
            object(json!({ "odd": { "type": "nonsense" } })),
            "Thing.odd",
            "a `nonsense` schema",
        ),
        (
            object(json!({ "twin": { "type": ["string", "integer"] } })),
            "Thing.twin",
            "no single type",
        ),
        (
            json!({ "title": "Thing", "type": "string" }),
            "Thing",
            "neither an object with properties nor a string enum",
        ),
        (
            json!({ "title": "Thing", "enum": [1, 2] }),
            "Thing",
            "not a string",
        ),
    ];
    for (document, at, why) in cases {
        let refusal = render(document).expect_err(why);
        let said = refusal.to_string();
        assert!(said.contains(at), "{said}");
        assert!(said.contains(why), "{said}");
    }
}

#[test]
fn the_rust_renderer_covers_the_shapes_the_rich_journey_does_not_reach() {
    let document = json!({
        "title": "Thing",
        "description": "A thing.",
        "type": "object",
        "properties": {
            "small": { "type": "integer", "format": "uint16" },
            "tiny": { "type": "integer", "format": "uint8" },
            "size": { "type": "integer", "format": "uint" },
            "signed": { "type": "integer", "format": "int32" },
            "short": { "type": "integer", "format": "int16" },
            "byte": { "type": "integer", "format": "int8" },
            "offset": { "type": "integer", "format": "int" },
            "plain": { "type": "integer" },
            "single": { "type": "number", "format": "float" },
            "type": { "type": "string" },
            "maybeRef": { "anyOf": [{ "type": "null" }, { "$ref": "#/$defs/Word" }] },
            "open": { "type": "object" }
        },
        "required": ["type"],
        "additionalProperties": true,
        "$defs": {
            "Word": { "enum": ["a-b", "c d"] }
        }
    });
    let rendered = render(document).expect("renders");
    for expected in [
        "pub small: u16,",
        "pub tiny: u8,",
        "pub size: usize,",
        "pub signed: i32,",
        "pub short: i16,",
        "pub byte: i8,",
        "pub offset: isize,",
        "pub plain: i64,",
        "pub single: f32,",
        "pub r#type: String,",
        "pub maybe_ref: Option<Word>,",
        "pub open: serde_json::Map<String, serde_json::Value>,",
        "#[serde(flatten)]",
        "pub extra: serde_json::Map<String, serde_json::Value>,",
        "#[serde(rename = \"a-b\")]",
        "    AB,",
        "    CD,",
        "/// A thing.",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in:\n{rendered}"
        );
    }
    assert!(!rendered.contains("deny_unknown_fields"));
}

#[test]
fn the_bundle_renders_as_json_and_formats_default_to_json() {
    let bundle: Bundle = sdk_schema::bundle::<Open>(&Registry::new());
    let text = bundle.to_json();
    assert!(text.ends_with('\n'));
    assert!(serde_json::from_str::<Value>(&text).is_ok());
    assert_eq!(Format::default(), Format::Json);
    assert_eq!(Lang::Json.as_str(), "json");
    assert_eq!(Lang::Rust.as_str(), "rust");
}

#[test]
fn a_schema_entry_whose_family_or_version_contradicts_its_id_is_refused() {
    let id: SchemaId = "billing.invoice@2".parse().expect("a well-formed id");
    let entry = sdk_schema::SchemaEntry::from(&id);
    let written = serde_json::to_value(&entry).expect("an entry serializes");
    assert_eq!(
        written,
        json!({"id": "billing.invoice@2", "family": "billing.invoice", "version": 2})
    );
    assert_eq!(
        serde_json::from_value::<sdk_schema::SchemaEntry>(written).expect("an entry reads back"),
        entry
    );

    for (field, value, restated) in [
        (
            "family",
            json!("billing.receipt"),
            "family billing.receipt at version 2",
        ),
        ("version", json!(3), "family billing.invoice at version 3"),
    ] {
        let mut contradicting =
            json!({"id": "billing.invoice@2", "family": "billing.invoice", "version": 2});
        contradicting[field] = value;
        let refusal = serde_json::from_value::<sdk_schema::SchemaEntry>(contradicting)
            .expect_err("an entry contradicting its id is refused")
            .to_string();
        assert!(
            refusal.contains("billing.invoice@2") && refusal.contains(restated),
            "the refusal of a contradicting {field} does not name the id and what it restated: {refusal}"
        );
    }

    let refusal = serde_json::from_value::<sdk_schema::SchemaEntry>(json!({
        "id": "billing.invoice@2", "family": "billing.invoice", "version": 2, "extra": 1
    }))
    .expect_err("an unknown field is refused")
    .to_string();
    assert!(refusal.contains("extra"), "{refusal}");
}
