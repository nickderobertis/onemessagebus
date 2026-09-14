//! Contract A through the built binary: `ask` and `reply` as separate
//! processes over one channel directory, under the `planner-channel` layout.
//!
//! An `ask` is spawned and left waiting the way a dispatched agent waits; the
//! journey reads the correlation it prints on stderr, answers — or does not —
//! with `reply` from another process, and holds what `ask` printed, how it
//! exited, and what the channel's files say afterwards.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::support::{assert_usage_refused, onemessagebus, run_in, Run};

/// How long a journey gives a waiting `ask` to finish before it is killed —
/// by the handle that started it — and the journey fails.
const GUARD: Duration = Duration::from_secs(60);

struct Scratch {
    dir: tempfile::TempDir,
}

/// An `ask` left running: its child, the correlation it printed, and the rest
/// of what it writes, gathered as it writes it.
struct Asking {
    child: Child,
    correlation: String,
    stdout: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
}

impl Asking {
    /// Wait for the `ask` to exit and hand back what it said.
    fn finish(mut self) -> Run {
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("the ask is waited on") {
                break status;
            }
            if started.elapsed() > GUARD {
                let _ = self.child.kill();
                panic!("the ask on {} never finished", self.correlation);
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Run {
            code: status.code().unwrap_or(-1),
            stdout: self.stdout.recv().unwrap_or_default(),
            stderr: self.stderr.recv().unwrap_or_default(),
        }
    }

    /// Whether the `ask` is still waiting.
    fn waiting(&mut self) -> bool {
        self.child.try_wait().expect("the ask is polled").is_none()
    }
}

impl Scratch {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        }
    }

    fn channel(&self) -> PathBuf {
        self.dir.path().join("channel")
    }

    fn argv<'a>(&'a self, args: &[&'a str], channel: &'a str) -> Vec<&'a str> {
        let mut argv = args.to_vec();
        argv.extend(["--transport-dir", channel]);
        argv
    }

    fn bus(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let channel = self.channel();
        let channel = channel.to_str().expect("a UTF-8 path");
        run_in(self.dir.path(), &self.argv(args, channel), stdin, &[])
    }

    /// Spawn `ask` on the `surfaces` queue with `question` on stdin, and hold it
    /// once it has printed its correlation.
    fn ask(&self, args: &[&str], question: Option<&str>) -> Asking {
        let channel = self.channel();
        let channel = channel.to_str().expect("a UTF-8 path");
        let mut argv = vec!["ask", "surfaces"];
        argv.extend_from_slice(args);
        let mut child = onemessagebus()
            .args(self.argv(&argv, channel))
            .current_dir(self.dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ask spawns");
        {
            let mut stdin = child.stdin.take().expect("a stdin pipe");
            if let Some(question) = question {
                stdin
                    .write_all(question.as_bytes())
                    .expect("the question is written");
            }
        }
        let (out_tx, stdout) = mpsc::channel();
        let mut out = child.stdout.take().expect("a stdout pipe");
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = out.read_to_string(&mut text);
            let _ = out_tx.send(text);
        });
        let (line_tx, lines) = mpsc::channel();
        let (err_tx, stderr) = mpsc::channel();
        let err = child.stderr.take().expect("a stderr pipe");
        std::thread::spawn(move || {
            let mut all = String::new();
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = line_tx.send(line.clone());
                all.push_str(&line);
                all.push('\n');
            }
            let _ = err_tx.send(all);
        });
        let correlation = match lines.recv_timeout(GUARD) {
            Ok(line) => line
                .strip_prefix("correlation: ")
                .unwrap_or_else(|| {
                    let _ = child.kill();
                    panic!("ask's first line on stderr is not its correlation: {line}")
                })
                .to_owned(),
            Err(_) => {
                let _ = child.kill();
                panic!("ask printed no correlation");
            }
        };
        Asking {
            child,
            correlation,
            stdout,
            stderr,
        }
    }

    fn lines(&self, name: &str) -> Vec<Value> {
        read_lines(&self.channel().join(name))
    }

    fn status(&self) -> Value {
        let run = self.bus(&["status", "surfaces"], None);
        assert_eq!(run.code, 0, "{}", run.stderr);
        serde_json::from_str::<Vec<Value>>(&run.stdout).expect("a status list")[0].clone()
    }

    /// Poll `status` until `holds` does, or fail.
    fn until(&self, what: &str, holds: impl Fn(&Value) -> bool) {
        let started = Instant::now();
        while !holds(&self.status()) {
            assert!(started.elapsed() < GUARD, "{what} never came about");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn read_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

fn question(message: &str) -> String {
    json!({"kind": "planner-question", "message": message, "source": "proposal"}).to_string()
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

fn verdict(reason: &str) -> String {
    json!({"version": 3, "completion": true, "reason": reason}).to_string()
}

fn abandoned_ids(status: &Value) -> Vec<Value> {
    status["abandoned"]
        .as_array()
        .expect("an abandoned list")
        .iter()
        .map(|record| record["id"].clone())
        .collect()
}

#[test]
fn an_ask_carries_a_minted_correlation_and_a_reply_echoing_it_answers_that_ask_alone() {
    let scratch = Scratch::new();
    let mut first = scratch.ask(
        &[
            "--blocking",
            "--asker",
            "worker-1",
            "--about",
            "build",
            "--timeout",
            "30",
        ],
        Some(&question("is the base right?")),
    );
    let second = scratch.ask(&["--timeout", "30"], Some(&question("which port?")));
    assert_ne!(first.correlation, second.correlation);
    for correlation in [&first.correlation, &second.correlation] {
        assert!(
            correlation.starts_with("c-") && correlation.len() == 34,
            "not a minted correlation: {correlation}"
        );
    }
    let queued: Vec<Value> = scratch
        .lines("surfaces.jsonl")
        .into_iter()
        .filter(|line| line["event"] == json!("queued"))
        .collect();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0]["correlation"], json!(first.correlation));
    assert_eq!(queued[0]["blocking"], json!(true));
    assert_eq!(queued[0]["asker"], json!("worker-1"));
    assert_eq!(queued[0]["workstream"], json!("build"));
    assert!(queued[0].get("about").is_none(), "{}", queued[0]);
    assert_eq!(queued[1]["correlation"], json!(second.correlation));
    assert_eq!(queued[1]["blocking"], json!(false));

    let replied = scratch.bus(
        &["reply", "surfaces", "--correlation", &second.correlation],
        Some(&verdict("the second one")),
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    let receipt = one(&replied);
    assert_eq!(receipt["correlation"], json!(second.correlation));
    assert_eq!(
        receipt["answered"]["record"]["message"],
        json!("which port?")
    );

    let answered = second.finish();
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    let answer = one(&answered);
    assert_eq!(answer["answer"], json!("reply"));
    assert_eq!(answer["correlation"], receipt["correlation"]);
    assert_eq!(answer["reply"]["reply"]["reason"], json!("the second one"));
    assert_eq!(answer["reply"]["correlation"], receipt["correlation"]);

    std::thread::sleep(Duration::from_millis(500));
    assert!(first.waiting(), "a reply to another ask answered this one");
    let replied = scratch.bus(
        &["reply", "surfaces", "--correlation", &first.correlation],
        Some(&verdict("the first one")),
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    let answered = first.finish();
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    assert_eq!(
        one(&answered)["reply"]["reply"]["reason"],
        json!("the first one")
    );
    assert_eq!(scratch.lines("replies.jsonl").len(), 2);
}

#[test]
fn a_reply_echoing_a_correlation_nothing_pending_holds_is_refused_naming_it_with_nothing_appended()
{
    let scratch = Scratch::new();
    let unknown = "c-00000000000000000000000000000000";
    let refused = scratch.bus(
        &["reply", "surfaces", "--correlation", unknown],
        Some(&verdict("to nobody")),
    );
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    assert!(refused.stderr.contains(unknown), "{}", refused.stderr);
    assert_eq!(refused.stdout, "");
    assert!(scratch.lines("replies.jsonl").is_empty());

    let malformed = scratch.bus(
        &["reply", "surfaces", "--correlation", "not a correlation"],
        Some(&verdict("x")),
    );
    assert_eq!(malformed.code, 2, "{}", malformed.stderr);
    assert!(
        malformed.stderr.contains("--correlation"),
        "{}",
        malformed.stderr
    );

    let asked = scratch.ask(&["--timeout", "30"], Some(&question("once")));
    let bound = scratch.bus(
        &["reply", "surfaces", "--correlation", &asked.correlation],
        Some(&verdict("answered")),
    );
    assert_eq!(bound.code, 0, "{}", bound.stderr);
    let answered_correlation = asked.correlation.clone();
    assert_eq!(asked.finish().code, 0);
    let again = scratch.bus(
        &["reply", "surfaces", "--correlation", &answered_correlation],
        Some(&verdict("answered twice")),
    );
    assert_eq!(
        again.code, 1,
        "an answered ask took a second reply: {}",
        again.stdout
    );
    assert!(
        again.stderr.contains(&answered_correlation),
        "{}",
        again.stderr
    );
    assert_eq!(
        scratch.lines("replies.jsonl").len(),
        1,
        "a refused reply was appended"
    );

    let none = scratch.bus(&["reply", "surfaces"], Some(&verdict("to whichever")));
    assert_eq!(none.code, 1, "{}", none.stdout);
    assert!(none.stderr.contains("no ask is pending"), "{}", none.stderr);

    let one_pending = scratch.ask(&["--timeout", "30"], Some(&question("only me")));
    let two_pending = scratch.ask(&["--timeout", "30"], Some(&question("and me")));
    let ambiguous = scratch.bus(&["reply", "surfaces"], Some(&verdict("to both?")));
    assert_eq!(ambiguous.code, 1, "{}", ambiguous.stdout);
    assert!(
        ambiguous.stderr.contains(&one_pending.correlation)
            && ambiguous.stderr.contains(&two_pending.correlation),
        "{}",
        ambiguous.stderr
    );
    assert_eq!(scratch.lines("replies.jsonl").len(), 1);
    let first = scratch.bus(
        &[
            "reply",
            "surfaces",
            "--correlation",
            &two_pending.correlation,
        ],
        Some(&verdict("you")),
    );
    assert_eq!(first.code, 0, "{}", first.stderr);
    let only = scratch.bus(&["reply", "surfaces"], Some(&verdict("the one left")));
    assert_eq!(only.code, 0, "{}", only.stderr);
    assert_eq!(one(&only)["correlation"], json!(one_pending.correlation));
    assert_eq!(
        one(&one_pending.finish())["reply"]["reply"]["reason"],
        json!("the one left")
    );
    assert_eq!(
        one(&two_pending.finish())["reply"]["reply"]["reason"],
        json!("you")
    );
}

#[test]
fn a_wait_that_elapses_answers_timeout_names_no_reply_and_leaves_the_question_standing() {
    let scratch = Scratch::new();
    let other = scratch.ask(&["--timeout", "30"], Some(&question("answered elsewhere")));
    let replied = scratch.bus(
        &["reply", "surfaces", "--correlation", &other.correlation],
        Some(&verdict("not yours")),
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    assert_eq!(other.finish().code, 0);
    let replies_before = std::fs::read(scratch.channel().join("replies.jsonl")).expect("replies");

    let elapsed = scratch
        .ask(
            &["--asker", "worker-1", "--timeout", "1"],
            Some(&question("anyone?")),
        )
        .finish();
    assert_eq!(elapsed.code, 1, "{}", elapsed.stdout);
    let answer = one(&elapsed);
    assert_eq!(answer["answer"], json!("timeout"));
    assert!(
        answer.get("reply").is_none(),
        "a timeout carried a reply member: {answer}"
    );
    let correlation = answer["correlation"]
        .as_str()
        .expect("a correlation")
        .to_owned();
    assert!(
        elapsed.stderr.contains("the question stands"),
        "{}",
        elapsed.stderr
    );
    assert_eq!(
        std::fs::read(scratch.channel().join("replies.jsonl")).expect("replies"),
        replies_before,
        "the wait appended to the reply queue, or took the other ask's reply"
    );
    let status = scratch.status();
    let standing: Vec<&Value> = status["waiting"]
        .as_array()
        .expect("a waiting list")
        .iter()
        .filter(|record| record["correlation"] == json!(correlation))
        .collect();
    assert_eq!(standing.len(), 1, "the question does not stand: {status}");
    assert_eq!(
        standing[0]["abandoned"],
        json!(true),
        "an ask that ended without its answer left it counted"
    );

    let answered_late = scratch.bus(
        &["reply", "surfaces", "--correlation", &correlation],
        Some(&verdict("late, but yours")),
    );
    assert_eq!(
        answered_late.code, 0,
        "an answer for an abandoned question was not matched to it: {}",
        answered_late.stderr
    );
}

#[test]
fn a_listener_abandoned_and_never_reattended_answers_abandoned_and_no_reply() {
    let scratch = Scratch::new();
    let lost = scratch
        .ask(
            &["--asker", "worker-1", "--timeout", "1"],
            Some(&question("still there?")),
        )
        .finish();
    assert_eq!(lost.code, 1);
    let correlation = one(&lost)["correlation"]
        .as_str()
        .expect("a correlation")
        .to_owned();
    let id = scratch.lines("surfaces.jsonl")[0]["id"].clone();
    assert_eq!(abandoned_ids(&scratch.status()), vec![id.clone()]);

    for listener in [vec![], vec!["--asker", "someone-else"]] {
        let mut args = vec!["--correlation", correlation.as_str(), "--timeout", "30"];
        args.extend(listener.iter().copied());
        let started = Instant::now();
        let answered = scratch.ask(&args, None).finish();
        assert_eq!(answered.code, 1, "{}", answered.stdout);
        let answer = one(&answered);
        assert_eq!(
            answer,
            json!({"answer": "abandoned", "correlation": correlation})
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "an abandoned question was waited on until the bound"
        );
        assert!(answered.stderr.contains("abandoned"), "{}", answered.stderr);
    }
    assert!(scratch.lines("replies.jsonl").is_empty());
    assert_eq!(
        abandoned_ids(&scratch.status()),
        vec![id],
        "a listener that attends nothing changed what is abandoned"
    );

    let stranger = "c-11111111111111111111111111111111";
    let unknown = scratch.bus(
        &[
            "ask",
            "surfaces",
            "--correlation",
            stranger,
            "--timeout",
            "1",
        ],
        None,
    );
    assert_eq!(unknown.code, 1, "{}", unknown.stdout);
    assert_eq!(
        unknown.stdout, "",
        "a listener for no question printed an answer"
    );
    assert!(
        unknown.stderr.contains(stranger),
        "the refusal does not name the correlation: {}",
        unknown.stderr
    );
}

#[test]
fn rearm_after_a_lost_wait_receives_the_eventual_reply() {
    let scratch = Scratch::new();
    let lost = scratch
        .ask(
            &["--blocking", "--asker", "worker-1", "--timeout", "1"],
            Some(&question("which base?")),
        )
        .finish();
    assert_eq!(lost.code, 1);
    let correlation = one(&lost)["correlation"]
        .as_str()
        .expect("a correlation")
        .to_owned();
    assert_eq!(abandoned_ids(&scratch.status()).len(), 1);

    let mut rearmed = scratch.ask(
        &[
            "--correlation",
            &correlation,
            "--asker",
            "worker-1",
            "--timeout",
            "30",
        ],
        None,
    );
    assert_eq!(rearmed.correlation, correlation);
    scratch.until("the re-armed question is attended", |status| {
        abandoned_ids(status).is_empty()
    });
    assert!(rearmed.waiting(), "the re-armed listener did not wait");
    let claimed = scratch.bus(&["next", "surfaces"], None);
    assert_eq!(claimed.code, 0, "{}", claimed.stderr);
    assert_eq!(one(&claimed)["record"]["correlation"], json!(correlation));

    let replied = scratch.bus(
        &["reply", "surfaces", "--correlation", &correlation],
        Some(&verdict("main")),
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    let answered = rearmed.finish();
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    let answer = one(&answered);
    assert_eq!(answer["answer"], json!("reply"));
    assert_eq!(answer["reply"]["reply"]["reason"], json!("main"));
    assert_eq!(
        scratch.status()["pending"],
        Value::Null,
        "the slot was not released"
    );
}

#[test]
fn a_question_the_bus_refuses_answers_refused_with_no_reply_and_appends_nothing() {
    let scratch = Scratch::new();
    // Well-formed, and a no: a surface its schema refuses for naming no kind.
    let refused = scratch.bus(
        &["ask", "surfaces"],
        Some(&json!({"message": "is the base right?", "source": "proposal"}).to_string()),
    );
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    let answer = one(&refused);
    assert_eq!(answer["answer"], json!("refused"));
    assert!(
        answer.get("reply").is_none() && answer.get("correlation").is_none(),
        "a refused question carried a reply or a correlation: {answer}"
    );
    let reason = answer["reason"].as_str().expect("a reason");
    assert!(
        reason.contains("agent.planner-surface@1") && reason.contains("\"kind\""),
        "{reason}"
    );
    assert!(
        !refused.stderr.contains("correlation:"),
        "a refused question printed a correlation: {}",
        refused.stderr
    );
    assert!(
        scratch.lines("surfaces.jsonl").is_empty(),
        "a refused question was appended"
    );
}

/// Refused input, as the exit-code table gives it: exit 2, nothing on stdout,
/// and the problem on stderr as `onemessagebus: <what is wrong>`.
fn assert_refused_input(run: &Run, problem: &str) {
    assert_eq!(
        run.code, 2,
        "{problem}: stdout {}\nstderr {}",
        run.stdout, run.stderr
    );
    assert_eq!(run.stdout, "", "{problem}");
    assert!(
        run.stderr.starts_with("onemessagebus: ") && run.stderr.contains(problem),
        "{problem}: {}",
        run.stderr
    );
    assert!(
        !run.stderr
            .lines()
            .any(|line| line.starts_with("correlation: ")),
        "refused input printed a correlation: {}",
        run.stderr
    );
}

#[test]
fn ask_refuses_input_it_cannot_take_with_exit_two_and_raises_nothing() {
    let scratch = Scratch::new();
    let asked = question("which base?");
    let too_long = format!("c-{}", "0".repeat(200));
    for (args, stdin, problem) in [
        (
            vec!["ask", "surfaces", "--timeout", "1"],
            r#"["is", "the", "base", "right?"]"#,
            "onemessagebus: surfaces: a record on this queue is a JSON object, and this is an array",
        ),
        (
            vec!["ask", "surfaces", "--timeout", "1"],
            r#""is the base right?""#,
            "onemessagebus: surfaces: a record on this queue is a JSON object, and this is a string",
        ),
        (
            vec!["ask", "surfaces", "--timeout", "1"],
            "is the base right?",
            "onemessagebus: the payload is not JSON",
        ),
        (
            vec!["ask", "surfaces", "--correlation", "not a correlation"],
            "",
            "onemessagebus: --correlation: \"not a correlation\" is not a correlation",
        ),
        (
            vec!["ask", "surfaces", "--correlation", too_long.as_str()],
            "",
            "onemessagebus: --correlation:",
        ),
        (
            vec!["ask", "surfaces", "--about", " ", "--timeout", "1"],
            asked.as_str(),
            "onemessagebus: --about:",
        ),
        (
            vec!["ask", "surfaces", "--timeout", "soon"],
            asked.as_str(),
            "onemessagebus: ask: invalid value 'soon' for '--timeout <SECONDS>'",
        ),
    ] {
        let refused = scratch.bus(&args, Some(stdin));
        assert_refused_input(&refused, problem);
    }
    assert!(
        scratch.lines("surfaces.jsonl").is_empty(),
        "refused input raised a question"
    );
}

#[test]
fn reply_refuses_input_it_cannot_take_with_exit_two_appending_nothing_and_the_ask_stands() {
    let scratch = Scratch::new();
    let mut asking = scratch.ask(&["--timeout", "30"], Some(&question("which base?")));
    let correlation = asking.correlation.clone();
    let answer = verdict("main");
    let too_long = format!("c-{}", "0".repeat(200));
    for (args, stdin, problem) in [
        // An array once read as an envelope field by field — `[3]` as version 3 —
        // and answered the ask with it.
        (
            vec!["reply", "surfaces", "--correlation", correlation.as_str()],
            "[3]",
            "onemessagebus: replies: a record on this queue is a JSON object, and this is an array",
        ),
        (
            vec!["reply", "surfaces"],
            r#""main""#,
            "onemessagebus: replies: a record on this queue is a JSON object, and this is a string",
        ),
        (
            vec!["reply", "surfaces", "--correlation", correlation.as_str()],
            r#"{"version": 3,"#,
            "onemessagebus: the payload is not JSON",
        ),
        (
            vec!["reply", "surfaces", "--correlation", too_long.as_str()],
            answer.as_str(),
            "onemessagebus: --correlation:",
        ),
        (
            vec![
                "reply",
                "surfaces",
                "0",
                "--correlation",
                correlation.as_str(),
            ],
            answer.as_str(),
            "onemessagebus: reply: the argument '[POSITION]' cannot be used with '--correlation <CORRELATION>'",
        ),
    ] {
        let refused = scratch.bus(&args, Some(stdin));
        assert_refused_input(&refused, problem);
    }
    assert!(
        scratch.lines("replies.jsonl").is_empty(),
        "a refused reply was appended"
    );
    assert!(
        scratch.lines("commands.jsonl").is_empty(),
        "a refused reply was routed"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(asking.waiting(), "refused input answered the ask");

    let replied = scratch.bus(
        &["reply", "surfaces", "--correlation", &correlation],
        Some(&answer),
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    let answered = asking.finish();
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    assert_eq!(one(&answered)["reply"]["reply"]["reason"], json!("main"));
}

#[test]
fn ask_refuses_a_usage_error_on_one_line_with_exit_two_and_nothing_on_stdout() {
    let scratch = Scratch::new();
    let asked = question("which base?");
    for (args, what) in [
        (
            vec!["ask", "surfaces", "--timeout", "soon"],
            "invalid value 'soon' for '--timeout <SECONDS>': invalid digit found in string",
        ),
        (
            vec!["ask"],
            "the following required arguments were not provided: <QUEUE>",
        ),
        (
            vec!["ask", "surfaces", "--bogus"],
            "unexpected argument '--bogus' found; tip: a similar argument exists: '--about'",
        ),
    ] {
        assert_usage_refused(&scratch.bus(&args, Some(&asked)), "ask", what);
    }
    assert!(
        scratch.lines("surfaces.jsonl").is_empty(),
        "a usage error raised a question"
    );
    // Asking for help is no usage error: it is answered on stdout at exit 0.
    let help = scratch.bus(&["ask", "--help"], None);
    assert_eq!(help.code, 0, "{}", help.stderr);
    // clap names the program as it was invoked, `onemessagebus.exe` on Windows.
    assert!(
        ["Usage: onemessagebus ask", "Usage: onemessagebus.exe ask"]
            .iter()
            .any(|usage| help.stdout.contains(usage)),
        "{}",
        help.stdout
    );
    assert_eq!(help.stderr, "");
}

#[test]
fn reply_refuses_a_usage_error_on_one_line_with_exit_two_appending_nothing() {
    let scratch = Scratch::new();
    let answer = verdict("main");
    for (args, what) in [
        (
            vec!["reply", "surfaces", "0", "--correlation", "c-1"],
            "the argument '[POSITION]' cannot be used with '--correlation <CORRELATION>'",
        ),
        (
            vec!["reply"],
            "the following required arguments were not provided: <QUEUE>",
        ),
        (
            vec!["reply", "surfaces", "--position", "0"],
            "unexpected argument '--position' found",
        ),
    ] {
        assert_usage_refused(&scratch.bus(&args, Some(&answer)), "reply", what);
    }
    assert!(
        scratch.lines("replies.jsonl").is_empty(),
        "a usage error appended a reply"
    );
}

#[test]
// llmlint: ignore[tests_mirror_real_usage] The invalid state is a reply the CLI schema boundary refuses to produce, so this journey writes it directly to the transport file to represent hand-edited or older storage. The behavior under test is still driven through the real `ask` binary: it answers Refused naming the schema id and pointer.
fn a_reply_record_its_schema_refuses_answers_refused_naming_the_id_and_pointer() {
    let scratch = Scratch::new();
    let lost = scratch
        .ask(&["--timeout", "1"], Some(&question("yes or no?")))
        .finish();
    let correlation = one(&lost)["correlation"]
        .as_str()
        .expect("a correlation")
        .to_owned();
    // A writer that bypassed the queue's schema — an older build, a hand edit.
    let forged =
        json!({"id": 0, "reply": {"completion": "yes"}, "at": 1, "correlation": correlation});
    std::fs::write(
        scratch.channel().join("replies.jsonl"),
        format!("{forged}\n"),
    )
    .expect("the forged reply is written");
    let answered = scratch
        .ask(&["--correlation", &correlation, "--timeout", "5"], None)
        .finish();
    assert_eq!(answered.code, 1, "{}", answered.stdout);
    let answer = one(&answered);
    assert_eq!(answer["answer"], json!("refused"));
    assert!(
        answer.get("reply").is_none(),
        "a refusal carried a reply: {answer}"
    );
    let reason = answer["reason"].as_str().expect("a reason");
    assert!(
        reason.contains("agent.queued-reply@1") && reason.contains("/reply/completion"),
        "the refusal does not name the id and the pointer: {reason}"
    );
}

#[test]
fn a_reply_carrying_only_commands_leaves_the_ask_standing_and_one_with_a_verdict_answers_it() {
    let scratch = Scratch::new();
    let mut asking = scratch.ask(&["--timeout", "30"], Some(&question("go on?")));
    let edits = scratch.bus(
        &["reply", "surfaces", "--correlation", &asking.correlation],
        Some(r#"{"version":3,"commands":[{"op":"cancel","id":"build"}]}"#),
    );
    assert_eq!(edits.code, 0, "{}", edits.stderr);
    let receipt = one(&edits);
    assert_eq!(receipt["answered"], Value::Null);
    assert_eq!(scratch.lines("commands.jsonl").len(), 1);
    assert!(scratch.lines("replies.jsonl").is_empty());
    std::thread::sleep(Duration::from_millis(500));
    assert!(asking.waiting(), "an edit with no verdict answered the ask");

    let both = scratch.bus(
        &["reply", "surfaces", "--correlation", &asking.correlation],
        Some(r#"{"version":3,"completion":false,"message":"keep going","commands":[{"op":"retry","id":"build","node":{"id":"build-2"}}]}"#),
    );
    assert_eq!(both.code, 0, "{}", both.stderr);
    assert_eq!(scratch.lines("commands.jsonl").len(), 2);
    let answered = asking.finish();
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    assert_eq!(
        one(&answered)["reply"]["reply"]["message"],
        json!("keep going")
    );
}
