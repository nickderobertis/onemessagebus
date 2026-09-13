//! The onejudge codec: its frames held to `onejudge`'s protocol v6, its reading
//! of a lost turn held to a transcript a real harness wrote, and the codec
//! itself served over a bus in-process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use onemessagebus::sdk_schema::{self, Lang};
use onemessagebus::{
    Asker, Bus, Config, Layouts, QueueName, ServeError, ServeOptions, Served, TransportKinds,
};
use onemessagebus_agent::channel::PlannerChannel;
use onemessagebus_agent::codec::onejudge::{
    self, cause_of, clipped, identity_in, is_safe_run, live_edit, liveness, lost_turn_error,
    run_in_task, transcript_frames, ConversationMessage, Frame, JudgeResponse, JudgeValue,
    Onejudge, Role, SupervisorResponse, Turn, TurnError,
};
use onemessagebus_agent::registry;
use serde_json::{json, Value};

const LOST_TURN: &str = include_str!("recorded/onejudge/lost-turn.jsonl");

const CONTRACT: &str = include_str!("../../../docs/contract.md");

/// The fenced block `docs/contract.md` tags `<!-- fixture: name -->`.
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
fn the_documented_frames_are_the_ones_the_profile_registers_and_serves() {
    let documented: Value =
        serde_json::from_str(&fixture("onejudge-frames")).expect("the fixture is JSON");
    assert_eq!(documented["protocol"], json!(onejudge::PROTOCOL_VERSION));
    assert_eq!(
        documented["transcribed from"],
        json!(onejudge::TRANSCRIBED_FROM)
    );
    let registered: Vec<String> = onejudge::schemas()
        .iter()
        .map(|(id, _)| id.to_string())
        .collect();
    assert_eq!(documented["frames"], json!(registered));
    assert_eq!(documented["served"], json!(onejudge::op::SERVED));
    assert_eq!(onemessagebus_agent::codec::CODECS, [onejudge::CODEC]);
}

/// The `codexHome` the recorded lost turn names.
const RECORDED_HOME: &str = "/tmp/lt.c6Y0/codex-home";

fn said(role: Role, content: &str) -> ConversationMessage {
    ConversationMessage {
        role,
        content: content.to_owned(),
        events: Vec::new(),
    }
}

fn messages() -> Value {
    json!([{"role": "user", "content": "hi"}])
}

/// `docs/protocol.md`'s request examples, with their elided transcripts filled.
fn protocol_examples() -> Vec<Value> {
    vec![
        json!({"op": "respond", "skill": {"name": "greeter", "path": "/skills/greeter", "instructions": "..."}, "messages": [{"role": "user", "content": "hi"}], "session": "run-42-skill"}),
        json!({"op": "user", "persona": "A hurried shopper.", "messages": messages(), "session": "run-42-user"}),
        json!({"op": "supervisor", "task": "fix it", "persona": "A strict reviewer.", "done_when": "tests pass", "worktree": "/repo", "history_name": "run-42-skill", "messages": messages(), "session": "run-42-user"}),
        json!({"op": "supervisor", "task": "fix it", "persona": "A strict reviewer.", "worktree": "/repo", "history_name": "run-42-skill", "notes": [{"note": {"addressee": "worker", "text": "the reviewer asked for a smaller diff", "criterion": "the diff touches only the migration"}, "delivered_to": "worker"}], "messages": messages()}),
        json!({"op": "assess", "prompt": "Identify useful follow-up work.", "messages": messages()}),
        json!({"op": "judge", "kind": "boolean", "criterion": "the reply was polite", "messages": [{"role": "assistant", "content": "ok", "events": [{"kind": "tool_call", "name": "bash", "input": {"command": "ls"}, "index": 0}]}], "evidence": {"worktree": "/repo", "history_files": ["/state/history/agent.jsonl"]}}),
    ]
}

