//! The planner channel against the directories `onepipeline` 0.28.2 wrote.
//!
//! `tests/recorded/channel/README.md` says where each directory came from and
//! which states it holds. Two properties, over every one of them:
//!
//! - **Read**: the directory reads, through the `planner-channel` layout over the
//!   local transport, to the waiting, pending and abandoned surfaces and the
//!   unread count its `queue.json` holds, the reply and command cursors its two
//!   cursor files hold, and the outcome of every command id its
//!   `command-outcomes.jsonl` holds — and reading it changes none of its files.
//! - **Re-apply**: the history the directory records, applied through this
//!   crate's queues into an empty directory — every surface queued, claimed,
//!   answered, abandoned and attended as its log's transitions say, replies and
//!   commands appended and claimed up to the recorded cursors, outcomes recorded
//!   — reproduces every file of the channel layout byte for byte.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use onemessagebus::{
    Asker, Author, Config, ConsumerName, Layout as _, Layouts, LocalTransport, Position, QueueName,
    Transport, TransportKinds,
};
use onemessagebus_agent::channel::{
    allowlist, allows, allows_completion, Channel, CommandOutcome, Op, PlannerChannel,
    QueuedCommands, QueuedReply, COMMANDS, COMMAND_OUTCOMES, REPLIES, SURFACES,
};
use serde_json::{json, Value};

/// Every recorded channel directory.
const RECORDED: &[&str] = &[
    "domain-driven-modularity-2",
    "onemessagebus-repair-2",
    "onemessagebus-repair-2-pending",
];

/// The files of the channel layout.
const LAYOUT: &[&str] = &[
    "surfaces.jsonl",
    "queue.json",
    "replies.jsonl",
    "replies-cursor.json",
    "commands.jsonl",
    "commands-cursor.json",
    "command-outcomes.jsonl",
];

fn recorded(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/recorded/channel")
        .join(name)
}

fn copy_of(name: &str, into: &Path) -> PathBuf {
    let copy = into.join(name);
    std::fs::create_dir_all(&copy).expect("a copy directory");
    for file in LAYOUT {
        let source = recorded(name).join(file);
        if source.exists() {
            std::fs::copy(&source, copy.join(file)).expect("a recorded file copies");
        }
    }
    copy
}

fn lines(path: &Path) -> Vec<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("a recorded line is JSON"))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn cursor_file(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().parse().expect("a recorded cursor is a number"))
}

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

/// How many records of `queue` lie before `position`: what a cursor file
/// holds for it.
fn records_before(
    transport: &dyn Transport,
    name: &str,
    position: Option<Position>,
) -> Option<u64> {
    let position = position?;
    let batch = transport
        .read(&queue(name), None, usize::MAX)
        .expect("the queue reads");
    Some(
        batch
            .records
            .iter()
            .filter(|stored| stored.after.token() <= position.token())
            .count() as u64,
    )
}

fn id_of(record: &Value) -> u64 {
    record["id"].as_u64().expect("a record with an id")
}

