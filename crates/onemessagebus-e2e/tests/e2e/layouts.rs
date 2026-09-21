//! A layout declared as data, through the built binary: the bus's own `desk`
//! bundle (`tests/layouts/desk.json`) linked by a configuration and named by its
//! `profile`, and every declaration the document makes — each queue key, the
//! operations and authors, and every preparation step — proven by what the
//! binary prints, the refusals it gives in the desk's own words, and the files
//! it leaves.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::support::{desk_bundle, desk_config, run_in, Run};

/// A scratch directory holding a configuration that links the desk.
struct Desk {
    dir: tempfile::TempDir,
    config: PathBuf,
}

impl Desk {
    fn new(extra: &str) -> Self {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let config = desk_config(dir.path(), extra);
        Self { dir, config }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The binary with `args` and `--config`, from the scratch root.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let mut argv = args.to_vec();
        argv.extend(["--config", self.config.to_str().expect("a UTF-8 path")]);
        run_in(self.root(), &argv, stdin, &[])
    }

    fn lines(&self, queue: &str) -> Vec<Value> {
        std::fs::read_to_string(self.root().join("bus").join(format!("{queue}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }

    /// Where each line `send` printed landed.
    fn sent(&self, queue: &str, record: &Value) -> Vec<String> {
        let run = self.run(&["send", queue], Some(&record.to_string()));
        assert_eq!(run.code, 0, "{record}: {}", run.stderr);
        run.lines()
            .iter()
            .map(|line| line["queue"].as_str().expect("a queue").to_owned())
            .collect()
    }

    /// The layout's refusal of `record` offered to `queue`: exit 1, one line.
    fn refused(&self, queue: &str, record: &Value) -> String {
        let run = self.run(&["send", queue], Some(&record.to_string()));
        assert_eq!(run.code, 1, "{record} was not refused: {}", run.stdout);
        assert_eq!(run.stdout, "");
        run.stderr
            .trim_end()
            .strip_prefix(&format!("onemessagebus: {queue}: "))
            .unwrap_or_else(|| panic!("not the layout's refusal: {}", run.stderr))
            .to_owned()
    }

    fn status(&self) -> Vec<Value> {
        let run = self.run(&["status"], None);
        assert_eq!(run.code, 0, "{}", run.stderr);
        serde_json::from_str(&run.stdout).expect("a JSON list")
    }
}

fn question(message: &str, source: &str, blocking: bool) -> Value {
    json!({"kind": "question", "message": message, "source": source, "blocking": blocking})
}

#[test]
fn the_profile_resolves_against_the_linked_desk_and_declares_each_queue_key() {
    let desk = Desk::new("");
    let statuses = desk.status();
    let names: Vec<&str> = statuses
        .iter()
        .map(|status| status["queue"].as_str().expect("a queue"))
        .collect();
    assert_eq!(names, ["actions", "answers", "outcomes", "questions"]);
    let of = |name: &str| {
        statuses
            .iter()
            .find(|status| status["queue"] == json!(name))
            .cloned()
            .expect("a status")
    };
    assert_eq!(
        of("questions")["events"],
        json!(true),
        "a policy that keeps events"
    );
    assert_eq!(of("answers")["events"], json!(false));
    assert_eq!(
        of("actions")["cursors"],
        json!({"auditor": null, "default": null}),
        "the declared consumers"
    );

    // `schema`: a record the queue's schema refuses is refused by the linked id.
    let run = desk.run(
        &["send", "outcomes"],
        Some(r#"{"id": 4, "applied": "yes"}"#),
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stderr.contains("desk.outcome@1"), "{}", run.stderr);
    assert_eq!(
        desk.sent("outcomes", &json!({"id": 4, "applied": true})),
        ["outcomes"]
    );
    assert_eq!(
        desk.lines("outcomes"),
        [json!({"id": 4, "applied": true})],
        "not numbered"
    );

    // `numbered`
    let answered = desk.run(
        &["send", "actions"],
        Some(r#"{"actions": [{"op": "note"}]}"#),
    );
    assert_eq!(answered.lines()[0]["id"], json!(0), "{}", answered.stderr);

    let unknown = desk.run(&["send", "tickets"], Some("{}"));
    assert_eq!(unknown.code, 2);
    assert_eq!(
        unknown.stderr.trim_end(),
        "onemessagebus: `tickets` is not a queue this configuration declares; it declares: actions, answers, outcomes, questions"
    );
}

#[test]
fn a_question_is_renamed_stamped_projected_superseded_and_claimed_blocking_first() {
    let desk = Desk::new("");
    let mut first = question("which base?", "proposal", false);
    first["about"] = json!("build");
    assert_eq!(desk.sent("questions", &first), ["questions"]);
    let queued = desk.lines("questions");
    assert_eq!(queued[0]["subject"], json!("build"), "`about` renamed");
    assert!(queued[0].get("about").is_none());
    assert!(
        queued[0]["raised_at"].as_u64().is_some_and(|at| at > 0),
        "stamped: {}",
        queued[0]
    );
    let mut kept = question("kept", "proposal", false);
    kept["about"] = json!("dropped");
    kept["subject"] = json!("kept");
    kept["raised_at"] = json!(7);
    desk.sent("questions", &kept);
    let queued = desk.lines("questions");
    assert_eq!(queued[1]["subject"], json!("kept"), "an existing name wins");
    assert_eq!(queued[1]["raised_at"], json!(7), "a stamp never overwrites");
    let projection: Value = serde_json::from_str(
        &std::fs::read_to_string(desk.root().join("bus/questions.json")).expect("the projection"),
    )
    .expect("JSON");
    assert_eq!(projection["next_id"], json!(2));

    // `supersede_on` a check-in, and nothing else
    desk.sent("questions", &question("first check-in", "check-in", false));
    desk.sent("questions", &question("second check-in", "check-in", false));
    let waiting = desk.status();
    let questions = waiting
        .iter()
        .find(|status| status["queue"] == json!("questions"))
        .expect("questions");
    let messages: Vec<&Value> = questions["waiting"]
        .as_array()
        .expect("waiting")
        .iter()
        .map(|record| &record["message"])
        .collect();
    assert_eq!(
        messages,
        [
            &json!("which base?"),
            &json!("kept"),
            &json!("second check-in")
        ],
        "the newer check-in superseded the waiting one"
    );

    // `blocking_first`, then `hold_pending`
    desk.sent("questions", &question("stop the line?", "proposal", true));
    let claimed = desk.run(&["next", "questions"], None);
    assert_eq!(claimed.code, 0, "{}", claimed.stderr);
    assert_eq!(
        claimed.lines()[0]["record"]["message"],
        json!("stop the line?")
    );
    let status = desk.run(&["status", "questions"], None);
    let status: Value = serde_json::from_str(&status.stdout).expect("JSON");
    assert_eq!(status[0]["pending"]["message"], json!("stop the line?"));

    // `answers`: the pending question's reply goes to `answers`, routed as an
    // offer to it is.
    let position = claimed.lines()[0]["position"].to_string();
    let replied = desk.run(
        &["reply", "questions", &position],
        Some(r#"{"message": "yes"}"#),
    );
    assert_eq!(replied.code, 0, "{}", replied.stderr);
    assert_eq!(desk.lines("answers")[0]["reply"], json!({"message": "yes"}));
}

#[test]
fn an_answer_is_checked_versioned_granted_and_routed_in_the_desks_own_words() {
    let desk = Desk::new("authors:\n  sentinel: {capabilities: [finding]}\n");
    // Both halves, the version read as 3.
    assert_eq!(
        desk.sent(
            "answers",
            &json!({"version": 2, "completion": false, "message": "go on", "actions": [{"op": "retry", "node": "build"}]})
        ),
        ["actions", "answers"]
    );
    // The actions half alone, and the fallback for an answer carrying neither.
    assert_eq!(
        desk.sent(
            "answers",
            &json!({"version": 3, "author": "sentinel", "actions": [{"op": "finding"}]})
        ),
        ["actions"]
    );
    assert_eq!(desk.sent("answers", &json!({})), ["answers"]);
    let answers = desk.lines("answers");
    assert_eq!(
        answers[0]["reply"],
        json!({"version": 3, "completion": false, "message": "go on", "actions": [{"op": "retry", "node": "build"}]}),
        "the answer's projection, under `reply`"
    );
    assert!(
        answers[0]["at"].as_u64().is_some_and(|at| at > 0),
        "stamped"
    );
    assert_eq!(answers[1]["reply"], json!({}));
    assert_eq!(
        desk.lines("actions"),
        [
            json!({"id": 0, "actions": [{"op": "retry", "node": "build"}]}),
            json!({"id": 1, "author": "sentinel", "actions": [{"op": "finding"}]})
        ],
        "the actions' projection"
    );

    // A framed answer is checked and kept whole; one carrying only actions is
    // passed over by a claim (`claims`).
    assert_eq!(
        desk.sent(
            "answers",
            &json!({"reply": {"version": 3, "actions": [{"op": "note"}]}, "at": 1})
        ),
        ["answers"]
    );
    let claims: Vec<i32> = (0..3)
        .map(|_| desk.run(&["next", "answers"], None).code)
        .collect();
    assert_eq!(
        claims,
        [0, 0, 1],
        "the actions-only answer is not handed out"
    );

    for (record, words) in [
        (
            json!({"message": "hi", "bogus": 1}),
            "the answer is malformed: Additional properties are not allowed ('bogus' was unexpected)",
        ),
        (
            json!({"reply": {"message": 5}, "at": 1}),
            "the answer is malformed: at /message: 5 is not of type \"string\"",
        ),
        (
            json!({"author": "ghost", "message": "hi"}),
            "the answer's author `ghost` is not one the desk declares; it declares: bot, lead, sentinel",
        ),
        (
            json!({"author": "sentinel", "completion": true}),
            "declaring the desk done is not something the sentinel may do: nothing grants it to this author",
        ),
        (
            json!({"actions": [{"op": "retry"}]}),
            "an answer carrying actions requires version 3; this one declares none",
        ),
        (
            json!({"version": 4, "actions": [{"op": "retry"}]}),
            "an answer carrying actions requires version 3; this one declares 4",
        ),
        (
            json!({"version": 3, "author": "sentinel", "actions": [{"op": "retry"}]}),
            "'retry' is not an op the sentinel may raise at the desk: nothing grants it to this author. Ask the lead instead",
        ),
        (
            json!({"version": 3, "author": "bot", "actions": [{"op": "drop"}]}),
            "'drop' is not an op the bot may raise at the desk: a bot drops nothing it did not add. Ask the lead instead",
        ),
        (
            json!({"version": 3, "actions": [{"op": "launch"}]}),
            "'launch' is not an op of the desk; the ops are: add, drop, retry, cancel, finding, note, complete",
        ),
        (
            json!({"reply": {"version": 3, "author": "bot", "completion": true}, "at": 1}),
            "declaring the desk done is not something the bot may do: a bot never declares the desk done",
        ),
    ] {
        assert_eq!(desk.refused("answers", &record), words, "{record}");
    }
    assert_eq!(
        desk.refused("actions", &json!({"actions": "retry"})),
        "the answer's actions are malformed: `actions` is not a list"
    );
    assert_eq!(
        desk.refused("actions", &json!({"author": "bot", "actions": [{"op": "cancel"}]})),
        "'cancel' is not an op the bot may raise at the desk: nothing grants it to this author. Ask the lead instead"
    );
    assert_eq!(
        desk.lines("actions").len(),
        2,
        "nothing refused was appended"
    );
    assert_eq!(desk.lines("answers").len(), 3);
}

#[test]
fn a_configuration_narrows_the_fully_granted_author_and_is_refused_widening_one() {
    let narrowed = Desk::new("authors:\n  lead: {capabilities: [note, finding]}\n");
    assert_eq!(
        narrowed.sent("actions", &json!({"actions": [{"op": "note"}]})),
        ["actions"],
        "the lead, the author an action names by default, keeps what it was narrowed to"
    );
    assert_eq!(
        narrowed.refused("actions", &json!({"actions": [{"op": "retry"}]})),
        "'retry' is not an op the lead may raise at the desk: the configuration does not grant it. Ask the lead instead"
    );

    for (extra, refusal) in [
        (
            "authors:\n  bot: {capabilities: [note, complete]}\n",
            "authors.bot.capabilities: `complete` is not granted to bot by the profile, and a configuration may narrow an author's grants but never widen them",
        ),
        (
            "authors:\n  lead: {capabilities: [launch]}\n",
            "authors.lead.capabilities: `launch` is not an op; the ops are: add, drop, retry, cancel, finding, note, complete",
        ),
    ] {
        let widened = Desk::new(extra);
        let run = widened.run(&["status"], None);
        assert_eq!(run.code, 2, "{extra}: {}", run.stdout);
        assert_eq!(run.stderr.trim_end(), format!("onemessagebus: {refusal}"));
    }
}

#[test]
fn a_program_that_links_a_desk_of_its_own_keeps_it_over_the_linked_one() {
    let desk = Desk::new("");
    let linked = desk.status();
    assert_eq!(linked.len(), 4, "the plain binary links no desk of its own");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_onemessagebus-with-desk"))
        .args(["status", "--config", desk.config.to_str().expect("a path")])
        .current_dir(desk.root())
        .env_remove("ONEMESSAGEBUS_CONFIG")
        .env_remove("ONEMESSAGEBUS_TRANSPORT_DIR")
        .env_remove("ONEMESSAGEBUS_REGISTRY")
        .output()
        .expect("the program runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let compiled: Value = serde_json::from_slice(&output.stdout).expect("a JSON list");
    let names: Vec<&Value> = compiled
        .as_array()
        .expect("a list")
        .iter()
        .map(|status| &status["queue"])
        .collect();
    assert_eq!(names, [&json!("compiled-desk")]);
}

#[test]
fn a_layout_that_cannot_be_linked_or_named_is_refused_naming_why() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let mut bundle: Value =
        serde_json::from_str(&std::fs::read_to_string(desk_bundle()).expect("the fixture"))
            .expect("JSON");
    bundle["layouts"][0]["authors"]["lead"]["capabilities"] = json!(["note"]);
    let broken = dir.path().join("broken.json");
    std::fs::write(&broken, bundle.to_string()).expect("written");
    let config = dir.path().join("broken.yaml");
    std::fs::write(
        &config,
        format!(
            "version: 1\ntransport: {{kind: local, dir: bus}}\nprofile: desk\nschemas: [{}]\n",
            serde_json::to_string(&broken).expect("a path")
        ),
    )
    .expect("written");
    let run = run_in(
        dir.path(),
        &["status", "--config", config.to_str().expect("a path")],
        None,
        &[],
    );
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert!(
        run.stderr.trim_end().ends_with(
            "not a schema bundle: layouts[0]: is not a layout: `every_op` grants every op, so it takes no `capabilities`"
        ),
        "{}",
        run.stderr
    );

    let unnamed = Desk::new("");
    std::fs::write(
        &unnamed.config,
        std::fs::read_to_string(&unnamed.config)
            .expect("the configuration")
            .replace("profile: desk", "profile: help-desk"),
    )
    .expect("rewritten");
    let run = unnamed.run(&["status"], None);
    assert_eq!(run.code, 2);
    assert_eq!(
        run.stderr.trim_end(),
        "onemessagebus: profile: `help-desk` is not a layout this build links or a linked bundle declares; the layouts are: planner-channel, desk"
    );
}
