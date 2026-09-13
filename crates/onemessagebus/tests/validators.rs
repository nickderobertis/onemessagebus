//! Contract V: validators judge a message before anything is appended.
//!
//! The external validator is driven for real: the command a configuration names
//! is this test binary, re-run as `scripted_validator`. When handed a script it
//! reads the message on its
//! stdin, logs it, and answers with the exit status, stderr and stdout its script
//! says. A test rewrites the script between sends to move the bar or change the
//! answer, and counts the log to see whether the command ran.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use onemessagebus::{
    Allowlist, BusError, CommandValidator, Config, ConfigError, Layout, Layouts, Message, OpWord,
    PassCache, Policy, QueueError, QueueName, QueueSpec, Registry, SchemaId, TransportKinds,
    ValidationContext, Validator, Validators, Verdict, VALIDATE_QUEUE_ENV,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The argument a validator command names this test by.
const DOUBLE: &str = "scripted_validator";

/// The scripted validator command. Its ordinary harness invocation checks that
/// it was not accidentally given the validator environment; a validator child
/// receives a script path and runs its `validate` or `fingerprint` role. A
/// harness flag after the test name (nextest passes `--exact`) is not a script.
#[test]
fn scripted_validator() {
    let arguments: Vec<String> = std::env::args().collect();
    let Some(script) = arguments
        .iter()
        .position(|argument| argument == DOUBLE)
        .and_then(|at| arguments.get(at + 1))
        .filter(|argument| !argument.starts_with('-'))
    else {
        assert!(
            std::env::var_os(VALIDATE_QUEUE_ENV).is_none(),
            "the ordinary test invocation must not look like a validator child"
        );
        return;
    };
    let role = if std::env::var_os(VALIDATE_QUEUE_ENV).is_some() {
        "validate"
    } else {
        "fingerprint"
    };
    let part: Value = serde_json::from_str::<Value>(
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
        json!({
            "role": role,
            "queue": std::env::var(VALIDATE_QUEUE_ENV).ok(),
            "stdin": stdin,
        })
    )
    .expect("the double logs");
    let mut out = std::io::stdout();
    out.write_all(part["stdout"].as_str().unwrap_or_default().as_bytes())
        .and_then(|()| out.flush())
        .expect("the double writes stdout");
    let mut err = std::io::stderr();
    err.write_all(part["stderr"].as_str().unwrap_or_default().as_bytes())
        .and_then(|()| err.flush())
        .expect("the double writes stderr");
    std::process::exit(
        part["exit"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok())
            .unwrap_or(0),
    );
}