#[test]
fn every_recorded_directory_reads_to_what_its_projection_cursors_and_outcomes_hold() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    for name in RECORDED {
        let dir = copy_of(name, scratch.path());
        let transport: Arc<dyn Transport> = Arc::new(LocalTransport::open(&dir).expect("opens"));
        let channel = Channel::open(&transport).expect("the channel opens");

        let projection: Value = serde_json::from_str(
            &std::fs::read_to_string(recorded(name).join("queue.json")).expect("queue.json"),
        )
        .expect("queue.json is JSON");
        let waiting = projection["waiting"].as_array().expect("waiting").clone();
        let pending = (!projection["pending"].is_null()).then(|| projection["pending"].clone());
        let status = channel
            .surfaces()
            .raw()
            .status()
            .expect("the surfaces read");
        assert_eq!(
            status.waiting, waiting,
            "{name}: the waiting surfaces differ from queue.json"
        );
        assert_eq!(
            status.pending, pending,
            "{name}: the pending surface differs from queue.json"
        );
        let abandoned: Vec<Value> = waiting
            .iter()
            .chain(pending.iter())
            .filter(|surface| surface["abandoned"] == json!(true))
            .cloned()
            .collect();
        assert_eq!(
            status.abandoned, abandoned,
            "{name}: the abandoned surfaces differ"
        );
        let unread = waiting
            .iter()
            .filter(|surface| surface["abandoned"] != json!(true))
            .count();
        assert_eq!(
            status.unread, unread as u64,
            "{name}: the unread count differs"
        );
        assert_eq!(
            channel.surfaces().unread_count().expect("a count"),
            unread,
            "{name}: the typed unread count differs"
        );
        let typed_pending = channel.pending().expect("a read").map(|surface| surface.id);
        assert_eq!(
            typed_pending,
            pending
                .as_ref()
                .filter(|surface| surface["abandoned"] != json!(true))
                .map(id_of),
            "{name}: the typed pending surface differs"
        );

        let default = ConsumerName::default_consumer();
        for (queue_name, file) in [
            (REPLIES, "replies-cursor.json"),
            (COMMANDS, "commands-cursor.json"),
        ] {
            let position = transport
                .cursor(&queue(queue_name), &default)
                .expect("the cursor reads");
            assert_eq!(
                records_before(transport.as_ref(), queue_name, position),
                cursor_file(&recorded(name).join(file)),
                "{name}: the {queue_name} cursor differs from {file}"
            );
        }

        let recorded_outcomes = lines(&recorded(name).join("command-outcomes.jsonl"));
        for line in &recorded_outcomes {
            let expected: CommandOutcome =
                serde_json::from_value(line.clone()).expect("a recorded outcome reads");
            assert_eq!(
                channel.outcome_of(expected.id).expect("a read"),
                Some(expected.clone()),
                "{name}: the outcome of command {} differs",
                expected.id
            );
        }
        if let Some(beyond) = recorded_outcomes.iter().map(id_of).max().map(|id| id + 1) {
            assert_eq!(channel.outcome_of(beyond).expect("a read"), None);
        }

        for file in LAYOUT {
            let source = recorded(name).join(file);
            if source.exists() {
                assert_eq!(
                    std::fs::read(dir.join(file)).expect("the copy"),
                    std::fs::read(&source).expect("the recording"),
                    "{name}: reading the directory changed {file}"
                );
            } else {
                assert!(
                    !dir.join(file).exists(),
                    "{name}: reading the directory wrote {file}"
                );
            }
        }
    }
}

