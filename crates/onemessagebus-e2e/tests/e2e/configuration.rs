//! What a queue verb opens its queues with, through the built binary.
//!
//! A queue verb opens a configuration — `--config`, or `ONEMESSAGEBUS_CONFIG` —
//! naming the transport and the layout its queues keep, and nothing else: given
//! none it is refused naming `--config`, and a transport directory alone
//! (`--transport-dir`, `ONEMESSAGEBUS_TRANSPORT_DIR`) implies no layout. Nor does
//! any variable of another program's reach a verb: the same sequence over the
//! desk runs byte for byte alike with `onepipeline`'s run variables set and
//! unset.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::json;

use crate::support::{desk_config, onemessagebus, Run};

/// The refusal every queue verb gives when it has no configuration.
const NO_CONFIGURATION: &str = "no configuration to open a queue with: pass --config <path> (or set ONEMESSAGEBUS_CONFIG) naming the transport and the layout its queues keep; --transport-dir only moves a configuration's transport";

/// The variables `onepipeline` sets for a run it launches. The bus reads none of
/// them; the ambient journey sets them to prove it.
const ONEPIPELINE_RUN: [&str; 3] = [
    "ONEPIPELINE_RUN_ID",
    "ONEPIPELINE_CHANNEL_ASKER",
    "ONEPIPELINE_SERVE_SESSION_SECONDS",
];

/// Plausible values for [`ONEPIPELINE_RUN`], in order.
const ONEPIPELINE_VALUES: [&str; 3] = ["r-ambient", "someone-else", "1"];

