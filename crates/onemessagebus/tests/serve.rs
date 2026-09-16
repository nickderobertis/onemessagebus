//! The serving loop every codec shares (`Bus::serve`), driven by a codec of
//! this test's own over a local transport: frames in, one response per frame
//! out, and what a session's ending makes of the questions it asked — a stream
//! that ends abandons them, a session bound leaves them counted.

use std::io::{BufReader, Read};
use std::sync::{mpsc, LazyLock};
use std::time::{Duration, Instant};

use onemessagebus::{
    Answer, Asker, Bus, BusError, Codec, CodecFailure, CodecName, Config, ConfigError,
    ConfiguredCodec, EnvName, Layouts, QueueName, Registry, SchemaId, ServeError, ServeOptions,
    ServeSession, Served, TransportKinds,
};
use serde_json::{json, Value};

/// A codec whose frames say what to do: `echo` a text, `ask` a question and
/// answer the word its wait answered, `raise` a record, `refuse` or `fail`.
struct Scripted;

static SCRIPTED_NAME: LazyLock<CodecName> =
    LazyLock::new(|| "scripted".parse().expect("a codec name"));

impl Codec for Scripted {
    fn name(&self) -> &CodecName {
        &SCRIPTED_NAME
    }

    fn answer(
        &mut self,
        frame: &str,
        session: &mut ServeSession<'_>,
    ) -> Result<Value, CodecFailure> {
        let frame: Value = serde_json::from_str(frame)
            .map_err(|failure| CodecFailure::Refused(format!("not JSON: {failure}")))?;
        match frame["do"].as_str() {
            Some("echo") => Ok(json!({"echo": frame["text"]})),
            Some("ask") => {
                let (correlation, answer) = session
                    .ask(
                        json!({"kind": "question", "message": frame["message"]}),
                        false,
                    )
                    .map_err(|failure| CodecFailure::Failed(failure.to_string()))?;
                Ok(json!({"answer": answer.word(), "correlation": correlation.as_str()}))
            }
            Some("raise") => {
                let raised = session
                    .raise(
                        json!({"kind": "notice", "message": frame["message"], "blocking": false}),
                    )
                    .map_err(|failure| CodecFailure::Failed(failure.to_string()))?;
                Ok(json!({"raised": raised.len()}))
            }
            Some("inspect") => Ok(json!({
                "queue": session.queue().as_str(),
                "reply_window_ms": session.options().reply_window.as_millis(),
                "debug": format!("{session:?}"),
            })),
            Some("refuse") => Err(CodecFailure::Refused(
                "`refuse` is refused by name".to_owned(),
            )),
            _ => Err(CodecFailure::Failed("the member failed".to_owned())),
        }
    }
}

struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        }
    }

    fn bus(&self) -> Bus {
        Config::parse(&format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\nqueues:\n  questions: {{policy: {{hold_pending: true}}, answers: replies}}\n  replies: {{}}\n",
            serde_json::to_string(&self.dir.path().join("channel")).expect("a path")
        ))
        .expect("loads")
        .resolve(&Layouts::new(), &TransportKinds::builtin())
        .expect("resolves")
    }

    fn questions(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("channel/questions.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }
}

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

fn frames(text: &str) -> Box<dyn std::io::BufRead + Send> {
    Box::new(BufReader::new(std::io::Cursor::new(text.to_owned())))
}

fn options(reply_window: Duration) -> ServeOptions {
    ServeOptions {
        reply_window,
        ..ServeOptions::default()
    }
}

fn lines(output: &[u8]) -> Vec<Value> {
    String::from_utf8(output.to_vec())
        .expect("UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a response line"))
        .collect()
}

/// A frame stream that hands over its frames and then stays open, as a member
/// that is still there does, until its sender is dropped.
struct HeldOpen {
    frames: std::io::Cursor<Vec<u8>>,
    open: mpsc::Receiver<()>,
}

impl Read for HeldOpen {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.frames.read(buf)?;
        if read > 0 {
            return Ok(read);
        }
        let _ = self.open.recv();
        Ok(0)
    }
}

#[test]
fn each_frame_is_answered_with_one_line_in_order_and_blank_lines_are_passed_over() {
    let rig = Rig::new();
    let bus = rig.bus();
    let mut output = Vec::new();
    let served = bus
        .serve(
            &queue("questions"),
            &mut Scripted,
            &ServeOptions::default(),
            frames("{\"do\":\"echo\",\"text\":\"a\"}\n\n   \n{\"do\":\"echo\",\"text\":\"b\"}\n"),
            &mut output,
        )
        .expect("served");
    assert_eq!(served, Served::StreamEnded { abandoned: 0 });
    assert_eq!(
        lines(&output),
        vec![json!({"echo": "a"}), json!({"echo": "b"})]
    );
    assert_eq!(Scripted.name().as_str(), "scripted");
}