#[test]
fn the_protocol_examples_read_as_frames_round_trip_and_satisfy_their_registered_schemas() {
    let registry = registry();
    for example in protocol_examples() {
        let frame: Frame = serde_json::from_value(example.clone())
            .unwrap_or_else(|failure| panic!("{example}: {failure}"));
        assert_eq!(
            serde_json::to_value(&frame).expect("serializes"),
            example,
            "the frame does not round-trip as onejudge writes it"
        );
        let word = example["op"].as_str().expect("an op");
        let id = format!("agent.onejudge-frame.{word}@6")
            .parse()
            .expect("an id");
        registry
            .check(&id, &example)
            .unwrap_or_else(|failure| panic!("{id}: {failure}"));
    }
    let mislabelled = json!({"op": "judge", "prompt": "x", "messages": messages()});
    let refused = registry
        .check(
            &"agent.onejudge-frame.assess@6".parse().expect("an id"),
            &mislabelled,
        )
        .expect_err("a frame of another op");
    assert!(refused.to_string().contains("/op"), "{refused}");
}

#[test]
fn the_frames_are_registered_as_the_five_ops_at_protocol_six_and_render_in_every_language_built() {
    let ids: Vec<String> = onejudge::schemas()
        .iter()
        .map(|(id, _)| id.to_string())
        .collect();
    assert_eq!(
        ids,
        onejudge::op::ALL.map(|word| format!("agent.onejudge-frame.{word}@6"))
    );
    let registry = registry();
    for (id, schema) in onejudge::schemas() {
        assert_eq!(
            (id.namespace(), id.version()),
            ("agent", onejudge::PROTOCOL_VERSION)
        );
        let document = schema.to_value();
        assert_eq!(registry.schema(&id), Some(&document));
        let word = id.name().trim_start_matches("onejudge-frame.");
        assert_eq!(document["properties"]["op"]["const"], json!(word));
        let rust = sdk_schema::generate(Lang::Rust, &id, &document)
            .unwrap_or_else(|failure| panic!("{id} does not render as Rust: {failure}"));
        assert!(rust.contains("pub op: String"), "{rust}");
        sdk_schema::generate(Lang::Json, &id, &document).expect("renders as JSON");
    }
}

