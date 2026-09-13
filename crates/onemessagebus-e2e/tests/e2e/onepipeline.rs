//! A channel directory this crate writes, read by the 0.28.2 release's own
//! reader.
//!
//! The reader is the `onepipeline` binary of the `onepipeline-cli` 0.28.2 wheel,
//! run through `uv tool run`. Its `next`, `results` and `status` open a run root
//! rather than a channel directory, so the journey copies the recorded finished
//! run root `onemessagebus-repair-2` (`crates/onemessagebus-agent/tests/recorded/`)
//! into a scratch runs directory and substitutes the directory this crate wrote
//! for its `channel/`. It then holds the release's answers to what this crate
//! answers for a twin copy of the same directory, and has this crate read back
//! a reply the release appended.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use onemessagebus::{Asker, LocalTransport, Transport};
use onemessagebus_agent::channel::{source, Channel};
use onemessagebus_agent::channel::{
    ChannelAuthor, CommandOutcome, CommandResult, CommandVerdict, ReplyEnvelope, Surface,
};
use serde_json::{json, Map, Value};

use crate::support::{fixture, run_in, Run};

/// The release whose reader the directory is held to.
const RELEASE: &str = "onepipeline-cli==0.28.2";

/// The recorded run root the written channel is substituted into.
const RUN: &str = "onemessagebus-repair-2";

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

/// The release's `onepipeline`, over the runs directory `runs`.
fn onepipeline(runs: &Path, args: &[&str], stdin: Option<&str>) -> Run {
    use std::io::Write as _;
    let mut child = Command::new("uv")
        .args(["tool", "run", "--from", RELEASE, "onepipeline"])
        .args(args)
        .env("ONEPIPELINE_RUNS_DIR", runs)
        .env_remove("ONEPIPELINE_CHANNEL_ASKER")
        .current_dir(runs)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|failure| {
            panic!(
                "this journey runs the {RELEASE} wheel's reader through `uv tool run`, and uv \
                 could not be started ({failure}); install uv (https://docs.astral.sh/uv/) — CI \
                 installs it for every job that runs this suite"
            )
        });
    if let Some(mut handle) = child.stdin.take() {
        if let Some(text) = stdin {
            handle.write_all(text.as_bytes()).expect("stdin is written");
        }
    }
    let output = child
        .wait_with_output()
        .expect("the release's reader exits");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn copy_files(from: &Path, to: &Path, names: &[&str]) {
    std::fs::create_dir_all(to).expect("a directory");
    for name in names {
        let source = from.join(name);
        if source.exists() {
            std::fs::copy(&source, to.join(name)).expect("a file copies");
        }
    }
}

/// A runs directory holding the recorded run root, its channel replaced by
/// `channel`'s files — or left as recorded when `channel` is `None`.
fn run_root(runs: &Path, channel: Option<&Path>) -> PathBuf {
    let root = runs.join(RUN);
    copy_files(
        &fixture(&format!("recorded/run-root/{RUN}")),
        &root,
        &[
            "launch.json",
            "plan.json",
            "checkpoint.json",
            "events.jsonl",
            "summary.json",
            "result.json",
        ],
    );
    let recorded = fixture(&format!("recorded/channel/{RUN}"));
    copy_files(channel.unwrap_or(&recorded), &root.join("channel"), LAYOUT);
    root
}

fn surface(kind: &str, message: &str, from: &str, blocking: bool, asker: Option<&str>) -> Surface {
    Surface {
        id: 0,
        kind: kind.to_owned(),
        message: message.to_owned(),
        source: from.to_owned(),
        blocking,
        queued_at: 1_789_300_000_000,
        workstream: None,
        abandoned: false,
        asker: asker.map(|name| Asker::new(name, "the journey").expect("an asker")),
        correlation: None,
    }
}

fn command(fields: Value) -> Map<String, Value> {
    fields.as_object().expect("a command object").clone()
}