#[test]
fn a_codec_can_inspect_the_public_session_context() {
    let rig = Rig::new();
    let mut output = Vec::new();
    rig.bus()
        .serve(
            &queue("questions"),
            &mut Scripted,
            &options(Duration::from_millis(25)),
            frames("{\"do\":\"inspect\"}\n"),
            &mut output,
        )
        .expect("served");
    let response = &lines(&output)[0];
    assert_eq!(response["queue"], "questions");
    assert_eq!(response["reply_window_ms"], 25);
    assert!(
        response["debug"]
            .as_str()
            .is_some_and(|text| text.contains("ServeSession") && text.contains("asked: 0")),
        "{response}"
    );
}

#[test]
fn a_stream_that_ends_abandons_what_the_session_asked_and_left_unanswered() {
    let rig = Rig::new();
    let bus = rig.bus();
    let mut output = Vec::new();
    let served = bus
        .serve(
            &queue("questions"),
            &mut Scripted,
            &ServeOptions {
                asker: Some(Asker::new("monitor-1", "the test").expect("an asker")),
                about: Some("build".parse().expect("an address")),
                ..options(Duration::from_millis(50))
            },
            frames("{\"do\":\"ask\",\"message\":\"did the watch meet its bar?\"}\n"),
            &mut output,
        )
        .expect("served");
    assert_eq!(served, Served::StreamEnded { abandoned: 1 });
    assert_eq!(lines(&output)[0]["answer"], json!("timeout"));
    let logged = rig.questions();
    assert_eq!(logged[0]["asker"], json!("monitor-1"));
    assert_eq!(logged[0]["about"], json!("build"));
    assert_eq!(
        logged.last().expect("a line")["event"],
        json!("abandoned"),
        "the unanswered question was not marked: {logged:?}"
    );
}

#[test]
fn a_session_bound_reached_with_the_stream_open_leaves_what_it_asked_counted() {
    let rig = Rig::new();
    let bus = rig.bus();
    let (keep_open, open) = mpsc::channel();
    let input = HeldOpen {
        frames: std::io::Cursor::new(b"{\"do\":\"ask\",\"message\":\"still there?\"}\n".to_vec()),
        open,
    };
    let mut output = Vec::new();
    let started = Instant::now();
    let served = bus
        .serve(
            &queue("questions"),
            &mut Scripted,
            &ServeOptions {
                session: Some(Duration::from_millis(600)),
                ..options(Duration::from_millis(50))
            },
            Box::new(BufReader::new(input)),
            &mut output,
        )
        .expect("served");
    assert_eq!(served, Served::SessionOver { standing: 1 });
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the bound was not a deadline"
    );
    assert_eq!(lines(&output)[0]["answer"], json!("timeout"));
    let status = bus
        .queue(&queue("questions"))
        .expect("a queue")
        .status()
        .expect("a status");
    assert!(
        status.abandoned.is_empty(),
        "a session bound abandoned: {status:?}"
    );
    assert_eq!(status.unread, 1, "the question stopped being counted");
    drop(keep_open);
}