#[test]
fn re_applying_each_recorded_history_reproduces_every_file_byte_for_byte() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    for name in RECORDED {
        let dir = scratch.path().join(format!("{name}-replayed"));
        let transport: Arc<dyn Transport> = Arc::new(LocalTransport::open(&dir).expect("opens"));
        let channel = Channel::open(&transport).expect("the channel opens");
        let surfaces = channel.surfaces().raw();
        let default = ConsumerName::default_consumer();

        let history = lines(&recorded(name).join("surfaces.jsonl"));
        let mut at = 0;
        while at < history.len() {
            let mut record = history[at].clone();
            let event = record
                .as_object_mut()
                .and_then(|fields| fields.remove("event"))
                .and_then(|event| event.as_str().map(str::to_owned))
                .expect("a 0.28.2 surface line names its event");
            let id = id_of(&record);
            match event.as_str() {
                "queued" => {
                    let pushed = surfaces.push(record).expect("the surface is queued");
                    assert_eq!(
                        pushed.id,
                        Some(id),
                        "{name}: line {at} queued under another id"
                    );
                }
                "claimed" => {
                    let claimed = surfaces
                        .claim(&default)
                        .expect("a claim")
                        .unwrap_or_else(|| {
                            panic!("{name}: line {at} claims {id}, and nothing was claimed")
                        });
                    assert_eq!(
                        claimed.id,
                        Some(id),
                        "{name}: line {at} claimed another surface"
                    );
                }
                "answered" => {
                    let answered = surfaces.answer_pending().expect("an answer");
                    assert_eq!(
                        answered.as_ref().map(id_of),
                        Some(id),
                        "{name}: line {at} answered another surface"
                    );
                }
                "abandoned" => {
                    let marked = surfaces.abandon(&[id]).expect("abandoned");
                    assert_eq!(
                        marked.iter().map(id_of).collect::<Vec<_>>(),
                        vec![id],
                        "{name}: line {at}"
                    );
                }
                "attended" => {
                    // One attend takes back every surface of its asker at once, so the
                    // run of attended lines naming that asker is one call.
                    let asker = record["asker"]
                        .as_str()
                        .expect("an attended surface names its asker")
                        .to_owned();
                    let mut group = vec![id];
                    while let Some(next) = history.get(at + 1).filter(|next| {
                        next["event"] == json!("attended") && next["asker"] == json!(asker)
                    }) {
                        group.push(id_of(next));
                        at += 1;
                    }
                    let taken = surfaces
                        .attend(&Asker::new(&asker, "the recorded asker").expect("an asker"))
                        .expect("attended");
                    assert_eq!(
                        taken.iter().map(id_of).collect::<Vec<_>>(),
                        group,
                        "{name}: line {at}"
                    );
                }
                other => {
                    panic!("{name}: line {at} records an event this crate does not know: {other}")
                }
            }
            at += 1;
        }

        for line in lines(&recorded(name).join("replies.jsonl")) {
            let reply: QueuedReply =
                serde_json::from_value(line.clone()).expect("a recorded reply reads");
            let pushed = channel
                .replies()
                .push(&reply)
                .expect("the reply is appended");
            assert_eq!(
                pushed.id,
                Some(reply.id),
                "{name}: a reply was numbered differently"
            );
        }
        if let Some(through) = cursor_file(&recorded(name).join("replies-cursor.json")) {
            while records_before(
                transport.as_ref(),
                REPLIES,
                transport
                    .cursor(&queue(REPLIES), &default)
                    .expect("a cursor"),
            ) < Some(through)
            {
                channel.claim_reply().expect("a claim").unwrap_or_else(|| {
                    panic!("{name}: the replies ran out before cursor {through}")
                });
            }
        }
        for line in lines(&recorded(name).join("commands.jsonl")) {
            let envelope: QueuedCommands =
                serde_json::from_value(line).expect("a recorded envelope reads");
            let id = channel
                .submit(envelope.author, envelope.commands.clone())
                .expect("the envelope is appended");
            assert_eq!(
                id, envelope.id,
                "{name}: an envelope was numbered differently"
            );
        }
        if let Some(through) = cursor_file(&recorded(name).join("commands-cursor.json")) {
            let claimed = channel.claim_commands().expect("the commands are claimed");
            assert_eq!(
                claimed.len() as u64,
                through,
                "{name}: the commands claimed stop short of the cursor"
            );
        }
        for line in lines(&recorded(name).join("command-outcomes.jsonl")) {
            let outcome: CommandOutcome =
                serde_json::from_value(line).expect("a recorded outcome reads");
            channel
                .answer_commands(&outcome)
                .expect("the outcome is recorded");
        }

        for file in LAYOUT {
            let source = recorded(name).join(file);
            let written = dir.join(file);
            match (source.exists(), written.exists()) {
                (true, true) => assert!(
                    std::fs::read(&written).expect("written") == std::fs::read(&source).expect("recorded"),
                    "{name}: re-applying the history wrote a different {file}:\n--- recorded\n{}\n--- written\n{}",
                    String::from_utf8_lossy(&std::fs::read(&source).expect("recorded")),
                    String::from_utf8_lossy(&std::fs::read(&written).expect("written"))
                ),
                (true, false) => panic!("{name}: re-applying the history wrote no {file}"),
                (false, true) => panic!("{name}: re-applying the history wrote a {file} the recording has none of"),
                (false, false) => {}
            }
        }
    }
}

/// The profile declares only the planner and grants it every operation.
#[test]
fn the_planner_is_the_only_builtin_author_and_has_every_op() {
    let allowlist = allowlist();
    let planner = Author::from("planner");
    assert_eq!(allowlist.authors(), vec![planner.clone()]);
    for op in Op::ALL {
        allows(&allowlist, &planner, op.word())
            .unwrap_or_else(|refusal| panic!("the planner was refused {op:?}: {refusal}"));
    }
    allows_completion(&allowlist, &planner, Some(true)).expect("the planner's completion");
    assert!(allows(&allowlist, &planner, "context")
        .expect_err("an op the channel does not have")
        .starts_with("'context' is not an op of the planner channel"));
}

