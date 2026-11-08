//! Hooks through the built binary (`docs/hooks.md`): a configured command that
//! receives each offered message before anything is appended, and whose answer
//! keeps, changes, reroutes, drops or refuses it.
//!
//! Every hook here is a real shell script the binary launches as a subprocess.
//! What a queue and its `-dropped` log hold afterwards is read back from the
//! transport's files as raw bytes, so a hook's spelling is compared exactly,
//! never as a parsed value.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::support::{desk_bundle, run_in, Run};

/// The bundle the journeys link for a layout whose preparation stamps a record.
fn hooks_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/layouts/hooks.json")
}

/// A scratch directory: a queue directory, the hook scripts, the markers they
/// write, and a configuration.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A path inside the scratch directory, as shell text.
    fn at(&self, name: &str) -> String {
        self.path(name).to_str().expect("a UTF-8 path").to_owned()
    }

    /// Write an executable `sh` script named `name` whose body is `body`, and
    /// answer its path.
    fn script(&self, name: &str, body: &str) -> String {
        let path = self.path(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("the script is written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("the script is executable");
        }
        path.to_str().expect("a UTF-8 path").to_owned()
    }

    /// Write `onemessagebus.yaml` with `rest` after the transport.
    fn configure(&self, rest: &str) {
        std::fs::write(
            self.path("onemessagebus.yaml"),
            format!(
                "version: 1\ntransport: {{kind: local, dir: {}}}\n{rest}",
                serde_json::to_string(&self.path("bus")).expect("a path")
            ),
        )
        .expect("the configuration is written");
    }

    /// The configuration lines naming the `stamped` layout and linking it.
    fn stamped(&self) -> String {
        format!(
            "profile: stamped\nschemas:\n  - {}\n",
            serde_json::to_string(&format!("{}@1", hooks_bundle().display())).expect("a path")
        )
    }

    /// The configuration lines naming the bus's own `desk` layout.
    fn desk(&self) -> String {
        format!(
            "profile: desk\nschemas:\n  - {}\n",
            serde_json::to_string(&format!("{}@1", desk_bundle().display())).expect("a path")
        )
    }

    fn bus(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let config = self.path("onemessagebus.yaml");
        let mut argv = args.to_vec();
        argv.extend(["--config", config.to_str().expect("a UTF-8 path")]);
        run_in(self.dir.path(), &argv, stdin, &[])
    }

    /// The raw bytes of `queue`'s file, empty when it was never written.
    fn raw(&self, queue: &str) -> String {
        std::fs::read_to_string(self.path("bus").join(format!("{queue}.jsonl"))).unwrap_or_default()
    }

    fn raw_lines(&self, queue: &str) -> Vec<String> {
        self.raw(queue).lines().map(str::to_owned).collect()
    }

    /// The lines of `queue`'s drop log, parsed.
    fn dropped(&self, queue: &str) -> Vec<Value> {
        self.raw_lines(&format!("{queue}-dropped"))
            .iter()
            .map(|line| serde_json::from_str(line).expect("a drop line is JSON"))
            .collect()
    }

    /// How many lines the marker file `name` holds: how many times the hook
    /// writing it ran.
    fn marker(&self, name: &str) -> usize {
        std::fs::read_to_string(self.path(name))
            .unwrap_or_default()
            .lines()
            .count()
    }
}

/// `argv` as a YAML flow sequence.
fn argv(words: &[&str]) -> String {
    serde_json::to_string(words).expect("an argv")
}

fn one_line(run: &Run) -> Value {
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

/// The `message` member of a drop log line, as the raw bytes it was written
/// with.
fn dropped_message_bytes(line: &str) -> String {
    #[derive(serde::Deserialize)]
    struct Line<'a> {
        #[serde(borrow)]
        message: &'a serde_json::value::RawValue,
    }
    serde_json::from_str::<Line<'_>>(line)
        .expect("a drop line")
        .message
        .get()
        .to_owned()
}

/// Assert `line` is a drop line in the stated shape: exactly its keys, its
/// version, a time, the queue, and the hook by index and command.
fn assert_drop_shape(line: &Value, queue: &str, index: u64, command: &[&str]) {
    let mut keys: Vec<&str> = line
        .as_object()
        .expect("a drop line is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.retain(|key| *key != "correlation");
    assert_eq!(
        keys,
        ["schema_version", "at", "queue", "hook", "message"],
        "{line}"
    );
    assert_eq!(line["schema_version"], json!(1), "{line}");
    assert!(line["at"].as_u64().is_some_and(|at| at > 0), "{line}");
    assert_eq!(line["queue"], json!(queue), "{line}");
    assert_eq!(
        line["hook"],
        json!({"index": index, "command": command}),
        "{line}"
    );
}

#[test]
fn a_hook_echoing_its_input_with_or_without_the_newline_keeps_the_record_byte_for_byte() {
    let scratch = Scratch::new();
    let echo_newline = scratch.script("echo-newline.sh", "cat");
    let echo_bare = scratch.script("echo-bare.sh", "IFS= read -r line; printf '%s' \"$line\"");
    scratch.configure(&format!(
        "queues:\n  plain: {{}}\n  newline: {{}}\n  bare: {{}}\nhooks:\n  - {{on: newline, command: {}}}\n  - {{on: bare, command: {}}}\n",
        argv(&[&echo_newline]),
        argv(&[&echo_bare]),
    ));
    let record = r#"{"text": "a \"quoted\" word",   "n": 1, "nested": {"k": [1, 2.5]}}"#;
    for queue in ["plain", "newline", "bare"] {
        let sent = scratch.bus(&["send", queue], Some(record));
        assert_eq!(sent.code, 0, "{queue}: {}", sent.stderr);
        assert_eq!(one_line(&sent)["queue"], json!(queue));
    }
    let unhooked = scratch.raw("plain");
    assert!(!unhooked.is_empty());
    assert_eq!(
        scratch.raw("newline"),
        unhooked,
        "a newline-echoing keep re-spelled the record"
    );
    assert_eq!(
        scratch.raw("bare"),
        unhooked,
        "a bare-echoing keep re-spelled the record"
    );

    for queue in ["newline", "bare"] {
        let validated = scratch.bus(&["validate", queue], Some(record));
        assert_eq!(validated.code, 0, "{}", validated.stderr);
        let line = one_line(&validated);
        assert_eq!(line["outcome"], json!("keep"), "{line}");
        assert_eq!(line["verdict"], json!("pass"), "{line}");
        assert_eq!(line["records"][0]["queue"], json!(queue), "{line}");
    }
    assert!(scratch.dropped("newline").is_empty() && scratch.dropped("bare").is_empty());
}

/// The re-formatted but equivalent spelling the first hook prints: other
/// spacing, `1.0` for `1`, and the keys in another order.
const RESPELLED: &str = "{ \"n\" : 1.0 ,\"text\":\"hello\"}\n";

#[test]
fn a_change_is_appended_and_handed_on_as_the_exact_bytes_the_hook_printed() {
    let scratch = Scratch::new();
    let respell = scratch.script(
        "respell.sh",
        &format!("cat > /dev/null\nprintf '%s' '{RESPELLED}'"),
    );
    let capture = scratch.script("capture.sh", &format!("tee {}", scratch.at("captured")));
    let record = r#"{"text":"hello","n":1}"#;

    // The first hook alone: `validate` names it a change, with its bytes.
    scratch.configure(&format!(
        "queues:\n  notes: {{}}\nhooks:\n  - {{on: notes, command: {}}}\n",
        argv(&[&respell])
    ));
    let validated = scratch.bus(&["validate", "notes"], Some(record));
    assert_eq!(validated.code, 0, "{}", validated.stderr);
    assert_eq!(one_line(&validated)["outcome"], json!("change"));
    assert!(
        validated.stdout.contains(&format!(
            "\"records\":[{{\"queue\":\"notes\",\"record\":{}}}]",
            RESPELLED.trim_end()
        )),
        "validate did not report the hook's bytes: {}",
        validated.stdout
    );

    // Chained: the second hook reads exactly what the first printed, and keeps
    // it, so the queue holds exactly those bytes.
    scratch.configure(&format!(
        "queues:\n  notes: {{}}\nhooks:\n  - {{on: notes, command: {}}}\n  - {{on: notes, command: {}}}\n",
        argv(&[&respell]),
        argv(&[&capture])
    ));
    let sent = scratch.bus(&["send", "notes"], Some(record));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    let captured = std::fs::read(scratch.path("captured")).expect("the second hook ran");
    assert_eq!(
        captured,
        RESPELLED.as_bytes(),
        "the second hook was not handed the first one's bytes"
    );
    assert_eq!(
        scratch.raw("notes").as_bytes(),
        RESPELLED.as_bytes(),
        "the appended record is not the hook's bytes"
    );
}

#[test]
fn a_change_a_stamping_preparation_rewrites_is_written_from_its_value_and_carries_the_stamp() {
    let scratch = Scratch::new();
    let respell = scratch.script(
        "respell.sh",
        &format!("cat > /dev/null\nprintf '%s' '{RESPELLED}'"),
    );
    let capture = scratch.script("capture.sh", &format!("tee {}", scratch.at("captured")));
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: notes, command: {}}}\n  - {{on: notes, command: {}}}\n",
        scratch.stamped(),
        argv(&[&respell]),
        argv(&[&capture])
    ));
    let sent = scratch.bus(&["send", "notes"], Some(r#"{"text":"hello","n":1}"#));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    assert_eq!(
        std::fs::read(scratch.path("captured")).expect("the second hook ran"),
        RESPELLED.as_bytes()
    );
    let lines = scratch.raw_lines("notes");
    assert_eq!(lines.len(), 1);
    let parsed: Value = serde_json::from_str(&lines[0]).expect("a JSON line");
    assert!(
        parsed["noted_at"].as_u64().is_some_and(|at| at > 0),
        "the record carries no stamp: {}",
        lines[0]
    );
    assert_eq!(parsed["text"], json!("hello"));
    assert_eq!(
        lines[0],
        serde_json::to_string(&parsed).expect("JSON"),
        "a rewritten record was not written from its parsed value"
    );
    assert_ne!(lines[0], RESPELLED.trim_end());
}

