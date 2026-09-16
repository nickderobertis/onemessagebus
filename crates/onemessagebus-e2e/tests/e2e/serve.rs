//! Configured codec journeys through the compiled binary and a linked schema.

use serde_json::json;

use crate::support::run_in;

struct Scratch {
    dir: tempfile::TempDir,
    config: String,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("scratch directory");
        let bundle = dir.path().join("example-frames.json");
        std::fs::write(
            &bundle,
            json!({
                "version": "3",
                "schemas": [{
                    "id": "example.frame.hello@3",
                    "schema": {
                        "type": "object",
                        "required": ["op", "value"],
                        "properties": {"op": {"enum": ["hello", "raise", "fail", "refuse", "conditional"]}, "value": {"type": "integer"}}
                    }
                }]
            })
            .to_string(),
        )
        .expect("bundle written");
        let config = dir.path().join("bus.yaml");
        std::fs::write(
            &config,
            format!(
                "version: 1\ntransport: {{kind: local, dir: {}}}\nqueues:\n  surfaces: {{}}\nschemas:\n  - \"file://{}@3\"\ncodecs:\n  example:\n    queue: surfaces\n    select: op\n    frames:\n      hello:\n        schema: example.frame.hello@3\n        bindings:\n          - when: {{field: mood, equals: lost}}\n            do: refuse\n            message: \"lost: {{frame.value}}\"\n          - do: answer\n            response: {{value: \"{{frame.value}}\", text: \"value={{frame.value}}\"}}\n      raise:\n        schema: example.frame.hello@3\n        bindings:\n          - do: raise\n            record: {{kind: example-raised, blocking: false, message: \"raised {{frame.value}}\"}}\n            response: {{raised: true}}\n      fail:\n        schema: example.frame.hello@3\n        bindings:\n          - do: raise\n            record: {{kind: example-failed, blocking: false, message: \"failed {{frame.value}}\"}}\n            fail: \"member failed {{frame.value}}\"\n      refuse:\n        schema: example.frame.hello@3\n        bindings:\n          - do: refuse\n            message: \"refused {{frame.value}}\"\n      conditional:\n        schema: example.frame.hello@3\n        bindings:\n          - when: {{field: mood, equals: ready}}\n            do: answer\n            response: {{ready: true}}\n",
                dir.path().join("channel").display(),
                bundle.display()
            ),
        )
        .expect("config written");
        Self {
            dir,
            config: config.to_string_lossy().into_owned(),
        }
    }

    fn serve(&self, frame: &str) -> crate::support::Run {
        run_in(
            self.dir.path(),
            &[
                "serve",
                "surfaces",
                "--codec",
                "example",
                "--config",
                &self.config,
            ],
            Some(frame),
            &[],
        )
    }

    fn run(&self, args: &[&str]) -> crate::support::Run {
        run_in(self.dir.path(), args, None, &[])
    }
}

#[test]
fn raise_queues_a_templated_record_and_responds_or_fails() {
    let scratch = Scratch::new();
    let answered = scratch.serve("{\"op\":\"raise\",\"value\":4}\n");
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    assert_eq!(answered.lines(), [json!({"raised": true})]);
    let first = scratch.run(&["next", "surfaces", "--config", &scratch.config]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(first.lines()[0]["record"]["message"], json!("raised 4"));

    let failed = scratch.serve("{\"op\":\"fail\",\"value\":5}\n");
    assert_eq!(failed.code, 1, "{}", failed.stderr);
    assert!(
        failed.stderr.contains("member failed 5"),
        "{}",
        failed.stderr
    );
    assert!(failed.stdout.is_empty());
    let second = scratch.run(&["next", "surfaces", "--config", &scratch.config]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(second.lines()[0]["record"]["message"], json!("failed 5"));

    let refused = scratch.serve("{\"op\":\"refuse\",\"value\":6}\n");
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(refused.stderr.contains("refused 6"), "{}", refused.stderr);
}

#[test]
fn configured_answer_preserves_placeholder_types_and_first_matching_refusal_wins() {
    let scratch = Scratch::new();
    let answered = scratch.serve("{\"op\":\"hello\",\"value\":7}\n");
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    assert_eq!(answered.lines(), [json!({"value": 7, "text": "value=7"})]);

    let refused = scratch.serve("{\"op\":\"hello\",\"value\":8,\"mood\":\"lost\"}\n");
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(refused.stderr.contains("lost: 8"), "{}", refused.stderr);
    assert!(refused.stdout.is_empty());
}

#[test]
fn malformed_unknown_and_schema_invalid_frames_are_refused() {
    let scratch = Scratch::new();
    for (frame, expected) in [
        ("not-json\n", "not JSON"),
        ("[]\n", "not a JSON object"),
        ("{\"value\":1}\n", "absent or not a string"),
        ("{\"op\":1,\"value\":1}\n", "absent or not a string"),
        ("{\"op\":\"other\",\"value\":1}\n", "declared entries"),
        (
            "{\"op\":\"hello\",\"value\":\"wrong\"}\n",
            "example.frame.hello@3",
        ),
        ("{\"op\":\"conditional\",\"value\":1}\n", "no binding holds"),
    ] {
        let refused = scratch.serve(frame);
        assert_eq!(refused.code, 2, "{}", refused.stderr);
        assert!(refused.stderr.contains(expected), "{}", refused.stderr);
        assert!(refused.stdout.is_empty());
    }
}

#[test]
fn undeclared_codec_is_refused_before_a_frame_is_read() {
    let scratch = Scratch::new();
    let refused = run_in(
        scratch.dir.path(),
        &[
            "serve",
            "surfaces",
            "--codec",
            "missing",
            "--config",
            &scratch.config,
        ],
        Some("not JSON\n"),
        &[],
    );
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("declared codecs: example"),
        "{}",
        refused.stderr
    );
}

#[test]
fn queue_mismatch_and_unregistered_schema_are_refused_before_input() {
    for (change, expected) in [
        ("queue: surfaces", "queue: replies"),
        ("example.frame.hello@3", "example.frame.missing@3"),
    ] {
        let scratch = Scratch::new();
        let text = std::fs::read_to_string(&scratch.config).expect("config read");
        std::fs::write(&scratch.config, text.replace(change, expected)).expect("config changed");
        let refused = scratch.serve("not-json\n");
        assert_eq!(refused.code, 2, "{}", refused.stderr);
        assert!(
            refused.stderr.contains(if change.starts_with("queue") {
                "serve was asked to serve `surfaces`"
            } else {
                "schema example.frame.missing@3 is not registered"
            }),
            "{}",
            refused.stderr
        );
        assert!(refused.stdout.is_empty());
    }
}
