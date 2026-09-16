//! The small public surface the journeys do not reach on their own: accessors,
//! conversions, renderings, and the refusals of the renderer and the emitter.

use std::sync::{Arc, Mutex};

use onemessagebus::sdk_schema::{self, Bundle, Format, Lang};
use onemessagebus::{
    Admits, Allowlist, Author, Emitter, Filter, FlagKind, Kind, LabelMatch, Labels, Matcher, Merge,
    OpWord, Open, Reader, Reading, Redactor, Registry, Reserved, SchemaId, Source,
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
fn an_allowlist_reports_its_declared_authors_and_grants() {
    let sentinel = Author::from("sentinel");
    let retry = OpWord("retry".to_owned());
    let mut allowlist = Allowlist::new([retry.clone()]);
    allowlist.grant(sentinel.clone(), retry.clone());

    assert_eq!(sentinel.as_str(), "sentinel");
    assert_eq!(allowlist.authors(), [sentinel.clone()]);
    assert_eq!(allowlist.granted(&sentinel), [retry]);
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
            "neither an object with properties, a string enum, nor a named scalar",
        ),
        (
            // A named scalar carrying a keyword no newtype regenerates.
            json!({
                "title": "Thing",
                "type": "object",
                "properties": { "code": { "$ref": "#/$defs/Code" } },
                "$defs": { "Code": { "type": "string", "pattern": "^[a-z]+$" } }
            }),
            "Code",
            "nor a named scalar",
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
fn the_rust_renderer_maps_names_that_need_it_to_identifiers() {
    let document = json!({
        "title": "Thing",
        "type": "object",
        "properties": {
            "kebab-key": { "type": "string" },
            "camelCase": { "type": "string" },
            "type": { "type": "string" },
            "async": { "type": "string" },
            "_private": { "type": "string" },
            "plain2": { "type": "string" },
            "kind": { "$ref": "#/$defs/match" }
        },
        "$defs": {
            "match": { "enum": ["in-flight", "done_now", "_quiet"] }
        }
    });
    let rendered = render(document).expect("renders");
    for expected in [
        "#[serde(rename = \"kebab-key\", default)]\n    pub kebab_key: String,",
        "#[serde(rename = \"camelCase\", default)]\n    pub camel_case: String,",
        "#[serde(rename = \"type\", default)]\n    pub r#type: String,",
        "#[serde(rename = \"async\", default)]\n    pub r#async: String,",
        "#[serde(default)]\n    pub _private: String,",
        "#[serde(default)]\n    pub plain2: String,",
        "pub kind: r#match,",
        "pub enum r#match {",
        "#[serde(rename = \"in-flight\")]\n    InFlight,",
        "#[serde(rename = \"done_now\")]\n    DoneNow,",
        "#[serde(rename = \"_quiet\")]\n    Quiet,",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in:\n{rendered}"
        );
    }
}

/// A name the document gives — a title, a `$defs` name, a property, an enum
/// value — is spliced into Rust source, so one that cannot become an
/// identifier is refused naming its pointer, the name, and what is wrong.
#[test]
fn the_rust_renderer_refuses_a_name_that_cannot_become_an_identifier() {
    let titled = |title: &str| json!({ "title": title, "type": "object", "properties": {} });
    let property = |name: &str| json!({ "title": "Thing", "type": "object", "properties": { name: { "type": "string" } } });
    let words = |words: Value| json!({ "title": "Thing", "enum": words });
    let cases = [
        (titled(""), "/title", "the title \"\"", "it is empty"),
        (
            titled("9Lives"),
            "/title",
            "the title \"9Lives\"",
            "it starts with a digit",
        ),
        (
            titled("Has Space"),
            "/title",
            "the title \"Has Space\"",
            "' ' is not an ASCII letter, digit or underscore",
        ),
        (titled("_"), "/title", "the title \"_\"", "a lone `_`"),
        (
            property(""),
            "/properties/",
            "the property name \"\"",
            "it is empty",
        ),
        (
            property("1st"),
            "/properties/1st",
            "the property name \"1st\"",
            "it starts with a digit",
        ),
        (
            property("a.b"),
            "/properties/a.b",
            "the property name \"a.b\"",
            "'.' is not an ASCII letter, digit or underscore",
        ),
        (
            property("a/b~"),
            "/properties/a~1b~0",
            "the property name \"a/b~\"",
            "'/' is not an ASCII letter",
        ),
        (
            property("self"),
            "/properties/self",
            "the property name \"self\"",
            "`self` is a keyword Rust does not take even as a raw identifier",
        ),
        (
            property("Super"),
            "/properties/Super",
            "the property name \"Super\" (as `super`)",
            "`super` is a keyword",
        ),
        (
            words(json!([""])),
            "/enum/0",
            "the enum value \"\"",
            "it is empty",
        ),
        (
            words(json!(["ok", "9"])),
            "/enum/1",
            "the enum value \"9\"",
            "it starts with a digit",
        ),
        (
            words(json!(["-"])),
            "/enum/0",
            "the enum value \"-\"",
            "nothing of it is left once mapped",
        ),
        (
            json!({ "title": "Thing", "oneOf": [{ "type": "string", "const": "self" }] }),
            "/oneOf/0/const",
            "the enum value \"self\" (as `Self`)",
            "`Self` is a keyword",
        ),
        (
            json!({ "title": "Thing", "type": "object", "properties": {}, "$defs": { "a b": { "enum": ["x"] } } }),
            "/$defs/a b",
            "the $defs name \"a b\"",
            "' ' is not",
        ),
        (
            json!({ "title": "Thing", "type": "object", "properties": {}, "$defs": { "Word": { "enum": ["x", "a.b"] } } }),
            "/$defs/Word/enum/1",
            "the enum value \"a.b\" (as `A.b`)",
            "'.' is not",
        ),
        (
            json!({ "title": "Thing", "type": "object", "properties": { "x": { "$ref": "#/$defs/no such" } } }),
            "Thing.x",
            "the reference target \"no such\"",
            "' ' is not",
        ),
        (
            json!({ "title": "Thing", "type": "object", "properties": { "a-b": { "type": "string" }, "a_b": { "type": "string" } } }),
            "/properties/a_b",
            "the property name \"a-b\" and the property name \"a_b\"",
            "both become the field `a_b`",
        ),
        (
            json!({ "title": "Thing", "type": "object", "properties": { "fooBar": { "type": "string" }, "foo_bar": { "type": "string" } } }),
            "/properties/foo_bar",
            "the property name \"fooBar\" and the property name \"foo_bar\"",
            "both become the field `foo_bar`",
        ),
        (
            words(json!(["in-flight", "in_flight"])),
            "/enum/1",
            "the enum value \"in-flight\" and the enum value \"in_flight\"",
            "both become the variant `InFlight`",
        ),
        (
            json!({ "title": "Word", "type": "object", "properties": {}, "$defs": { "Word": { "enum": ["x"] } } }),
            "/$defs/Word",
            "the title \"Word\" and the $defs name \"Word\"",
            "both become the type `Word`",
        ),
        (
            json!({ "title": "Thing", "type": "object", "properties": { "extra": { "type": "string" } }, "additionalProperties": true }),
            "/properties/extra",
            "the property name \"extra\" becomes `extra`",
            "the field the renderer declares for every other key",
        ),
    ];
    for (document, at, named, defect) in cases {
        let said = render(document).expect_err(named).to_string();
        assert!(
            said.starts_with(&format!("test.thing@1: cannot render {at} as Rust: ")),
            "{said}"
        );
        assert!(said.contains(named), "{said}");
        assert!(said.contains(defect), "{said}");
    }
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
