//! The queue verbs, through the built binary, over the local transport.
//!
//! Every journey spawns `onemessagebus` against a channel directory of its own
//! under the `planner-channel` layout — `--transport-dir`, or a configuration
//! file — and reads back both what the binary printed and the files it left,
//! because those files are what `onepipeline` reads.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use onemessagebus::{Asker, LocalTransport, Transport};
use onemessagebus_agent::channel::Channel;
use serde_json::{json, Value};

use crate::support::{onemessagebus, run_in, Run};

/// A scratch directory holding one channel directory.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn channel(&self) -> PathBuf {
        self.root().join("channel")
    }

    /// The binary, from the scratch root, over the channel directory.
    fn bus(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let channel = self.channel();
        let mut argv: Vec<&str> = args.to_vec();
        let dir = channel.to_str().expect("a UTF-8 path");
        argv.extend(["--transport-dir", dir]);
        run_in(self.root(), &argv, stdin, &[])
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.channel().join(name)).unwrap_or_default()
    }

    fn lines(&self, name: &str) -> Vec<Value> {
        self.file(name)
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }

    fn status(&self, queue: &str) -> Value {
        let run = self.bus(&["status", queue], None);
        assert_eq!(run.code, 0, "{}", run.stderr);
        let statuses: Vec<Value> = serde_json::from_str(&run.stdout).expect("a JSON list");
        assert_eq!(statuses.len(), 1, "{}", run.stdout);
        statuses[0].clone()
    }
}

fn surface(kind: &str, message: &str, source: &str, blocking: bool) -> String {
    json!({"kind": kind, "message": message, "source": source, "blocking": blocking}).to_string()
}

