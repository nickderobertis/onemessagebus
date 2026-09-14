//! The resident core, `serve --resident`, driven from a raw unix socket.
//!
//! Every journey starts the built binary as a resident over a scratch directory
//! and speaks `bus.resident-protocol@1` to it line by line, the way an SDK does:
//! every capability answered, each refusal in the words and exit code the
//! one-shot verb gives, a subscription streamed until its predicate holds or it
//! is cancelled, a schema registered after the core started seen by the next
//! request, and a second resident on a live socket refused naming the first's
//! pid. Each resident is stopped by removing its socket, which ends the process
//! as a normal exit.

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use onemessagebus::CAPABILITIES;
use serde_json::{json, Value};

use crate::support::{onemessagebus, run_in, Run};

/// The schema the journeys register at run time, after the resident started.
fn greeting_schema() -> Value {
    json!({
        "title": "Greeting",
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
        "additionalProperties": false
    })
}

/// A scratch directory: a configuration declaring a `greetings` queue whose
/// schema nothing registers yet, beside the planner channel's queues.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let config = format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: planner-channel\nqueues:\n  greetings: {{schema: demo.greeting@1}}\n",
            dir.path().join("channel").display()
        );
        std::fs::write(dir.path().join("onemessagebus.yaml"), config).expect("a config");
        std::fs::write(
            dir.path().join("greeting.json"),
            greeting_schema().to_string(),
        )
        .expect("a schema file");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn text(&self, name: &str) -> String {
        self.path(name).to_str().expect("a UTF-8 path").to_owned()
    }

    fn socket(&self) -> PathBuf {
        self.path("bus.sock")
    }

    /// The one-shot queue verb over the same configuration and registry.
    fn one_shot(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let config = self.text("onemessagebus.yaml");
        let mut argv = args.to_vec();
        argv.extend(["--config", &config]);
        self.one_shot_schema(&argv, stdin)
    }

    /// The one-shot verb over the same registry, and nothing else: the flag goes
    /// before a `--`, after which every word is a positional.
    fn one_shot_schema(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let registry = self.text("registry");
        let mut argv = args.to_vec();
        let at = argv
            .iter()
            .position(|word| *word == "--")
            .unwrap_or(argv.len());
        argv.splice(at..at, ["--registry", registry.as_str()]);
        run_in(self.dir.path(), &argv, stdin, &[])
    }

    fn log(&self, queue: &str) -> Vec<Value> {
        std::fs::read_to_string(self.path("channel").join(format!("{queue}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }
}

/// A resident core this journey started, stopped by removing its socket.
struct Resident {
    child: Child,
    socket: PathBuf,
}

impl Resident {
    /// A resident over `scratch`'s configuration and registry, once it answers.
    fn start(scratch: &Scratch) -> Self {
        let child = resident_command(scratch, &scratch.socket())
            .spawn()
            .expect("the resident spawns");
        let resident = Self {
            child,
            socket: scratch.socket(),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while UnixStream::connect(&resident.socket).is_err() {
            assert!(
                Instant::now() < deadline,
                "the resident never listened on {}",
                resident.socket.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        resident
    }

    fn client(&self) -> Client {
        Client::connect(&self.socket)
    }

    /// Remove the socket and wait for the process to end on its own, exit 0.
    fn stop(mut self) {
        std::fs::remove_file(&self.socket).expect("the socket is removed");
        let status = wait(&mut self.child, Duration::from_secs(20));
        assert_eq!(status, Some(0), "the resident stops cleanly");
    }
}

impl Drop for Resident {
    fn drop(&mut self) {
        // A journey that panicked before `stop` still ends the process it began.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = std::fs::remove_file(&self.socket);
            if wait(&mut self.child, Duration::from_secs(5)).is_none() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

fn resident_command(scratch: &Scratch, socket: &Path) -> std::process::Command {
    let mut command = onemessagebus();
    command
        .args(["serve", "--resident", "--socket"])
        .arg(socket)
        .args(["--config", &scratch.text("onemessagebus.yaml")])
        .args(["--registry", &scratch.text("registry")])
        .current_dir(scratch.dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// The exit code `child` ends with within `within`, or `None` if it has not.
fn wait(child: &mut Child, within: Duration) -> Option<i32> {
    let deadline = Instant::now() + within;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return status.code();
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One connection to a resident.
struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("the resident accepts");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("a read timeout");
        Self {
            writer: stream.try_clone().expect("a write half"),
            reader: BufReader::new(stream),
        }
    }

    fn write(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .expect("the line is written");
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .expect("the resident answers within the timeout");
        assert!(!line.is_empty(), "the resident closed the connection");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON: {e}: {line}"))
    }

    /// Run `verb` with `args` and `input`, and hand back its one answering line.
    fn call(&mut self, id: u64, verb: &str, args: Value, input: Option<&str>) -> Value {
        let mut request = json!({"id": id, "verb": verb, "args": args});
        if let Some(input) = input {
            request["input"] = json!(input);
        }
        self.write(&request.to_string());
        let answer = self.read();
        assert_eq!(answer["id"], json!(id), "{answer}");
        answer
    }
}

/// `answer` is the resident's rendering of `run`: a success of a success, and
/// a refusal with the same exit code and the same words.
fn same_as_one_shot(verb: &str, answer: &Value, run: &Run) {
    match answer.get("error") {
        None => assert_eq!(run.code, 0, "{verb}: {answer} but one-shot: {}", run.stderr),
        Some(error) => {
            assert_eq!(
                error["exit"],
                json!(run.code),
                "{verb}: {answer}: {}",
                run.stderr
            );
            let said = run.stderr.lines().last().unwrap_or_default();
            assert_eq!(
                said,
                format!(
                    "onemessagebus: {}",
                    error["message"].as_str().expect("a message")
                ),
                "{verb}"
            );
        }
    }
}

fn ok(answer: &Value) -> &Value {
    answer
        .get("ok")
        .unwrap_or_else(|| panic!("a refusal where an answer was expected: {answer}"))
}

fn refused(answer: &Value, exit: i32) -> &str {
    let error = answer
        .get("error")
        .unwrap_or_else(|| panic!("an answer where a refusal was expected: {answer}"));
    assert_eq!(error["exit"], json!(exit), "{answer}");
    error["message"].as_str().expect("a message")
}

#[test]
fn the_resident_answers_every_capability_as_the_one_shot_verb_does() {
    let scratch = Scratch::new();
    let resident = Resident::start(&scratch);
    let mut client = resident.client();
    let mut answered: BTreeSet<&str> = BTreeSet::new();
    let greeting = r#"{"text":"hello"}"#;

    // Before anything registers demo.greeting@1, the queue naming it is refused
    // exactly as the one-shot verb refuses it.
    let unregistered = client.call(1, "send", json!({"queue": "greetings"}), Some(greeting));
    assert!(
        refused(&unregistered, 2).starts_with("queues.greetings.schema: demo.greeting@1"),
        "{unregistered}"
    );
    same_as_one_shot(
        "send",
        &unregistered,
        &scratch.one_shot(&["send", "greetings"], Some(greeting)),
    );

    // schemaRegister: recorded in the resident's registry directory, and seen by
    // the very next request with no restart.
    let registered = client.call(
        2,
        "schemaRegister",
        json!({"id": "demo.greeting@1", "file": scratch.text("greeting.json")}),
        None,
    );
    assert_eq!(ok(&registered), &json!(""));
    assert!(scratch.path("registry/demo.greeting@1.json").is_file());
    answered.insert("schemaRegister");

    let sent = client.call(3, "send", json!({"queue": "greetings"}), Some(greeting));
    assert_eq!(ok(&sent)[0]["queue"], json!("greetings"), "{sent}");
    assert_eq!(scratch.log("greetings"), vec![json!({"text": "hello"})]);
    answered.insert("send");

    // A record the registered schema refuses names the id and the pointer, and
    // is appended nowhere.
    let violating = client.call(
        4,
        "send",
        json!({"queue": "greetings"}),
        Some(r#"{"text":7}"#),
    );
    let why = refused(&violating, 1);
    assert!(
        why.contains("demo.greeting@1") && why.contains("/text"),
        "{why}"
    );
    same_as_one_shot(
        "send",
        &violating,
        &scratch.one_shot(&["send", "greetings"], Some(r#"{"text":7}"#)),
    );
    assert_eq!(scratch.log("greetings").len(), 1);

    let list = client.call(5, "schemaList", json!({}), None);
    let ids: Vec<&str> = ok(&list)
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert!(ids.contains(&"bus.resident-protocol@1") && ids.contains(&"demo.greeting@1"));
    let text_list = client.call(6, "schemaList", json!({"format": "text"}), None);
    assert!(ok(&text_list)
        .as_str()
        .is_some_and(|text| text.lines().any(|id| id == "demo.greeting@1")));
    answered.insert("schemaList");

    let check = client.call(
        7,
        "schemaCheck",
        json!({"id": "demo.greeting@1"}),
        Some(r#"{"text":false}"#),
    );
    assert!(refused(&check, 1).contains("/text"));
    same_as_one_shot(
        "schemaCheck",
        &check,
        &scratch.one_shot_schema(
            &["schema", "check", "demo.greeting@1"],
            Some(r#"{"text":false}"#),
        ),
    );
    answered.insert("schemaCheck");

    let generated = client.call(
        8,
        "schemaGen",
        json!({"id": "bus.resident-protocol@1", "lang": "json"}),
        None,
    );
    let protocol: Value =
        serde_json::from_str(ok(&generated).as_str().expect("text")).expect("the schema is JSON");
    assert_eq!(protocol["title"], json!("ResidentLine"));
    answered.insert("schemaGen");

    let validated = client.call(9, "validate", json!({"queue": "greetings"}), Some(greeting));
    assert_eq!(
        ok(&validated),
        &json!({"queue": "greetings", "verdict": "pass"})
    );
    answered.insert("validate");

    let claimed = client.call(10, "next", json!({"queue": "greetings"}), None);
    assert_eq!(
        ok(&claimed)["record"],
        json!({"text": "hello"}),
        "{claimed}"
    );
    let nothing = client.call(11, "next", json!({"queue": "greetings"}), None);
    assert_eq!(refused(&nothing, 1), "nothing on greetings to claim");
    answered.insert("next");

    let status = client.call(12, "status", json!({"queue": "greetings"}), None);
    assert_eq!(ok(&status)[0]["queue"], json!("greetings"), "{status}");
    same_as_one_shot(
        "status",
        &status,
        &scratch.one_shot(&["status", "greetings"], None),
    );
    answered.insert("status");

    let transports = client.call(13, "transports", json!({}), None);
    assert!(ok(&transports)
        .as_array()
        .expect("a list")
        .iter()
        .any(|kind| kind["kind"] == json!("local")));
    answered.insert("transports");

    let reply = client.call(14, "reply", json!({"queue": "greetings"}), Some("{}"));
    assert!(refused(&reply, 2).contains("greetings declares no queue its replies"));
    same_as_one_shot(
        "reply",
        &reply,
        &scratch.one_shot(&["reply", "greetings"], Some("{}")),
    );
    answered.insert("reply");

    // An ask nobody answers within its timeout answers the tagged `timeout`
    // beside the refusal, as the one-shot verb prints it beside its exit 1.
    let question =
        json!({"kind": "finding", "message": "anyone?", "source": "proposal", "blocking": false});
    let asked = client.call(
        15,
        "ask",
        json!({"queue": "surfaces", "timeout": 1}),
        Some(&question.to_string()),
    );
    refused(&asked, 1);
    assert_eq!(
        asked["error"]["output"]["answer"],
        json!("timeout"),
        "{asked}"
    );
    answered.insert("ask");

    let stream = scratch.text("events.jsonl");
    let emitted = client.call(
        16,
        "eventsEmit",
        json!({"path": stream, "kind": "change-merged", "stream": "s1", "labels": {"run_id": "R"}}),
        Some(r#"{"branch":"main"}"#),
    );
    assert_eq!(ok(&emitted)["seq"], json!(1), "{emitted}");
    answered.insert("eventsEmit");

    let merged = client.call(17, "eventsMerge", json!({"files": [stream]}), None);
    assert_eq!(ok(&merged).as_array().map(Vec::len), Some(1), "{merged}");
    answered.insert("eventsMerge");

    let absent = scratch.text("no-such-spool");
    let delivered = client.call(
        18,
        "deliver",
        json!({"address": absent, "message": "{}", "wait": 1}),
        None,
    );
    same_as_one_shot(
        "deliver",
        &delivered,
        &run_in(
            scratch.dir.path(),
            &["deliver", &absent, "--message", "{}", "--wait", "1"],
            None,
            &[],
        ),
    );
    refused(&delivered, 2);
    answered.insert("deliver");

    let store = scratch.text("no-such-store");
    let carried = client.call(19, "inboxCarried", json!({"store": store}), None);
    same_as_one_shot(
        "inboxCarried",
        &carried,
        &run_in(scratch.dir.path(), &["inbox", "carried", &store], None, &[]),
    );
    answered.insert("inboxCarried");

    // A codec session over no frames ends with its stream, answering nothing.
    let served = client.call(
        20,
        "serve",
        json!({"queue": "surfaces", "codec": "onejudge"}),
        Some(""),
    );
    assert_eq!(ok(&served), &json!([]), "{served}");
    answered.insert("serve");

    let subscribed = {
        client.write(
            &json!({"id": 21, "verb": "subscribe", "args": {"queue": "greetings", "until": r#"{"field":"text","equals":"hello"}"#}})
                .to_string(),
        );
        let event = client.read();
        assert_eq!(event["id"], json!(21));
        assert_eq!(
            event["event"]["record"],
            json!({"text": "hello"}),
            "{event}"
        );
        client.read()
    };
    assert_eq!(ok(&subscribed), &json!("until"));
    answered.insert("subscribe");

    let every: BTreeSet<&str> = CAPABILITIES.iter().map(|c| c.method).collect();
    assert_eq!(
        answered, every,
        "a capability the resident answers is one this journey drives"
    );
    drop(client);
    resident.stop();
}

#[test]
fn a_subscription_streams_what_another_connection_sends_until_its_predicate_or_a_cancel() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.path("registry")).expect("a registry");
    let resident = Resident::start(&scratch);
    let mut writer = resident.client();
    ok(&writer.call(
        1,
        "schemaRegister",
        json!({"id": "demo.greeting@1", "file": scratch.text("greeting.json")}),
        None,
    ));

    let mut listener = resident.client();
    listener.write(
        &json!({"id": 7, "verb": "subscribe", "args": {"queue": "greetings", "until": r#"{"field":"text","equals":"bye"}"#, "timeout": 30}})
            .to_string(),
    );
    ok(&writer.call(
        2,
        "send",
        json!({"queue": "greetings"}),
        Some(r#"{"text":"hi"}"#),
    ));
    ok(&writer.call(
        3,
        "send",
        json!({"queue": "greetings"}),
        Some(r#"{"text":"bye"}"#),
    ));
    let first = listener.read();
    let second = listener.read();
    let end = listener.read();
    assert_eq!(first["event"]["record"], json!({"text": "hi"}), "{first}");
    assert_eq!(
        second["event"]["record"],
        json!({"text": "bye"}),
        "{second}"
    );
    assert_eq!(end, json!({"id": 7, "ok": "until"}));

    // A predicate nothing admits streams until the client cancels it; the
    // records already there arrive first, as text under `--format text`.
    listener.write(
        &json!({"id": 8, "verb": "subscribe", "args": {"queue": "greetings", "until": r#"{"field":"text","equals":"never"}"#, "format": "text"}})
            .to_string(),
    );
    let text = listener.read();
    assert_eq!(text["id"], json!(8));
    assert!(
        text["event"]
            .as_str()
            .is_some_and(|line| line.ends_with(r#"{"text":"hi"}"#)),
        "{text}"
    );
    listener.read();
    listener.write(r#"{"id":8,"cancel":true}"#);
    assert_eq!(listener.read(), json!({"id": 8, "ok": "cancelled"}));

    // Nothing is running under id 8 any more.
    listener.write(r#"{"id":8,"cancel":true}"#);
    assert_eq!(
        refused(&listener.read(), 2),
        "no request 8 is running on this connection to cancel"
    );

    // A subscription whose timeout elapses is refused as the one-shot verb is.
    let lapsed = listener.call(
        9,
        "subscribe",
        json!({"queue": "commands", "until": r#"{"field":"x","present":true}"#, "timeout": 1}),
        None,
    );
    assert_eq!(
        refused(&lapsed, 1),
        "commands: no record --until admits arrived within 1 seconds"
    );
    drop((writer, listener));
    resident.stop();
}

#[test]
fn a_line_the_protocol_does_not_admit_is_refused_by_name_and_the_connection_goes_on() {
    let scratch = Scratch::new();
    let resident = Resident::start(&scratch);
    let mut client = resident.client();
    client.write("not json");
    let line = client.read();
    assert_eq!(line["id"], Value::Null);
    assert!(refused(&line, 2).starts_with("the line is not JSON"));

    client.write(r#"{"id":1,"hello":true}"#);
    assert!(refused(&client.read(), 2).starts_with("the line is neither a request"));

    client.write(r#"{"id":2,"verb":"send","extra":1}"#);
    let line = client.read();
    assert_eq!(line["id"], json!(2));
    assert!(refused(&line, 2).starts_with("the line is not a request of bus.resident-protocol@1"));

    client.write(r#"{"id":3,"cancel":false}"#);
    assert!(refused(&client.read(), 2).contains("a cancel line's `cancel` is `true`"));

    let unknown = client.call(4, "publish", json!({}), None);
    assert!(refused(&unknown, 2).starts_with(
        "the line is not a request of bus.resident-protocol@1: `publish` is not a verb of the resident core; it answers each capability's method: schemaList, "
    ));

    let option = client.call(5, "status", json!({"queues": "greetings"}), None);
    assert!(refused(&option, 2).starts_with("`queues` is not an option of status"));

    let shaped = client.call(6, "status", json!({"queue": {"name": "a"}}), None);
    assert_eq!(
        refused(&shaped, 2),
        "status: `queue` takes a string, a number or a boolean, not an object"
    );
    let switch = client.call(
        7,
        "ask",
        json!({"queue": "surfaces", "blocking": "yes"}),
        None,
    );
    assert_eq!(
        refused(&switch, 2),
        "ask: `blocking` is true or false, not a string"
    );
    let labels = client.call(
        8,
        "eventsEmit",
        json!({"path": "x", "kind": "k", "stream": "s", "labels": "run_id=R"}),
        None,
    );
    assert_eq!(
        refused(&labels, 2),
        "eventsEmit: `labels` is an object of keys to values, not a string"
    );

    // A usage error is refused as the one-shot verb's usage refusal.
    let usage = client.call(
        9,
        "ask",
        json!({"queue": "surfaces", "timeout": "soon"}),
        None,
    );
    assert!(
        refused(&usage, 2).starts_with("ask: invalid value 'soon' for '--timeout <SECONDS>'"),
        "{usage}"
    );

    // A dash-led positional reaches the verb as a positional, not as a flag.
    let dashed = client.call(10, "schemaCheck", json!({"id": "-x"}), Some("{}"));
    assert!(
        refused(&dashed, 2).starts_with("\"-x\" is not a schema id"),
        "{dashed}"
    );
    same_as_one_shot(
        "schemaCheck",
        &dashed,
        &scratch.one_shot_schema(&["schema", "check", "--", "-x"], Some("{}")),
    );
    drop(client);
    resident.stop();
}

#[test]
fn a_second_resident_on_a_live_socket_is_refused_naming_the_first_ones_pid() {
    let scratch = Scratch::new();
    let first = Resident::start(&scratch);
    let second = resident_command(&scratch, &scratch.socket())
        .output()
        .expect("the second resident runs");
    assert_eq!(second.status.code(), Some(1));
    let said = String::from_utf8(second.stderr).expect("UTF-8");
    assert_eq!(
        said.trim_end(),
        format!(
            "onemessagebus: serve --resident: {} is held by the live resident pid {}; stop that process, or pass another --socket",
            scratch.socket().display(),
            first.child.id()
        )
    );

    // A path that is not a socket is refused rather than replaced.
    let file = scratch.path("not-a-socket");
    std::fs::write(&file, "").expect("a file");
    let refused_file = resident_command(&scratch, &file)
        .output()
        .expect("the resident runs");
    assert_eq!(refused_file.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused_file.stderr).contains("is not a socket"));

    // Once the first is gone its pid file goes with it; a socket left behind by
    // a resident that was killed is stale, and the next resident takes it over.
    first.stop();
    assert!(!scratch.path("bus.sock.pid").exists());
    let killed = Resident::start(&scratch);
    let mut child = killed;
    child
        .child
        .kill()
        .expect("this journey's own resident is killed");
    child.child.wait().expect("it ends");
    assert!(
        scratch.socket().exists(),
        "a killed resident leaves its socket"
    );
    let taken_over = Resident::start(&scratch);
    ok(&taken_over.client().call(1, "transports", json!({}), None));
    taken_over.stop();
    drop(child);
}

#[test]
fn a_resident_refuses_a_configuration_or_registry_it_cannot_use_before_it_listens() {
    let scratch = Scratch::new();
    std::fs::write(scratch.path("registry"), "a file").expect("a file");
    let refused_registry = resident_command(&scratch, &scratch.socket())
        .output()
        .expect("the resident runs");
    assert_eq!(refused_registry.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&refused_registry.stderr).contains("is not a directory"),
        "{}",
        String::from_utf8_lossy(&refused_registry.stderr)
    );
    assert!(!scratch.socket().exists(), "nothing was bound");

    let socket = scratch.text("bus.sock");
    let refused_config = run_in(
        scratch.dir.path(),
        &[
            "serve",
            "--resident",
            "--socket",
            &socket,
            "--config",
            "missing.yaml",
        ],
        None,
        &[],
    );
    assert_eq!(refused_config.code, 2, "{}", refused_config.stderr);
    assert!(refused_config.stderr.contains("missing.yaml"));

    let memory = scratch.path("memory.yaml");
    std::fs::write(&memory, "version: 1\ntransport: {kind: memory, dir: x}\n").expect("a config");
    let refused_transport = run_in(
        scratch.dir.path(),
        &[
            "serve",
            "--resident",
            "--socket",
            &socket,
            "--config",
            memory.to_str().expect("UTF-8"),
        ],
        None,
        &[],
    );
    assert_eq!(refused_transport.code, 2, "{}", refused_transport.stderr);
    assert!(refused_transport
        .stderr
        .starts_with("onemessagebus: transport: "));

    let usage = run_in(scratch.dir.path(), &["serve", "--resident"], None, &[]);
    assert_eq!(usage.code, 2, "{}", usage.stderr);
    assert!(usage.stderr.contains("--socket"), "{}", usage.stderr);
}

#[test]
fn a_resident_with_no_configuration_answers_the_schema_verbs_and_refuses_the_queue_verbs() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let socket = dir.path().join("bus.sock");
    let mut child = onemessagebus()
        .args(["serve", "--resident", "--socket"])
        .arg(&socket)
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the resident spawns");
    let deadline = Instant::now() + Duration::from_secs(20);
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "the resident never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut client = Client::connect(&socket);
    let list = client.call(1, "schemaList", json!({"format": "text"}), None);
    assert!(ok(&list)
        .as_str()
        .is_some_and(|text| text.contains("bus.resident-protocol@1")));
    let queue = client.call(2, "status", json!({}), None);
    assert!(refused(&queue, 2).starts_with("no configuration to open a queue with"));
    // A request naming a transport directory of its own opens that one.
    let own = client.call(
        3,
        "status",
        json!({"queue": "surfaces", "transportDir": dir.path().join("channel").to_str().expect("UTF-8")}),
        None,
    );
    assert_eq!(ok(&own)[0]["queue"], json!("surfaces"), "{own}");
    drop(client);
    std::fs::remove_file(&socket).expect("the socket is removed");
    assert_eq!(wait(&mut child, Duration::from_secs(20)), Some(0));
}

#[test]
fn schema_gen_prints_the_protocol_under_its_id_and_refuses_a_bare_word() {
    let printed = crate::support::run(
        &["schema", "gen", "--lang", "json", "bus.resident-protocol@1"],
        None,
    );
    assert_eq!(printed.code, 0, "{}", printed.stderr);
    let document: Value = serde_json::from_str(&printed.stdout).expect("JSON");
    assert_eq!(document["title"], json!("ResidentLine"));
    assert_eq!(
        document["anyOf"].as_array().map(Vec::len),
        Some(5),
        "request, cancel, answer, refusal and event"
    );
    let bare = crate::support::run(
        &["schema", "gen", "--lang", "json", "resident-protocol"],
        None,
    );
    assert_eq!(bare.code, 2);
    assert!(bare.stderr.contains("resident-protocol"), "{}", bare.stderr);
}

/// Wait until something accepts on `socket`.
fn listening(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while UnixStream::connect(socket).is_err() {
        assert!(
            Instant::now() < deadline,
            "nothing listened on {}",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The transport a resident opens is the one every request runs over: a memory
/// transport keeps between two requests what a transport opened again for each
/// would have forgotten, and a one-shot verb over the same configuration opens a
/// transport of its own that holds none of it.
#[test]
fn a_resident_holds_the_transport_it_opened_across_requests() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let config = dir.path().join("memory.yaml");
    std::fs::write(
        &config,
        "version: 1\ntransport: {kind: memory}\nqueues:\n  notes: {}\n",
    )
    .expect("a config");
    let config = config.to_str().expect("a UTF-8 path").to_owned();
    let socket = dir.path().join("bus.sock");
    let mut child = onemessagebus()
        .args(["serve", "--resident", "--socket"])
        .arg(&socket)
        .args(["--config", &config])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the resident spawns");
    listening(&socket);
    let mut client = Client::connect(&socket);
    let sent = client.call(
        1,
        "send",
        json!({"queue": "notes"}),
        Some(r#"{"text":"kept"}"#),
    );
    assert_eq!(ok(&sent)[0]["queue"], json!("notes"), "{sent}");
    let held = client.call(2, "status", json!({"queue": "notes"}), None);
    assert_eq!(ok(&held)[0]["records"], json!(1), "{held}");
    assert_eq!(ok(&held)[0]["waiting"], json!([{"text": "kept"}]), "{held}");

    let one_shot = run_in(
        dir.path(),
        &["status", "notes", "--config", &config],
        None,
        &[],
    );
    assert_eq!(one_shot.code, 0, "{}", one_shot.stderr);
    let statuses: Vec<Value> = serde_json::from_str(&one_shot.stdout).expect("a JSON list");
    assert_eq!(statuses[0]["records"], json!(0));

    drop(client);
    std::fs::remove_file(&socket).expect("the socket is removed");
    assert_eq!(wait(&mut child, Duration::from_secs(20)), Some(0));
}

/// A socket path the resident cannot use is refused before it listens, naming
/// what went wrong: a path beneath a file, a pid it cannot record, a directory it
/// may not bind in, and a stale socket it may not remove.
#[test]
fn a_resident_refuses_a_socket_path_it_cannot_inspect_bind_or_record_a_pid_beside() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = Scratch::new();
    let said = |socket: &Path| {
        let output = resident_command(&scratch, socket)
            .output()
            .expect("the resident runs");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };

    let file = scratch.path("plain");
    std::fs::write(&file, "").expect("a file");
    let (code, stderr) = said(&file.join("bus.sock"));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("serve --resident: cannot inspect"),
        "{stderr}"
    );

    std::fs::create_dir(scratch.path("pidless.sock.pid")).expect("a directory where the pid goes");
    let (code, stderr) = said(&scratch.path("pidless.sock"));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("cannot record this resident's pid in"),
        "{stderr}"
    );

    // A resident killed where it stood leaves its socket behind in a directory
    // that is then closed to writing.
    let locked = scratch.path("locked");
    std::fs::create_dir(&locked).expect("a directory");
    let stale = locked.join("bus.sock");
    let mut killed = resident_command(&scratch, &stale)
        .spawn()
        .expect("the resident spawns");
    listening(&stale);
    killed
        .kill()
        .expect("this journey's own resident is killed");
    killed.wait().expect("it ends");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))
        .expect("the directory is closed to writing");
    let stale_refused = said(&stale);
    let fresh_refused = said(&locked.join("fresh.sock"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
        .expect("the directory is opened again");

    let (code, stderr) = stale_refused;
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("serve --resident: cannot remove the stale socket"),
        "{stderr}"
    );
    let (code, stderr) = fresh_refused;
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("serve --resident: cannot listen on"),
        "{stderr}"
    );
}

/// A live resident is refused as live even when its pid file is gone, saying the
/// pid is not recorded rather than guessing one.
#[test]
fn a_live_resident_whose_pid_file_is_gone_is_still_refused_as_live() {
    let scratch = Scratch::new();
    let first = Resident::start(&scratch);
    let recorded = scratch.path("bus.sock.pid");
    std::fs::remove_file(&recorded).expect("the pid file is removed");
    let second = resident_command(&scratch, &scratch.socket())
        .output()
        .expect("the second resident runs");
    assert_eq!(second.status.code(), Some(1));
    let said = String::from_utf8_lossy(&second.stderr);
    assert!(
        said.contains(&format!(
            "is held by a live resident whose pid {} does not record",
            recorded.display()
        )),
        "{said}"
    );
    first.stop();
}

/// One connection's lines: a blank one is nothing, a second request under an id
/// still running is refused, a switch and a boolean reach the verb as its flags
/// take them, and a line that is not UTF-8 ends the connection — cancelling what
/// it left running — while the core goes on accepting others.
#[test]
fn a_connection_refuses_a_running_id_and_ends_on_a_line_that_is_not_text() {
    use std::io::Read as _;
    let scratch = Scratch::new();
    let resident = Resident::start(&scratch);
    let mut client = resident.client();
    client.write("");
    // The configuration names demo.greeting@1, and a bus over it opens only once
    // the schema is registered.
    ok(&client.call(
        9,
        "schemaRegister",
        json!({"id": "demo.greeting@1", "file": scratch.text("greeting.json")}),
        None,
    ));
    client.write(
        &json!({"id": 5, "verb": "subscribe", "args": {"queue": "commands", "until": r#"{"field":"never","present":true}"#}})
            .to_string(),
    );
    let duplicate = client.call(5, "transports", json!({}), None);
    assert_eq!(
        refused(&duplicate, 2),
        "request 5 is still running on this connection; give each request an id of its own"
    );

    let question =
        json!({"kind": "finding", "message": "anyone?", "source": "proposal", "blocking": false});
    for (id, blocking) in [(6, false), (7, true)] {
        let asked = client.call(
            id,
            "ask",
            json!({"queue": "surfaces", "blocking": blocking, "timeout": 1}),
            Some(&question.to_string()),
        );
        refused(&asked, 1);
        assert_eq!(
            asked["error"]["output"]["answer"],
            json!("timeout"),
            "{asked}"
        );
    }
    let boolean = client.call(
        8,
        "deliver",
        json!({"address": "spool", "wait": true}),
        None,
    );
    assert!(
        refused(&boolean, 2).contains("invalid value 'true' for '--wait"),
        "{boolean}"
    );

    client
        .writer
        .write_all(b"\xff\xfe\n")
        .expect("the line is written");
    let mut rest = String::new();
    client
        .reader
        .read_to_string(&mut rest)
        .expect("the resident closes the connection");
    // The subscription the connection left running is cancelled, and its answer
    // is the last thing written before the connection closes.
    assert_eq!(rest.trim(), r#"{"id":5,"ok":"cancelled"}"#);

    let mut again = resident.client();
    ok(&again.call(1, "transports", json!({}), None));
    drop((client, again));
    resident.stop();
}