#[test]
fn an_answered_question_is_not_abandoned_when_the_stream_ends() {
    let rig = Rig::new();
    let bus = rig.bus();
    let replier = rig.bus();
    let questions = rig.dir.path().join("channel/questions.jsonl");
    let replying = std::thread::spawn(move || {
        let started = Instant::now();
        loop {
            let asked = std::fs::read_to_string(&questions).unwrap_or_default();
            if let Some(line) = asked.lines().next() {
                let question: Value = serde_json::from_str(line).expect("a line");
                let correlation = question["correlation"]
                    .as_str()
                    .expect("a correlation")
                    .parse()
                    .expect("parses");
                return replier
                    .reply(
                        &queue("questions"),
                        Some(&correlation),
                        json!({"completion": true}),
                    )
                    .map(|_| ());
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "nothing was asked"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let mut output = Vec::new();
    let served = bus
        .serve(
            &queue("questions"),
            &mut Scripted,
            &options(Duration::from_secs(20)),
            frames("{\"do\":\"ask\",\"message\":\"yes?\"}\n"),
            &mut output,
        )
        .expect("served");
    replying
        .join()
        .expect("the replier finishes")
        .expect("replied");
    assert_eq!(lines(&output)[0]["answer"], json!("reply"));
    assert_eq!(served, Served::StreamEnded { abandoned: 0 });
    assert_eq!(Answer::<Value>::Timeout.word(), "timeout");
}

#[test]
fn a_refused_or_failed_frame_ends_the_session_with_nothing_it_asked_marked() {
    let rig = Rig::new();
    let bus = rig.bus();
    let mut output = Vec::new();
    let refused = bus
        .serve(
            &queue("questions"),
            &mut Scripted,
            &options(Duration::from_millis(20)),
            frames("{\"do\":\"ask\",\"message\":\"one\"}\n{\"do\":\"refuse\"}\n{\"do\":\"echo\",\"text\":\"never\"}\n"),
            &mut output,
        )
        .expect_err("refused");
    assert!(
        matches!(&refused, ServeError::Refused(why) if why.contains("`refuse`")),
        "{refused}"
    );
    assert_eq!(
        lines(&output).len(),
        1,
        "a frame after the refusal was answered"
    );
    assert!(
        rig.questions()
            .iter()
            .all(|line| line["event"] != json!("abandoned")),
        "a refused session abandoned what it asked"
    );
    let failed = bus
        .serve(
            &queue("questions"),
            &mut Scripted,
            &options(Duration::from_millis(20)),
            frames("{\"do\":\"raise\",\"message\":\"the turn was lost\"}\n{\"do\":\"fail\"}\n"),
            &mut Vec::new(),
        )
        .expect_err("failed");
    assert!(matches!(failed, ServeError::Failed(_)), "{failed}");
    assert_eq!(
        rig.questions().last().expect("a line")["message"],
        json!("the turn was lost"),
        "what the codec raised before failing is not on the queue"
    );
    let unknown = bus
        .serve(
            &queue("nowhere"),
            &mut Scripted,
            &ServeOptions::default(),
            frames("{\"do\":\"echo\",\"text\":\"x\"}\n"),
            &mut Vec::new(),
        )
        .expect_err("an undeclared queue");
    assert!(
        matches!(unknown, ServeError::Bus(BusError::UnknownQueue { .. })),
        "{unknown}"
    );
}

const CONTRACT: &str = include_str!("../../../docs/codecs.md");

/// The fenced block `docs/codecs.md` tags `<!-- fixture: name -->`.
fn fixture(name: &str) -> String {
    let tag = format!("<!-- fixture: {name} -->");
    let mut lines = CONTRACT.lines();
    lines
        .by_ref()
        .find(|line| line.trim() == tag)
        .unwrap_or_else(|| panic!("docs/contract.md has no fixture tagged {name:?}"));
    assert!(lines.next().is_some_and(|line| line.starts_with("```")));
    lines
        .take_while(|line| !line.starts_with("```"))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn the_documented_codecs_block_loads_by_name_into_the_config_schema_the_sdk_bundle_carries() {
    let text = format!(
        "version: 1\ntransport: {{kind: local, dir: runs/r1/channel}}\n{}",
        fixture("codecs-config")
    );
    let config = Config::parse(&text).expect("the documented codecs block loads");
    let block = &config.codecs[&"example".parse::<CodecName>().expect("a codec name")];
    assert_eq!(block.queue, Some(queue("surfaces")));
    assert_eq!(block.reply_window_seconds.map(u64::from), Some(3000));
    assert_eq!(block.select, "op");
    assert!(!block.frames.is_empty());
    let refused = Config::parse(&text.replace("about_env", "node_env")).expect_err("unknown");
    assert!(
        refused.to_string().contains("unknown field `node_env`"),
        "{refused}"
    );
    let bundle =
        onemessagebus::sdk_schema::bundle::<onemessagebus::Open>(&onemessagebus::Registry::new());
    let document: Value = serde_json::from_str(&bundle.to_json()).expect("the bundle is JSON");
    assert!(
        document["config"]["properties"]["codecs"].is_object(),
        "the SDK bundle's config root has no codecs block"
    );
    assert!(document["codec_response"].is_object());
}

#[test]
fn a_codecs_block_loads_by_name_and_is_refused_by_the_key_it_is_wrong_at() {
    let text = |codecs: &str| {
        format!(
            "version: 1\ntransport: {{kind: memory}}\nqueues:\n  surfaces: {{}}\ncodecs:\n{codecs}"
        )
    };
    let config = Config::parse(&text(
        "  example:\n    queue: surfaces\n    reply_window_seconds: 5\n    session_env: SERVE_SESSION\n    asker_env: CHANNEL_ASKER\n    about_env: NODE\n    select: op\n    frames:\n      hello:\n        schema: example.hello@1\n        bindings:\n          - do: answer\n            response: {ok: true}\n",
    ))
    .expect("the block loads");
    let name: CodecName = "example".parse().expect("a codec name");
    let settings = &config.codecs[&name];
    assert_eq!(settings.queue, Some(queue("surfaces")));
    assert_eq!(settings.reply_window_seconds.map(u64::from), Some(5));
    let written = serde_norway::to_string(&config).expect("writes");
    assert_eq!(Config::parse(&written).expect("reads back"), config);

    for (codecs, names) in [
        ("  example: {window: 5}\n", "unknown field `window`"),
        ("  example: {reply_window_seconds: 0}\n", "nonzero"),
        ("  example: {run_env: RUN}\n", "unknown field `run_env`"),
        (
            "  example: {asker_env: \"CHANNEL-ASKER\"}\n",
            "ASCII letters, digits and `_`",
        ),
        ("  Example: {}\n", "does not start with a lowercase letter"),
        ("  example: {queue: \"no queue\"}\n", "no queue"),
    ] {
        let refused = Config::parse(&text(codecs)).expect_err(codecs);
        assert!(
            matches!(refused, ConfigError::Parse { .. }) && refused.to_string().contains(names),
            "{codecs}: {refused}"
        );
    }
    for (text, names) in [
        ("", "it is empty"),
        ("a_b", "lowercase letters, digits and `-`"),
        (&"a".repeat(65), "longer than"),
    ] {
        let refused = text.parse::<CodecName>().expect_err(text);
        assert!(refused.to_string().contains(names), "{text}: {refused}");
    }
    assert_eq!(name.to_string(), "example");
    assert_eq!(name.as_str(), "example");

    for (text, names) in [
        ("", "it is empty"),
        ("7CHANNEL", "starts with a digit"),
        ("CHANNEL-NAME", "ASCII letters, digits and `_`"),
        (&"A".repeat(129), "longer than"),
    ] {
        let refused = text.parse::<EnvName>().expect_err(text);
        assert!(refused.to_string().contains(names), "{text}: {refused}");
    }
    assert_eq!(
        "CHANNEL_NAME"
            .parse::<EnvName>()
            .expect("an env name")
            .as_str(),
        "CHANNEL_NAME"
    );

    let codec = ConfiguredCodec::new(name, settings.clone()).expect("a configured codec");
    assert_eq!(codec.name().as_str(), "example");
}

#[test]
fn binding_semantics_are_checked_at_load_with_their_full_location() {
    let config = |binding: &str| {
        Config::parse(&format!(
            "version: 1\ntransport: {{kind: memory}}\ncodecs:\n  example:\n    select: op\n    frames:\n      hello:\n        schema: example.hello@1\n        bindings:\n          - {binding}\n"
        ))
        .expect_err("binding is malformed")
        .to_string()
    };
    for (binding, reason) in [
        ("when: {field: '', equals: x}\n            do: answer\n            response: {}", "when.field"),
        ("when: {field: mood, equals: {nested: x}}\n            do: answer\n            response: {}", "when.equals"),
        ("do: raise\n            record: {}", "exactly one"),
        ("do: raise\n            record: {}\n            response: {}\n            fail: bad", "exactly one"),
        ("do: answer\n            response: '{reply.value}'", "only allowed in an ask response"),
        ("do: ask\n            record: {}\n            response: {value: {from: frame.value}}\n            unanswered: {}", "under `reply`"),
        ("do: answer\n            response: '{frame.value'", "no closing"),
        ("do: answer\n            response: '{frame}'", "no root and path"),
        ("do: answer\n            response: '{other.value}'", "not a frame or reply path"),
        ("do: answer\n            response: 'value}'", "no opening"),
        ("do: ask\n            record: {}\n            response: {value: {from: reply.value, extra: no}}\n            unanswered: {}", "key other than"),
        ("do: ask\n            record: {}\n            response: {value: {from: []}}\n            unanswered: {}", "non-empty list"),
        ("do: ask\n            record: {}\n            response: {value: {from: [reply.value, 7]}}\n            unanswered: {}", "non-string path"),
        ("do: ask\n            record: {}\n            response: {value: {from: reply.}}\n            unanswered: {}", "under `reply`"),
        ("do: ask\n            record: {}\n            response: {value: {from: reply.value, default: '{broken}'}}\n            unanswered: {}", "no root and path"),
    ] {
        let failure = config(binding);
        assert!(
            failure.contains("codecs.example.frames.hello.bindings[0]")
                && failure.contains(reason),
            "{failure}"
        );
    }

    for (body, reason) in [
        ("select: ''\n    frames: {}", "select"),
        ("select: op\n    frames: {}", "frames is empty"),
        (
            "select: op\n    frames:\n      hello: {schema: example.hello@1, bindings: []}",
            "bindings is empty",
        ),
    ] {
        let failure = Config::parse(&format!(
            "version: 1\ntransport: {{kind: memory}}\ncodecs:\n  example:\n    {body}\n"
        ))
        .expect_err("codec is malformed")
        .to_string();
        assert!(failure.contains(reason), "{failure}");
    }
}

#[test]
fn a_configured_codec_selects_validates_conditions_and_renders_typed_responses() {
    let config = Config::parse(
        "version: 1
transport: {kind: memory}
queues:
  surfaces: {}
codecs:
  example:
    select: turn.op
    frames:
      hello:
        schema: example.hello@1
        bindings:
          - when: {field: mood, equals: ready}
            do: answer
            response:
              exact: '{frame.count}'
              text: 'hello {frame.name}: {frame.count} {{ok}} {frame.missing}'
              nested: ['{frame.enabled}', 3]
          - do: refuse
            message: 'not ready: {frame.mood}'
      maybe:
        schema: example.maybe@1
        bindings:
          - when: {field: mood, equals: ready}
            do: answer
            response: {ok: true}
      raise:
        schema: example.raise@1
        bindings:
          - do: raise
            record: {kind: notice, message: 'raised {frame.count}'}
            response: {raised: true}
",
    )
    .expect("loads");
    let name: CodecName = "example".parse().expect("a codec name");
    let mut codec = ConfiguredCodec::new(name.clone(), config.codecs[&name].clone())
        .expect("a configured codec");
    let mut registry = Registry::new();
    registry
        .register_schema(
            "example.hello@1".parse::<SchemaId>().expect("a schema id"),
            json!({
                "type": "object",
                "required": ["turn", "count", "enabled"],
                "properties": {
                    "turn": {"type": "object", "required": ["op"], "properties": {"op": {"const": "hello"}}},
                    "count": {"type": "number"},
                    "enabled": {"type": "boolean"}
                }
            }),
        )
        .expect("registers");
    for id in ["example.maybe@1", "example.raise@1"] {
        registry
            .register_schema(
                id.parse::<SchemaId>().expect("a schema id"),
                json!({"type": "object"}),
            )
            .expect("registers");
    }
    let bus = config
        .resolve_with_registry(&Layouts::new(), &TransportKinds::builtin(), &registry)
        .expect("resolves");

    let mut output = Vec::new();
    bus.serve(
        &queue("surfaces"),
        &mut codec,
        &ServeOptions::default(),
        frames("{\"turn\":{\"op\":\"hello\"},\"mood\":\"ready\",\"name\":\"Ada\",\"count\":7,\"enabled\":true}\n"),
        &mut output,
    )
    .expect("served");
    assert_eq!(
        lines(&output),
        [json!({"exact": 7, "text": "hello Ada: 7 {ok} ", "nested": [true, 3]})]
    );

    let mut raised_output = Vec::new();
    bus.serve(
        &queue("surfaces"),
        &mut codec,
        &ServeOptions::default(),
        frames("{\"turn\":{\"op\":\"raise\"},\"count\":8}\n"),
        &mut raised_output,
    )
    .expect("raised and answered");
    assert_eq!(lines(&raised_output), [json!({"raised": true})]);
    assert_eq!(
        bus.queue(&queue("surfaces"))
            .expect("a queue")
            .status()
            .expect("status")
            .unread,
        1
    );

    for (frame, names) in [
        (json!(7), "not a JSON object"),
        (json!({"turn": {"op": 7}}), "absent or not a string"),
        (json!({"turn": {"op": "maybe"}}), "no binding holds"),
        (
            json!({"turn": {"op": "hello"}, "count": 1, "enabled": true}),
            "not ready",
        ),
        (
            json!({"turn": {"op": "other"}, "count": 1, "enabled": true}),
            "declared entries",
        ),
        (
            json!({"turn": {}, "count": 1, "enabled": true}),
            "absent or not a string",
        ),
        (
            json!({"turn": {"op": "hello"}, "count": "wrong", "enabled": true}),
            "does not validate",
        ),
    ] {
        let failure = bus
            .serve(
                &queue("surfaces"),
                &mut codec,
                &ServeOptions::default(),
                frames(&format!("{frame}\n")),
                &mut Vec::new(),
            )
            .expect_err("the frame is refused");
        assert!(failure.to_string().contains(names), "{failure}");
    }
}