fn one_line(run: &Run) -> Value {
    assert_eq!(run.code, 0, "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines.len(), 1, "{}", run.stdout);
    lines[0].clone()
}

#[test]
fn send_appends_a_record_from_stdin_or_file_and_prints_where_it_landed() {
    let scratch = Scratch::new();
    let first = one_line(&scratch.bus(
        &["send", "surfaces"],
        Some(&surface("finding", "the base moved", "proposal", false)),
    ));
    assert_eq!(first["queue"], json!("surfaces"));
    assert_eq!(first["id"], json!(0));
    let logged = scratch.lines("surfaces.jsonl");
    assert_eq!(logged[0]["event"], json!("queued"));
    assert_eq!(logged[0]["message"], json!("the base moved"));
    assert!(
        logged[0]["queued_at"].as_u64().is_some_and(|at| at > 0),
        "the surface was not stamped"
    );
    assert_eq!(
        first["position"],
        json!(scratch.file("surfaces.jsonl").len()),
        "the position is not the byte offset after the record"
    );
    let projection: Value = serde_json::from_str(&scratch.file("queue.json")).expect("queue.json");
    assert_eq!(projection["next_id"], json!(1));
    assert!(projection["seal"].is_string(), "{projection}");

    let record = scratch.root().join("record.json");
    std::fs::write(&record, surface("finding", "on file", "proposal", false))
        .expect("a record file");
    let on_file = one_line(&scratch.bus(
        &[
            "send",
            "surfaces",
            "--file",
            record.to_str().expect("a path"),
        ],
        None,
    ));
    assert_eq!(on_file["id"], json!(1));

    let as_argument = scratch.bus(
        &[
            "send",
            "surfaces",
            &surface("finding", "an argument", "proposal", false),
        ],
        None,
    );
    assert_eq!(
        as_argument.code, 2,
        "a record passed as an argument was accepted: {}",
        as_argument.stdout
    );
    assert!(
        as_argument.stderr.contains("unexpected argument"),
        "{}",
        as_argument.stderr
    );

    let unknown = scratch.bus(&["send", "findings"], Some("{}"));
    assert_eq!(unknown.code, 2);
    assert_eq!(
        unknown.stderr.trim(),
        "onemessagebus: `findings` is not a queue this configuration declares; it declares: command-outcomes, commands, replies, surfaces"
    );
    let incomplete = scratch.bus(&["send", "surfaces"], Some(r#"{"kind": "finding"}"#));
    assert_eq!(incomplete.code, 1, "{}", incomplete.stderr);
    assert!(
        incomplete.stderr.contains("surfaces") && incomplete.stderr.contains("message"),
        "{}",
        incomplete.stderr
    );
    let not_json = scratch.bus(&["send", "surfaces"], Some("not json"));
    assert_eq!(not_json.code, 2, "{}", not_json.stderr);
    assert_eq!(
        scratch.lines("surfaces.jsonl").len(),
        2,
        "a refused record was appended"
    );
}

#[test]
fn a_reply_sent_to_the_reply_queue_is_routed_by_its_halves_and_checked_against_its_author() {
    let scratch = Scratch::new();
    let both = scratch.bus(
        &["send", "replies"],
        Some(r#"{"version":2,"completion":false,"message":"go on","commands":[{"op":"retry","id":"build","node":{"id":"build-2"}}]}"#),
    );
    assert_eq!(both.code, 0, "{}", both.stderr);
    let landed: Vec<Value> = both
        .lines()
        .iter()
        .map(|line| line["queue"].clone())
        .collect();
    assert_eq!(landed, vec![json!("commands"), json!("replies")]);
    assert_eq!(
        scratch.lines("replies.jsonl")[0]["reply"]["version"],
        json!(3)
    );
    assert_eq!(
        scratch.lines("commands.jsonl")[0]["author"],
        json!("planner")
    );

    let commands_only = scratch.bus(
        &["send", "replies"],
        Some(r#"{"version":3,"author":"monitor","commands":[{"op":"finding","message":"the gate is red"}]}"#),
    );
    let landed: Vec<Value> = commands_only
        .lines()
        .iter()
        .map(|line| line["queue"].clone())
        .collect();
    assert_eq!(
        landed,
        vec![json!("commands")],
        "a commands-only reply reached the reply queue"
    );
    assert_eq!(scratch.lines("replies.jsonl").len(), 1);

    let refused = scratch.bus(
        &["send", "commands"],
        Some(
            r#"{"author":"monitor","commands":[{"op":"drop","id":"build","dependents":"detach"}]}"#,
        ),
    );
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert_eq!(
        refused.stderr.trim(),
        "onemessagebus: commands: 'drop' is not an op the monitor may issue: removing work from the graph is a decomposition decision the planner owns. Surface it to the planner instead"
    );
    let completion = scratch.bus(
        &["send", "replies"],
        Some(r#"{"author":"monitor","completion":true}"#),
    );
    assert_eq!(completion.code, 1);
    assert_eq!(
        completion.stderr.trim(),
        "onemessagebus: replies: declaring the run complete is not something the monitor may do: whether the run is finished is the planner's verdict, not an observation. Surface it to the planner instead"
    );
    assert_eq!(
        scratch.lines("commands.jsonl").len(),
        2,
        "a refused envelope was appended"
    );
}

#[test]
fn next_claims_blocking_first_holds_it_pending_and_exits_one_when_nothing_is_left() {
    let scratch = Scratch::new();
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("finding", "narration", "proposal", false)),
    );
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("planner-question", "a question", "proposal", true)),
    );

    let question = one_line(&scratch.bus(&["next", "surfaces"], None));
    assert_eq!(
        question["record"]["message"],
        json!("a question"),
        "a blocking surface was not claimed first"
    );
    assert_eq!(question["id"], json!(1));
    let status = scratch.status("surfaces");
    assert_eq!(status["pending"]["id"], json!(1));
    assert_eq!(status["pending_position"], question["position"]);
    assert_eq!(status["waiting"].as_array().map(Vec::len), Some(1));
    assert_eq!(status["unread"], json!(1));

    let narration = scratch.bus(&["next", "surfaces", "--format", "text"], None);
    assert_eq!(narration.code, 0, "{}", narration.stderr);
    assert!(
        narration.stdout.starts_with("surfaces "),
        "{}",
        narration.stdout
    );
    assert!(
        narration.stdout.contains("\"narration\""),
        "{}",
        narration.stdout
    );
    assert_eq!(
        scratch.status("surfaces")["pending"]["id"],
        json!(1),
        "reading narration answered the pending question"
    );

    // One pending at a time: a second blocking claim takes the slot, and the
    // question it displaces is not held beside it.
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface(
            "planner-question",
            "a later question",
            "proposal",
            true,
        )),
    );
    let later = one_line(&scratch.bus(&["next", "surfaces"], None));
    assert_eq!(later["record"]["message"], json!("a later question"));
    let status = scratch.status("surfaces");
    assert_eq!(status["pending"]["message"], json!("a later question"));
    assert_eq!(status["pending_position"], later["position"]);
    assert_eq!(
        status["waiting"],
        json!([]),
        "a displaced question is held beside the pending one"
    );

    let empty = scratch.bus(&["next", "surfaces"], None);
    assert_eq!(empty.code, 1);
    assert_eq!(
        empty.stderr.trim(),
        "onemessagebus: nothing on surfaces to claim"
    );
    assert!(empty.stdout.is_empty());
    let as_payload = scratch.bus(&["next", "surfaces", "{}"], None);
    assert_eq!(
        as_payload.code, 2,
        "next took a payload: {}",
        as_payload.stdout
    );
}

