//! Contract V through the built binary: `validate`, and `send` refused before
//! anything is appended, with a scripted validator command.
//!
//! The validator a configuration names is this test binary, re-run as
//! `validators::scripted_validator` with a script path after it. It logs the
//! message it read on stdin and answers with the exit status, stderr and stdout its script
//! gives its role: `validate` when the binary tells it a queue, `fingerprint`
//! when it is run as a pass cache's bar. A journey rewrites the script between
//! invocations to move the bar or change the answer.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use onemessagebus::VALIDATE_QUEUE_ENV;
use serde_json::{json, Value};

use crate::support::{run_in, Run};

/// The test path a validator command names this subprocess fixture by.
const DOUBLE: &str = "validators::scripted_validator";

/// The subprocess fixture, whose ordinary invocation verifies it is not a child.
// llmlint: ignore-block[e2e_not_mocked] This scripted command is the real external validator Contract V admits: the shipped binary launches it as a real subprocess and maps its exit 0, 1, or other status to pass, refuse, or unjudged. Nothing above that process boundary is doubled; the script varies the external command's observed response so the journeys cover every contract outcome.
#[test]
fn scripted_validator() {
    let arguments: Vec<String> = std::env::args().collect();
    let Some(script) = arguments
        .iter()
        .position(|argument| argument == DOUBLE)
        .and_then(|at| arguments.get(at + 1))
    else {
        assert!(
            std::env::var_os(VALIDATE_QUEUE_ENV).is_none(),
            "the ordinary journey invocation must not look like a validator child"
        );
        return;
    };
    let role = if std::env::var_os(VALIDATE_QUEUE_ENV).is_some() {
        "validate"
    } else {
        "fingerprint"
    };
    let part = serde_json::from_str::<Value>(
        &std::fs::read_to_string(script).expect("the subprocess fixture's script is readable"),
    )
    .expect("the subprocess fixture's script is JSON")[role]
        .clone();
    let mut stdin = String::new();
    if role == "validate" {
        std::io::stdin()
            .read_to_string(&mut stdin)
            .expect("the message is readable");
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{script}.log"))
        .expect("the subprocess fixture's log opens");
    writeln!(
        log,
        "{}",
        json!({"role": role, "queue": std::env::var(VALIDATE_QUEUE_ENV).ok(), "stdin": stdin})
    )
    .expect("the subprocess fixture logs");
    let mut out = std::io::stdout();
    out.write_all(part["stdout"].as_str().unwrap_or_default().as_bytes())
        .and_then(|()| out.flush())
        .expect("the subprocess fixture writes stdout");
    let mut err = std::io::stderr();
    err.write_all(part["stderr"].as_str().unwrap_or_default().as_bytes())
        .and_then(|()| err.flush())
        .expect("the subprocess fixture writes stderr");
    std::process::exit(
        part["exit"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok())
            .unwrap_or(0),
    );
}
// llmlint: ignore-end[e2e_not_mocked] The external validator subprocess fixture ends here.

/// A scratch directory holding a channel, the subprocess fixture's script and a
/// configuration.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let scratch = Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        };
        scratch.script(json!({"exit": 0}), json!({"exit": 0, "stdout": "bar-1"}));
        scratch
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn script(&self, validate: Value, fingerprint: Value) {
        std::fs::write(
            self.path("script.json"),
            json!({"validate": validate, "fingerprint": fingerprint}).to_string(),
        )
        .expect("the script is written");
    }

    /// The double's argv as a YAML flow sequence.
    fn command(&self) -> String {
        let exe = std::env::current_exe().expect("this test binary");
        serde_json::to_string(&[
            exe.to_str().expect("a UTF-8 path"),
            "--exact",
            DOUBLE,
            self.path("script.json").to_str().expect("a UTF-8 path"),
        ])
        .expect("an argv")
    }

    fn quoted(&self, path: &Path) -> String {
        serde_json::to_string(path).expect("a path")
    }

    /// Write `onemessagebus.yaml` with `rest` after the transport.
    fn configure(&self, rest: &str) {
        std::fs::write(
            self.path("onemessagebus.yaml"),
            format!(
                "version: 1\ntransport: {{kind: local, dir: {}}}\n{rest}",
                self.quoted(&self.path("channel"))
            ),
        )
        .expect("the configuration is written");
    }

    fn bus(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let config = self.path("onemessagebus.yaml");
        let mut argv = args.to_vec();
        argv.extend(["--config", config.to_str().expect("a UTF-8 path")]);
        run_in(self.dir.path(), &argv, stdin, &[])
    }

    fn ran(&self, role: &str) -> Vec<Value> {
        std::fs::read_to_string(self.path("script.json.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("a log line"))
            .filter(|line| line["role"] == json!(role))
            .collect()
    }

    fn lines(&self, queue: &str) -> Vec<Value> {
        std::fs::read_to_string(self.path("channel").join(format!("{queue}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }

    fn passes(&self) -> usize {
        std::fs::read_dir(self.path("passes")).map_or(0, Iterator::count)
    }
}

fn verdict(run: &Run) -> Value {
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
fn validate_prints_each_verdict_the_scripted_command_reaches_and_appends_nothing() {
    let scratch = Scratch::new();
    scratch.configure(&format!(
        "queues:\n  findings: {{}}\nvalidators:\n  - {{on: findings, kind: command, command: {}}}\n",
        scratch.command()
    ));
    let record = r#"{"what":"the base moved"}"#;

    let passed = scratch.bus(&["validate", "findings"], Some(record));
    assert_eq!(passed.code, 0, "{}", passed.stderr);
    assert_eq!(
        verdict(&passed),
        json!({"queue": "findings", "verdict": "pass"})
    );
    assert_eq!(passed.stderr, "");
    let ran = scratch.ran("validate");
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0]["queue"], json!("findings"));
    assert_eq!(
        serde_json::from_str::<Value>(ran[0]["stdin"].as_str().expect("stdin")).expect("JSON"),
        serde_json::from_str::<Value>(record).expect("JSON"),
        "the validator was not handed the record"
    );

    let reason = "the criterion names no observable outcome:\n  `make it better`\n";
    scratch.script(
        json!({"exit": 1, "stderr": reason}),
        json!({"exit": 0, "stdout": "bar-1"}),
    );
    let refused = scratch.bus(&["validate", "findings"], Some(record));
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert_eq!(
        verdict(&refused),
        json!({"queue": "findings", "verdict": "refuse", "reason": reason}),
        "the reason on stdout is not the validator's, unaltered"
    );
    assert!(
        refused.stderr.contains(reason),
        "the reason on stderr is not the validator's, unaltered: {}",
        refused.stderr
    );

    scratch.script(
        json!({"exit": 7, "stderr": "no quota left"}),
        json!({"exit": 0, "stdout": "bar-1"}),
    );
    let unjudged = scratch.bus(&["validate", "findings"], Some(record));
    assert_eq!(
        unjudged.code, 1,
        "an unjudged record passed: {}",
        unjudged.stdout
    );
    let judged = verdict(&unjudged);
    assert_eq!(judged["verdict"], json!("unjudged"));
    let why = judged["reason"].as_str().expect("a reason");
    assert!(
        why.contains("exited 7") && why.contains("no quota left"),
        "{why}"
    );

    let unknown = scratch.bus(&["validate", "reviews"], Some(record));
    assert_eq!(unknown.code, 2, "{}", unknown.stderr);
    assert!(
        unknown.stderr.contains("`reviews` is not a queue"),
        "{}",
        unknown.stderr
    );
    assert_eq!(
        scratch.ran("validate").len(),
        3,
        "a refused queue ran the validator"
    );
    assert!(
        !scratch.path("channel").join("findings.jsonl").exists(),
        "validate appended"
    );
}

#[test]
fn a_reply_carrying_commands_that_its_validator_refuses_is_appended_nowhere() {
    let scratch = Scratch::new();
    let reason = "an `add` states task prose the bar refuses\n";
    scratch.script(
        json!({"exit": 1, "stderr": reason}),
        json!({"exit": 0, "stdout": "bar-1"}),
    );
    scratch.configure(&format!(
        "profile: planner-channel\nvalidators:\n  - {{on: replies, when: {{carries: commands}}, kind: command, command: {}}}\n",
        scratch.command()
    ));
    let edit = r#"{"version":3,"completion":false,"message":"go on","commands":[{"op":"add","id":"n","task":"make it better"}]}"#;
    let refused = scratch.bus(&["send", "replies"], Some(edit));
    assert_eq!(refused.code, 1, "{}", refused.stdout);
    assert_eq!(refused.stdout, "", "a refused send printed a landing");
    assert!(refused.stderr.contains(reason), "{}", refused.stderr);
    assert!(
        scratch.lines("replies").is_empty(),
        "the verdict half was appended"
    );
    assert!(
        scratch.lines("commands").is_empty(),
        "the commands half was appended"
    );
    let ran = scratch.ran("validate");
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0]["queue"], json!("replies"));

    let verdict_only = scratch.bus(
        &["send", "replies"],
        Some(r#"{"version":3,"completion":false,"message":"go on"}"#),
    );
    assert_eq!(verdict_only.code, 0, "{}", verdict_only.stderr);
    assert_eq!(
        scratch.ran("validate").len(),
        1,
        "a reply carrying no commands was judged"
    );
    assert_eq!(scratch.lines("replies").len(), 1);
}

#[test]
fn a_pass_cache_records_only_a_pass_and_a_moved_bar_runs_the_validator_again() {
    let scratch = Scratch::new();
    scratch.configure(&format!(
        "queues:\n  findings: {{}}\nvalidators:\n  - {{on: findings, kind: command, command: {command}, cache: {{dir: {dir}, bar_fingerprint: {command}}}}}\n",
        command = scratch.command(),
        dir = scratch.quoted(&scratch.path("passes")),
    ));
    let record = r#"{"what":"the base moved"}"#;
    for _ in 0..2 {
        let sent = scratch.bus(&["send", "findings"], Some(record));
        assert_eq!(sent.code, 0, "{}", sent.stderr);
    }
    assert_eq!(
        scratch.ran("validate").len(),
        1,
        "a recorded pass ran the validator again"
    );
    assert_eq!(scratch.passes(), 1);
    assert_eq!(scratch.lines("findings").len(), 2);

    scratch.script(json!({"exit": 0}), json!({"exit": 0, "stdout": "bar-2"}));
    let moved = scratch.bus(&["validate", "findings"], Some(record));
    assert_eq!(moved.code, 0, "{}", moved.stderr);
    assert_eq!(
        scratch.ran("validate").len(),
        2,
        "a pass recorded under the old bar was trusted under the new one"
    );
    assert_eq!(scratch.passes(), 2);

    scratch.script(
        json!({"exit": 1, "stderr": "refused"}),
        json!({"exit": 0, "stdout": "bar-3"}),
    );
    for _ in 0..2 {
        let refused = scratch.bus(&["send", "findings"], Some(record));
        assert_eq!(refused.code, 1, "{}", refused.stdout);
    }
    assert_eq!(
        scratch.ran("validate").len(),
        4,
        "a refusal was answered from a record"
    );
    assert_eq!(scratch.passes(), 2, "a refusal was recorded");
    assert_eq!(scratch.lines("findings").len(), 2);
}

#[test]
fn a_validators_block_with_an_unknown_key_or_an_undeclared_queue_is_refused_by_name() {
    let scratch = Scratch::new();
    scratch.configure(&format!(
        "queues:\n  findings: {{}}\nvalidators:\n  - {{on: findings, kind: command, command: {}, retries: 2}}\n",
        scratch.command()
    ));
    let unknown = scratch.bus(&["validate", "findings"], Some("{}"));
    assert_eq!(unknown.code, 2, "{}", unknown.stderr);
    assert!(unknown.stderr.contains("retries"), "{}", unknown.stderr);

    scratch.configure(&format!(
        "queues:\n  findings: {{}}\nvalidators:\n  - {{on: reviews, kind: command, command: {}}}\n",
        scratch.command()
    ));
    let undeclared = scratch.bus(&["send", "findings"], Some("{}"));
    assert_eq!(undeclared.code, 2, "{}", undeclared.stderr);
    assert!(
        undeclared.stderr.contains("validators[0].on") && undeclared.stderr.contains("reviews"),
        "{}",
        undeclared.stderr
    );
    assert!(scratch.ran("validate").is_empty());
}