/// Write a channel directory from scratch through this crate, across a journey
/// that exercises every queue.
fn write_channel(dir: &Path) {
    let transport: Arc<dyn Transport> = Arc::new(LocalTransport::open(dir).expect("opens"));
    let channel = Channel::open(&transport).expect("the channel opens");

    // Surfaces raised: a check-in superseded by the next, and a finding.
    channel
        .push(&surface(
            "check-in",
            "first update",
            source::CHECK_IN,
            false,
            None,
        ))
        .expect("queued");
    channel
        .push(&surface(
            "check-in",
            "second update",
            source::CHECK_IN,
            false,
            None,
        ))
        .expect("queued");
    channel
        .push(&surface(
            "finding",
            "the gate went red",
            source::PROPOSAL,
            false,
            None,
        ))
        .expect("queued");

    // A question claimed, abandoned by its listener, attended by a later one of
    // the same asker, and answered by a reply.
    let asked = channel
        .push(&surface(
            "planner-question",
            "asked by a listener that left and came back",
            source::PROPOSAL,
            true,
            Some("listener-a"),
        ))
        .expect("queued");
    let claimed = channel.claim().expect("a claim").expect("the question");
    assert_eq!(Some(claimed.id), asked.id);
    let asked_id = asked.id.expect("an id");
    assert_eq!(channel.abandon(&[asked_id]).expect("abandoned").len(), 1);
    assert_eq!(
        channel
            .attend(&Asker::new("listener-a", "the journey").expect("an asker"))
            .expect("attended")
            .len(),
        1
    );
    let answered = channel
        .answer(
            &ReplyEnvelope {
                completion: Some(false),
                message: Some("carry on".to_owned()),
                ..ReplyEnvelope::default()
            },
            1_789_300_000_100,
        )
        .expect("answered");
    assert_eq!(
        channel
            .claim_reply()
            .expect("a claim")
            .map(|reply| reply.id),
        Some(answered)
    );

    // A commands-only reply — its envelope on the command queue alone — and a
    // planner's envelope, both claimed and answered.
    channel
        .submit(
            ChannelAuthor::Monitor,
            vec![command(
                json!({"op": "finding", "message": "look at the gate"}),
            )],
        )
        .expect("submitted");
    channel
        .submit(
            ChannelAuthor::Planner,
            vec![command(json!({"op": "note", "id": "plan", "addressee": "worker", "text": "a smaller diff"}))],
        )
        .expect("submitted");
    assert_eq!(channel.claim_commands().expect("claimed").len(), 2);
    for (id, op, applied) in [(0, "finding", true), (1, "note", false)] {
        channel
            .answer_commands(&CommandOutcome {
                id,
                applied,
                reason: (!applied).then(|| "the node has settled".to_owned()),
                results: vec![CommandResult {
                    index: 0,
                    op: op.to_owned(),
                    outcome: if applied {
                        CommandVerdict::Applied
                    } else {
                        CommandVerdict::Refused
                    },
                    reason: (!applied).then(|| "the node has settled".to_owned()),
                }],
            })
            .expect("answered");
    }

    // A question whose listener left and never came back, and one still pending.
    let gone = channel
        .push(&surface(
            "planner-question",
            "asked by a listener that never came back",
            source::PROPOSAL,
            true,
            Some("listener-b"),
        ))
        .expect("queued");
    channel
        .abandon(&[gone.id.expect("an id")])
        .expect("abandoned");
    channel
        .push(&surface(
            "planner-question",
            "still pending",
            source::PROPOSAL,
            true,
            None,
        ))
        .expect("queued");
    let pending = channel.claim().expect("a claim").expect("the question");
    assert_eq!(pending.message, "still pending");

    for file in LAYOUT {
        assert!(dir.join(file).is_file(), "the journey wrote no {file}");
    }
}

/// This crate's own answer, through the binary, over `channel`.
fn ours(channel: &Path, args: &[&str]) -> Run {
    let mut argv = args.to_vec();
    argv.extend(["--transport-dir", channel.to_str().expect("a UTF-8 path")]);
    run_in(channel.parent().expect("a parent"), &argv, None, &[])
}