#[test]
fn a_check_in_supersedes_a_waiting_check_in_and_never_a_finding() {
    let scratch = Scratch::new();
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("check-in", "first update", "check-in", false)),
    );
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("finding", "a finding", "proposal", false)),
    );
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("check-in", "second update", "check-in", false)),
    );
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("finding", "another finding", "proposal", false)),
    );
    let waiting: Vec<Value> = scratch.status("surfaces")["waiting"]
        .as_array()
        .expect("waiting")
        .iter()
        .map(|surface| surface["message"].clone())
        .collect();
    assert_eq!(
        waiting,
        vec![
            json!("a finding"),
            json!("second update"),
            json!("another finding")
        ],
        "a waiting check-in was not superseded, or a finding was"
    );
}

#[test]
fn two_processes_claiming_from_one_queue_at_once_receive_distinct_records() {
    const RECORDS: usize = 12;
    const CLAIMANTS: usize = 8;
    let scratch = Scratch::new();
    for n in 0..RECORDS {
        scratch.bus(
            &["send", "surfaces"],
            Some(&surface(
                "finding",
                &format!("finding {n}"),
                "proposal",
                false,
            )),
        );
    }
    let claimed: Vec<Run> = std::thread::scope(|scope| {
        let claimants: Vec<_> = (0..CLAIMANTS)
            .map(|_| scope.spawn(|| scratch.bus(&["next", "surfaces"], None)))
            .collect();
        claimants
            .into_iter()
            .map(|claimant| claimant.join().expect("a claimant finishes"))
            .collect()
    });
    let mut ids: Vec<u64> = claimed
        .iter()
        .map(|run| one_line(run)["id"].as_u64().expect("an id"))
        .collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(
        ids.len(),
        before,
        "two processes were handed the same record: {ids:?}"
    );
    assert_eq!(
        scratch.status("surfaces")["waiting"]
            .as_array()
            .map(Vec::len),
        Some(RECORDS - CLAIMANTS)
    );
}