/// The layout routes an offered reply by its halves, as 0.28.2 does: its
/// commands to the command queue, its verdict to the reply queue, and a
/// commands-only envelope to the command queue alone — each checked against
/// the author first.
#[test]
fn a_reply_is_routed_by_its_halves_and_checked_against_its_author() {
    let layout = PlannerChannel;
    let mut grants = layout.allowlist();
    let sentinel = Author::from("sentinel");
    grants.grant(
        sentinel.clone(),
        onemessagebus::OpWord("finding".to_owned()),
    );
    grants.refuse(
        sentinel.clone(),
        &onemessagebus::OpWord("attest".to_owned()),
        "only a person may attest",
    );
    grants.refuse(
        sentinel.clone(),
        &onemessagebus::OpWord("complete".to_owned()),
        "the planner decides completion",
    );
    let replies = queue(REPLIES);
    let queues = |routed: &[(QueueName, Value)]| -> Vec<String> {
        routed.iter().map(|(queue, _)| queue.to_string()).collect()
    };

    let both = layout
        .prepare(
            &replies,
            json!({"version": 2, "completion": false, "message": "go on", "commands": [{"op": "retry", "id": "a", "node": {}}]}),
            &grants,
        )
        .expect("the planner's reply is routed");
    assert_eq!(queues(&both), vec![COMMANDS, REPLIES]);
    assert_eq!(
        both[0].1["commands"],
        json!([{"op": "retry", "id": "a", "node": {}}])
    );
    assert_eq!(
        both[1].1["reply"]["version"],
        json!(3),
        "a version-2 envelope was not read at 3"
    );

    let commands_only = layout
        .prepare(
            &replies,
            json!({"version": 3, "author": "sentinel", "commands": [{"op": "finding", "message": "look"}]}),
            &grants,
        )
        .expect("the configured author's finding is routed");
    assert_eq!(
        queues(&commands_only),
        vec![COMMANDS],
        "a commands-only reply reached the reply queue"
    );
    assert_eq!(commands_only[0].1["author"], json!("sentinel"));

    let refused = layout
        .prepare(
            &replies,
            json!({"version": 3, "author": "sentinel", "commands": [{"op": "attest", "ref": "x"}]}),
            &grants,
        )
        .expect_err("the configured author may not attest");
    assert!(
        refused.starts_with("'attest' is not an op the sentinel may issue"),
        "{refused}"
    );
    let completion = layout
        .prepare(
            &replies,
            json!({"author": "sentinel", "completion": true}),
            &grants,
        )
        .expect_err("the configured author may not declare the run complete");
    assert!(
        completion.starts_with("declaring the run complete is not something the sentinel may do"),
        "{completion}"
    );
    let old = layout
        .prepare(
            &replies,
            json!({"version": 1, "commands": [{"op": "cancel", "id": "a"}]}),
            &grants,
        )
        .expect_err("an edit envelope at a version this build does not read");
    assert_eq!(old, "an edit envelope requires version 3");
    let unknown = layout
        .prepare(
            &replies,
            json!({"message": "hi", "context": "retired"}),
            &grants,
        )
        .expect_err("an unknown key");
    assert!(unknown.contains("context"), "{unknown}");

    let verdict_only = layout
        .prepare(&replies, json!({"message": "carry on"}), &grants)
        .expect("a verdict is routed");
    assert_eq!(queues(&verdict_only), vec![REPLIES]);
    assert!(
        verdict_only[0].1["at"].as_u64().is_some_and(|at| at > 0),
        "the reply was not stamped"
    );

    let command_refused = layout
        .prepare(
            &queue(COMMANDS),
            json!({"id": 0, "author": "sentinel", "commands": [{"op": "settle", "id": "a"}]}),
            &grants,
        )
        .expect_err("the configured author may not settle");
    assert!(
        command_refused.starts_with("'settle' is not an op the sentinel may issue"),
        "{command_refused}"
    );
    let surface = layout
        .prepare(&queue(SURFACES), json!({"id": 0, "kind": "finding", "message": "m", "source": "proposal", "blocking": false}), &grants)
        .expect("a surface is stamped");
    assert!(surface[0].1["queued_at"].is_u64());
    let outcome = layout
        .prepare(
            &queue(COMMAND_OUTCOMES),
            json!({"id": 3, "applied": true}),
            &grants,
        )
        .expect("an outcome passes");
    assert_eq!(outcome[0].1, json!({"id": 3, "applied": true}));
}

/// The typed channel answers a pending surface only with a verdict, keeps its
/// command and outcome queues where their readers find them, and names its ops
/// by their wire words.
#[test]
fn the_typed_channel_answers_only_a_verdict_and_names_its_ops_by_word() {
    use onemessagebus::MemoryTransport;
    use onemessagebus_agent::channel::{ReplyEnvelope, Surface};
    let transport: Arc<dyn Transport> = Arc::new(MemoryTransport::new());
    let channel = Channel::open(&transport).expect("opens");
    channel
        .push(&Surface {
            id: 0,
            kind: "planner-question".to_owned(),
            message: "is the base right?".to_owned(),
            source: "proposal".to_owned(),
            blocking: true,
            queued_at: 1,
            workstream: Some("plan".to_owned()),
            abandoned: false,
            asker: None,
            correlation: None,
        })
        .expect("queued");
    channel.claim().expect("a claim").expect("claimed");
    let edits = ReplyEnvelope {
        version: Some(3),
        commands: vec![json!({"op": "cancel", "id": "plan"})
            .as_object()
            .expect("an object")
            .clone()],
        ..ReplyEnvelope::default()
    };
    assert!(edits.carries_edits_without_a_verdict());
    assert_eq!(channel.answer_if_verdict(&edits, 2).expect("routed"), None);
    assert!(
        channel.pending().expect("a read").is_some(),
        "a commands-only reply answered the question"
    );
    let verdict = ReplyEnvelope {
        message: Some("yes".to_owned()),
        ..edits.clone()
    };
    assert!(!verdict.carries_edits_without_a_verdict());
    assert_eq!(
        channel.answer_if_verdict(&verdict, 3).expect("answered"),
        Some(0)
    );
    assert_eq!(channel.pending().expect("a read"), None);

    channel
        .submit(Author::from("sentinel"), edits.commands.clone())
        .expect("submitted");
    assert_eq!(channel.commands().unread_count().expect("a count"), 1);
    channel
        .answer_commands(&CommandOutcome {
            id: 0,
            applied: true,
            reason: None,
            results: Vec::new(),
        })
        .expect("answered");
    assert_eq!(channel.outcomes().unread_count().expect("a count"), 1);

    for op in Op::ALL {
        assert_eq!(Op::of_word(op.word()), Some(op));
    }
    assert_eq!(Op::of_word("context"), None);
}