#[test]
fn a_directory_this_crate_writes_is_read_by_the_0_28_2_release_and_what_it_appends_is_read_back() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let written = scratch.path().join("written");
    write_channel(&written);

    let release_runs = scratch.path().join("release");
    let release_root = run_root(&release_runs, Some(&written));
    let twin = scratch.path().join("twin");
    copy_files(&written, &twin, LAYOUT);

    // status: the pending decision, the unread count and the abandoned count
    // 0.28.2 reports are what this crate reports for the same directory.
    let ours_status = ours(&twin, &["status", "surfaces"]);
    assert_eq!(ours_status.code, 0, "{}", ours_status.stderr);
    let statuses: Vec<Value> = serde_json::from_str(&ours_status.stdout).expect("JSON");
    let status = &statuses[0];
    let pending = &status["pending"];
    assert_eq!(
        pending["abandoned"],
        Value::Null,
        "the pending question was left abandoned"
    );
    let unread = status["unread"].as_u64().expect("a count");
    let abandoned = status["abandoned"].as_array().expect("a list").len();
    assert_eq!((unread, abandoned), (2, 1), "{status}");

    let release_status = onepipeline(&release_runs, &["status", RUN], None);
    assert_eq!(release_status.code, 0, "{}", release_status.stderr);
    for line in [
        format!(
            "  waiting for planner decision: {} — {}",
            pending["kind"].as_str().expect("a kind"),
            pending["message"].as_str().expect("a message")
        ),
        format!("  {unread} planner update(s) waiting (1 check-in, 1 finding), unread for "),
        format!("  {abandoned} planner update(s) nobody is waiting on: "),
    ] {
        assert!(
            release_status.stdout.contains(&line),
            "0.28.2's status does not say `{line}` of the directory this crate wrote:\n{}",
            release_status.stdout
        );
    }

    // results: the release reads the run root with the written channel exactly
    // as it reads it with the channel the run recorded.
    let recorded_runs = scratch.path().join("recorded");
    run_root(&recorded_runs, None);
    let over_written = onepipeline(&release_runs, &["results", RUN], None);
    let over_recorded = onepipeline(&recorded_runs, &["results", RUN], None);
    assert_eq!(over_written.code, 0, "{}", over_written.stderr);
    assert_eq!(
        (over_written.code, &over_written.stdout),
        (over_recorded.code, &over_recorded.stdout),
        "0.28.2's results differ over the written channel"
    );

    // next: the release claims the surface this crate claims, and the two
    // directories are left byte for byte alike.
    let release_next = onepipeline(&release_runs, &["next", RUN], None);
    assert_eq!(release_next.code, 0, "{}", release_next.stderr);
    let release_next: Value =
        serde_json::from_str(&release_next.stdout).expect("0.28.2's next is JSON");
    assert_eq!(release_next["status"], json!("surface"), "{release_next}");
    let ours_next = ours(&twin, &["next", "surfaces"]);
    assert_eq!(ours_next.code, 0, "{}", ours_next.stderr);
    let ours_next: Value = serde_json::from_str(&ours_next.stdout).expect("JSON");
    assert_eq!(
        release_next["surface"], ours_next["record"],
        "0.28.2 claimed another surface than this crate"
    );
    assert_eq!(ours_next["record"]["message"], json!("second update"));
    for file in ["surfaces.jsonl", "queue.json"] {
        assert_eq!(
            std::fs::read(release_root.join("channel").join(file)).expect("the release's copy"),
            std::fs::read(twin.join(file)).expect("this crate's copy"),
            "after both claims, {file} differs between 0.28.2's copy and this crate's"
        );
    }

    // reply: the release answers the pending question, and this crate reads its
    // reply back.
    let release_reply = onepipeline(
        &release_runs,
        &["reply", RUN],
        Some(r#"{"completion": false, "message": "answered by the 0.28.2 release"}"#),
    );
    assert_eq!(release_reply.code, 0, "{}", release_reply.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&release_reply.stdout).expect("a receipt"),
        json!({"reply": 1, "state": "delivered", "verdict": "delivered"})
    );
    let channel_dir = release_root.join("channel");
    let transport: Arc<dyn Transport> =
        Arc::new(LocalTransport::open(&channel_dir).expect("opens"));
    let channel = Channel::open(&transport).expect("the channel opens");
    let read_back = channel
        .claim_reply()
        .expect("a claim")
        .expect("the release's reply is read back");
    assert_eq!(read_back.id, 1);
    assert_eq!(
        read_back.reply.message.as_deref(),
        Some("answered by the 0.28.2 release")
    );
    assert_eq!(
        channel.pending().expect("a read"),
        None,
        "the release's answer did not release the slot"
    );
    let after = ours(&channel_dir, &["status", "surfaces"]);
    let after: Vec<Value> = serde_json::from_str(&after.stdout).expect("JSON");
    assert_eq!(after[0]["pending"], Value::Null);
    assert_eq!(after[0]["unread"], json!(1), "{}", after[0]);
}
