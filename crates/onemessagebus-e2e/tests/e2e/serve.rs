//! Contract K through the built binary: `serve --codec onejudge` handed real
//! `onejudge` frames on stdin, over a planner channel directory — and the
//! planner answering, or not, from another process.
//!
//! Every variable the codec reads is one this suite names in the configuration
//! (`TEST_SERVE_*`), so nothing in the environment the suite runs under decides
//! a journey.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use onemessagebus_agent::codec::onejudge;
use serde_json::{json, Value};

use crate::support::{fixture, onemessagebus, run_in, Run};

const GUARD: Duration = Duration::from_secs(60);

/// A home no recorded transcript names, so a journey's identity is the
/// primary one unless it says otherwise.
const NOT_THE_ALTERNATE: &str = "/nowhere/codex-alt";

struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    /// A channel directory, and a configuration over it whose `codecs.onejudge`
    /// block waits `window` seconds for a ruling.
    fn new(window: u64) -> Self {
        let scratch = Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        };
        std::fs::write(
            scratch.config(),
            format!(
                "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: planner-channel\ncodecs:\n  onejudge: {{reply_window_seconds: {window}, run_env: TEST_SERVE_RUN, asker_env: TEST_SERVE_ASKER, session_env: TEST_SERVE_SESSION}}\n",
                serde_json::to_string(&scratch.channel()).expect("a path")
            ),
        )
        .expect("the configuration is written");
        scratch
    }

    fn channel(&self) -> PathBuf {
        self.dir.path().join("channel")
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("onemessagebus.yaml")
    }

    fn argv<'a>(&'a self, args: &[&'a str], config: &'a str) -> Vec<&'a str> {
        let mut argv = args.to_vec();
        argv.extend(["--config", config]);
        argv
    }

    fn serve(&self, extra: &[&str], frame: &str, env: &[(&str, &str)]) -> Run {
        let config = self.config();
        let mut args = vec!["serve", "surfaces", "--codec", "onejudge"];
        args.extend_from_slice(extra);
        let mut env = env.to_vec();
        if !env
            .iter()
            .any(|(name, _)| *name == onejudge::CODEX_ALT_HOME_ENV)
        {
            env.push((onejudge::CODEX_ALT_HOME_ENV, NOT_THE_ALTERNATE));
        }
        run_in(
            self.dir.path(),
            &self.argv(&args, config.to_str().expect("a UTF-8 path")),
            Some(&format!("{frame}\n")),
            &env,
        )
    }

    /// Spawn `serve` with `frame` on stdin, closing stdin after it unless
    /// `held` — a member still there.
    fn spawn(
        &self,
        extra: &[&str],
        frame: &str,
        held: bool,
    ) -> (Child, Option<std::process::ChildStdin>) {
        let config = self.config();
        let mut args = vec!["serve", "surfaces", "--codec", "onejudge"];
        args.extend_from_slice(extra);
        let mut child = onemessagebus()
            .args(self.argv(&args, config.to_str().expect("a UTF-8 path")))
            .current_dir(self.dir.path())
            .env("TEST_SERVE_RUN", "r-7")
            .env(onejudge::CODEX_ALT_HOME_ENV, NOT_THE_ALTERNATE)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("serve spawns");
        let mut stdin = child.stdin.take().expect("a stdin pipe");
        stdin
            .write_all(format!("{frame}\n").as_bytes())
            .expect("the frame is written");
        stdin.flush().expect("flushed");
        (child, held.then_some(stdin))
    }

    fn bus(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let config = self.config();
        run_in(
            self.dir.path(),
            &self.argv(args, config.to_str().expect("a UTF-8 path")),
            stdin,
            &[],
        )
    }

    fn lines(&self, name: &str) -> Vec<Value> {
        std::fs::read_to_string(self.channel().join(name))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }

    fn queued(&self) -> Vec<Value> {
        self.lines("surfaces.jsonl")
            .into_iter()
            .filter(|line| line["event"] == json!("queued"))
            .collect()
    }

    /// The first question `serve` asked, once it has asked one.
    fn asked(&self) -> Value {
        let started = Instant::now();
        loop {
            if let Some(question) = self.queued().into_iter().next() {
                return question;
            }
            assert!(started.elapsed() < GUARD, "serve asked nothing");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn status(&self) -> Value {
        let run = self.bus(&["status", "surfaces"], None);
        assert_eq!(run.code, 0, "{}", run.stderr);
        serde_json::from_str::<Vec<Value>>(&run.stdout).expect("a status list")[0].clone()
    }
}

fn finish(child: Child) -> Run {
    let started = Instant::now();
    let mut child = child;
    while child.try_wait().expect("polled").is_none() {
        if started.elapsed() > GUARD {
            let _ = child.kill();
            panic!("serve never finished");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("serve's output");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).expect("UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("UTF-8"),
    }
}

fn supervisor(messages: Value) -> String {
    json!({
        "op": "supervisor",
        "task": "onepipeline run `r-7`.\nWatch the build and file findings.",
        "persona": "A careful monitor.",
        "done_when": "the watch is kept",
        "worktree": "/repo",
        "history_name": "r-7-monitor",
        "messages": messages,
        "session": "r-7-user"
    })
    .to_string()
}

fn judge(kind: &str) -> String {
    let mut frame = json!({
        "op": "judge",
        "kind": kind,
        "criterion": "the monitor filed every drift it observed as a finding",
        "messages": [{"role": "user", "content": "watch"}, {"role": "assistant", "content": "filed one finding"}],
        "evidence": {"worktree": "/repo", "history_files": ["/state/history/monitor.jsonl"]}
    });
    if kind == "numeric" {
        frame["min"] = json!(0);
        frame["max"] = json!(1);
    }
    frame.to_string()
}

fn one(run: &Run) -> Value {
    let lines = run.lines();
    assert_eq!(
        lines.len(),
        1,
        "stdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
    lines[0].clone()
}

#[test]
fn a_supervisor_frame_with_assistant_content_raises_nothing_and_answers_a_non_completion() {
    let scratch = Scratch::new(30);
    let taken = scratch.serve(
        &[],
        &supervisor(json!([
            {"role": "user", "content": "watch"},
            {"role": "assistant", "content": "nothing drifted this turn"}
        ])),
        &[],
    );
    assert_eq!(taken.code, 0, "{}", taken.stderr);
    assert_eq!(
        one(&taken),
        json!({"completion": false, "message": onejudge::TURN_TAKEN_ACKNOWLEDGED, "reason": onejudge::TURN_TAKEN_REASON})
    );
    assert!(scratch.queued().is_empty(), "a turn taken raised a surface");
}

#[test]
fn a_supervisor_frame_with_no_assistant_content_exits_non_zero_after_one_bounded_monitor_failed_surface(
) {
    let scratch = Scratch::new(30);
    let silent = scratch.serve(
        &[],
        &supervisor(json!([
            {"role": "user", "content": "watch"},
            {"role": "assistant", "content": "   "}
        ])),
        &[],
    );
    assert_ne!(
        silent.code, 0,
        "a silent turn was served: {}",
        silent.stdout
    );
    assert_eq!(silent.code, 1);
    assert_eq!(silent.stdout, "", "a lost turn was answered on stdout");
    assert!(
        silent.stderr.contains(onejudge::NO_ASSISTANT_CONTENT),
        "{}",
        silent.stderr
    );
    let raised = scratch.queued();
    assert_eq!(raised.len(), 1, "{raised:?}");
    assert_eq!(raised[0]["kind"], json!("monitor-failed"));
    assert_eq!(raised[0]["blocking"], json!(false));
    let message = raised[0]["message"].as_str().expect("a message");
    assert!(
        message.contains(onejudge::NO_ASSISTANT_CONTENT)
            && message.contains(onejudge::UNIDENTIFIED_HARNESS)
            && message.contains("r-7"),
        "{message}"
    );
    assert!(message.len() < 400, "the surface is not bounded: {message}");
}

#[test]
fn a_supervisor_frame_ending_in_the_recorded_lost_turn_raises_one_surface_naming_the_cause_and_identity(
) {
    let transcript = std::fs::read_to_string(fixture("recorded/onejudge/lost-turn.jsonl"))
        .expect("the recorded lost turn");
    let frame = supervisor(json!([
        {"role": "user", "content": "Report what has drifted from the plan."},
        {"role": "assistant", "content": "looking"},
        {"role": "assistant", "content": transcript}
    ]));
    for (alternate, identity) in [
        (NOT_THE_ALTERNATE, onejudge::CODEX_IDENTITY),
        (
            "/tmp/lt.c6Y0/codex-home",
            onejudge::CODEX_ALTERNATE_IDENTITY,
        ),
    ] {
        let scratch = Scratch::new(30);
        let lost = scratch.serve(&[], &frame, &[(onejudge::CODEX_ALT_HOME_ENV, alternate)]);
        assert_eq!(lost.code, 1, "{}", lost.stdout);
        assert_eq!(lost.stdout, "");
        let raised = scratch.queued();
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert_eq!(raised[0]["kind"], json!("monitor-failed"));
        let message = raised[0]["message"].as_str().expect("a message");
        assert!(
            message.contains(&format!("monitor turn failed: other on {identity}.")),
            "{message}"
        );
        assert!(
            message.contains(&format!(
                "{}-character transcript",
                transcript.chars().count()
            )),
            "{message}"
        );
        assert!(
            !message.contains("turn/completed") && message.len() < 400,
            "the transcript reached the surface: {message}"
        );
        assert!(
            lost.stderr.contains(&format!("other on {identity}")),
            "{}",
            lost.stderr
        );
    }
}

#[test]
fn a_judge_frame_raises_a_non_blocking_ask_and_the_ruling_that_answers_it_is_the_score() {
    for (ruling, score) in [
        (
            json!({"version": 3, "completion": true, "reason": "every drift was filed"}),
            json!({"value": true, "reason": "every drift was filed"}),
        ),
        (
            json!({"version": 3, "completion": false, "message": "one drift went unfiled"}),
            json!({"value": false, "reason": "one drift went unfiled"}),
        ),
    ] {
        let scratch = Scratch::new(30);
        let (child, _) = scratch.spawn(&["--asker", "monitor-1"], &judge("boolean"), false);
        let question = scratch.asked();
        assert_eq!(question["kind"], json!("monitor-completion"));
        assert_eq!(question["blocking"], json!(false));
        assert_eq!(question["asker"], json!("monitor-1"));
        assert!(
            question["message"]
                .as_str()
                .is_some_and(|message| message.contains("filed every drift it observed")),
            "{question}"
        );
        let correlation = question["correlation"].as_str().expect("a correlation");
        let replied = scratch.bus(
            &["reply", "surfaces", "--correlation", correlation],
            Some(&ruling.to_string()),
        );
        assert_eq!(replied.code, 0, "{}", replied.stderr);
        let served = finish(child);
        assert_eq!(served.code, 0, "{}", served.stderr);
        assert_eq!(one(&served), score);
        assert!(scratch.status()["abandoned"]
            .as_array()
            .expect("a list")
            .is_empty());
    }
}

#[test]
fn a_judge_frame_nobody_rules_on_is_scored_unsatisfied_and_never_a_pass() {
    let scratch = Scratch::new(1);
    let unanswered = scratch.serve(
        &[],
        &judge("boolean"),
        &[("TEST_SERVE_RUN", "r-7"), ("TEST_SERVE_ASKER", "monitor-2")],
    );
    assert_eq!(unanswered.code, 0, "{}", unanswered.stderr);
    let score = one(&unanswered);
    assert_eq!(
        score["value"],
        json!(false),
        "a ruling nobody gave passed: {score}"
    );
    assert!(
        score["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("unsatisfied")),
        "{score}"
    );
    assert!(score.get("rationale").is_none());
    assert!(scratch.lines("replies.jsonl").is_empty());
    assert_eq!(scratch.queued()[0]["asker"], json!("monitor-2"));
}

#[test]
fn assess_a_numeric_judge_and_the_other_ops_are_refused_by_name() {
    let scratch = Scratch::new(30);
    let run = [("TEST_SERVE_RUN", "r-7")];
    let assess =
        json!({"op": "assess", "prompt": "Identify follow-up work.", "messages": []}).to_string();
    for (frame, named) in [
        (assess, "`assess`"),
        (judge("numeric"), "`numeric`"),
        (
            json!({"op": "user", "persona": "A shopper.", "messages": []}).to_string(),
            "`user`",
        ),
        ("{\"op\":\"transmogrify\"}".to_owned(), "`transmogrify`"),
        ("not a frame".to_owned(), "not JSON"),
    ] {
        let refused = scratch.serve(&[], &frame, &run);
        assert_eq!(refused.code, 2, "{frame}: {}", refused.stdout);
        assert!(
            refused.stderr.contains(named),
            "{frame}: {}",
            refused.stderr
        );
        assert_eq!(refused.stdout, "");
    }
    assert!(
        scratch.queued().is_empty(),
        "a refused frame raised something"
    );
}

#[test]
// llmlint: ignore[tests_mirror_real_usage] The invalid state is a commands-only reply the router refuses to deliver here, so this journey writes it directly to the transport file to represent a regressed transport below the public boundary. The behavior under test is driven through the real `serve` binary: it answers a non-completion naming the edits and applies nothing.
fn a_commands_only_reply_injected_onto_the_reply_queue_answers_a_non_completion_naming_the_edits_and_applies_nothing(
) {
    let scratch = Scratch::new(30);
    let (child, _) = scratch.spawn(&[], &judge("boolean"), false);
    let question = scratch.asked();
    // A regressed transport: the edit reaches the reply queue, echoing the
    // question, where the reply router would have kept it on the command path.
    let injected = json!({
        "id": 0,
        "reply": {"version": 3, "message": "stop the build", "commands": [{"op": "cancel", "id": "build"}]},
        "at": 1_789_300_000_000u64,
        "correlation": question["correlation"]
    });
    std::fs::write(
        scratch.channel().join("replies.jsonl"),
        format!("{injected}\n"),
    )
    .expect("the edit is injected");
    let served = finish(child);
    assert_eq!(
        served.code, 0,
        "the member did not survive the edit: {}",
        served.stderr
    );
    let score = one(&served);
    assert_eq!(score["value"], json!(false));
    let reason = score["reason"].as_str().expect("a reason");
    assert!(
        reason.contains("cancel build") && reason.contains("The planner also said: stop the build"),
        "{reason}"
    );
    assert!(
        scratch.lines("commands.jsonl").is_empty(),
        "the edit was re-applied"
    );
}

#[test]
fn a_session_bound_leaves_what_serve_asked_counted_while_a_stream_ending_marks_it_abandoned() {
    let bounded = Scratch::new(1);
    let (child, held) = bounded.spawn(&["--session-seconds", "3"], &judge("boolean"), true);
    let served = finish(child);
    drop(held);
    assert_eq!(served.code, 0, "{}", served.stderr);
    assert_eq!(one(&served)["value"], json!(false));
    assert!(
        served.stderr.contains("bound") && served.stderr.contains("still counted"),
        "{}",
        served.stderr
    );
    let status = bounded.status();
    assert!(
        status["abandoned"].as_array().expect("a list").is_empty(),
        "a session bound abandoned what it asked: {status}"
    );
    assert_eq!(status["unread"], json!(1));

    let ended = Scratch::new(1);
    let (child, _) = ended.spawn(&[], &judge("boolean"), false);
    let served = finish(child);
    assert_eq!(served.code, 0, "{}", served.stderr);
    let status = ended.status();
    assert_eq!(
        status["abandoned"].as_array().expect("a list").len(),
        1,
        "a stream that ended left what it asked counted: {status}"
    );
    assert_eq!(status["unread"], json!(0));

    let from_env = Scratch::new(1);
    let bounded_by_env = {
        let config = from_env.config();
        let mut command = onemessagebus();
        command
            .args([
                "serve",
                "surfaces",
                "--codec",
                "onejudge",
                "--config",
                config.to_str().expect("a UTF-8 path"),
            ])
            .current_dir(from_env.dir.path())
            .env("TEST_SERVE_RUN", "r-7")
            .env("TEST_SERVE_SESSION", "2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("serve spawns");
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(format!("{}\n", judge("boolean")).as_bytes())
            .expect("written");
        let served = finish(child);
        drop(stdin);
        served
    };
    assert_eq!(bounded_by_env.code, 0, "{}", bounded_by_env.stderr);
    assert!(
        bounded_by_env.stderr.contains("2-second bound"),
        "{}",
        bounded_by_env.stderr
    );
}

#[test]
fn serve_refuses_what_it_cannot_serve_before_it_reads_a_frame() {
    let scratch = Scratch::new(30);
    let frame = supervisor(json!([{"role": "assistant", "content": "here"}]));
    let unknown = scratch.serve(&["--codec", "onepipeline"], &frame, &[]);
    assert_eq!(unknown.code, 2, "{}", unknown.stdout);
    let unknown = scratch.bus(
        &["serve", "surfaces", "--codec", "onepipeline"],
        Some(&frame),
    );
    assert_eq!(unknown.code, 2);
    assert!(
        unknown
            .stderr
            .contains("`onepipeline` is not a codec this build links; it links: onejudge"),
        "{}",
        unknown.stderr
    );
    let zero = scratch.serve(&["--session-seconds", "0"], &frame, &[]);
    assert_eq!(zero.code, 2);
    assert!(zero.stderr.contains("--session-seconds"), "{}", zero.stderr);
    let nonsense = scratch.serve(&[], &frame, &[("TEST_SERVE_SESSION", "soon")]);
    assert_eq!(nonsense.code, 2);
    assert!(
        nonsense.stderr.contains("TEST_SERVE_SESSION"),
        "{}",
        nonsense.stderr
    );
    let blank = scratch.serve(&[], &frame, &[("TEST_SERVE_ASKER", "  ")]);
    assert_eq!(blank.code, 2);
    assert!(
        blank.stderr.contains("TEST_SERVE_ASKER"),
        "{}",
        blank.stderr
    );
    let no_run = scratch.serve(&[], &judge("boolean"), &[]);
    assert_eq!(no_run.code, 2, "{}", no_run.stdout);
    assert!(
        no_run.stderr.contains("TEST_SERVE_RUN"),
        "{}",
        no_run.stderr
    );
    for (malformed_frame, problem) in [
        (
            json!({"op": "supervisor", "task": "onepipeline run `r-7`", "persona": "monitor", "worktree": "/repo", "history_name": "r-7-monitor", "messages": [], "sesion": "misspelled"}).to_string(),
            "unknown field",
        ),
        (
            json!({"op": "judge", "kind": "numeric", "criterion": "score it", "messages": []}).to_string(),
            "missing field",
        ),
        (
            json!({"op": "supervisor", "task": "onepipeline run `r-7`", "persona": "monitor", "worktree": "/repo", "history_name": "r-7-monitor", "messages": [{"role": "assistant", "content": "here", "events": [{"kind": "tool_guess", "index": 0}]}]}).to_string(),
            "unknown variant",
        ),
    ] {
        let malformed = scratch.serve(
            &[],
            &malformed_frame,
            &[("TEST_SERVE_RUN", "r-7")],
        );
        assert_eq!(malformed.code, 2, "{malformed_frame}: {}", malformed.stderr);
        assert!(
            malformed.stderr.contains(problem),
            "{malformed_frame}: {}",
            malformed.stderr
        );
        assert_eq!(malformed.stdout, "");
    }
    let undeclared = scratch.bus(&["serve", "findings", "--codec", "onejudge"], Some(&frame));
    assert_eq!(undeclared.code, 2);

    std::fs::write(
        scratch.config(),
        format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: planner-channel\ncodecs:\n  onejudge: {{queue: replies}}\n",
            serde_json::to_string(&scratch.channel()).expect("a path")
        ),
    )
    .expect("written");
    let elsewhere = scratch.serve(&[], &frame, &[]);
    assert_eq!(elsewhere.code, 2);
    assert!(
        elsewhere.stderr.contains("codecs.onejudge.queue"),
        "{}",
        elsewhere.stderr
    );
    assert!(scratch.queued().is_empty());
}

#[cfg(unix)]
#[test]
fn serve_refuses_non_unicode_run_and_harness_identity_environment_values_by_name() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let scratch = Scratch::new(30);
    let frame = supervisor(json!([{"role": "assistant", "content": "here"}]));
    for name in ["TEST_SERVE_RUN", onejudge::CODEX_ALT_HOME_ENV] {
        let config = scratch.config();
        let mut child = onemessagebus()
            .args([
                "serve",
                "surfaces",
                "--codec",
                "onejudge",
                "--config",
                config.to_str().expect("a UTF-8 path"),
            ])
            .current_dir(scratch.dir.path())
            .env(name, OsString::from_vec(vec![0xff]))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("serve spawns");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(format!("{frame}\n").as_bytes())
            .expect("the frame is written");
        let output = child.wait_with_output().expect("serve exits");
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
        assert!(stderr.contains(name), "{stderr}");
        assert!(stderr.contains("cannot read as text"), "{stderr}");
    }
}

#[test]
fn every_onejudge_frame_is_registered_and_rendered_by_the_schema_verbs() {
    for word in onejudge::op::ALL {
        let id = format!("agent.onejudge-frame.{word}@6");
        let rendered = crate::support::run(&["schema", "gen", "--lang", "rust", &id], None);
        assert_eq!(rendered.code, 0, "{id}: {}", rendered.stderr);
        assert!(
            rendered.stdout.contains("pub op: String"),
            "{id}: {}",
            rendered.stdout
        );
        let document = crate::support::run(&["schema", "gen", "--lang", "json", &id], None);
        assert_eq!(document.code, 0, "{id}: {}", document.stderr);
        let schema: Value = serde_json::from_str(&document.stdout).expect("a JSON document");
        assert_eq!(schema["properties"]["op"]["const"], json!(word));
    }
    let checked = crate::support::run(
        &["schema", "check", "agent.onejudge-frame.judge@6"],
        Some(&judge("boolean")),
    );
    assert_eq!(checked.code, 0, "{}", checked.stderr);
}