/// A commands-only reply by position, arriving after another reply answered the
/// surface, is command traffic rather than a lost answer: it reaches the command
/// path alone, answers nothing, and is not refused.
#[test]
fn a_commands_only_reply_by_position_after_the_surface_was_answered_reaches_the_command_path_alone()
{
    let bus = Config::parse("version: 1\ntransport: {kind: memory}\nprofile: planner-channel\n")
        .expect("loads")
        .resolve(
            &Layouts::new().with(Arc::new(PlannerChannel)),
            &TransportKinds::builtin(),
        )
        .expect("resolves");
    let surfaces = queue(SURFACES);
    bus.send(
        &surfaces,
        json!({"kind": "planner-question", "message": "is the base right?", "source": "proposal", "blocking": true}),
    )
    .expect("raised");
    let claimed = bus
        .queue(&surfaces)
        .expect("a queue")
        .claim(&ConsumerName::default_consumer())
        .expect("a claim")
        .expect("claimed");
    let first = bus
        .reply_at(
            &surfaces,
            &claimed.position,
            json!({"message": "yes, carry on"}),
        )
        .expect("answered");
    assert!(first.answered);

    let late = bus
        .reply_at(
            &surfaces,
            &claimed.position,
            json!({"version": 3, "commands": [{"op": "cancel", "id": "build"}]}),
        )
        .expect("a commands-only reply is not refused as a lost answer");
    assert!(!late.answered, "a commands-only reply answered the surface");
    assert_eq!(late.question.id, claimed.id);
    assert_eq!(
        late.sent
            .iter()
            .map(|(queue, _)| queue.to_string())
            .collect::<Vec<_>>(),
        vec![COMMANDS]
    );
    let records = |name: &str| {
        bus.queue(&queue(name))
            .expect("a queue")
            .status()
            .expect("a status")
            .records
    };
    assert_eq!(
        records(REPLIES),
        1,
        "the late reply reached the reply queue"
    );
    assert_eq!(records(COMMANDS), 1);
}

/// A reply offered already framed is checked against its author and kept whole,
/// and a surface that is not an object is left for its schema to refuse.
#[test]
fn a_framed_reply_is_checked_and_kept_whole_and_a_surface_that_is_not_an_object_passes_to_its_schema(
) {
    let layout = PlannerChannel;
    let grants = layout.allowlist();
    let framed = json!({"id": 0, "reply": {"message": "carry on"}, "at": 5});
    assert_eq!(
        layout
            .prepare(&queue(REPLIES), framed.clone(), &grants)
            .expect("kept"),
        vec![(queue(REPLIES), framed)]
    );
    let refused = layout
        .prepare(
            &queue(REPLIES),
            json!({"id": 0, "reply": {"author": "sentinel", "completion": true}, "at": 5}),
            &grants,
        )
        .expect_err("a framed completion from an undeclared author");
    assert!(
        refused.starts_with("the envelope's author `sentinel` is not declared"),
        "{refused}"
    );
    for shapeless in [json!([3]), json!({"id": 0, "reply": [3], "at": 5})] {
        assert_eq!(
            layout
                .prepare(&queue(REPLIES), shapeless.clone(), &grants)
                .expect_err("an array is no envelope"),
            "the reply is malformed: a reply envelope is a JSON object",
            "{shapeless}"
        );
    }
    assert_eq!(
        layout
            .prepare(&queue(SURFACES), json!("text"), &grants)
            .expect("passed on"),
        vec![(queue(SURFACES), json!("text"))]
    );
}
