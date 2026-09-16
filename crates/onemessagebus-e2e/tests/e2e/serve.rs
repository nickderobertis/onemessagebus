//! Configured codec journeys through the compiled binary and a linked schema.

use std::io::Write as _;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::support::{onemessagebus, run_in, Run};

const GUARD: Duration = Duration::from_secs(20);

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
                        "properties": {"op": {"enum": ["hello", "raise", "fail", "refuse", "conditional", "ask"]}, "value": {"type": "integer"}}
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
                "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: planner-channel\nschemas:\n  - \"file://{}@3\"\ncodecs:\n  example:\n    queue: surfaces\n    reply_window_seconds: 1\n    session_env: EXAMPLE_SESSION\n    asker_env: EXAMPLE_ASKER\n    about_env: EXAMPLE_ABOUT\n    select: op\n    frames:\n      hello:\n        schema: example.frame.hello@3\n        bindings:\n          - when: {{field: mood, equals: lost}}\n            do: refuse\n            message: \"lost: {{frame.value}}\"\n          - do: answer\n            response:\n              value: \"{{frame.value}}\"\n              text: \"value={{frame.value}}\"\n              nested: {{items: [\"{{{{\", \"missing={{frame.missing}}\"], object: {{close: \"}}}}\"}}}}\n      raise:\n        schema: example.frame.hello@3\n        bindings:\n          - do: raise\n            record: {{kind: example-raised, blocking: false, source: example, message: \"raised {{frame.value}}\"}}\n            response: {{raised: true}}\n      fail:\n        schema: example.frame.hello@3\n        bindings:\n          - do: raise\n            record: {{kind: example-failed, blocking: false, source: example, message: \"failed {{frame.value}}\"}}\n            fail: \"member failed {{frame.value}}\"\n      refuse:\n        schema: example.frame.hello@3\n        bindings:\n          - do: refuse\n            message: \"refused {{frame.value}}\"\n      conditional:\n        schema: example.frame.hello@3\n        bindings:\n          - when: {{field: mood, equals: ready}}\n            do: answer\n            response: {{ready: true}}\n      ask:\n        schema: example.frame.hello@3\n        bindings:\n          - do: ask\n            record: {{kind: example-question, source: example, message: \"rule on {{frame.value}}\"}}\n            response:\n              value: {{from: reply.completion}}\n              reason: {{from: [reply.reason, reply.message], default: \"ruled on {{frame.value}} without a reason\"}}\n              literal: \"{{reply.completion}}\"\n            unanswered: {{value: false, reason: \"no ruling for {{frame.value}}\"}}\n",
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

    fn spawn_ask(
        &self,
        held: bool,
        env: &[(&str, &str)],
    ) -> (Child, Option<std::process::ChildStdin>) {
        let mut command = onemessagebus();
        command
            .args([
                "serve",
                "surfaces",
                "--codec",
                "example",
                "--config",
                &self.config,
            ])
            .current_dir(self.dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("serve spawns");
        let mut stdin = child.stdin.take().expect("stdin pipe");
        stdin
            .write_all(b"{\"op\":\"ask\",\"value\":9}\n")
            .expect("frame written");
        stdin.flush().expect("frame flushed");
        (child, held.then_some(stdin))
    }

    fn asked(&self) -> Value {
        let started = Instant::now();
        loop {
            let claimed = self.run(&["next", "surfaces", "--config", &self.config]);
            if claimed.code == 0 {
                return claimed.lines()[0]["record"].clone();
            }
            assert!(started.elapsed() < GUARD, "serve asked nothing");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn finish(mut child: Child) -> Run {
    let started = Instant::now();
    while child.try_wait().expect("serve polled").is_none() {
        assert!(started.elapsed() < GUARD, "serve never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("serve output");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
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
    assert_eq!(
        answered.lines(),
        [json!({
            "value": 7,
            "text": "value=7",
            "nested": {"items": ["{", "missing="], "object": {"close": "}"}}
        })]
    );

    let refused = scratch.serve("{\"op\":\"hello\",\"value\":8,\"mood\":\"lost\"}\n");
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(refused.stderr.contains("lost: 8"), "{}", refused.stderr);
    assert!(refused.stdout.is_empty());
}

#[test]
fn ask_relays_a_ruling_through_mappings_and_uses_the_configured_asker() {
    let scratch = Scratch::new();
    let text = std::fs::read_to_string(&scratch.config).expect("config read");
    std::fs::write(
        &scratch.config,
        text.replace(
            "literal: \"{reply.completion}\"",
            "literal: \"{reply.completion}\"\n              nested: [{from: reply.completion}, \"frame {frame.value}\"]",
        ),
    )
    .expect("array mapping config written");
    let (child, _) = scratch.spawn_ask(
        false,
        &[
            ("EXAMPLE_ASKER", "example-host"),
            ("EXAMPLE_ABOUT", "build-9"),
        ],
    );
    let question = scratch.asked();
    assert_eq!(question["asker"], json!("example-host"));
    assert_eq!(question["workstream"], json!("build-9"));
    assert_eq!(question["message"], json!("rule on 9"));
    let correlation = question["correlation"].as_str().expect("correlation");
    let replied = run_in(
        scratch.dir.path(),
        &[
            "reply",
            "surfaces",
            "--correlation",
            correlation,
            "--config",
            &scratch.config,
        ],
        Some(r#"{"version":3,"completion":true,"message":"accepted"}"#),
        &[],
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    let served = finish(child);
    assert_eq!(served.code, 0, "{}", served.stderr);
    assert_eq!(
        served.lines(),
        [json!({
            "value": true,
            "reason": "accepted",
            "literal": true,
            "nested": [true, "frame 9"]
        })]
    );
}

#[test]
fn nested_paths_and_malformed_templates_are_handled_at_the_binary_boundary() {
    let nested = Scratch::new();
    let text = std::fs::read_to_string(&nested.config).expect("config read");
    std::fs::write(
        &nested.config,
        text.replace("select: op", "select: turn.op")
            .replace("field: mood", "field: turn.mood"),
    )
    .expect("nested-path config written");
    let refused = nested
        .serve("{\"op\":\"hello\",\"turn\":{\"op\":\"hello\",\"mood\":\"lost\"},\"value\":8}\n");
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(refused.stderr.contains("lost: 8"), "{}", refused.stderr);

    let malformed = Scratch::new();
    let text = std::fs::read_to_string(&malformed.config).expect("config read");
    std::fs::write(
        &malformed.config,
        text.replace("value={frame.value}", "value=}"),
    )
    .expect("malformed config written");
    let refused = malformed.serve("not JSON\n");
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("codecs.example.frames.hello.bindings[1]")
            && refused.stderr.contains("unescaped `}`"),
        "{}",
        refused.stderr
    );
}

#[test]
fn configured_about_is_validated_and_raise_reports_a_queue_refusal() {
    let invalid_about = Scratch::new();
    let refused = run_in(
        invalid_about.dir.path(),
        &[
            "serve",
            "surfaces",
            "--codec",
            "example",
            "--config",
            &invalid_about.config,
        ],
        Some("not JSON\n"),
        &[("EXAMPLE_ABOUT", " ")],
    );
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("EXAMPLE_ABOUT"),
        "{}",
        refused.stderr
    );

    let invalid_record = Scratch::new();
    let text = std::fs::read_to_string(&invalid_record.config).expect("config read");
    std::fs::write(
        &invalid_record.config,
        text.replacen("source: example, ", "", 1),
    )
    .expect("invalid record config written");
    let refused = invalid_record.serve("{\"op\":\"raise\",\"value\":4}\n");
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("queue refused the raised record")
            && refused.stderr.contains("source"),
        "{}",
        refused.stderr
    );
}

#[test]
// llmlint: ignore[tests_mirror_real_usage] The public `reply` boundary rejects this schema-invalid answer before appending it, so corrupting the local transport is the only reachable representation of the persisted bad answer this runtime recovery path must refuse; `serve`, the behavior under test, remains driven through the compiled binary.
fn ask_uses_unanswered_for_an_unresolved_mapping_and_fails_on_a_corrupt_answer() {
    let unresolved = Scratch::new();
    let text = std::fs::read_to_string(&unresolved.config).expect("config read");
    std::fs::write(
        &unresolved.config,
        text.replace(
            "value: {from: reply.completion}",
            "value: {from: reply.missing}",
        ),
    )
    .expect("mapping config written");
    let (child, _) = unresolved.spawn_ask(false, &[]);
    let question = unresolved.asked();
    let correlation = question["correlation"].as_str().expect("correlation");
    let replied = run_in(
        unresolved.dir.path(),
        &[
            "reply",
            "surfaces",
            "--correlation",
            correlation,
            "--config",
            &unresolved.config,
        ],
        Some(r#"{"version":3,"completion":true}"#),
        &[],
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    let served = finish(child);
    assert_eq!(
        served.lines(),
        [json!({"value": false, "reason": "no ruling for 9"})]
    );

    let corrupt = Scratch::new();
    let (child, _) = corrupt.spawn_ask(false, &[]);
    let question = corrupt.asked();
    let record = json!({
        "id": 0,
        "reply": {"version": 3, "completion": "not-a-boolean"},
        "at": 1_789_300_000_000u64,
        "correlation": question["correlation"]
    });
    std::fs::write(
        corrupt.dir.path().join("channel/replies.jsonl"),
        format!("{record}\n"),
    )
    .expect("corrupt reply injected");
    let served = finish(child);
    assert_eq!(served.code, 1, "{}", served.stderr);
    assert!(
        served.stderr.contains("answer was refused"),
        "{}",
        served.stderr
    );
}

#[test]
fn ask_timeout_writes_unanswered_and_a_configured_session_bound_stays_counted() {
    let timed_out = Scratch::new();
    let unanswered = timed_out.serve("{\"op\":\"ask\",\"value\":9}\n");
    assert_eq!(unanswered.code, 0, "{}", unanswered.stderr);
    assert_eq!(
        unanswered.lines(),
        [json!({"value": false, "reason": "no ruling for 9"})]
    );
    let status = timed_out.run(&["status", "surfaces", "--config", &timed_out.config]);
    assert_eq!(status.code, 0, "{}", status.stderr);
    let status: Vec<Value> = serde_json::from_str(&status.stdout).expect("status JSON");
    assert_eq!(status[0]["abandoned"].as_array().map(Vec::len), Some(1));

    let bounded = Scratch::new();
    let (child, held) = bounded.spawn_ask(true, &[("EXAMPLE_SESSION", "2")]);
    let served = finish(child);
    drop(held);
    assert_eq!(served.code, 0, "{}", served.stderr);
    assert!(
        served.stderr.contains("2-second bound") && served.stderr.contains("still counted"),
        "{}",
        served.stderr
    );
    let status = bounded.run(&["status", "surfaces", "--config", &bounded.config]);
    let status: Vec<Value> = serde_json::from_str(&status.stdout).expect("status JSON");
    assert_eq!(status[0]["abandoned"].as_array().map(Vec::len), Some(0));
    assert_eq!(status[0]["unread"], json!(1));
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
    for (change, replacement) in [
        ("queue: surfaces", "queue: replies"),
        ("example.frame.hello@3", "example.frame.missing@3"),
    ] {
        let scratch = Scratch::new();
        let text = std::fs::read_to_string(&scratch.config).expect("config read");
        std::fs::write(&scratch.config, text.replace(change, replacement)).expect("config changed");
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