#[test]
fn the_recorded_lost_turn_is_read_as_lost_naming_its_cause_and_identity() {
    let frames = transcript_frames(LOST_TURN).expect("the recorded turn is a transcript");
    assert_eq!(frames.len(), 13);
    let error = lost_turn_error(&frames).expect("the turn was lost");
    assert_eq!(error.codex_error_info.as_deref(), Some("other"));
    assert_eq!(cause_of(&error), "other");
    assert_eq!(identity_in(&frames, None), onejudge::CODEX_IDENTITY);
    assert_eq!(
        identity_in(&frames, Some(&format!("{RECORDED_HOME}/"))),
        onejudge::CODEX_ALTERNATE_IDENTITY
    );
    assert_eq!(
        identity_in(&frames, Some("/home/elsewhere/.codex-alt")),
        onejudge::CODEX_IDENTITY
    );

    let conversation = [
        said(Role::User, "Report what has drifted from the plan."),
        said(Role::Assistant, LOST_TURN),
    ];
    assert_eq!(
        liveness(&conversation, None),
        Turn::Lost {
            cause: "other".to_owned(),
            identity: onejudge::CODEX_IDENTITY,
            transcript_chars: Some(LOST_TURN.chars().count()),
        }
    );

    // The same real transcript cut back to the frames that prove nothing: all 26
    // oversized surfaces the host measured were this shape, and each is a turn
    // taken, not one lost.
    let unproven: String = LOST_TURN
        .lines()
        .filter(|line| {
            let frame: Value = serde_json::from_str(line).expect("a frame");
            frame["method"] != json!("error")
                && frame["params"]["turn"]["status"] != json!("failed")
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        liveness(&[said(Role::Assistant, &unproven)], None),
        Turn::Taken
    );
}

#[test]
fn liveness_is_whether_the_member_said_anything_and_nothing_else() {
    assert_eq!(
        liveness(
            &[said(Role::Assistant, "nothing drifted; no finding to file")],
            None
        ),
        Turn::Taken
    );
    let silent = Turn::Lost {
        cause: onejudge::NO_ASSISTANT_CONTENT.to_owned(),
        identity: onejudge::UNIDENTIFIED_HARNESS,
        transcript_chars: None,
    };
    assert_eq!(liveness(&[], None), silent);
    assert_eq!(
        liveness(
            &[said(Role::User, "watch"), said(Role::Assistant, "  \n ")],
            None
        ),
        silent
    );
    let failed_quietly =
        "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"status\":\"failed\"}}}";
    assert!(matches!(
        liveness(&[said(Role::Assistant, failed_quietly)], None),
        Turn::Lost { cause, identity: onejudge::UNIDENTIFIED_HARNESS, .. } if cause == onejudge::NO_CAUSE_RECORDED
    ));
    let error_frame = "{\"method\":\"error\",\"params\":{\"error\":{\"code\":\"rate_limited\"}}}";
    assert!(matches!(
        liveness(&[said(Role::Assistant, error_frame)], None),
        Turn::Lost { cause, .. } if cause == "rate_limited"
    ));
    assert_eq!(transcript_frames("{} and prose"), None);
    assert_eq!(transcript_frames("[1, 2]"), None);
    assert_eq!(transcript_frames("\n\n"), None);
}

#[test]
fn a_cause_is_one_bounded_line_named_by_its_classification_first() {
    assert_eq!(
        clipped("usage\nlimit\t\u{1b}[31m exceeded  "),
        "usage limit [31m exceeded"
    );
    let long = "x".repeat(200);
    let bounded = clipped(&long);
    assert_eq!(bounded.chars().count(), onejudge::CAUSE_LIMIT);
    assert!(bounded.ends_with('\u{2026}'));
    let error = TurnError {
        codex_error_info: Some(" \n ".to_owned()),
        code: Some("quota".to_owned()),
        message: Some("a paragraph about buying credits".to_owned()),
    };
    assert_eq!(cause_of(&error), "quota");
    assert_eq!(cause_of(&TurnError::default()), onejudge::NO_CAUSE_RECORDED);
}

#[test]
fn the_run_is_read_from_the_tasks_opening_line_and_held_to_one_word() {
    assert_eq!(
        run_in_task("onepipeline run `scheduler-research`.\nWatch the build."),
        Some("scheduler-research")
    );
    assert_eq!(
        run_in_task("Watch the build.\nonepipeline run `late`."),
        None
    );
    assert_eq!(run_in_task("onepipeline run ``."), None);
    assert_eq!(run_in_task("onepipeline run `unclosed"), None);
    for run in ["r-7", "serve_e2e", "a.b"] {
        assert!(is_safe_run(run), "{run}");
    }
    for run in ["", "-flag", ".hidden", "a/b", "a b"] {
        assert!(!is_safe_run(run), "{run}");
    }
}

#[test]
fn a_live_edit_is_named_as_its_planner_would_and_carries_its_prose() {
    let edit = live_edit(&json!({
        "version": 3,
        "message": " stop that ",
        "commands": [{"op": "cancel", "id": "build"}, {"op": "add"}, {"id": "x"}, {}]
    }))
    .expect("a live edit");
    assert_eq!(
        edit.named,
        "cancel build, add, an unnamed edit x, an unnamed edit"
    );
    assert_eq!(edit.said, "stop that");
    assert_eq!(
        live_edit(&json!({"completion": false, "commands": [{"op": "retry"}]})),
        None
    );
    assert_eq!(live_edit(&json!({"commands": []})), None);
    assert_eq!(live_edit(&json!("commands")), None);
}

#[test]
fn a_supervisor_ruling_has_exactly_two_shapes_and_a_score_writes_its_prose_as_reason() {
    let continuing = SupervisorResponse::Continue {
        message: "keep watching".to_owned(),
        reason: "the turn was taken".to_owned(),
    };
    let written = serde_json::to_value(&continuing).expect("serializes");
    assert_eq!(
        written,
        json!({"completion": false, "message": "keep watching", "reason": "the turn was taken"})
    );
    assert_eq!(
        serde_json::from_value::<SupervisorResponse>(written).expect("reads"),
        continuing
    );
    let completed = SupervisorResponse::Completed {
        reason: "done".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(&completed).expect("serializes"),
        json!({"completion": true, "reason": "done"})
    );
    for malformed in [
        json!({"completion": true, "reason": "done", "message": "x"}),
        json!({"completion": true, "reason": " "}),
        json!({"completion": false, "reason": "no message"}),
    ] {
        assert!(
            serde_json::from_value::<SupervisorResponse>(malformed.clone()).is_err(),
            "{malformed}"
        );
    }
    let score = serde_json::to_value(JudgeResponse {
        value: JudgeValue::Bool(true),
        reason: Some("the watch met its bar".to_owned()),
        usage: None,
    })
    .expect("serializes");
    assert_eq!(
        score,
        json!({"value": true, "reason": "the watch met its bar"})
    );
    assert!(score.get("rationale").is_none());
}

fn planner_bus() -> Bus {
    Config::parse("version: 1\ntransport: {kind: memory}\nprofile: planner-channel\n")
        .expect("loads")
        .resolve(
            &Layouts::new().with(Arc::new(PlannerChannel)),
            &TransportKinds::builtin(),
        )
        .expect("resolves")
}

fn surfaces() -> QueueName {
    "surfaces".parse().expect("a queue")
}

fn serve(
    bus: &Bus,
    codec: &mut Onejudge,
    options: &ServeOptions,
    frame: &Value,
) -> (Result<Served, ServeError>, Vec<Value>) {
    let mut output = Vec::new();
    let served = bus.serve(
        &surfaces(),
        codec,
        options,
        Box::new(std::io::Cursor::new(format!("{frame}\n"))),
        &mut output,
    );
    let lines = String::from_utf8(output)
        .expect("UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a response"))
        .collect();
    (served, lines)
}

