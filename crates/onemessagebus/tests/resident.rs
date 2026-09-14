//! The resident protocol's lines: each reads back as the kind of line it is,
//! the registered document validates what the resident writes and refuses what
//! no side may write, and the two literal fields refuse every other value by
//! name.

use onemessagebus::resident::{
    RequestId, ResidentAnswer, ResidentCancel, ResidentEvent, ResidentExit, ResidentFailure,
    ResidentLine, ResidentRefusal, ResidentRequest, ResidentVerb, True, RESIDENT_PROTOCOL,
};
use onemessagebus::{Registry, CAPABILITIES};
use serde_json::{json, Value};

fn line(text: &str) -> ResidentLine {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .register::<ResidentLine>()
        .expect("the protocol registers");
    registry
}

#[test]
fn each_line_reads_back_as_the_kind_of_line_it_is_and_writes_the_same_bytes() {
    let cases = [
        (
            r#"{"id":1,"verb":"send","args":{"queue":"greetings"},"input":"{\"text\":\"hi\"}"}"#,
            ResidentLine::Request(ResidentRequest {
                id: RequestId(1),
                verb: ResidentVerb::named("send").expect("a capability"),
                args: json!({"queue": "greetings"})
                    .as_object()
                    .cloned()
                    .expect("an object"),
                input: Some(r#"{"text":"hi"}"#.to_owned()),
            }),
        ),
        (
            r#"{"id":2,"verb":"transports"}"#,
            ResidentLine::Request(ResidentRequest {
                id: RequestId(2),
                verb: ResidentVerb::named("transports").expect("a capability"),
                args: serde_json::Map::new(),
                input: None,
            }),
        ),
        (
            r#"{"id":2,"cancel":true}"#,
            ResidentLine::Cancel(ResidentCancel {
                id: RequestId(2),
                cancel: True,
            }),
        ),
        (
            r#"{"id":3,"ok":[{"queue":"greetings","position":"1"}]}"#,
            ResidentLine::Answer(ResidentAnswer {
                id: RequestId(3),
                ok: json!([{"queue": "greetings", "position": "1"}]),
            }),
        ),
        (
            r#"{"id":4,"error":{"exit":1,"message":"nothing on greetings to claim"}}"#,
            ResidentLine::Failure(ResidentFailure {
                id: Some(RequestId(4)),
                error: ResidentRefusal {
                    exit: ResidentExit::Failed,
                    message: "nothing on greetings to claim".to_owned(),
                    output: None,
                },
            }),
        ),
        (
            r#"{"id":null,"error":{"exit":2,"message":"not a line","output":{"answer":"timeout"}}}"#,
            ResidentLine::Failure(ResidentFailure {
                id: None,
                error: ResidentRefusal {
                    exit: ResidentExit::Invalid,
                    message: "not a line".to_owned(),
                    output: Some(json!({"answer": "timeout"})),
                },
            }),
        ),
        (
            r#"{"id":5,"event":{"position":"1","record":{}}}"#,
            ResidentLine::Event(ResidentEvent {
                id: RequestId(5),
                event: json!({"position": "1", "record": {}}),
            }),
        ),
    ];
    let registry = registry();
    for (text, expected) in cases {
        let read = line(text);
        assert_eq!(read, expected, "{text}");
        assert_eq!(serde_json::to_string(&read).expect("serializes"), text);
        let value: Value = serde_json::from_str(text).expect("JSON");
        registry
            .check(&RESIDENT_PROTOCOL, &value)
            .unwrap_or_else(|e| panic!("{text}: {e}"));
    }
    assert_eq!(ResidentExit::Failed.code(), 1);
    assert_eq!(ResidentExit::Invalid.code(), 2);
}

#[test]
fn a_line_no_side_writes_is_refused_by_the_reader_and_the_document_alike() {
    let registry = registry();
    for (text, why) in [
        (
            r#"{"id":1,"cancel":false}"#,
            "a cancel line's `cancel` is `true`",
        ),
        (
            r#"{"id":1,"error":{"exit":3,"message":"m"}}"#,
            "3 is not an exit code a refusal carries",
        ),
        (r#"{"id":1,"verb":"send","extra":1}"#, ""),
        (r#"{"id":1}"#, ""),
    ] {
        let value: Value = serde_json::from_str(text).expect("JSON");
        assert!(
            registry.check(&RESIDENT_PROTOCOL, &value).is_err(),
            "the document admits {text}"
        );
        assert!(
            serde_json::from_value::<ResidentLine>(value).is_err(),
            "{text}"
        );
        // An untagged read names no variant's own reason; each literal type does.
        if !why.is_empty() {
            let direct = if text.contains("cancel") {
                serde_json::from_str::<ResidentCancel>(text)
                    .expect_err(text)
                    .to_string()
            } else {
                serde_json::from_str::<ResidentFailure>(text)
                    .expect_err(text)
                    .to_string()
            };
            assert!(direct.contains(why), "{text}: {direct}");
        }
    }
    assert_eq!(RESIDENT_PROTOCOL.to_string(), "bus.resident-protocol@1");
}

/// A request names a capability of the manifest, and a method the manifest does
/// not name is refused by the reader and the registered document alike, naming
/// the methods there are.
#[test]
fn a_request_names_a_capability_of_the_manifest_and_no_other() {
    let methods: Vec<&str> = CAPABILITIES.iter().map(|c| c.method).collect();
    let schema = schemars::schema_for!(ResidentVerb);
    assert_eq!(schema.as_value()["enum"], json!(methods));
    for method in &methods {
        let verb = ResidentVerb::named(method).expect("a capability");
        assert_eq!(verb.method(), *method);
        assert_eq!(verb.capability().method, *method);
        assert_eq!(
            serde_json::to_value(verb).expect("serializes"),
            json!(method)
        );
    }
    assert_eq!(ResidentVerb::named("publish"), None);
    let refused = serde_json::from_str::<ResidentRequest>(r#"{"id":1,"verb":"publish"}"#)
        .expect_err("publish is no capability")
        .to_string();
    assert!(
        refused.starts_with(&format!(
            "`publish` is not a verb of the resident core; it answers each capability's method: {}",
            methods.join(", ")
        )),
        "{refused}"
    );
    assert!(registry()
        .check(&RESIDENT_PROTOCOL, &json!({"id": 1, "verb": "publish"}))
        .is_err());
}