#[test]
fn a_multi_line_change_is_unjudged_because_a_record_is_one_line() {
    let scratch = Scratch::new();
    let indented = scratch.script(
        "indented.sh",
        "cat > /dev/null\nprintf '{\\n  \"text\": \"hello\"\\n}\\n'",
    );
    scratch.configure(&format!(
        "queues:\n  notes: {{}}\nhooks:\n  - {{on: notes, command: {}}}\n",
        argv(&[&indented])
    ));
    let sent = scratch.bus(&["send", "notes"], Some(r#"{"text":"hello"}"#));
    assert_eq!(sent.code, 1, "{}", sent.stdout);
    assert_eq!(sent.stdout, "");
    assert!(
        sent.stderr.contains("a queue record is one line"),
        "{}",
        sent.stderr
    );
    assert_eq!(scratch.raw("notes"), "");
    assert_eq!(scratch.raw("notes-dropped"), "");
}

#[test]
fn a_change_breaking_the_queues_schema_or_the_authors_grants_is_refused_with_nothing_appended() {
    let scratch = Scratch::new();
    let untexted = scratch.script("untexted.sh", "cat > /dev/null\necho '{\"n\":2}'");
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: typed, command: {}}}\n",
        scratch.stamped(),
        argv(&[&untexted])
    ));
    let sent = scratch.bus(&["send", "typed"], Some(r#"{"text":"hello"}"#));
    assert_eq!(sent.code, 1, "{}", sent.stdout);
    assert_eq!(sent.stdout, "");
    assert!(
        sent.stderr.contains("typed: the record is not a"),
        "{}",
        sent.stderr
    );
    assert_eq!(scratch.raw("typed"), "");

    let promoted = scratch.script(
        "promoted.sh",
        "cat > /dev/null\necho '{\"author\":\"bot\",\"completion\":true}'",
    );
    let other = Scratch::new();
    other.configure(&format!(
        "{}hooks:\n  - {{on: answers, command: {}}}\n",
        other.desk(),
        argv(&[&promoted])
    ));
    let sent = other.bus(&["send", "answers"], Some(r#"{"message":"hi"}"#));
    assert_eq!(sent.code, 1, "{}", sent.stdout);
    assert!(
        sent.stderr.contains("a bot never declares the desk done"),
        "{}",
        sent.stderr
    );
    assert_eq!(other.raw("answers"), "");
    assert_eq!(other.raw("actions"), "");
}

#[test]
fn a_hook_printing_nothing_drops_the_record_into_the_drop_log_exactly_as_the_queue_would_hold_it() {
    let scratch = Scratch::new();
    let drop = scratch.script("drop.sh", "cat > /dev/null");
    let keep = scratch.script("keep.sh", "cat");
    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\n  kept: {{}}\nhooks:\n  - {{on: inbox, command: {}}}\n  - {{on: kept, command: {}}}\n",
        argv(&[&drop]),
        argv(&[&keep])
    ));
    // A credential-shaped value, which `events emit` would redact: a queue
    // record is not, and neither is its drop.
    let record = r#"{"text":"use ghp_abcdefghijklmnopqrstuvwxyz0123456789","password":"hunter2"}"#;
    let sent = scratch.bus(&["send", "inbox"], Some(record));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    assert_eq!(one_line(&sent), json!({"queue": "inbox", "dropped": true}));
    assert_eq!(scratch.raw("inbox"), "", "a dropped record was appended");

    let would_hold = {
        let kept = scratch.bus(&["send", "kept"], Some(record));
        assert_eq!(kept.code, 0, "{}", kept.stderr);
        scratch.raw_lines("kept").remove(0)
    };
    let lines = scratch.raw_lines("inbox-dropped");
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).expect("a JSON line");
    assert_drop_shape(&line, "inbox", 0, &[&drop]);
    assert!(line.get("correlation").is_none(), "{line}");
    assert_eq!(
        dropped_message_bytes(&lines[0]),
        would_hold,
        "the drop log does not hold the record the queue would have held"
    );
    assert!(lines[0].contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));

    let status = scratch.bus(&["status", "inbox", "--format", "text"], None);
    assert_eq!(status.code, 0, "{}", status.stderr);
    assert!(
        status
            .stdout
            .starts_with("inbox records=0 waiting=0 pending=- abandoned=0 unread=0 dropped=1\n"),
        "{}",
        status.stdout
    );
    let status = scratch.bus(&["status", "inbox"], None);
    let statuses: Value = serde_json::from_str(&status.stdout).expect("JSON");
    assert_eq!(statuses[0]["dropped"], json!(1));

    // The drop log is read as any log is.
    let read = scratch.bus(
        &[
            "subscribe",
            "inbox-dropped",
            "--until",
            r#"{"field":"queue","equals":"inbox"}"#,
        ],
        None,
    );
    assert_eq!(read.code, 0, "{}", read.stderr);
    assert_eq!(one_line(&read)["record"]["hook"]["index"], json!(0));
}