fn supervisor_frame(content: &str) -> Value {
    json!({
        "op": "supervisor", "task": "onepipeline run `r-7`.\nWatch the build.", "persona": "A monitor.",
        "worktree": "/repo", "history_name": "r-7-skill",
        "messages": [{"role": "user", "content": "watch"}, {"role": "assistant", "content": content}]
    })
}

#[test]
fn served_over_a_bus_a_turn_taken_raises_nothing_and_a_lost_one_raises_one_surface() {
    let bus = planner_bus();
    let options = ServeOptions {
        asker: Some(Asker::new("monitor-1", "the test").expect("an asker")),
        about: Some("build".parse().expect("an address")),
        ..ServeOptions::default()
    };
    let mut codec = Onejudge::new();
    let (served, lines) = serve(&bus, &mut codec, &options, &supervisor_frame("all quiet"));
    assert_eq!(
        served.expect("served"),
        Served::StreamEnded { abandoned: 0 }
    );
    assert_eq!(
        lines,
        vec![
            json!({"completion": false, "message": onejudge::TURN_TAKEN_ACKNOWLEDGED, "reason": onejudge::TURN_TAKEN_REASON})
        ]
    );
    let queue = bus.queue(&surfaces()).expect("a queue");
    assert_eq!(queue.status().expect("a status").records, 0);

    let (served, lines) = serve(&bus, &mut codec, &options, &supervisor_frame(LOST_TURN));
    let failed = served.expect_err("a lost turn fails the member");
    assert!(
        matches!(&failed, ServeError::Failed(why) if why.contains("other on codex")),
        "{failed}"
    );
    assert!(lines.is_empty(), "a lost turn was answered: {lines:?}");
    let raised = queue.waiting().expect("a read");
    assert_eq!(raised.len(), 1);
    assert_eq!(
        raised[0]["kind"],
        json!(onejudge::SURFACE_KIND_OF_A_FAILED_TURN)
    );
    assert_eq!(raised[0]["blocking"], json!(false));
    assert_eq!(raised[0]["asker"], json!("monitor-1"));
    assert_eq!(raised[0]["workstream"], json!("build"));
    let message = raised[0]["message"].as_str().expect("a message");
    assert!(message.contains("Run r-7"), "{message}");
    assert!(
        !message.contains("turn/completed"),
        "the transcript was repeated: {message}"
    );
}