/// A scratch directory, the subprocess fixture's script in it, and a configuration over a
/// local transport there.
struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let rig = Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        };
        rig.script(json!({"exit": 0}), json!({"exit": 0, "stdout": "bar-1"}));
        rig
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

    /// The double's argv, as a YAML flow sequence.
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

    fn argv(&self) -> Vec<String> {
        serde_json::from_str(&self.command()).expect("an argv")
    }

    /// How many times the double ran as `role`, and what it was handed.
    fn ran(&self, role: &str) -> Vec<Value> {
        std::fs::read_to_string(self.path("script.json.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("a log line"))
            .filter(|line| line["role"] == json!(role))
            .collect()
    }

    fn config(&self, validators: &str) -> Config {
        let text = format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\nqueues:\n  findings: {{}}\n  alerts: {{}}\nvalidators:\n{validators}",
            serde_json::to_string(&self.path("channel")).expect("a path")
        );
        std::fs::write(self.path("onemessagebus.yaml"), text).expect("the file is written");
        Config::load(self.path("onemessagebus.yaml")).expect("the configuration loads")
    }

    fn bus(&self, validators: &str) -> onemessagebus::Bus {
        self.config(validators)
            .resolve(&Layouts::new(), &TransportKinds::builtin())
            .expect("the configuration resolves")
    }

    fn records(&self, queue: &str) -> Vec<Value> {
        std::fs::read_to_string(self.path("channel").join(format!("{queue}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }

    fn cached(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.path("passes"))
            .map(|entries| {
                entries
                    .map(|entry| entry.expect("an entry").path())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

#[test]
fn a_pass_is_judged_on_the_message_as_offered_and_then_appended() {
    let rig = Rig::new();
    let bus = rig.bus(&format!(
        "  - {{on: findings, kind: command, command: {}}}\n",
        rig.command()
    ));
    let message = json!({"what": "the base moved"});
    bus.send(&queue("findings"), message.clone())
        .expect("a pass is sent");
    let ran = rig.ran("validate");
    assert_eq!(ran.len(), 1, "{ran:?}");
    assert_eq!(ran[0]["queue"], json!("findings"));
    assert_eq!(
        serde_json::from_str::<Value>(ran[0]["stdin"].as_str().expect("stdin")).expect("JSON"),
        message
    );
    assert_eq!(rig.records("findings"), vec![message]);
}

#[test]
fn a_refused_message_is_appended_nowhere_and_carries_the_validators_reason_unaltered() {
    let rig = Rig::new();
    let reason = "the criterion names no observable outcome:\n  `make it better`\n";
    rig.script(
        json!({"exit": 1, "stderr": reason}),
        json!({"exit": 0, "stdout": "bar-1"}),
    );
    let bus = rig.bus(&format!(
        "  - {{on: findings, kind: command, command: {}}}\n",
        rig.command()
    ));
    let refused = bus
        .send(&queue("findings"), json!({"what": "make it better"}))
        .expect_err("a refusal is not sent");
    match refused {
        BusError::Queue(QueueError::Refused {
            queue: at,
            reason: given,
        }) => {
            assert_eq!(at, queue("findings"));
            assert_eq!(given, reason, "the reason was altered");
        }
        other => panic!("not a refusal: {other}"),
    }
    assert!(
        rig.records("findings").is_empty(),
        "a refused message was appended"
    );
    let raw = bus.queue(&queue("findings")).expect("a queue");
    let direct = raw
        .push(json!({"what": "pushed directly"}))
        .expect_err("a push is judged too");
    assert!(
        matches!(&direct, QueueError::Refused { reason: given, .. } if given == reason),
        "{direct}"
    );
    assert!(
        rig.records("findings").is_empty(),
        "a refused push was appended"
    );
}

#[test]
fn an_exit_other_than_zero_or_one_is_unjudged_and_never_passes() {
    let rig = Rig::new();
    rig.script(
        json!({"exit": 3, "stderr": "the reviewer is out of quota"}),
        json!({"exit": 0, "stdout": "bar-1"}),
    );
    let bus = rig.bus(&format!(
        "  - {{on: findings, kind: command, command: {}}}\n",
        rig.command()
    ));
    let verdict = bus
        .validate(&queue("findings"), json!({"what": "anything"}))
        .expect("judged");
    let Verdict::Unjudged { reason } = &verdict else {
        panic!("not unjudged: {verdict:?}");
    };
    assert!(reason.contains("exited 3"), "{reason}");
    assert!(reason.contains("the reviewer is out of quota"), "{reason}");
    assert!(!verdict.passes());
    let refused = bus
        .send(&queue("findings"), json!({"what": "anything"}))
        .expect_err("unjudged is not sent");
    assert!(
        matches!(refused, BusError::Queue(QueueError::Unjudged { .. })),
        "{refused}"
    );
    assert!(rig.records("findings").is_empty());

    let missing =
        CommandValidator::new([rig.path("no-such-validator").to_string_lossy()]).expect("an argv");
    let context = ValidationContext::new(queue("findings"));
    let verdict = missing.judge(b"{}", &context);
    assert!(
        matches!(&verdict, Verdict::Unjudged { reason } if reason.contains("could not be run")),
        "{verdict:?}"
    );
    rig.script(json!({"exit": 1}), json!({"exit": 0, "stdout": "bar-1"}));
    let silent = CommandValidator::new(rig.argv())
        .expect("an argv")
        .judge(b"{}", &context);
    assert!(
        matches!(&silent, Verdict::Refuse { reason } if reason.contains("wrote no reason")),
        "{silent:?}"
    );
}

/// A deterministic validator answering a fixed verdict and counting its runs.
struct Fixed(Verdict, Arc<AtomicUsize>);

impl<M: Message> Validator<M> for Fixed {
    fn validate(&self, _: &M, _: &ValidationContext) -> Verdict {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0.clone()
    }
}

#[test]
fn every_validator_runs_the_first_refusal_wins_and_an_unjudged_one_makes_the_verdict_unjudged() {
    let runs = Arc::new(AtomicUsize::new(0));
    let fixed = |verdict: Verdict| Fixed(verdict, Arc::clone(&runs));
    let refuse = |reason: &str| Verdict::Refuse {
        reason: reason.to_owned(),
    };
    let unjudged = |reason: &str| Verdict::Unjudged {
        reason: reason.to_owned(),
    };
    let context = ValidationContext::new(queue("findings"));
    let message = json!({});

    let validators = Validators::<Value>::new()
        .with(fixed(Verdict::Pass))
        .with(fixed(unjudged("no quota")))
        .with(fixed(refuse("first")))
        .with(fixed(refuse("second")));
    assert_eq!(validators.len(), 4);
    assert_eq!(format!("{validators:?}"), "Validators { count: 4 }");
    assert_eq!(validators.judge(&message, &context), refuse("first"));
    assert_eq!(runs.load(Ordering::SeqCst), 4, "a validator was skipped");

    let validators = Validators::<Value>::new()
        .with(fixed(Verdict::Pass))
        .with(fixed(unjudged("no quota")))
        .with(fixed(Verdict::Pass));
    assert_eq!(validators.judge(&message, &context), unjudged("no quota"));
    assert_eq!(
        Validators::<Value>::new().judge(&message, &context),
        Verdict::Pass
    );
    assert!(Validators::<Value>::new().is_empty());
}

#[test]
fn a_pass_is_recorded_under_the_content_and_the_bar_and_a_moved_bar_runs_the_command_again() {
    let rig = Rig::new();
    let bus = rig.bus(&format!(
        "  - {{on: findings, kind: command, command: {command}, cache: {{dir: {dir}, bar_fingerprint: {command}}}}}\n",
        command = rig.command(),
        dir = serde_json::to_string(&rig.path("passes")).expect("a path"),
    ));
    let first = json!({"what": "the base moved"});
    bus.send(&queue("findings"), first.clone()).expect("sent");
    assert_eq!(rig.ran("validate").len(), 1);
    assert_eq!(rig.cached().len(), 1, "the pass was not recorded");
    let record: Value =
        serde_json::from_slice(&std::fs::read(&rig.cached()[0]).expect("the record reads"))
            .expect("the record is JSON");
    assert_eq!(record["schema_version"], json!(1));
    assert!(
        record["fingerprint"]
            .as_str()
            .is_some_and(|f| f.contains("bar-1")),
        "{record}"
    );

    bus.send(&queue("findings"), first.clone())
        .expect("sent again");
    assert_eq!(
        rig.ran("validate").len(),
        1,
        "the same content under the same bar ran the command again"
    );
    assert_eq!(
        rig.records("findings").len(),
        2,
        "a cached pass was not sent"
    );

    let second = json!({"what": "the gate is red"});
    bus.send(&queue("findings"), second).expect("sent");
    assert_eq!(
        rig.ran("validate").len(),
        2,
        "different content was passed from a record"
    );

    rig.script(json!({"exit": 0}), json!({"exit": 0, "stdout": "bar-2"}));
    bus.send(&queue("findings"), first)
        .expect("sent under the moved bar");
    assert_eq!(
        rig.ran("validate").len(),
        3,
        "a pass recorded under the old bar was trusted under the new one"
    );
    assert_eq!(rig.cached().len(), 3);
}

#[test]
fn a_cache_record_whose_fingerprint_or_command_was_changed_grants_no_pass() {
    let rig = Rig::new();
    let bus = rig.bus(&format!(
        "  - {{on: findings, kind: command, command: {command}, cache: {{dir: {dir}, bar_fingerprint: {command}}}}}\n",
        command = rig.command(),
        dir = serde_json::to_string(&rig.path("passes")).expect("a path"),
    ));
    let message = json!({"what": "the cache is evidence"});
    bus.send(&queue("findings"), message.clone()).expect("sent");
    let path = rig.cached().into_iter().next().expect("a pass record");

    for (field, replacement) in [
        ("fingerprint", json!("a different bar")),
        ("command", json!(["a", "different", "command"])),
    ] {
        let mut record: Value = serde_json::from_slice(
            &std::fs::read(&path).expect("the pass record remains readable"),
        )
        .expect("the pass record remains JSON");
        record[field] = replacement;
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&record).expect("the changed record renders"),
        )
        .expect("the changed record is written");
        bus.send(&queue("findings"), message.clone())
            .expect("the validator reruns and passes");
    }

    assert_eq!(
        rig.ran("validate").len(),
        3,
        "a changed cache record granted a pass"
    );
}

#[test]
fn only_a_pass_is_recorded_and_a_bar_with_no_fingerprint_records_nothing() {
    let rig = Rig::new();
    rig.script(
        json!({"exit": 1, "stderr": "refused"}),
        json!({"exit": 0, "stdout": "bar-1"}),
    );
    let validators = format!(
        "  - {{on: findings, kind: command, command: {command}, cache: {{dir: {dir}, bar_fingerprint: {command}}}}}\n",
        command = rig.command(),
        dir = serde_json::to_string(&rig.path("passes")).expect("a path"),
    );
    let bus = rig.bus(&validators);
    let message = json!({"what": "refuse me"});
    for _ in 0..2 {
        bus.send(&queue("findings"), message.clone())
            .expect_err("refused");
    }
    assert_eq!(
        rig.ran("validate").len(),
        2,
        "a refusal was answered from a record"
    );
    assert!(rig.cached().is_empty(), "a refusal was recorded");

    rig.script(json!({"exit": 3}), json!({"exit": 0, "stdout": "bar-1"}));
    bus.send(&queue("findings"), message.clone())
        .expect_err("unjudged");
    assert!(rig.cached().is_empty(), "an unjudged verdict was recorded");

    rig.script(json!({"exit": 0}), json!({"exit": 2, "stdout": ""}));
    for _ in 0..2 {
        bus.send(&queue("findings"), message.clone())
            .expect("passes");
    }
    assert_eq!(
        rig.ran("validate").len(),
        5,
        "a pass keyed on no fingerprint was answered from a record"
    );
    assert!(
        rig.cached().is_empty(),
        "a pass was recorded under no fingerprint"
    );

    let cache = PassCache::new(rig.path("passes"), rig.argv()).expect("a cache");
    assert_eq!(cache.dir(), rig.path("passes"));
    assert_eq!(cache.bar_fingerprint(), rig.argv().as_slice());
    assert_eq!(cache.fingerprint(), None);
    assert!(!cache.holds_pass(&rig.argv(), b"{}"));
}

#[test]
fn when_carries_judges_only_the_messages_carrying_the_field() {
    let rig = Rig::new();
    rig.script(
        json!({"exit": 1, "stderr": "an edit must clear the bar"}),
        json!({"exit": 0, "stdout": "bar-1"}),
    );
    let bus = rig.bus(&format!(
        "  - {{on: findings, when: {{carries: commands}}, kind: command, command: {}}}\n",
        rig.command()
    ));
    bus.send(
        &queue("findings"),
        json!({"completion": false, "commands": []}),
    )
    .expect("an empty list carries nothing, so it is not judged");
    bus.send(&queue("findings"), json!({"completion": true}))
        .expect("no commands, so it is not judged");
    assert!(rig.ran("validate").is_empty());
    bus.send(
        &queue("findings"),
        json!({"commands": [{"op": "add", "id": "n"}]}),
    )
    .expect_err("an envelope carrying commands is judged");
    assert_eq!(rig.ran("validate").len(), 1);

    let predicate = rig.bus(&format!(
        "  - {{on: findings, when: {{field: author, equals: monitor}}, kind: command, command: {}}}\n",
        rig.command()
    ));
    predicate
        .send(&queue("findings"), json!({"author": "planner"}))
        .expect("the predicate does not admit it");
    predicate
        .send(&queue("findings"), json!({"author": "monitor"}))
        .expect_err("the predicate admits it");
    assert_eq!(rig.ran("validate").len(), 2);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Ticket {
    title: String,
}

impl Message for Ticket {
    const SCHEMA: SchemaId = SchemaId::literal("test", "ticket", 1);
}

/// A deterministic validator: a ticket's title must not be blank.
struct TitledTicket;

impl Validator<Ticket> for TitledTicket {
    fn validate(&self, ticket: &Ticket, context: &ValidationContext) -> Verdict {
        if ticket.title.trim().is_empty() {
            Verdict::Refuse {
                reason: format!("a ticket on {} has a title", context.queue),
            }
        } else {
            Verdict::Pass
        }
    }
}

#[test]
fn a_rust_validator_judges_a_bus_queue_and_a_typed_queue_before_anything_is_appended() {
    let rig = Rig::new();
    let bus = rig
        .bus("  []\n")
        .with_validator(&queue("findings"), TitledTicket)
        .expect("a declared queue");
    bus.send(&queue("findings"), json!({"title": "the base moved"}))
        .expect("a titled ticket passes");
    let blank = bus
        .send(&queue("findings"), json!({"title": " "}))
        .expect_err("a blank title is refused");
    assert!(
        blank
            .to_string()
            .contains("a ticket on findings has a title"),
        "{blank}"
    );
    let untyped = bus
        .send(&queue("findings"), json!({"heading": "not a ticket"}))
        .expect_err("a record that is not a ticket is refused");
    assert!(untyped.to_string().contains("test.ticket@1"), "{untyped}");
    assert_eq!(rig.records("findings").len(), 1);
    assert!(
        bus.clone()
            .with_validator(&queue("nowhere"), TitledTicket)
            .is_err(),
        "an undeclared queue was given a validator"
    );

    let transport: Arc<dyn onemessagebus::Transport> =
        Arc::new(onemessagebus::MemoryTransport::new());
    let tickets = onemessagebus::Queue::<Ticket>::open(
        Arc::clone(&transport),
        QueueSpec::new(queue("tickets"), Policy::default()),
    )
    .expect("a typed queue")
    .with_validators(Validators::new().with(TitledTicket));
    tickets
        .push(&Ticket {
            title: "kept".to_owned(),
        })
        .expect("passes");
    let refused = tickets
        .push(&Ticket {
            title: String::new(),
        })
        .expect_err("refused");
    assert!(matches!(refused, QueueError::Refused { .. }), "{refused}");
    assert_eq!(tickets.waiting().expect("a read").len(), 1);
}

/// A layout that routes one offer into a note and an alert.
struct Splitter;

impl Layout for Splitter {
    fn name(&self) -> &str {
        "splitter"
    }

    fn queues(&self) -> Vec<QueueSpec> {
        ["findings", "alerts"]
            .into_iter()
            .map(|name| QueueSpec::new(queue(name), Policy::default()))
            .collect()
    }

    fn allowlist(&self) -> Allowlist<OpWord> {
        Allowlist::new(Vec::<OpWord>::new())
    }

    fn registry(&self) -> Registry {
        Registry::new()
    }

    fn prepare(
        &self,
        offered_to: &QueueName,
        record: Value,
        _: &Allowlist<OpWord>,
    ) -> Result<Vec<(QueueName, Value)>, String> {
        Ok(vec![
            (offered_to.clone(), json!({"note": record["note"]})),
            (queue("alerts"), json!({"alert": record["alert"]})),
        ])
    }
}

#[test]
fn a_record_routed_to_another_queue_is_judged_by_that_queues_validators_before_either_is_appended()
{
    let rig = Rig::new();
    rig.script(
        json!({"exit": 1, "stderr": "alerts are paused"}),
        json!({"exit": 0, "stdout": "bar-1"}),
    );
    let text = format!(
        "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: splitter\nvalidators:\n  - {{on: alerts, kind: command, command: {}}}\n",
        serde_json::to_string(&rig.path("channel")).expect("a path"),
        rig.command()
    );
    let bus = Config::parse(&text)
        .expect("loads")
        .resolve(
            &Layouts::new().with(Arc::new(Splitter)),
            &TransportKinds::builtin(),
        )
        .expect("resolves");
    let offer = json!({"note": "n", "alert": "a"});
    let verdict = bus
        .validate(&queue("findings"), offer.clone())
        .expect("judged");
    assert_eq!(
        verdict,
        Verdict::Refuse {
            reason: "alerts are paused".to_owned()
        }
    );
    let ran = rig.ran("validate");
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0]["queue"], json!("alerts"));
    assert_eq!(
        serde_json::from_str::<Value>(ran[0]["stdin"].as_str().expect("stdin")).expect("JSON"),
        json!({"alert": "a"}),
        "the routed record was not what the target queue judged"
    );
    bus.send(&queue("findings"), offer).expect_err("refused");
    assert!(
        rig.records("findings").is_empty(),
        "the offered queue was appended to"
    );
    assert!(
        rig.records("alerts").is_empty(),
        "the routed queue was appended to"
    );
}

#[test]
fn validate_judges_without_appending_anything() {
    let rig = Rig::new();
    let bus = rig.bus(&format!(
        "  - {{on: findings, kind: command, command: {}}}\n",
        rig.command()
    ));
    assert_eq!(
        bus.validate(&queue("findings"), json!({"what": "x"}))
            .expect("judged"),
        Verdict::Pass
    );
    assert_eq!(rig.ran("validate").len(), 1);
    assert!(rig.records("findings").is_empty(), "validate appended");
    assert!(
        matches!(
            bus.validate(&queue("nowhere"), json!({})),
            Err(BusError::UnknownQueue { .. })
        ),
        "an undeclared queue was judged"
    );
}

fn loaded(rig: &Rig, validators: &str) -> Result<Config, ConfigError> {
    let text = format!(
        "version: 1\ntransport: {{kind: local, dir: {}}}\nqueues:\n  findings: {{}}\nvalidators:\n{validators}",
        serde_json::to_string(&rig.path("channel")).expect("a path")
    );
    Config::parse(&text)
}

#[test]
fn a_validators_block_is_refused_by_the_key_it_is_wrong_at() {
    let rig = Rig::new();
    let command = rig.command();
    for (validators, names) in [
        (
            format!("  - {{on: findings, kind: command, command: {command}, timeout: 5}}\n"),
            "timeout",
        ),
        (
            format!("  - {{on: findings, kind: script, command: {command}}}\n"),
            "script",
        ),
        (
            "  - {on: findings, kind: command, command: []}\n".to_owned(),
            "validators[0].command",
        ),
        (
            format!("  - {{on: findings, kind: command, command: {command}, cache: {{dir: passes}}}}\n"),
            "bar_fingerprint",
        ),
        (
            format!("  - {{on: findings, kind: command, command: {command}, cache: {{dir: passes, bar_fingerprint: []}}}}\n"),
            "validators[0].cache.bar_fingerprint",
        ),
        (
            format!("  - {{on: findings, kind: command, command: {command}, cache: {{dir: passes, bar_fingerprint: {command}, ttl: 5}}}}\n"),
            "ttl",
        ),
        (
            format!("  - {{on: findings, when: {{carries: commands, field: x}}, kind: command, command: {command}}}\n"),
            "`carries` stands alone",
        ),
        (
            format!("  - {{on: findings, when: {{carries: \"\"}}, kind: command, command: {command}}}\n"),
            "not a field path",
        ),
        (
            format!("  - {{on: findings, when: {{field: x, matches: y}}, kind: command, command: {command}}}\n"),
            "matches",
        ),
        (
            format!("  - {{on: \"not a queue\", kind: command, command: {command}}}\n"),
            "not a queue",
        ),
    ] {
        let refused = loaded(&rig, &validators).expect_err(&validators);
        assert!(
            matches!(refused, ConfigError::Parse { .. }),
            "{validators}: {refused}"
        );
        assert!(
            refused.to_string().contains(names),
            "{validators}: the refusal does not name {names}: {refused}"
        );
    }
    let undeclared = loaded(
        &rig,
        &format!("  - {{on: findings, kind: command, command: {command}}}\n  - {{on: reviews, kind: command, command: {command}}}\n"),
    )
    .expect("loads: only resolving knows the queues")
    .resolve(&Layouts::new(), &TransportKinds::builtin())
    .expect_err("an undeclared queue is refused");
    assert_eq!(
        undeclared.to_string(),
        "validators[1].on: `reviews` is not a queue this configuration declares; it declares: findings"
    );
    let round_trip = loaded(
        &rig,
        &format!("  - {{on: findings, when: {{carries: reply.commands}}, kind: command, command: {command}, cache: {{dir: passes, bar_fingerprint: {command}}}}}\n  - {{on: findings, when: {{not: {{field: author, present: true}}}}, kind: command, command: {command}}}\n"),
    )
    .expect("loads");
    let written = serde_norway::to_string(&round_trip).expect("writes");
    assert_eq!(Config::parse(&written).expect("reads back"), round_trip);
}

#[test]
fn a_validator_and_a_cache_are_refused_an_empty_argv() {
    assert!(CommandValidator::new(Vec::<String>::new()).is_err());
    assert!(CommandValidator::new([""]).is_err());
    assert!(PassCache::new("passes", Vec::<String>::new()).is_err());
    let validator = CommandValidator::new(["true"]).expect("an argv");
    assert_eq!(validator.command(), ["true".to_owned()]);
    assert!(validator.cache().is_none());
    assert_eq!(Verdict::Pass.reason(), None);
    assert_eq!(
        Verdict::Refuse {
            reason: "r".to_owned()
        }
        .reason(),
        Some("r")
    );
}