#[test]
fn an_ask_whose_question_a_hook_drops_returns_at_once_saying_no_reply_will_come() {
    let scratch = Scratch::new();
    let drop = scratch.script(
        "drop.sh",
        &format!(
            "printf '%s' \"$ONEMESSAGEBUS_HOOK_CORRELATION\" > {}\ncat > /dev/null",
            scratch.at("told")
        ),
    );
    scratch.configure(&format!(
        "queues:\n  questions: {{policy: {{hold_pending: true}}, answers: replies}}\n  replies: {{}}\nhooks:\n  - {{on: questions, command: {}}}\n",
        argv(&[&drop])
    ));
    let asked = scratch.bus(&["ask", "questions"], Some(r#"{"message":"anyone?"}"#));
    assert_eq!(asked.code, 1, "{}", asked.stderr);
    assert_eq!(
        one_line(&asked),
        json!({"answer": "dropped", "queue": "questions", "hook": {"index": 0, "command": [drop]}})
    );
    assert_eq!(asked.stderr.lines().count(), 1, "{}", asked.stderr);
    assert!(
        asked.stderr.contains("no reply will come"),
        "{}",
        asked.stderr
    );
    assert_eq!(scratch.raw("questions"), "");
    let dropped = scratch.dropped("questions");
    assert_eq!(dropped.len(), 1);
    assert_drop_shape(&dropped[0], "questions", 0, &[&drop]);
    assert!(
        dropped[0]["correlation"]
            .as_str()
            .is_some_and(|c| c.starts_with("c-")),
        "a dropped question names its correlation: {}",
        dropped[0]
    );
    assert_eq!(dropped[0]["message"]["message"], json!("anyone?"));
    assert_eq!(
        json!(std::fs::read_to_string(scratch.path("told")).expect("the hook ran")),
        dropped[0]["correlation"],
        "the hook was not told the question's correlation"
    );
}

#[test]
fn a_hook_printing_only_whitespace_is_unjudged_and_drops_nothing() {
    let scratch = Scratch::new();
    let blank = scratch.script("blank.sh", "cat > /dev/null\nprintf '  \\n'");
    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nhooks:\n  - {{on: inbox, command: {}}}\n",
        argv(&[&blank])
    ));
    let sent = scratch.bus(&["send", "inbox"], Some(r#"{"text":"x"}"#));
    assert_eq!(sent.code, 1, "{}", sent.stdout);
    assert_eq!(sent.stdout, "");
    assert!(sent.stderr.contains("could not decide"), "{}", sent.stderr);
    assert_eq!(scratch.raw("inbox"), "");
    assert_eq!(scratch.raw("inbox-dropped"), "");
}

#[test]
fn a_refusing_hook_returns_its_stderr_unaltered_and_appends_nothing_anywhere() {
    let scratch = Scratch::new();
    let refuse = scratch.script(
        "refuse.sh",
        "cat > /dev/null\nprintf 'not on my watch: %s\\n' \"$ONEMESSAGEBUS_HOOK_QUEUE\" >&2\nexit 1",
    );
    let keep = scratch.script("keep.sh", "cat");
    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nhooks:\n  - {{on: inbox, command: {}}}\n  - {{on: inbox, command: {}}}\n",
        argv(&[&keep]),
        argv(&[&refuse])
    ));
    let reason = "not on my watch: inbox\n";
    let sent = scratch.bus(&["send", "inbox"], Some(r#"{"text":"x"}"#));
    assert_eq!(sent.code, 1, "{}", sent.stdout);
    assert_eq!(sent.stdout, "", "a refused send printed something");
    assert_eq!(sent.stderr.lines().count(), 1, "{}", sent.stderr);
    let line = sent.stderr.trim_end();
    assert!(line.starts_with("onemessagebus: inbox: "), "{line}");
    assert!(line.contains(&format!("hook 1 (`{refuse}`)")), "{line}");
    assert!(line.ends_with(reason.trim_end()), "{line}");

    let validated = scratch.bus(&["validate", "inbox"], Some(r#"{"text":"x"}"#));
    assert_eq!(validated.code, 1);
    let judged = one_line(&validated);
    assert_eq!(
        (
            &judged["outcome"],
            &judged["verdict"],
            &judged["reason"],
            &judged["records"]
        ),
        (
            &json!("refuse"),
            &json!("refuse"),
            &json!(reason),
            &json!([])
        ),
        "{judged}"
    );

    let empty = scratch.script("silent-refuse.sh", "cat > /dev/null\nexit 1");
    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nhooks:\n  - {{on: inbox, command: {}}}\n",
        argv(&[&empty])
    ));
    let silent = scratch.bus(&["send", "inbox"], Some(r#"{"text":"x"}"#));
    assert_eq!(silent.code, 1);
    assert!(
        silent.stderr.contains("wrote no reason"),
        "{}",
        silent.stderr
    );
    assert_eq!(scratch.raw("inbox"), "");
    assert_eq!(scratch.raw("inbox-dropped"), "");
}

#[test]
fn a_hook_exiting_two_killed_by_a_signal_missing_or_printing_no_json_appends_nothing() {
    let scratch = Scratch::new();
    let two = scratch.script(
        "two.sh",
        "cat > /dev/null\necho 'no quota left' >&2\nexit 2",
    );
    let killed = scratch.script("killed.sh", "cat > /dev/null\nkill -9 $$");
    let prose = scratch.script("prose.sh", "cat > /dev/null\necho 'looks fine to me'");
    let missing = scratch.at("no-such-hook");
    for (command, why) in [
        (two.as_str(), "exited 2"),
        (killed.as_str(), "ended by a signal"),
        (missing.as_str(), "could not be run"),
        (prose.as_str(), "not exactly one JSON value"),
    ] {
        scratch.configure(&format!(
            "queues:\n  inbox: {{}}\nhooks:\n  - {{on: inbox, command: {}}}\n",
            argv(&[command])
        ));
        let sent = scratch.bus(&["send", "inbox"], Some(r#"{"text":"x"}"#));
        assert_eq!(sent.code, 1, "{why}: {}", sent.stdout);
        assert_eq!(sent.stdout, "", "{why}");
        assert!(sent.stderr.contains(why), "{why}: {}", sent.stderr);
        assert!(sent.stderr.contains("hook 0"), "{why}: {}", sent.stderr);
        let validated = scratch.bus(&["validate", "inbox"], Some(r#"{"text":"x"}"#));
        assert_eq!(validated.code, 1, "{why}");
        assert_eq!(one_line(&validated)["outcome"], json!("unjudged"), "{why}");
    }
    assert_eq!(scratch.raw("inbox"), "");
    assert_eq!(scratch.raw("inbox-dropped"), "");
}

#[test]
fn a_chain_hands_each_change_on_and_a_drop_ends_it() {
    let scratch = Scratch::new();
    let first = scratch.script("first.sh", "cat");
    let second = scratch.script("second.sh", "cat > /dev/null\necho '{\"step\":2}'");
    let third = scratch.script(
        "third.sh",
        &format!(
            "echo ran >> {}\ntee {}",
            scratch.at("third-ran"),
            scratch.at("third-read")
        ),
    );
    scratch.configure(&format!(
        "queues:\n  steps: {{}}\nhooks:\n  - {{on: steps, command: {}}}\n  - {{on: steps, command: {}}}\n  - {{on: steps, command: {}}}\n",
        argv(&[&first]),
        argv(&[&second]),
        argv(&[&third])
    ));
    let sent = scratch.bus(&["send", "steps"], Some(r#"{"step":1}"#));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    assert_eq!(
        std::fs::read_to_string(scratch.path("third-read")).expect("the third hook ran"),
        "{\"step\":2}\n"
    );
    assert_eq!(scratch.raw("steps"), "{\"step\":2}\n");

    let drop = scratch.script("drop.sh", "cat > /dev/null");
    scratch.configure(&format!(
        "queues:\n  steps: {{}}\nhooks:\n  - {{on: steps, command: {}}}\n  - {{on: steps, command: {}}}\n  - {{on: steps, command: {}}}\n",
        argv(&[&drop]),
        argv(&[&second]),
        argv(&[&third])
    ));
    let sent = scratch.bus(&["send", "steps"], Some(r#"{"step":1}"#));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    assert_eq!(one_line(&sent), json!({"queue": "steps", "dropped": true}));
    assert_eq!(scratch.marker("third-ran"), 1, "a hook after a drop ran");
    assert_eq!(scratch.raw_lines("steps").len(), 1);
    assert_eq!(scratch.dropped("steps").len(), 1);
}

#[test]
fn a_when_the_message_does_not_satisfy_skips_the_hook() {
    let scratch = Scratch::new();
    let drop = scratch.script("drop.sh", "cat > /dev/null");
    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nhooks:\n  - {{on: inbox, when: {{carries: spam}}, command: {}}}\n",
        argv(&[&drop])
    ));
    assert_eq!(
        scratch
            .bus(&["send", "inbox"], Some(r#"{"text":"x"}"#))
            .code,
        0
    );
    assert_eq!(
        scratch
            .bus(&["send", "inbox"], Some(r#"{"spam":"yes"}"#))
            .code,
        0
    );
    assert_eq!(scratch.raw("inbox"), "{\"text\":\"x\"}\n");
    assert_eq!(scratch.dropped("inbox").len(), 1);
}

/// The scripts of the cache journey: each counts its runs in a marker and
/// answers by the bar fingerprint it reads, so a stale cached outcome would be
/// visible.
struct Cached {
    scratch: Scratch,
    keep: String,
    change: String,
    drop: String,
    refuse: String,
}

impl Cached {
    fn new() -> Self {
        let scratch = Scratch::new();
        let bar = scratch.at("bar");
        std::fs::write(&bar, "bar-1\n").expect("the bar is written");
        let keep = scratch.script(
            "keep.sh",
            &format!(
                "echo ran >> {m}\nif [ \"$(cat {bar})\" = bar-1 ]; then cat; else cat > /dev/null; echo '{{\"fresh\":\"keep\"}}'; fi",
                m = scratch.at("keep-ran")
            ),
        );
        let change = scratch.script(
            "change.sh",
            &format!(
                "echo ran >> {m}\ncat > /dev/null\nprintf '{{ \"changed\" : \"%s\"}}\\n' \"$(cat {bar})\"",
                m = scratch.at("change-ran")
            ),
        );
        let drop = scratch.script(
            "drop.sh",
            &format!(
                "echo ran >> {m}\nif [ \"$(cat {bar})\" = bar-1 ]; then cat > /dev/null; else cat; fi",
                m = scratch.at("drop-ran")
            ),
        );
        let refuse = scratch.script(
            "refuse.sh",
            &format!(
                "echo ran >> {m}\ncat > /dev/null\necho no >&2\nexit 1",
                m = scratch.at("refuse-ran")
            ),
        );
        let fingerprint = argv(&["cat", &bar]);
        let cache = |name: &str| {
            format!(
                "cache: {{dir: {}, bar_fingerprint: {fingerprint}}}",
                serde_json::to_string(&scratch.path(name)).expect("a path")
            )
        };
        scratch.configure(&format!(
            "queues:\n  k: {{}}\n  c: {{}}\n  d: {{}}\n  r: {{}}\nhooks:\n  - {{on: k, command: {}, {}}}\n  - {{on: c, command: {}, {}}}\n  - {{on: d, command: {}, {}}}\n  - {{on: r, command: {}, {}}}\n",
            argv(&[&keep]),
            cache("cache-k"),
            argv(&[&change]),
            cache("cache-c"),
            argv(&[&drop]),
            cache("cache-d"),
            argv(&[&refuse]),
            cache("cache-r"),
        ));
        Self {
            scratch,
            keep,
            change,
            drop,
            refuse,
        }
    }

    fn records(&self, dir: &str) -> Vec<Value> {
        std::fs::read_dir(self.scratch.path(dir))
            .map(|entries| {
                entries
                    .map(|entry| {
                        serde_json::from_slice(
                            &std::fs::read(entry.expect("an entry").path()).expect("a record"),
                        )
                        .expect("a JSON record")
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[test]
fn a_cached_keep_change_and_drop_are_not_run_again_until_the_bar_moves() {
    let cached = Cached::new();
    let scratch = &cached.scratch;
    let record = r#"{"text":"same"}"#;
    for _ in 0..2 {
        for queue in ["k", "c"] {
            let sent = scratch.bus(&["send", queue], Some(record));
            assert_eq!(sent.code, 0, "{queue}: {}", sent.stderr);
        }
        let dropped = scratch.bus(&["send", "d"], Some(record));
        assert_eq!(dropped.code, 0, "{}", dropped.stderr);
        assert_eq!(one_line(&dropped), json!({"queue": "d", "dropped": true}));
    }
    for marker in ["keep-ran", "change-ran", "drop-ran"] {
        assert_eq!(
            scratch.marker(marker),
            1,
            "{marker}: a cached outcome ran its command again"
        );
    }
    assert_eq!(scratch.raw("k"), format!("{record}\n{record}\n"));
    let changed = scratch.raw_lines("c");
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[0], r#"{ "changed" : "bar-1"}"#);
    assert_eq!(
        changed[1], changed[0],
        "the cached change is not the bytes first appended"
    );
    assert_eq!(scratch.raw("d"), "");
    let drops = scratch.dropped("d");
    assert_eq!(drops.len(), 2);
    for line in &drops {
        assert_drop_shape(line, "d", 0, &[&cached.drop]);
    }

    let validated = scratch.bus(&["validate", "c"], Some(record));
    assert_eq!(validated.code, 0, "{}", validated.stderr);
    assert_eq!(one_line(&validated)["outcome"], json!("change"));
    assert!(
        validated
            .stdout
            .contains(&format!("\"record\":{}", changed[0])),
        "{}",
        validated.stdout
    );
    assert_eq!(scratch.marker("change-ran"), 1);

    for (dir, outcome, command) in [
        ("cache-k", "keep", &cached.keep),
        ("cache-c", "change", &cached.change),
        ("cache-d", "drop", &cached.drop),
    ] {
        let records = cached.records(dir);
        assert_eq!(records.len(), 1, "{dir}: {records:?}");
        let record = &records[0];
        assert_eq!(record["schema_version"], json!(2), "{record}");
        assert_eq!(record["outcome"], json!(outcome), "{record}");
        assert_eq!(record["command"], json!([command]), "{record}");
        assert_eq!(record["fingerprint"], json!("bar-1"), "{record}");
        if outcome == "change" {
            assert_eq!(record["output"], json!(changed[0]), "{record}");
        } else {
            assert!(record.get("output").is_none(), "{record}");
        }
    }

    // Each script answers otherwise under the moved bar, and its fresh answer
    // is the one applied.
    std::fs::write(scratch.path("bar"), "bar-2\n").expect("the bar moves");
    for queue in ["k", "c", "d"] {
        let sent = scratch.bus(&["send", queue], Some(record));
        assert_eq!(sent.code, 0, "{queue}: {}", sent.stderr);
    }
    for marker in ["keep-ran", "change-ran", "drop-ran"] {
        assert_eq!(
            scratch.marker(marker),
            2,
            "{marker}: a moved bar did not run the command"
        );
    }
    assert_eq!(scratch.raw_lines("k")[2], r#"{"fresh":"keep"}"#);
    assert_eq!(scratch.raw_lines("c")[2], r#"{ "changed" : "bar-2"}"#);
    assert_eq!(scratch.raw("d"), format!("{record}\n"));
    assert_eq!(scratch.dropped("d").len(), 2);

    // A refusal is never recorded: it runs on every offer.
    for _ in 0..2 {
        assert_eq!(scratch.bus(&["send", "r"], Some(record)).code, 1);
    }
    assert_eq!(
        scratch.marker("refuse-ran"),
        2,
        "a refusal was answered from the cache"
    );
    assert!(cached.records("cache-r").is_empty());
    let _ = &cached.refuse;
}

#[test]
fn a_pass_a_validator_recorded_for_the_same_argv_and_content_is_read_as_a_keep() {
    let scratch = Scratch::new();
    std::fs::write(scratch.path("bar"), "bar-1\n").expect("the bar is written");
    let counted = scratch.script(
        "counted.sh",
        &format!("echo ran >> {}\ncat", scratch.at("ran")),
    );
    let cache = format!(
        "cache: {{dir: {}, bar_fingerprint: {}}}",
        serde_json::to_string(&scratch.path("passes")).expect("a path"),
        argv(&["cat", &scratch.at("bar")])
    );
    let record = r#"{"text":"judged once"}"#;
    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nvalidators:\n  - {{on: inbox, kind: command, command: {}, {cache}}}\n",
        argv(&[&counted])
    ));
    assert_eq!(scratch.bus(&["send", "inbox"], Some(record)).code, 0);
    assert_eq!(scratch.marker("ran"), 1);
    let passes: Vec<Value> = std::fs::read_dir(scratch.path("passes"))
        .expect("the pass cache")
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.expect("an entry").path()).expect("read"))
                .expect("JSON")
        })
        .collect();
    assert_eq!(passes.len(), 1);
    assert_eq!(passes[0]["schema_version"], json!(1));

    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nhooks:\n  - {{on: inbox, command: {}, {cache}}}\n",
        argv(&[&counted])
    ));
    let sent = scratch.bus(&["send", "inbox"], Some(record));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    assert_eq!(scratch.marker("ran"), 1, "a recorded pass ran the hook");
    assert_eq!(scratch.raw("inbox"), format!("{record}\n{record}\n"));
}

#[test]
fn a_cached_hook_that_reroutes_runs_on_every_offer_and_obeys_the_routes_as_they_stand() {
    let scratch = Scratch::new();
    std::fs::write(scratch.path("bar"), "bar-1\n").expect("the bar is written");
    let reroute = scratch.script(
        "reroute.sh",
        &format!(
            "echo ran >> {}\ncat\nprintf target > \"$ONEMESSAGEBUS_HOOK_ROUTE\"",
            scratch.at("ran")
        ),
    );
    let configure = |routes: &str| {
        scratch.configure(&format!(
            "queues:\n  source: {{}}\n  target: {{}}\n  other: {{}}\nhooks:\n  - {{on: source, command: {}, routes: {routes}, cache: {{dir: {}, bar_fingerprint: {}}}}}\n",
            argv(&[&reroute]),
            serde_json::to_string(&scratch.path("cache")).expect("a path"),
            argv(&["cat", &scratch.at("bar")])
        ));
    };
    configure("[target, other]");
    let record = r#"{"text":"move me"}"#;
    for _ in 0..2 {
        let sent = scratch.bus(&["send", "source"], Some(record));
        assert_eq!(sent.code, 0, "{}", sent.stderr);
        assert_eq!(one_line(&sent)["queue"], json!("target"));
    }
    configure("[other]");
    let narrowed = scratch.bus(&["send", "source"], Some(record));
    assert_eq!(narrowed.code, 1, "{}", narrowed.stdout);
    assert!(
        narrowed.stderr.contains("not one of its routes"),
        "{}",
        narrowed.stderr
    );
    assert_eq!(
        scratch.marker("ran"),
        3,
        "a reroute was answered from the cache"
    );
    assert_eq!(scratch.raw_lines("target").len(), 2);
    assert_eq!(scratch.raw("source"), "");
    assert_eq!(scratch.raw("other"), "");
    assert!(
        !scratch.path("cache").exists()
            || std::fs::read_dir(scratch.path("cache")).map_or(0, Iterator::count) == 0,
        "a reroute was recorded"
    );
}

#[test]
fn a_reroute_goes_through_the_target_queues_preparation_and_hooks_once() {
    let scratch = Scratch::new();
    let reroute = scratch.script(
        "reroute.sh",
        "cat\nprintf notes > \"$ONEMESSAGEBUS_HOOK_ROUTE\"",
    );
    let capture = scratch.script("capture.sh", &format!("tee {}", scratch.at("captured")));
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: elsewhere, command: {}, routes: [notes]}}\n  - {{on: notes, command: {}}}\n",
        scratch.stamped(),
        argv(&[&reroute]),
        argv(&[&capture])
    ));
    let sent = scratch.bus(&["send", "elsewhere"], Some(r#"{"text":"moved"}"#));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    assert_eq!(one_line(&sent)["queue"], json!("notes"));
    assert_eq!(scratch.raw("elsewhere"), "");
    assert_eq!(
        std::fs::read_to_string(scratch.path("captured")).expect("the target's hook ran"),
        "{\"text\":\"moved\"}\n"
    );
    let landed: Value = serde_json::from_str(&scratch.raw_lines("notes")[0]).expect("a JSON line");
    assert!(landed["noted_at"].as_u64().is_some(), "{landed}");

    let validated = scratch.bus(&["validate", "elsewhere"], Some(r#"{"text":"moved"}"#));
    assert_eq!(one_line(&validated)["outcome"], json!("change"));
    assert_eq!(one_line(&validated)["records"][0]["queue"], json!("notes"));
}

#[test]
fn a_reroute_outside_its_routes_or_a_second_reroute_is_unjudged_with_nothing_appended() {
    let scratch = Scratch::new();
    let to_typed = scratch.script(
        "to-typed.sh",
        "cat\nprintf typed > \"$ONEMESSAGEBUS_HOOK_ROUTE\"",
    );
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: elsewhere, command: {}, routes: [notes]}}\n",
        scratch.stamped(),
        argv(&[&to_typed])
    ));
    let outside = scratch.bus(&["send", "elsewhere"], Some(r#"{"text":"x"}"#));
    assert_eq!(outside.code, 1, "{}", outside.stdout);
    assert!(
        outside.stderr.contains("not one of its routes"),
        "{}",
        outside.stderr
    );

    let to_notes = scratch.script(
        "to-notes.sh",
        "cat\nprintf notes > \"$ONEMESSAGEBUS_HOOK_ROUTE\"",
    );
    let back = scratch.script(
        "back.sh",
        "cat\nprintf elsewhere > \"$ONEMESSAGEBUS_HOOK_ROUTE\"",
    );
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: elsewhere, command: {}, routes: [notes]}}\n  - {{on: notes, command: {}, routes: [elsewhere]}}\n",
        scratch.stamped(),
        argv(&[&to_notes]),
        argv(&[&back])
    ));
    let twice = scratch.bus(&["send", "elsewhere"], Some(r#"{"text":"x"}"#));
    assert_eq!(twice.code, 1, "{}", twice.stdout);
    assert!(
        twice.stderr.contains("rerouted at most once"),
        "{}",
        twice.stderr
    );
    for queue in ["elsewhere", "notes", "typed"] {
        assert_eq!(scratch.raw(queue), "", "{queue}");
    }

    // A hook declaring no routes is offered no route file.
    let no_routes = scratch.script(
        "no-routes.sh",
        "cat\n[ -z \"$ONEMESSAGEBUS_HOOK_ROUTE\" ] || { echo 'offered a route' >&2; exit 1; }",
    );
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: elsewhere, command: {}}}\n",
        scratch.stamped(),
        argv(&[&no_routes])
    ));
    let plain = scratch.bus(&["send", "elsewhere"], Some(r#"{"text":"x"}"#));
    assert_eq!(plain.code, 0, "{}", plain.stderr);
}

#[test]
fn a_configuration_naming_an_undeclared_route_or_hook_queue_is_refused_by_key() {
    let scratch = Scratch::new();
    let keep = scratch.script("keep.sh", "cat");
    for (hooks, key) in [
        (
            format!(
                "[{{on: inbox, command: {}, routes: [inbox, nowhere]}}]",
                argv(&[&keep])
            ),
            "hooks[0].routes[1]",
        ),
        (
            format!("[{{on: nowhere, command: {}}}]", argv(&[&keep])),
            "hooks[0].on",
        ),
        ("[{on: inbox, command: []}]".to_owned(), "hooks[0].command"),
        (
            format!(
                "[{{on: inbox, kind: command, command: {}}}]",
                argv(&[&keep])
            ),
            "unknown field `kind`",
        ),
    ] {
        scratch.configure(&format!("queues:\n  inbox: {{}}\nhooks: {hooks}\n"));
        let refused = scratch.bus(&["send", "inbox"], Some(r#"{"text":"x"}"#));
        assert_eq!(refused.code, 2, "{key}: {}", refused.stderr);
        assert!(refused.stderr.contains(key), "{key}: {}", refused.stderr);
    }
    assert_eq!(scratch.raw("inbox"), "");
}

#[test]
fn an_offer_the_layout_splits_is_refused_whole_when_one_routed_records_hook_refuses() {
    let scratch = Scratch::new();
    let refuse = scratch.script(
        "refuse.sh",
        "cat > /dev/null\necho 'no actions today' >&2\nexit 1",
    );
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: actions, command: {}}}\n",
        scratch.desk(),
        argv(&[&refuse])
    ));
    let envelope = r#"{"version":3,"completion":false,"message":"go on","actions":[{"op":"add","id":"n","task":"write it down"}]}"#;
    let sent = scratch.bus(&["send", "answers"], Some(envelope));
    assert_eq!(sent.code, 1, "{}", sent.stdout);
    assert_eq!(sent.stdout, "");
    assert!(
        sent.stderr.contains("onemessagebus: actions: "),
        "{}",
        sent.stderr
    );
    assert!(sent.stderr.contains("no actions today"), "{}", sent.stderr);
    assert_eq!(scratch.raw("answers"), "", "the verdict half was appended");
    assert_eq!(scratch.raw("actions"), "", "the actions half was appended");

    // The verdict alone is not routed to the refusing queue, and lands.
    let verdict = scratch.bus(
        &["send", "answers"],
        Some(r#"{"version":3,"completion":false,"message":"go on"}"#),
    );
    assert_eq!(verdict.code, 0, "{}", verdict.stderr);
    assert_eq!(scratch.raw_lines("answers").len(), 1);
}

#[test]
fn a_queue_whose_name_ends_in_dropped_is_refused_naming_it() {
    let scratch = Scratch::new();
    scratch.configure("queues:\n  audit-dropped: {}\n");
    let refused = scratch.bus(&["status"], None);
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("queues.audit-dropped") && refused.stderr.contains("`-dropped`"),
        "{}",
        refused.stderr
    );

    let bundle = scratch.path("reserved.json");
    std::fs::write(
        &bundle,
        json!({
            "version": "1.0",
            "schemas": [],
            "layouts": [{"name": "reserved", "queues": [{"name": "log"}, {"name": "log-dropped"}]}]
        })
        .to_string(),
    )
    .expect("the bundle is written");
    scratch.configure(&format!(
        "profile: reserved\nschemas:\n  - {}\n",
        serde_json::to_string(&format!("{}@1", bundle.display())).expect("a path")
    ));
    let refused = scratch.bus(&["status"], None);
    assert_ne!(refused.code, 0, "{}", refused.stdout);
    assert!(
        refused.stderr.contains("`log-dropped`") && refused.stderr.contains("reserved"),
        "{}",
        refused.stderr
    );
}

#[test]
fn validators_are_deprecated_on_stderr_and_judge_the_record_a_hook_changed() {
    let scratch = Scratch::new();
    let judge = scratch.script("judge.sh", &format!("cat > {}", scratch.at("judged")));
    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nvalidators:\n  - {{on: inbox, kind: command, command: {}}}\n",
        argv(&[&judge])
    ));
    let sent = scratch.bus(&["send", "inbox"], Some(r#"{"text":"x"}"#));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    assert_eq!(sent.stderr.lines().count(), 1, "{}", sent.stderr);
    let warning = sent.stderr.trim_end();
    assert!(
        warning.starts_with("onemessagebus: validators are deprecated")
            && warning.contains("`hooks:`")
            && warning.contains("prints its input to pass")
            && warning.contains("hooks run first and validators judge the records the hooks made"),
        "{warning}"
    );

    let tag = scratch.script(
        "tag.sh",
        "cat > /dev/null\necho '{\"text\":\"x\",\"hooked\":true}'",
    );
    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nhooks:\n  - {{on: inbox, command: {}}}\nvalidators:\n  - {{on: inbox, kind: command, command: {}}}\n",
        argv(&[&tag]),
        argv(&[&judge])
    ));
    let both = scratch.bus(&["send", "inbox"], Some(r#"{"text":"x"}"#));
    assert_eq!(both.code, 0, "{}", both.stderr);
    let judged: Value = serde_json::from_str(
        &std::fs::read_to_string(scratch.path("judged")).expect("the validator ran"),
    )
    .expect("JSON");
    assert_eq!(
        judged,
        json!({"text": "x", "hooked": true}),
        "the validator did not judge the record the hook changed"
    );
    assert_eq!(
        scratch.raw_lines("inbox")[1],
        r#"{"text":"x","hooked":true}"#
    );

    scratch.configure(&format!(
        "queues:\n  inbox: {{}}\nhooks:\n  - {{on: inbox, command: {}}}\n",
        argv(&[&tag])
    ));
    assert_eq!(
        scratch
            .bus(&["send", "inbox"], Some(r#"{"text":"x"}"#))
            .stderr,
        ""
    );
}

#[test]
fn a_reply_a_hook_drops_answers_nothing_and_is_recorded_in_the_answer_queues_drop_log() {
    let scratch = Scratch::new();
    let drop = scratch.script("drop.sh", "cat > /dev/null");
    scratch.configure(&format!(
        "queues:\n  questions: {{policy: {{hold_pending: true}}, answers: replies}}\n  replies: {{}}\nhooks:\n  - {{on: replies, command: {}}}\n",
        argv(&[&drop])
    ));
    let asked = scratch.bus(
        &["ask", "questions", "--timeout", "1"],
        Some(r#"{"message":"anyone?"}"#),
    );
    assert_eq!(asked.code, 1, "{}", asked.stderr);
    let correlation = one_line(&asked)["correlation"]
        .as_str()
        .expect("a correlation")
        .to_owned();
    let replied = scratch.bus(
        &["reply", "questions", "--correlation", &correlation],
        Some(r#"{"message":"me"}"#),
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    assert_eq!(
        one_line(&replied),
        json!({"queue": "replies", "dropped": true})
    );
    assert_eq!(scratch.raw("replies"), "");
    let dropped = scratch.dropped("replies");
    assert_eq!(dropped.len(), 1);
    assert_drop_shape(&dropped[0], "replies", 0, &[&drop]);
    assert_eq!(dropped[0]["correlation"], json!(correlation));
    assert_eq!(dropped[0]["message"], json!({"message": "me"}));
}

#[test]
fn an_ask_a_hook_refuses_or_cannot_decide_prints_nothing_and_raises_nothing() {
    let scratch = Scratch::new();
    let refuse = scratch.script(
        "refuse.sh",
        "cat > /dev/null\necho 'not that question' >&2\nexit 1",
    );
    let undecided = scratch.script(
        "undecided.sh",
        "cat > /dev/null\necho 'out of quota' >&2\nexit 2",
    );
    for (command, says) in [(&refuse, "not that question"), (&undecided, "exited 2")] {
        scratch.configure(&format!(
            "queues:\n  questions: {{policy: {{hold_pending: true}}, answers: replies}}\n  replies: {{}}\nhooks:\n  - {{on: questions, command: {}}}\n",
            argv(&[command])
        ));
        let asked = scratch.bus(
            &["ask", "questions", "--timeout", "30"],
            Some(r#"{"message":"anyone?"}"#),
        );
        assert_eq!(asked.code, 1, "{says}: {}", asked.stderr);
        assert_eq!(
            asked.stdout, "",
            "{says}: an ask a hook stopped printed an answer"
        );
        assert_eq!(asked.stderr.lines().count(), 1, "{says}: {}", asked.stderr);
        let line = asked.stderr.trim_end();
        assert!(
            line.starts_with("onemessagebus: questions: ")
                && line.contains(&format!("hook 0 (`{command}`)"))
                && line.contains(says),
            "{line}"
        );
    }
    assert_eq!(scratch.raw("questions"), "");
    assert_eq!(scratch.raw("questions-dropped"), "");
}

#[test]
fn an_event_queue_splices_its_event_and_id_in_front_of_a_hooks_bytes() {
    let scratch = Scratch::new();
    let respell = scratch.script("respell.sh", "cat > /dev/null\necho '{ \"text\" : \"x\" }'");
    let numbered = scratch.script(
        "numbered.sh",
        "cat > /dev/null\necho '{ \"id\" : 7, \"text\" : \"y\" }'",
    );
    let empty = scratch.script("empty.sh", "cat > /dev/null\necho '{ }'");
    scratch.configure(&format!(
        "queues:\n  spelled: {{policy: {{hold_pending: true}}}}\n  renumbered: {{policy: {{hold_pending: true}}}}\n  bare: {{policy: {{hold_pending: true}}}}\nhooks:\n  - {{on: spelled, command: {}}}\n  - {{on: renumbered, command: {}}}\n  - {{on: bare, command: {}}}\n",
        argv(&[&respell]),
        argv(&[&numbered]),
        argv(&[&empty])
    ));
    for queue in ["spelled", "renumbered", "bare"] {
        let sent = scratch.bus(&["send", queue], Some(r#"{"text":"offered"}"#));
        assert_eq!(sent.code, 0, "{queue}: {}", sent.stderr);
        assert_eq!(one_line(&sent)["id"], json!(0), "{queue}");
    }
    assert_eq!(
        scratch.raw("spelled"),
        "{\"event\":\"queued\",\"id\":0, \"text\" : \"x\" }\n"
    );
    assert_eq!(scratch.raw("bare"), "{\"event\":\"queued\",\"id\":0 }\n");
    // A record carrying the id the queue sets is written from its value, its id
    // the queue's.
    assert_eq!(
        scratch.raw("renumbered"),
        "{\"event\":\"queued\",\"id\":0,\"text\":\"y\"}\n"
    );
    let claimed = scratch.bus(&["next", "spelled"], None);
    assert_eq!(claimed.code, 0, "{}", claimed.stderr);
    assert_eq!(one_line(&claimed)["record"], json!({"id": 0, "text": "x"}));
}

#[test]
fn a_hook_cache_that_cannot_be_read_or_written_or_keyed_costs_a_run_never_another_outcome() {
    let scratch = Scratch::new();
    let counted = scratch.script(
        "counted.sh",
        &format!("echo ran >> {}\ncat", scratch.at("ran")),
    );
    let unkeyed = scratch.script(
        "unkeyed.sh",
        &format!("echo ran >> {}\ncat", scratch.at("unkeyed-ran")),
    );
    let undecided = scratch.script(
        "undecided.sh",
        &format!(
            "echo ran >> {}\ncat > /dev/null\nexit 2",
            scratch.at("undecided-ran")
        ),
    );
    std::fs::write(
        scratch.path("blocked"),
        "a file where the cache directory would be",
    )
    .expect("written");
    let cache = |dir: &str, bar: &[&str]| {
        format!(
            "cache: {{dir: {}, bar_fingerprint: {}}}",
            serde_json::to_string(&scratch.path(dir)).expect("a path"),
            argv(bar)
        )
    };
    scratch.configure(&format!(
        "queues:\n  kept: {{}}\n  blocked: {{}}\n  unkeyed: {{}}\n  undecided: {{}}\nhooks:\n  - {{on: kept, command: {}, {}}}\n  - {{on: blocked, command: {}, {}}}\n  - {{on: unkeyed, command: {}, {}}}\n  - {{on: undecided, command: {}, {}}}\n",
        argv(&[&counted]),
        cache("cache", &["echo", "bar-1"]),
        argv(&[&counted]),
        cache("blocked", &["echo", "bar-1"]),
        argv(&[&unkeyed]),
        cache("unkeyed-cache", &["false"]),
        argv(&[&undecided]),
        cache("undecided-cache", &["echo", "bar-1"]),
    ));
    let record = r#"{"text":"same"}"#;

    // A record corrupted on disk is no record: the command runs, and records
    // again.
    assert_eq!(scratch.bus(&["send", "kept"], Some(record)).code, 0);
    let recorded: Vec<PathBuf> = std::fs::read_dir(scratch.path("cache"))
        .expect("the cache")
        .map(|entry| entry.expect("an entry").path())
        .collect();
    assert_eq!(recorded.len(), 1);
    std::fs::write(&recorded[0], "{ not a record").expect("corrupted");
    assert_eq!(scratch.bus(&["send", "kept"], Some(record)).code, 0);
    assert_eq!(
        scratch.marker("ran"),
        2,
        "a corrupt record was read as an outcome"
    );
    let rewritten: Value =
        serde_json::from_slice(&std::fs::read(&recorded[0]).expect("read")).expect("rewritten");
    assert_eq!(rewritten["outcome"], json!("keep"));
    assert_eq!(scratch.bus(&["send", "kept"], Some(record)).code, 0);
    assert_eq!(scratch.marker("ran"), 2);

    // A cache that cannot be written runs the command each time and keeps.
    for _ in 0..2 {
        assert_eq!(scratch.bus(&["send", "blocked"], Some(record)).code, 0);
    }
    assert_eq!(scratch.marker("ran"), 4);
    assert_eq!(scratch.raw_lines("blocked").len(), 2);

    // A fingerprint that cannot be read keys nothing.
    for _ in 0..2 {
        assert_eq!(scratch.bus(&["send", "unkeyed"], Some(record)).code, 0);
    }
    assert_eq!(scratch.marker("unkeyed-ran"), 2);
    assert!(
        !scratch.path("unkeyed-cache").exists(),
        "an unkeyed outcome was recorded"
    );

    // An unjudged answer is never recorded.
    for _ in 0..2 {
        assert_eq!(scratch.bus(&["send", "undecided"], Some(record)).code, 1);
    }
    assert_eq!(scratch.marker("undecided-ran"), 2);
    assert!(
        !scratch.path("undecided-cache").exists(),
        "an unjudged answer was recorded"
    );
    assert_eq!(scratch.raw("undecided"), "");
}

/// The desk's split offer: a verdict for `answers`, and an action for `actions`.
const ENVELOPE: &str = r#"{"version":3,"completion":false,"message":"go on","actions":[{"op":"add","id":"n","task":"write it down"}]}"#;

#[test]
fn a_record_the_layout_routes_elsewhere_is_changed_dropped_or_rerouted_by_that_queues_hook() {
    let scratch = Scratch::new();
    let retitle = scratch.script(
        "retitle.sh",
        "cat > /dev/null\necho '{\"actions\":[{\"op\":\"add\",\"id\":\"n\",\"task\":\"written\"}]}'",
    );
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: actions, command: {}}}\n",
        scratch.desk(),
        argv(&[&retitle])
    ));
    let changed = scratch.bus(&["send", "answers"], Some(ENVELOPE));
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(scratch.raw_lines("answers").len(), 1);
    assert_eq!(
        scratch.raw("actions"),
        "{\"id\":0,\"actions\":[{\"op\":\"add\",\"id\":\"n\",\"task\":\"written\"}]}\n",
        "the routed change was not what was appended"
    );

    // What the hook made is prepared again: the author's grants apply to it.
    let demoted = scratch.script(
        "demoted.sh",
        "cat > /dev/null\necho '{\"author\":\"bot\",\"actions\":[{\"op\":\"drop\",\"id\":\"n\"}]}'",
    );
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: actions, command: {}}}\n",
        scratch.desk(),
        argv(&[&demoted])
    ));
    let refused = scratch.bus(&["send", "answers"], Some(ENVELOPE));
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    assert!(
        refused
            .stderr
            .contains("'drop' is not an op the bot may raise at the desk"),
        "{}",
        refused.stderr
    );
    assert_eq!(
        scratch.raw_lines("answers").len(),
        1,
        "the verdict half of a refused offer landed"
    );
    assert_eq!(scratch.raw_lines("actions").len(), 1);

    // A drop of one half leaves the other.
    let drop = scratch.script("drop.sh", "cat > /dev/null");
    scratch.configure(&format!(
        "{}hooks:\n  - {{on: actions, command: {}}}\n",
        scratch.desk(),
        argv(&[&drop])
    ));
    let validated = scratch.bus(&["validate", "answers"], Some(ENVELOPE));
    assert_eq!(validated.code, 0, "{}", validated.stderr);
    let judged = one_line(&validated);
    assert_eq!(judged["outcome"], json!("change"), "{judged}");
    assert_eq!(
        judged["records"].as_array().map(Vec::len),
        Some(1),
        "{judged}"
    );
    assert_eq!(judged["records"][0]["queue"], json!("answers"), "{judged}");
    let halved = scratch.bus(&["send", "answers"], Some(ENVELOPE));
    assert_eq!(halved.code, 0, "{}", halved.stderr);
    let printed = halved.lines();
    assert_eq!(printed.len(), 2, "{}", halved.stdout);
    assert_eq!(printed[0]["queue"], json!("answers"));
    assert_eq!(printed[1], json!({"queue": "actions", "dropped": true}));
    assert_eq!(scratch.raw_lines("answers").len(), 2);
    assert_eq!(scratch.raw_lines("actions").len(), 1);
    let dropped = scratch.dropped("actions");
    assert_eq!(dropped.len(), 1);
    assert_eq!(
        dropped[0]["message"]["actions"][0]["task"],
        json!("write it down")
    );

    // A reroute of one half sends it on, through its new queue's hooks.
    let aside = scratch.script(
        "aside.sh",
        "cat\nprintf audit > \"$ONEMESSAGEBUS_HOOK_ROUTE\"",
    );
    let mark = scratch.script(
        "mark.sh",
        &format!("echo ran >> {}\ncat", scratch.at("audit-ran")),
    );
    scratch.configure(&format!(
        "{}queues:\n  audit: {{}}\nhooks:\n  - {{on: actions, command: {}, routes: [audit]}}\n  - {{on: audit, command: {}}}\n",
        scratch.desk(),
        argv(&[&aside]),
        argv(&[&mark])
    ));
    let moved = scratch.bus(&["send", "answers"], Some(ENVELOPE));
    assert_eq!(moved.code, 0, "{}", moved.stderr);
    assert_eq!(scratch.raw_lines("answers").len(), 3);
    assert_eq!(
        scratch.raw_lines("actions").len(),
        1,
        "a rerouted half landed where it was routed"
    );
    assert_eq!(
        scratch.marker("audit-ran"),
        1,
        "the new queue's hooks did not run"
    );
    let audited: Value = serde_json::from_str(&scratch.raw_lines("audit")[0]).expect("JSON");
    assert_eq!(audited["actions"][0]["op"], json!("add"));
}

#[test]
fn a_split_offer_one_of_whose_records_breaks_its_schema_appends_nothing_and_drops_nothing() {
    let scratch = Scratch::new();
    let drop = scratch.script("drop.sh", "cat > /dev/null");
    scratch.configure(&format!(
        "profile: split\nschemas:\n  - {}\nhooks:\n  - {{on: left, command: {}}}\n",
        serde_json::to_string(&format!("{}@1", hooks_bundle().display())).expect("a path"),
        argv(&[&drop])
    ));
    let broken = scratch.bus(&["send", "inbox"], Some(r#"{"l":1,"r":2}"#));
    assert_eq!(broken.code, 1, "{}", broken.stdout);
    assert!(
        broken.stderr.contains("right: the record is not a"),
        "{}",
        broken.stderr
    );
    for queue in ["inbox", "left", "right", "left-dropped"] {
        assert_eq!(scratch.raw(queue), "", "{queue}");
    }

    let whole = scratch.bus(&["send", "inbox"], Some(r#"{"l":1,"r":2,"text":"t"}"#));
    assert_eq!(whole.code, 0, "{}", whole.stderr);
    assert_eq!(scratch.raw("right"), "{\"r\":2,\"text\":\"t\"}\n");
    assert_eq!(scratch.raw("left"), "");
    assert_eq!(scratch.dropped("left").len(), 1);
}

#[test]
fn a_cache_record_that_is_json_but_not_this_hooks_outcome_is_run_again_and_rewritten() {
    let scratch = Scratch::new();
    let change = scratch.script(
        "change.sh",
        &format!(
            "echo ran >> {}\ncat > /dev/null\necho '{{\"changed\":true}}'",
            scratch.at("ran")
        ),
    );
    scratch.configure(&format!(
        "queues:\n  notes: {{}}\nhooks:\n  - {{on: notes, command: {}, cache: {{dir: {}, bar_fingerprint: [echo, bar-1]}}}}\n",
        argv(&[&change]),
        serde_json::to_string(&scratch.path("cache")).expect("a path")
    ));
    let record = r#"{"text":"same"}"#;
    assert_eq!(scratch.bus(&["send", "notes"], Some(record)).code, 0);
    let path = std::fs::read_dir(scratch.path("cache"))
        .expect("the cache")
        .next()
        .expect("one record")
        .expect("an entry")
        .path();
    let good: Value = serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("JSON");
    assert_eq!(good["outcome"], json!("change"));
    let altered = |edit: &dyn Fn(&mut Value)| {
        let mut record = good.clone();
        edit(&mut record);
        record
    };
    let variants = [
        (
            "a later version",
            altered(&|r| r["schema_version"] = json!(3)),
        ),
        (
            "a change without its output",
            altered(&|r| {
                r.as_object_mut().expect("an object").remove("output");
            }),
        ),
        (
            "a keep carrying an output",
            altered(&|r| r["outcome"] = json!("keep")),
        ),
        (
            "an output that is no record",
            altered(&|r| r["output"] = json!("{\n}")),
        ),
        (
            "another bar's fingerprint",
            altered(&|r| r["fingerprint"] = json!("bar-0")),
        ),
        (
            "an outcome no hook answers",
            altered(&|r| r["outcome"] = json!("reroute")),
        ),
    ];
    for (runs, (what, variant)) in variants.iter().enumerate() {
        std::fs::write(&path, variant.to_string()).expect("written");
        let sent = scratch.bus(&["send", "notes"], Some(record));
        assert_eq!(sent.code, 0, "{what}: {}", sent.stderr);
        assert_eq!(
            scratch.marker("ran"),
            runs + 2,
            "{what} was read as an outcome"
        );
        let rewritten: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("JSON");
        assert_eq!(rewritten, good, "{what} was not recorded again");
    }
    assert!(
        scratch
            .raw_lines("notes")
            .iter()
            .all(|line| line == r#"{"changed":true}"#),
        "{}",
        scratch.raw("notes")
    );
}

#[test]
fn a_routed_change_its_queue_prepares_onto_another_queue_is_appended_without_that_queues_hooks() {
    let scratch = Scratch::new();
    let turn = scratch.script("turn.sh", "cat > /dev/null\necho '{\"n\":1}'");
    let sink = scratch.script(
        "sink.sh",
        &format!("echo ran >> {}\ncat", scratch.at("sink-ran")),
    );
    scratch.configure(&format!(
        "profile: relay\nschemas:\n  - {}\nhooks:\n  - {{on: relay, command: {}}}\n  - {{on: sink, command: {}}}\n",
        serde_json::to_string(&format!("{}@1", hooks_bundle().display())).expect("a path"),
        argv(&[&turn]),
        argv(&[&sink])
    ));
    let sent = scratch.bus(&["send", "inbox"], Some(r#"{"m":1}"#));
    assert_eq!(sent.code, 0, "{}", sent.stderr);
    assert_eq!(one_line(&sent)["queue"], json!("sink"));
    assert_eq!(scratch.raw("sink"), "{\"n\":1}\n");
    assert_eq!(scratch.raw("relay"), "");
    assert_eq!(scratch.raw("inbox"), "");
    assert_eq!(
        scratch.marker("sink-ran"),
        0,
        "a record a routed change was prepared onto ran its queue's hooks again"
    );

    // Offered to the sink itself, its hooks run.
    assert_eq!(scratch.bus(&["send", "sink"], Some(r#"{"n":2}"#)).code, 0);
    assert_eq!(scratch.marker("sink-ran"), 1);
}

#[test]
fn a_reply_a_hook_refuses_or_cannot_decide_prints_nothing_and_leaves_the_question_pending() {
    let scratch = Scratch::new();
    let refuse = scratch.script(
        "refuse.sh",
        "cat > /dev/null\necho 'not that answer' >&2\nexit 1",
    );
    let undecided = scratch.script(
        "undecided.sh",
        "cat > /dev/null\necho 'out of quota' >&2\nexit 2",
    );
    let configure = |command: &str| {
        scratch.configure(&format!(
            "queues:\n  questions: {{policy: {{hold_pending: true}}, answers: replies}}\n  replies: {{}}\nhooks:\n  - {{on: replies, command: {}}}\n",
            argv(&[command])
        ));
    };
    configure(&refuse);
    let asked = scratch.bus(
        &["ask", "questions", "--timeout", "1"],
        Some(r#"{"message":"anyone?"}"#),
    );
    let correlation = one_line(&asked)["correlation"]
        .as_str()
        .expect("a correlation")
        .to_owned();
    for (command, says) in [(&refuse, "not that answer"), (&undecided, "exited 2")] {
        configure(command);
        let replied = scratch.bus(
            &["reply", "questions", "--correlation", &correlation],
            Some(r#"{"message":"me"}"#),
        );
        assert_eq!(replied.code, 1, "{says}: {}", replied.stderr);
        assert_eq!(replied.stdout, "", "{says}");
        assert_eq!(
            replied.stderr.lines().count(),
            1,
            "{says}: {}",
            replied.stderr
        );
        let line = replied.stderr.trim_end();
        assert!(
            line.starts_with("onemessagebus: replies: ")
                && line.contains(&format!("hook 0 (`{command}`)"))
                && line.contains(says),
            "{line}"
        );
    }
    assert_eq!(scratch.raw("replies"), "");
    assert_eq!(scratch.raw("replies-dropped"), "");
    // The question still stands: a reply that lets the hook through answers it.
    let keep = scratch.script("keep.sh", "cat");
    configure(&keep);
    let answered = scratch.bus(
        &["reply", "questions", "--correlation", &correlation],
        Some(r#"{"message":"me"}"#),
    );
    assert_eq!(answered.code, 0, "{}", answered.stderr);
    assert_eq!(one_line(&answered)["correlation"], json!(correlation));
}