#[test]
fn what_a_session_is_about_is_held_to_the_consumers_check_and_a_run_to_its_sources() {
    let bus = planner_bus();
    let options = ServeOptions {
        about: Some("deploy".parse().expect("an address")),
        ..ServeOptions::default()
    };
    let mut checked = Onejudge::new().with_about_check(Box::new(|run, about| {
        if about.as_str() == "build" {
            Ok(())
        } else {
            Err(format!("run {run}'s graph has no node {about}"))
        }
    }));
    let (served, _) = serve(&bus, &mut checked, &options, &supervisor_frame("all quiet"));
    let refused = served.expect_err("refused");
    assert!(
        matches!(&refused, ServeError::Refused(why) if why.contains("has no node deploy")),
        "{refused}"
    );

    let judge = json!({"op": "judge", "kind": "boolean", "criterion": "the watch is kept", "messages": messages()});
    let (served, _) = serve(&bus, &mut Onejudge::new(), &ServeOptions::default(), &judge);
    let refused = served.expect_err("no run");
    assert!(
        matches!(&refused, ServeError::Refused(why) if why.contains(onejudge::RUN_ENV)),
        "{refused}"
    );
    let mut unsafe_run = Onejudge::new().with_run_env(
        "RUN".parse().expect("an environment name"),
        Some("../elsewhere".to_owned()),
    );
    let (served, _) = serve(&bus, &mut unsafe_run, &ServeOptions::default(), &judge);
    assert!(
        matches!(served, Err(ServeError::Refused(why)) if why.contains("not a run id")),
        "an unsafe run was served"
    );
    assert!(format!("{unsafe_run:?}").contains("RUN"));
}

#[test]
fn a_reply_that_is_neither_a_ruling_nor_a_live_edit_fails_the_member_and_an_unexplained_ruling_is_scored(
) {
    for (envelope, expect) in [
        (
            json!({"version": 3, "completion": true}),
            Some(onejudge::SCORE_UNEXPLAINED),
        ),
        (json!({"version": 3, "message": "hmm"}), None),
    ] {
        let bus = planner_bus();
        let injector = bus.clone();
        let injecting = std::thread::spawn(move || {
            let started = Instant::now();
            let queue = injector.queue(&surfaces()).expect("a queue");
            loop {
                if let Some(question) = queue.waiting().expect("a read").first() {
                    let record = json!({"id": 0, "reply": envelope, "at": 1, "correlation": question["correlation"]});
                    injector
                        .transport()
                        .append(
                            &"replies".parse().expect("a queue"),
                            record.to_string().as_bytes(),
                        )
                        .expect("appended");
                    return;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "nothing was asked"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let mut codec = Onejudge::new().with_run_env(
            "RUN".parse().expect("an environment name"),
            Some("r-7".to_owned()),
        );
        let options = ServeOptions {
            reply_window: Duration::from_secs(20),
            ..ServeOptions::default()
        };
        let judge = json!({"op": "judge", "kind": "boolean", "criterion": "the watch is kept", "messages": messages()});
        let (served, lines) = serve(&bus, &mut codec, &options, &judge);
        injecting.join().expect("the injector finishes");
        match expect {
            Some(reason) => {
                served.expect("served");
                assert_eq!(lines, vec![json!({"value": true, "reason": reason})]);
            }
            None => {
                let failed = served.expect_err("not a ruling");
                assert!(
                    matches!(&failed, ServeError::Failed(why) if why.contains("not a ruling")),
                    "{failed}"
                );
            }
        }
    }
}