/// Run `args` from `cwd` with `stdin` and `env` set, and with every variable
/// of [`ONEPIPELINE_RUN`] removed unless `env` sets it.
fn run(cwd: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Run {
    let mut command = onemessagebus();
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in ONEPIPELINE_RUN {
        command.env_remove(name);
    }
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("the binary spawns");
    {
        let mut handle = child.stdin.take().expect("a stdin pipe");
        if let Some(text) = stdin {
            // A verb refused before it reads stdin closes the pipe unread.
            match handle.write_all(text.as_bytes()) {
                Ok(()) => {}
                Err(failure) if failure.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(failure) => panic!("stdin is written: {failure}"),
            }
        }
    }
    let output = child.wait_with_output().expect("the binary exits");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}

/// Every entry under `dir`, relative to it, sorted.
fn entries(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, into: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("the directory is read") {
            let path = entry.expect("an entry").path();
            into.push(
                path.strip_prefix(root)
                    .expect("under the root")
                    .display()
                    .to_string(),
            );
            if path.is_dir() {
                walk(root, &path, into);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

/// Each queue verb, with the arguments and input a user would give it.
fn queue_verbs() -> Vec<(Vec<&'static str>, Option<&'static str>)> {
    let question =
        r#"{"kind": "question", "message": "which base?", "source": "proposal", "blocking": true}"#;
    vec![
        (vec!["send", "questions"], Some(question)),
        (vec!["next", "questions"], None),
        (vec!["ask", "questions", "--timeout", "1"], Some(question)),
        (
            vec!["reply", "questions", "0"],
            Some(r#"{"message": "main"}"#),
        ),
        (
            vec![
                "subscribe",
                "questions",
                "--until",
                r#"{"field": "event", "equals": "answered"}"#,
                "--timeout",
                "1",
            ],
            None,
        ),
        (vec!["status"], None),
        (vec!["validate", "answers"], Some(r#"{"message": "main"}"#)),
        (
            vec!["serve", "questions", "--codec", "desk"],
            Some("{\"op\": \"raise\", \"id\": 1, \"applied\": true}\n"),
        ),
    ]
}

#[test]
fn a_queue_verb_given_no_configuration_is_refused_naming_config_and_creates_nothing() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let moved = scratch.path().join("moved");
    let moved = moved.to_str().expect("a UTF-8 path");
    let socket = scratch.path().join("bus.sock");
    let socket = socket.to_str().expect("a UTF-8 path");

    let mut verbs = queue_verbs();
    // The resident is refused before it listens, given nothing at all as given a
    // transport directory alone.
    verbs.push((vec!["serve", "--resident", "--socket", socket], None));
    for (given, flags, env) in [
        ("nothing", vec![], vec![]),
        (
            "--transport-dir alone",
            vec!["--transport-dir", moved],
            vec![],
        ),
        (
            "ONEMESSAGEBUS_TRANSPORT_DIR alone",
            vec![],
            vec![("ONEMESSAGEBUS_TRANSPORT_DIR", moved)],
        ),
    ] {
        for (args, stdin) in &verbs {
            let mut argv = args.clone();
            argv.extend(flags.iter().copied());
            let refused = run(scratch.path(), &argv, *stdin, &env);
            let what = format!("`{}` given {given}", argv.join(" "));
            assert_eq!(refused.code, 2, "{what}: {}", refused.stderr);
            assert_eq!(refused.stdout, "", "{what}");
            assert_eq!(
                refused.stderr,
                format!("onemessagebus: {NO_CONFIGURATION}\n"),
                "{what}"
            );
            assert_eq!(
                entries(scratch.path()),
                Vec::<String>::new(),
                "{what} created something"
            );
        }
    }

    #[cfg(unix)]
    a_configured_resident_answers_requests_and_one_naming_its_own_configuration();
}

/// A resident started with a configuration listens, answers a queue request
/// over it, and answers one naming a configuration of its own over that one.
#[cfg(unix)]
fn a_configured_resident_answers_requests_and_one_naming_its_own_configuration() {
    use serde_json::Value;
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    let scratch = tempfile::tempdir().expect("a scratch directory");
    let started = scratch.path().join("started");
    let named = scratch.path().join("named");
    std::fs::create_dir_all(&started).expect("a directory");
    std::fs::create_dir_all(&named).expect("a directory");
    let started_config = desk_config(&started, "");
    let named_config = desk_config(&named, "queues:\n  findings: {}\n");
    let socket = scratch.path().join("bus.sock");
    let mut child = onemessagebus()
        .args(["serve", "--resident", "--socket"])
        .arg(&socket)
        .arg("--config")
        .arg(&started_config)
        .current_dir(scratch.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the resident spawns");
    let deadline = Instant::now() + Duration::from_secs(20);
    let stream = loop {
        if let Ok(stream) = UnixStream::connect(&socket) {
            break stream;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the configured resident never listened");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    let mut writer = stream.try_clone().expect("a write half");
    let mut reader = BufReader::new(stream);
    let named_text = named_config.to_str().expect("a UTF-8 path");
    for (id, args, queues) in [
        (
            0,
            json!({}),
            vec!["actions", "answers", "outcomes", "questions"],
        ),
        (
            1,
            json!({"config": named_text}),
            vec!["actions", "answers", "findings", "outcomes", "questions"],
        ),
    ] {
        let request = json!({"id": id, "verb": "status", "args": args});
        writer
            .write_all(format!("{request}\n").as_bytes())
            .expect("the request is written");
        let mut line = String::new();
        reader.read_line(&mut line).expect("the resident answers");
        let answer: Value = serde_json::from_str(&line).expect("a JSON line");
        let answered: Vec<&Value> = answer["ok"]
            .as_array()
            .unwrap_or_else(|| panic!("request {id} was not answered: {answer}"))
            .iter()
            .map(|status| &status["queue"])
            .collect();
        assert_eq!(
            answered,
            queues
                .iter()
                .map(|queue| json!(queue))
                .collect::<Vec<_>>()
                .iter()
                .collect::<Vec<_>>(),
            "request {id}"
        );
    }
    drop((writer, reader));
    // Stopped by removing its socket, which ends it as a normal exit.
    std::fs::remove_file(&socket).expect("the socket is removed");
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the resident is polled") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the resident did not stop when its socket was removed");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0));
}

/// One scratch directory holding a desk configuration with a codec, and what a
/// run over it printed and left, with only what legitimately differs between
/// two runs normalised.
struct Desk {
    dir: tempfile::TempDir,
    config: PathBuf,
}

impl Desk {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let config = desk_config(
            dir.path(),
            "codecs:\n  desk:\n    queue: questions\n    select: op\n    frames:\n      raise:\n        schema: desk.outcome@1\n        bindings:\n          - do: raise\n            record: {kind: served, source: codec, blocking: false, message: \"raised {frame.id}\"}\n            response: {raised: \"{frame.id}\"}\n",
        );
        Self { dir, config }
    }

    fn run(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Run {
        let mut argv = args.to_vec();
        argv.extend(["--config", self.config.to_str().expect("a UTF-8 path")]);
        run(self.dir.path(), &argv, stdin, env)
    }

    /// `text` with this scratch directory's path, the timestamps a queue stamps,
    /// the correlations `ask` mints, and the projection's seal — a digest over
    /// the projection, those stamped values among it, whose every other member is
    /// compared as it is — replaced by placeholders.
    fn normalised(&self, text: &str) -> String {
        let text = text.replace(self.dir.path().to_str().expect("a UTF-8 path"), "<scratch>");
        let text = digits_after(&text, "\"raised_at\":");
        let text = digits_after(&text, "\"at\":");
        let text = seal(&text);
        correlations(&text)
    }

    /// Every file under the queue directory, by name, normalised.
    fn files(&self) -> Vec<(String, String)> {
        let bus = self.dir.path().join("bus");
        entries(&bus)
            .into_iter()
            .map(|name| {
                let path = bus.join(&name);
                if path.is_dir() {
                    return (name, "<directory>".to_owned());
                }
                let text = std::fs::read_to_string(path).expect("a queue file");
                (name, self.normalised(&text))
            })
            .collect()
    }
}

/// `text` with the run of digits after each `key` (and any spaces) replaced by
/// `<ms>`. A key followed by anything but digits is left as it is.
fn digits_after(text: &str, key: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(key) {
        let (before, after) = rest.split_at(at + key.len());
        out.push_str(before);
        let spaces = after.len() - after.trim_start_matches(' ').len();
        let (gap, value) = after.split_at(spaces);
        let digits = value.len() - value.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        out.push_str(gap);
        if digits > 0 {
            out.push_str("<ms>");
        }
        rest = &value[digits..];
    }
    out.push_str(rest);
    out
}

/// `text` with the value of a projection's `"seal"` member — 32 lowercase hex
/// digits — replaced by `<seal>`. Any other value is left as it is.
fn seal(text: &str) -> String {
    const KEY: &str = "\"seal\": \"";
    let Some(at) = text.find(KEY) else {
        return text.to_owned();
    };
    let (before, after) = text.split_at(at + KEY.len());
    let digest = after.get(..32).filter(|digest| {
        digest
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
            && after[32..].starts_with('"')
    });
    match digest {
        Some(_) => format!("{before}<seal>{}", &after[32..]),
        None => text.to_owned(),
    }
}

/// `text` with every minted correlation — `c-` and 32 lowercase hex digits —
/// replaced by `c-<correlation>`.
fn correlations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("c-") {
        let (before, after) = rest.split_at(at);
        out.push_str(before);
        let hex = &after[2..];
        let minted = hex.len() >= 32
            && hex[..32]
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        if minted {
            out.push_str("c-<correlation>");
            rest = &hex[32..];
        } else {
            out.push_str("c-");
            rest = hex;
        }
    }
    out.push_str(rest);
    out
}

/// What one pass of the sequence printed, step by step, and left.
type Transcript = (Vec<(String, i32, String, String)>, Vec<(String, String)>);

/// A realistic pass over the desk: raise a question, claim it, answer it by
/// position, send an answer with both halves, read the status, subscribe until
/// the answer lands, ask a question nobody answers, validate an answer, and
/// serve one frame through a codec.
fn sequence(desk: &Desk, env: &[(&str, &str)]) -> Transcript {
    let mut steps = Vec::new();
    let mut step = |name: &str, args: &[&str], stdin: Option<&str>| -> Run {
        let run = desk.run(args, stdin, env);
        steps.push((
            name.to_owned(),
            run.code,
            desk.normalised(&run.stdout),
            desk.normalised(&run.stderr),
        ));
        run
    };
    let sent = step(
        "send a question",
        &["send", "questions"],
        Some(
            r#"{"kind": "question", "message": "which base?", "source": "proposal", "blocking": true, "about": "build"}"#,
        ),
    );
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    let claimed = step("next", &["next", "questions"], None);
    assert_eq!(claimed.code, 0, "{}", claimed.stderr);
    let position = claimed.lines()[0]["position"].to_string();
    let replied = step(
        "reply by position",
        &["reply", "questions", &position],
        Some(r#"{"completion": false, "message": "main"}"#),
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    let both = step(
        "send an answer with both halves",
        &["send", "answers"],
        Some(
            r#"{"version": 3, "completion": false, "message": "go on", "actions": [{"op": "retry", "node": "build"}]}"#,
        ),
    );
    assert_eq!(both.code, 0, "{}", both.stderr);
    let status = step("status", &["status"], None);
    assert_eq!(status.code, 0, "{}", status.stderr);
    let subscribed = step(
        "subscribe --until",
        &[
            "subscribe",
            "answers",
            "--until",
            r#"{"field": "reply.message", "equals": "go on"}"#,
            "--timeout",
            "5",
        ],
        None,
    );
    assert_eq!(subscribed.code, 0, "{}", subscribed.stderr);
    let asked = step(
        "ask, unanswered",
        &["ask", "questions", "--timeout", "1"],
        Some(r#"{"kind": "question", "message": "anyone?", "source": "proposal"}"#),
    );
    assert_eq!(asked.code, 1, "{}", asked.stderr);
    assert_eq!(asked.lines()[0]["answer"], json!("timeout"));
    let validated = step(
        "validate",
        &["validate", "answers"],
        Some(r#"{"message": "fine"}"#),
    );
    assert_eq!(validated.code, 0, "{}", validated.stderr);
    let served = step(
        "serve a frame",
        &["serve", "questions", "--codec", "desk"],
        Some("{\"op\": \"raise\", \"id\": 7, \"applied\": true}\n"),
    );
    assert_eq!(served.code, 0, "{}", served.stderr);
    (steps, desk.files())
}

#[test]
fn onepipelines_run_variables_change_no_verbs_behaviour() {
    let quiet = Desk::new();
    let unset = sequence(&quiet, &[]);
    let ambient = Desk::new();
    let env: Vec<(&str, &str)> = ONEPIPELINE_RUN
        .iter()
        .copied()
        .zip(ONEPIPELINE_VALUES)
        .collect();
    let set = sequence(&ambient, &env);

    for (without, with) in unset.0.iter().zip(&set.0) {
        assert_eq!(
            without, with,
            "`{}` behaved differently with {env:?} set",
            without.0
        );
    }
    assert_eq!(unset.0.len(), set.0.len());
    assert_eq!(unset.1, set.1, "the queue files differ with {env:?} set");
    // The normalisation hides nothing it should not: every queue is there, and
    // the values it replaced were really written.
    let names: Vec<&str> = unset.1.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            ".lock",
            ".lock/actions.lock",
            ".lock/answers.lock",
            ".lock/questions.lock",
            "actions.jsonl",
            "answers.jsonl",
            "questions.json",
            "questions.jsonl"
        ]
    );
    let projection = &unset.1[6].1;
    assert!(projection.contains("\"seal\": \"<seal>\""), "{projection}");
    let answers = &unset.1[5].1;
    assert!(answers.contains("\"at\":<ms>"), "{answers}");
    let questions = &unset.1[7].1;
    assert!(questions.contains("\"raised_at\":<ms>"), "{questions}");
    assert!(
        questions.contains("\"correlation\":\"c-<correlation>\""),
        "{questions}"
    );
    assert!(!questions.contains("asker"), "{questions}");
    for (_, _, stdout, stderr) in &set.0 {
        for value in ONEPIPELINE_VALUES.iter().filter(|value| value.len() > 1) {
            assert!(
                !stdout.contains(value) && !stderr.contains(value),
                "a verb printed {value}: {stdout}{stderr}"
            );
        }
    }
}