#[test]
fn a_claimant_that_exits_without_answering_leaves_its_record_claimed() {
    let scratch = Scratch::new();
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("planner-question", "taken", "proposal", true)),
    );
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("finding", "still waiting", "proposal", false)),
    );
    // The first claimant prints what it took and its process ends: nothing
    // answers the question, and nothing hands it out again.
    let first = one_line(&scratch.bus(&["next", "surfaces"], None));
    assert_eq!(first["record"]["message"], json!("taken"));
    let second = one_line(&scratch.bus(&["next", "surfaces"], None));
    assert_eq!(
        second["record"]["message"],
        json!("still waiting"),
        "a claimed record was handed out twice"
    );
    let status = scratch.status("surfaces");
    assert_eq!(status["pending"]["message"], json!("taken"));

    // A plain queue records the claim as the consumer's cursor.
    scratch.bus(
        &["send", "command-outcomes"],
        Some(r#"{"id":0,"applied":true}"#),
    );
    scratch.bus(
        &["send", "command-outcomes"],
        Some(r#"{"id":1,"applied":false,"reason":"refused"}"#),
    );
    let outcome =
        one_line(&scratch.bus(&["next", "command-outcomes", "--consumer", "reader"], None));
    assert_eq!(outcome["record"]["id"], json!(0));
    assert_eq!(scratch.file("command-outcomes-cursor.reader.json"), "1");
    let next = one_line(&scratch.bus(&["next", "command-outcomes", "--consumer", "reader"], None));
    assert_eq!(next["record"]["id"], json!(1));
    let bad = scratch.bus(&["next", "command-outcomes", "--consumer", "../x"], None);
    let plain = scratch.bus(&["next", "command-outcomes", "--asker", "dispatch-a"], None);
    assert_eq!(plain.code, 2, "{}", plain.stderr);
    assert!(
        plain.stderr.contains("is a plain queue"),
        "{}",
        plain.stderr
    );
    assert_eq!(bad.code, 2, "{}", bad.stderr);
}

#[test]
fn reply_answers_the_record_pending_at_a_position_from_stdin_or_file() {
    let scratch = Scratch::new();
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface(
            "planner-question",
            "is the base right?",
            "proposal",
            true,
        )),
    );
    let claimed = one_line(&scratch.bus(&["next", "surfaces"], None));
    let position = claimed["position"].to_string();

    let wrong = scratch.bus(&["reply", "surfaces", "3"], Some(r#"{"message":"yes"}"#));
    assert_eq!(wrong.code, 1);
    assert!(
        wrong.stderr.contains(&format!(
            "the pending record was claimed at position {position}, not 3"
        )),
        "{}",
        wrong.stderr
    );
    let as_argument = scratch.bus(
        &["reply", "surfaces", &position, r#"{"message":"yes"}"#],
        None,
    );
    assert_eq!(
        as_argument.code, 2,
        "a reply passed as an argument was accepted"
    );
    assert!(
        scratch.file("replies.jsonl").is_empty(),
        "a refused reply was appended"
    );

    let commands_only = scratch.bus(
        &["reply", "surfaces", &position],
        Some(r#"{"version":3,"commands":[{"op":"cancel","id":"build"}]}"#),
    );
    assert_eq!(commands_only.code, 0, "{}", commands_only.stderr);
    let replied: Value = serde_json::from_str(&commands_only.stdout).expect("JSON");
    assert_eq!(
        replied["answered"],
        Value::Null,
        "a commands-only reply answered the question"
    );
    assert_eq!(replied["sent"][0]["queue"], json!("commands"));
    assert_eq!(scratch.status("surfaces")["pending"]["id"], json!(0));

    let reply = scratch.root().join("reply.json");
    std::fs::write(&reply, r#"{"completion":false,"message":"yes, carry on"}"#)
        .expect("a reply file");
    let answered = scratch.bus(
        &[
            "reply",
            "surfaces",
            &position,
            "--file",
            reply.to_str().expect("a path"),
        ],
        None,
    );
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    let replied: Value = serde_json::from_str(&answered.stdout).expect("JSON");
    assert_eq!(replied["answered"]["id"], json!(0));
    assert_eq!(
        replied["sent"],
        json!([{"queue": "replies", "position": scratch.file("replies.jsonl").len(), "id": 0}])
    );
    assert_eq!(
        scratch.status("surfaces")["pending"],
        Value::Null,
        "the answer did not release the slot"
    );
    let logged = scratch.lines("surfaces.jsonl");
    assert_eq!(logged.last().expect("a line")["event"], json!("answered"));

    let again = scratch.bus(
        &["reply", "surfaces", &position],
        Some(r#"{"message":"twice"}"#),
    );
    assert_eq!(again.code, 1);
    assert!(
        again.stderr.contains("nothing is pending"),
        "{}",
        again.stderr
    );
    let plain = scratch.bus(&["reply", "replies", "1"], Some(r#"{"message":"x"}"#));
    assert_eq!(plain.code, 2);
    assert!(
        plain
            .stderr
            .contains("declares no queue its replies are appended to"),
        "{}",
        plain.stderr
    );

    // On stdin, too.
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("planner-question", "another?", "proposal", true)),
    );
    let claimed = one_line(&scratch.bus(&["next", "surfaces"], None));
    let on_stdin = scratch.bus(
        &["reply", "surfaces", &claimed["position"].to_string()],
        Some(r#"{"message":"on stdin"}"#),
    );
    assert_eq!(on_stdin.code, 0, "{}", on_stdin.stderr);
    assert_eq!(
        scratch.lines("replies.jsonl")[1]["reply"]["message"],
        json!("on stdin")
    );
}

#[test]
fn subscribe_streams_until_its_predicate_admits_a_record_and_times_out_otherwise() {
    let scratch = Scratch::new();
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface(
            "planner-question",
            "waiting on you",
            "proposal",
            true,
        )),
    );
    let channel = scratch.channel();
    let subscriber = onemessagebus()
        .args([
            "subscribe",
            "surfaces",
            "--until",
            r#"{"field":"event","equals":"answered"}"#,
            "--timeout",
            "60",
            "--transport-dir",
            channel.to_str().expect("a path"),
        ])
        .current_dir(scratch.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the subscriber spawns");
    std::thread::sleep(Duration::from_millis(200));
    let claimed = one_line(&scratch.bus(&["next", "surfaces"], None));
    std::thread::sleep(Duration::from_millis(200));
    let answered = scratch.bus(
        &["reply", "surfaces", &claimed["position"].to_string()],
        Some(r#"{"message":"answered"}"#),
    );
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    let output = subscriber.wait_with_output().expect("the subscriber ends");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .expect("UTF-8")
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).expect("a JSON line")["record"]["event"].clone()
        })
        .collect();
    assert_eq!(
        events,
        vec![json!("queued"), json!("claimed"), json!("answered")],
        "the stream did not end on the answer"
    );

    // With no --timeout, the stream waits for as long as it takes.
    let late = onemessagebus()
        .args([
            "subscribe",
            "replies",
            "--until",
            r#"{"field":"reply.message","equals":"late"}"#,
            "--transport-dir",
            channel.to_str().expect("a path"),
        ])
        .current_dir(scratch.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the subscriber spawns");
    std::thread::sleep(Duration::from_millis(1500));
    let sent = scratch.bus(&["send", "replies"], Some(r#"{"message":"late"}"#));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    let output = late.wait_with_output().expect("the subscriber ends");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let last: Value = serde_json::from_str(
        String::from_utf8(output.stdout)
            .expect("UTF-8")
            .lines()
            .last()
            .expect("a line"),
    )
    .expect("JSON");
    assert_eq!(last["record"]["reply"]["message"], json!("late"));

    let timed_out = scratch.bus(
        &[
            "subscribe",
            "surfaces",
            "--until",
            r#"{"field":"event","equals":"abandoned"}"#,
            "--timeout",
            "1",
            "--format",
            "text",
        ],
        None,
    );
    assert_eq!(timed_out.code, 1);
    assert_eq!(timed_out.stdout.lines().count(), 3, "{}", timed_out.stdout);
    assert!(
        timed_out.stderr.contains("within 1 seconds"),
        "{}",
        timed_out.stderr
    );
    let as_payload = scratch.bus(
        &[
            "subscribe",
            "surfaces",
            r#"{"kind":"finding"}"#,
            "--until",
            r#"{"field":"event","equals":"answered"}"#,
        ],
        None,
    );
    assert_eq!(
        as_payload.code, 2,
        "subscribe took a payload: {}",
        as_payload.stdout
    );
    let unbounded = scratch.bus(
        &[
            "subscribe",
            "surfaces",
            "--until",
            r#"{"field":"event","equals":"answered"}"#,
            "--timeout",
            "18446744073709551615",
        ],
        None,
    );
    assert_eq!(
        unbounded.code, 0,
        "a timeout past what a deadline can hold: {}",
        unbounded.stderr
    );

    let bad = scratch.bus(
        &["subscribe", "surfaces", "--until", r#"{"field":"event"}"#],
        None,
    );
    assert_eq!(bad.code, 2);
    assert!(bad.stderr.contains("--until"), "{}", bad.stderr);
}

#[test]
fn status_reports_every_declared_queue_and_transports_lists_the_kinds() {
    let scratch = Scratch::new();
    scratch.bus(&["send", "replies"], Some(r#"{"message":"a verdict"}"#));
    scratch.bus(&["send", "replies"], Some(r#"{"message":"another"}"#));
    one_line(&scratch.bus(&["next", "replies"], None));
    let run = scratch.bus(&["status"], None);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let statuses: Vec<Value> = serde_json::from_str(&run.stdout).expect("a JSON list");
    let queues: Vec<Value> = statuses
        .iter()
        .map(|status| status["queue"].clone())
        .collect();
    assert_eq!(
        queues,
        vec![
            json!("command-outcomes"),
            json!("commands"),
            json!("replies"),
            json!("surfaces")
        ]
    );
    let replies = &statuses[2];
    assert_eq!(replies["records"], json!(2));
    assert_eq!(replies["unread"], json!(1));
    assert_eq!(replies["events"], json!(false));
    assert_eq!(
        replies["cursors"]["default"],
        json!(
            scratch
                .file("replies.jsonl")
                .lines()
                .next()
                .expect("a line")
                .len()
                + 1
        )
    );

    let text = scratch.bus(&["status", "replies", "--format", "text"], None);
    assert_eq!(
        text.stdout,
        format!(
            "replies records=2 waiting=1 pending=- abandoned=0 unread=1\n  cursor default={}\n",
            replies["cursors"]["default"]
        )
    );
    let unknown = scratch.bus(&["status", "findings"], None);
    assert_eq!(unknown.code, 2);
    let as_payload = scratch.bus(&["status", "surfaces", r#"{"kind":"finding"}"#], None);
    assert_eq!(
        as_payload.code, 2,
        "status took a payload: {}",
        as_payload.stdout
    );
    let as_queue = scratch.bus(&["status", r#"{"kind":"finding"}"#], None);
    assert_eq!(
        as_queue.code, 2,
        "status read a payload as a queue: {}",
        as_queue.stdout
    );
    assert!(
        as_queue.stderr.contains("is not a queue name"),
        "{}",
        as_queue.stderr
    );

    let kinds = run_in(scratch.root(), &["transports"], None, &[("PATH", "")]);
    assert_eq!(kinds.code, 0, "{}", kinds.stderr);
    let listed: Value = serde_json::from_str(&kinds.stdout).expect("JSON");
    assert_eq!(
        listed,
        json!([{"kind": "local", "origin": "builtin"}, {"kind": "memory", "origin": "builtin"}])
    );
    let text = run_in(
        scratch.root(),
        &["transports", "--format", "text"],
        None,
        &[("PATH", "")],
    );
    assert_eq!(text.stdout, "local builtin\nmemory builtin\n");
    let as_payload = run_in(scratch.root(), &["transports", "{}"], None, &[]);
    assert_eq!(as_payload.code, 2);
}

/// A configuration that adds a queue reaches the binary on `--config` and on
/// `ONEMESSAGEBUS_CONFIG`; without it the queue is refused by name; and the
/// transport directory is chosen flag first, then variable, then file.
#[test]
fn a_configuration_reaches_the_binary_by_every_route_and_the_directory_by_precedence() {
    let scratch = Scratch::new();
    let root = scratch.root();
    let from_file = root.join("from-file");
    let config = root.join("onemessagebus.yaml");
    std::fs::write(
        &config,
        format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: planner-channel\nqueues:\n  findings: {{policy: {{hold_pending: false}}}}\n",
            from_file.display()
        ),
    )
    .expect("a configuration");
    let config = config.to_str().expect("a path");
    let record = r#"{"what":"a finding"}"#;

    let on_flag = run_in(
        root,
        &["send", "findings", "--config", config],
        Some(record),
        &[],
    );
    assert_eq!(on_flag.code, 0, "{}", on_flag.stderr);
    assert!(
        from_file.join("findings.jsonl").is_file(),
        "--config did not add the queue under transport.dir"
    );
    let listed = run_in(root, &["status", "--config", config], None, &[]);
    assert!(
        listed.stdout.contains("\"queue\": \"findings\""),
        "{}",
        listed.stdout
    );

    let on_env = run_in(
        root,
        &["send", "findings"],
        Some(record),
        &[("ONEMESSAGEBUS_CONFIG", config)],
    );
    assert_eq!(on_env.code, 0, "{}", on_env.stderr);
    assert_eq!(
        std::fs::read_to_string(from_file.join("findings.jsonl"))
            .expect("the queue")
            .lines()
            .count(),
        2
    );
    let listed = run_in(
        root,
        &["status", "--format", "text"],
        None,
        &[("ONEMESSAGEBUS_CONFIG", config)],
    );
    assert!(
        listed
            .stdout
            .lines()
            .any(|line| line.starts_with("findings ")),
        "{}",
        listed.stdout
    );

    let without = scratch.bus(&["send", "findings"], Some(record));
    assert_eq!(without.code, 2);
    assert!(
        without
            .stderr
            .contains("`findings` is not a queue this configuration declares"),
        "{}",
        without.stderr
    );

    let from_env = root.join("from-env");
    let from_flag = root.join("from-flag");
    let variable = [(
        "ONEMESSAGEBUS_TRANSPORT_DIR",
        from_env.to_str().expect("a path"),
    )];
    let by_variable = run_in(
        root,
        &["send", "findings", "--config", config],
        Some(record),
        &variable,
    );
    assert_eq!(by_variable.code, 0, "{}", by_variable.stderr);
    assert!(
        from_env.join("findings.jsonl").is_file(),
        "the variable did not win over the file"
    );
    let by_flag = run_in(
        root,
        &[
            "send",
            "findings",
            "--config",
            config,
            "--transport-dir",
            from_flag.to_str().expect("a path"),
        ],
        Some(record),
        &variable,
    );
    assert_eq!(by_flag.code, 0, "{}", by_flag.stderr);
    assert!(
        from_flag.join("findings.jsonl").is_file(),
        "the flag did not win over the variable"
    );
    assert_eq!(
        std::fs::read_to_string(from_env.join("findings.jsonl"))
            .expect("the queue")
            .lines()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(from_file.join("findings.jsonl"))
            .expect("the queue")
            .lines()
            .count(),
        2
    );

    let nothing = run_in(root, &["status"], None, &[]);
    assert_eq!(nothing.code, 2);
    assert!(
        nothing
            .stderr
            .contains("no configuration to open a queue with"),
        "{}",
        nothing.stderr
    );
}

#[test]
fn a_configuration_with_an_unknown_key_or_a_widened_grant_is_refused_naming_the_key() {
    let scratch = Scratch::new();
    let root = scratch.root();
    let channel = scratch.channel();
    let write = |name: &str, body: &str| -> String {
        let path = root.join(name);
        std::fs::write(
            &path,
            format!(
                "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: planner-channel\n{body}",
                channel.display()
            ),
        )
        .expect("a configuration");
        path.to_str().expect("a path").to_owned()
    };
    let unknown = write(
        "unknown.yaml",
        "queues:\n  findings: {polcy: {hold_pending: false}}\n",
    );
    let run = run_in(root, &["status", "--config", &unknown], None, &[]);
    assert_eq!(run.code, 2);
    assert!(
        run.stderr.contains("unknown field `polcy`"),
        "{}",
        run.stderr
    );

    let widened = write(
        "widened.yaml",
        "authors:\n  monitor: {capabilities: [retry, attest]}\n",
    );
    let run = run_in(root, &["status", "--config", &widened], None, &[]);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stderr.trim(),
        "onemessagebus: authors.monitor.capabilities: `attest` is not granted to monitor by the profile, and a configuration may narrow an author's grants but never widen them"
    );

    let narrowed = write(
        "narrowed.yaml",
        "authors:\n  monitor: {capabilities: [finding]}\n",
    );
    let run = run_in(
        root,
        &["send", "commands", "--config", &narrowed],
        Some(r#"{"author":"monitor","commands":[{"op":"retry","id":"build","node":{}}]}"#),
        &[],
    );
    assert_eq!(run.code, 1);
    assert_eq!(
        run.stderr.trim(),
        "onemessagebus: commands: 'retry' is not an op the monitor may issue: the configuration does not grant it. Surface it to the planner instead"
    );
    let allowed = run_in(
        root,
        &["send", "commands", "--config", &narrowed],
        Some(r#"{"author":"monitor","commands":[{"op":"finding","message":"look"}]}"#),
        &[],
    );
    assert_eq!(allowed.code, 0, "{}", allowed.stderr);
}

/// A serving session that ended abandoned what it raised; a later `next` of the
/// same asker takes it back, and one of another asker or none takes nothing.
#[test]
fn next_with_the_same_asker_takes_back_what_a_listener_abandoned_and_no_other_does() {
    let scratch = Scratch::new();
    {
        let transport: Arc<dyn Transport> =
            Arc::new(LocalTransport::open(scratch.channel()).expect("opens"));
        let channel = Channel::open(&transport).expect("the channel opens");
        let raised = channel
            .surfaces()
            .raw()
            .push(json!({
                "kind": "planner-question", "message": "raised by a session that ended",
                "source": "proposal", "blocking": true, "queued_at": 1, "asker": "dispatch-a",
            }))
            .expect("raised");
        channel.claim().expect("a claim").expect("claimed");
        channel
            .abandon(&[raised.id.expect("an id")])
            .expect("abandoned");
    }
    let status = scratch.status("surfaces");
    assert_eq!(status["abandoned"].as_array().map(Vec::len), Some(1));
    assert_eq!(status["pending"]["abandoned"], json!(true));

    for other in [
        vec!["next", "surfaces", "--asker", "dispatch-b"],
        vec!["next", "surfaces"],
    ] {
        let run = scratch.bus(&other, None);
        assert_eq!(run.code, 1, "{}", run.stdout);
        assert_eq!(
            scratch.status("surfaces")["abandoned"]
                .as_array()
                .map(Vec::len),
            Some(1),
            "`{}` took back another asker's question",
            other.join(" ")
        );
    }
    let same = scratch.bus(&["next", "surfaces", "--asker", "dispatch-a"], None);
    assert_eq!(
        same.code, 1,
        "nothing is left to claim once the question is taken back"
    );
    let status = scratch.status("surfaces");
    assert_eq!(
        status["abandoned"],
        json!([]),
        "the same asker took nothing back"
    );
    assert_eq!(
        status["pending"]["message"],
        json!("raised by a session that ended")
    );
    assert!(
        status["pending"].get("abandoned").is_none(),
        "{}",
        status["pending"]
    );
    assert_eq!(
        scratch.lines("surfaces.jsonl").last().expect("a line")["event"],
        json!("attended")
    );

    let blank = scratch.bus(&["next", "surfaces", "--asker", "  "], None);
    assert_eq!(blank.code, 2);
    assert_eq!(
        blank.stderr.trim(),
        "onemessagebus: --asker is set to a blank value, which names no asker; leave it unset for a session that listens on its own, or set it to the one value every session of this asker carries"
    );
    let _ = Asker::new("dispatch-a", "--asker").expect("a word");
}

/// An asker that is not Unicode is refused before anything is read.
#[cfg(unix)]
#[test]
fn an_asker_that_is_not_unicode_is_refused() {
    use std::os::unix::ffi::OsStrExt as _;
    let scratch = Scratch::new();
    let output = onemessagebus()
        .arg("next")
        .arg("surfaces")
        .arg("--asker")
        .arg(std::ffi::OsStr::from_bytes(b"dispatch-\xff"))
        .arg("--transport-dir")
        .arg(scratch.channel())
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--asker is set to a value this host cannot read as text"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A projection whose stamp still matches the log but whose claims were moved
/// is read as no projection: `status` folds the log whole and writes the repair.
#[test]
fn status_reads_a_stamped_projection_that_does_not_seal_as_no_document() {
    let scratch = Scratch::new();
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("finding", "logged", "proposal", false)),
    );
    scratch.bus(
        &["send", "surfaces"],
        Some(&surface("finding", "also logged", "proposal", false)),
    );
    let written = scratch.file("queue.json");
    let mut tampered: Value = serde_json::from_str(&written).expect("queue.json");
    tampered["waiting"] = json!([]);
    std::fs::write(
        scratch.channel().join("queue.json"),
        serde_json::to_string_pretty(&tampered).expect("JSON"),
    )
    .expect("the projection is tampered with");
    let status = scratch.status("surfaces");
    assert_eq!(
        status["waiting"].as_array().map(Vec::len),
        Some(2),
        "the tampered projection was trusted"
    );
    assert_eq!(
        scratch.file("queue.json"),
        written,
        "the repaired projection was not written back"
    );
}
