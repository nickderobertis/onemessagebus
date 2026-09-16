//! The configuration file: which refusals `Config::load` makes from the file
//! alone, and which `Config::resolve` makes once the file is bound to the
//! layouts and transports a process has.

use std::sync::Arc;

use onemessagebus::{
    Allowlist, Author, Config, ConfigError, Layout, Layouts, OpWord, Policy, QueueName, QueueSpec,
    Registry, TransportKinds, CONFIG_VERSION,
};
use serde_json::json;

/// A layout with one event queue and one author granted two of three ops.
struct Ledger;

impl Layout for Ledger {
    fn name(&self) -> &str {
        "ledger"
    }

    fn queues(&self) -> Vec<QueueSpec> {
        vec![QueueSpec::new(
            "entries".parse().expect("a queue"),
            Policy {
                hold_pending: true,
                ..Policy::default()
            },
        )]
    }

    fn allowlist(&self) -> Allowlist<OpWord> {
        let word = |text: &str| OpWord(text.to_owned());
        let mut allowlist = Allowlist::new(["post", "void", "audit"].map(word));
        allowlist
            .grant(Author::from("teller"), word("post"))
            .grant(Author::from("teller"), word("audit"));
        allowlist
    }

    fn registry(&self) -> Registry {
        Registry::new()
    }
}

fn write(dir: &tempfile::TempDir, text: &str) -> std::path::PathBuf {
    let path = dir.path().join("onemessagebus.yaml");
    std::fs::write(&path, text).expect("the file is written");
    path
}

fn layouts() -> Layouts {
    Layouts::new().with(Arc::new(Ledger))
}

#[test]
fn load_reads_the_documented_shape_and_resolve_opens_its_bus() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let channel = dir.path().join("runs/r1/channel");
    let path = write(
        &dir,
        &format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: ledger\nqueues:\n  findings: {{policy: {{hold_pending: false}}}}\n  entries: {{consumers: [default, auditor]}}\nauthors:\n  teller: {{capabilities: [post]}}\n",
            channel.display()
        ),
    );
    let config = Config::load(&path).expect("the file loads");
    assert_eq!(config.version, CONFIG_VERSION);
    assert_eq!(config.profile.as_deref(), Some("ledger"));
    let bus = config
        .resolve(&layouts(), &TransportKinds::builtin())
        .expect("the configuration resolves");
    assert_eq!(bus.kind(), "local");
    let names: Vec<String> = bus.queues().iter().map(ToString::to_string).collect();
    assert_eq!(
        names,
        vec!["entries", "findings"],
        "an added queue was not declared beside the layout's"
    );
    let entries = bus
        .queue(&"entries".parse().expect("a queue"))
        .expect("declared");
    assert!(
        entries.spec().policy.hold_pending,
        "an override dropped the layout's policy"
    );
    assert_eq!(entries.spec().consumers.len(), 2);
    let findings = bus
        .queue(&"findings".parse().expect("a queue"))
        .expect("declared");
    assert!(!findings.spec().policy.keeps_events());
    findings
        .push(json!({"what": "a finding"}))
        .expect("appended");
    assert!(
        channel.join("findings.jsonl").is_file(),
        "the queue was not kept under transport.dir"
    );
    let teller = Author::from("teller");
    assert!(bus
        .allowlist()
        .allows(&teller, &OpWord("post".to_owned()))
        .is_ok());
    assert!(
        bus.allowlist()
            .allows(&teller, &OpWord("audit".to_owned()))
            .is_err(),
        "narrowing did not apply"
    );

    let elsewhere = dir.path().join("elsewhere");
    let bus = config
        .clone()
        .with_transport_dir(&elsewhere)
        .resolve(&layouts(), &TransportKinds::builtin())
        .expect("resolves");
    bus.queue(&"findings".parse().expect("a queue"))
        .expect("declared")
        .push(json!({"what": "redirected"}))
        .expect("appended");
    assert!(
        elsewhere.join("findings.jsonl").is_file(),
        "the transport directory was not overridden"
    );
}

/// What the file alone decides is refused by `load`, naming the key.
#[test]
fn load_refuses_an_unknown_key_a_version_and_a_malformed_value_by_name() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    for (text, names) in [
        ("version: 1\ntransport: {kind: local, dir: x}\nqueus: {}\n", "unknown field `queus`"),
        (
            "version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  findings: {policy: {hold_pendin: false}}\n",
            "unknown field `hold_pendin`",
        ),
        (
            "version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  findings: {retain: forever}\n",
            "unknown field `retain`",
        ),
        (
            "version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  sentinel: {capabilities: [retry], extra: 1}\n",
            "unknown field `extra`",
        ),
        (
            "version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  Bad_Name: {capabilities: []}\n",
            "authors.Bad_Name",
        ),
        (
            "version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  sentinel: {}\n",
            "missing field `capabilities`",
        ),
        ("version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  ../escape: {}\n", "is not a queue name"),
        (
            "version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  q: {schema: not-an-id}\n",
            "is not a schema id",
        ),
        (
            "version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  q: {claims: {field: a, equals: 1, present: true}}\n",
            "exactly one of",
        ),
        ("transport: {kind: local, dir: x}\n", "missing field `version`"),
        ("version: 1\ntransport: {kind: NATS, dir: x}\n", "is not a transport kind name"),
    ] {
        let path = write(&dir, text);
        match Config::load(&path) {
            Err(ConfigError::Parse { why, .. }) => assert!(why.contains(names), "{text:?}: {why}"),
            other => panic!("{text:?} was not refused by load: {other:?}"),
        }
    }
    let path = write(&dir, "version: 2\ntransport: {kind: local, dir: x}\n");
    match Config::load(&path) {
        Err(refusal @ ConfigError::Version { found: 2 }) => {
            assert!(refusal.to_string().starts_with("version: 2"), "{refusal}");
        }
        other => panic!("version 2 was not refused: {other:?}"),
    }
    match Config::load(dir.path().join("absent.yaml")) {
        Err(refusal @ ConfigError::Read { .. }) => {
            assert!(refusal.to_string().contains("absent.yaml"), "{refusal}");
        }
        other => panic!("{other:?}"),
    }
}

/// What only the linked layouts and transports decide is refused by `resolve`,
/// naming the key — a widened grant most of all.
#[test]
fn resolve_refuses_a_widened_grant_an_unknown_profile_and_a_dangling_key_by_name() {
    let kinds = TransportKinds::builtin();
    let refused = |text: &str| -> String {
        Config::parse(text)
            .expect("the file alone is fine")
            .resolve(&layouts(), &kinds)
            .expect_err("resolve refuses")
            .to_string()
    };
    let dir = tempfile::tempdir().expect("a scratch directory");
    let local = format!("transport: {{kind: local, dir: {}}}", dir.path().display());

    let widened = refused(&format!(
        "version: 1\n{local}\nprofile: ledger\nauthors:\n  teller: {{capabilities: [post, void]}}\n"
    ));
    assert_eq!(
        widened,
        "authors.teller.capabilities: `void` is not granted to teller by the profile, and a configuration may narrow an author's grants but never widen them"
    );
    let unknown_op = refused(&format!(
        "version: 1\n{local}\nprofile: ledger\nauthors:\n  sentinel: {{capabilities: [sign]}}\n"
    ));
    assert!(
        unknown_op.starts_with("authors.sentinel.capabilities: `sign` is not an op"),
        "{unknown_op}"
    );
    for (fragment, key) in [
        (
            "capabilities: [], refusals: {sign: no}",
            "authors.sentinel.refusals.sign",
        ),
        (
            "capabilities: [post], refusals: {post: no}",
            "authors.sentinel.refusals.post",
        ),
        (
            "capabilities: [], refusals: {post: '   '}",
            "authors.sentinel.refusals.post",
        ),
    ] {
        let failure = refused(&format!(
            "version: 1\n{local}\nprofile: ledger\nauthors:\n  sentinel: {{{fragment}}}\n"
        ));
        assert!(failure.starts_with(key), "{failure}");
    }
    let profile = refused(&format!("version: 1\n{local}\nprofile: bank\n"));
    assert_eq!(
        profile,
        "profile: `bank` is not a layout this build links; the layouts are: ledger"
    );
    let answers = refused(&format!(
        "version: 1\n{local}\nprofile: ledger\nqueues:\n  entries: {{answers: replies}}\n"
    ));
    assert_eq!(
        answers,
        "queues.entries.answers: `replies` is not a queue this configuration declares"
    );
    let schema = refused(&format!(
        "version: 1\n{local}\nprofile: ledger\nqueues:\n  entries: {{schema: ledger.entry@1}}\n"
    ));
    assert!(
        schema.starts_with(
            "queues.entries.schema: ledger.entry@1 is not a schema this layout registers"
        ),
        "{schema}"
    );
    let transport = refused("version: 1\ntransport: {kind: local, dir: x, url: nats://h}\n");
    assert_eq!(
        transport,
        "transport: transport.url is not a key the local transport takes"
    );
    let kind = refused("version: 1\ntransport: {kind: nats}\n");
    assert!(
        kind.starts_with("transport: \"nats\" is not a transport kind"),
        "{kind}"
    );
    let unknown = QueueName::try_from("elsewhere").expect("a queue");
    let bus = Config::parse(&format!("version: 1\n{local}\nprofile: ledger\n"))
        .expect("parses")
        .resolve(&layouts(), &kinds)
        .expect("resolves");
    assert_eq!(
        bus.queue(&unknown).expect_err("undeclared").to_string(),
        "`elsewhere` is not a queue this configuration declares; it declares: entries"
    );
}

/// The configuration's JSON Schema is generated from the one reader's type, so
/// what the SDK bundle carries is what `load` reads.
#[test]
fn the_configuration_schema_accepts_the_documented_file_and_refuses_an_unknown_key() {
    let schema = schemars::schema_for!(Config).to_value();
    let validator = jsonschema::validator_for(&schema).expect("a usable schema");
    let documented = json!({
        "version": 1,
        "transport": {"kind": "local", "dir": "runs/r/channel"},
        "profile": "planner-channel",
        "queues": {"findings": {"policy": {"hold_pending": false}}},
        "authors": {"sentinel": {"capabilities": ["retry", "finding"], "refusals": {"complete": "the planner decides"}}}
    });
    assert!(
        validator.is_valid(&documented),
        "{:?}",
        validator
            .iter_errors(&documented)
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
    let mut unknown = documented.clone();
    unknown["queus"] = json!({});
    assert!(
        !validator.is_valid(&unknown),
        "the schema admits an unknown key"
    );
}

/// A schema registered at run time is one a queue's `schema` may name, and every
/// record pushed onto that queue is validated against it; one the layout already
/// holds under a different document is refused naming the key.
#[test]
fn resolve_with_registry_validates_a_queue_against_a_schema_registered_at_run_time() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let text = format!(
        "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: ledger\nqueues:\n  greetings: {{schema: demo.greeting@1}}\n",
        dir.path().display()
    );
    let config = Config::parse(&text).expect("loads");
    let unregistered = config
        .resolve(&layouts(), &TransportKinds::default())
        .expect_err("the layout registers no demo.greeting@1");
    assert!(
        unregistered
            .to_string()
            .starts_with("queues.greetings.schema: demo.greeting@1 is not a schema"),
        "{unregistered}"
    );

    let mut added = Registry::new();
    let greeting = json!({
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"]
    });
    added
        .register_schema("demo.greeting@1".parse().expect("an id"), greeting)
        .expect("a schema");
    let bus = config
        .resolve_with_registry(&layouts(), &TransportKinds::default(), &added)
        .expect("resolves with the added schema");
    let greetings: QueueName = "greetings".parse().expect("a queue");
    bus.send(&greetings, json!({"text": "hello"}))
        .expect("a greeting conforms");
    let refused = bus
        .send(&greetings, json!({"text": 7}))
        .expect_err("a number is no greeting");
    assert!(refused.to_string().contains("demo.greeting@1"), "{refused}");
    assert!(refused.to_string().contains("/text"), "{refused}");

    struct Holding;
    impl Layout for Holding {
        fn name(&self) -> &str {
            "holding"
        }
        fn queues(&self) -> Vec<QueueSpec> {
            Vec::new()
        }
        fn allowlist(&self) -> Allowlist<OpWord> {
            Allowlist::new(Vec::<OpWord>::new())
        }
        fn registry(&self) -> Registry {
            let mut registry = Registry::new();
            registry
                .register_schema(
                    "demo.greeting@1".parse().expect("an id"),
                    json!({"type": "string"}),
                )
                .expect("a schema");
            registry
        }
    }
    let conflict = Config::parse(&format!(
        "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: holding\n",
        dir.path().display()
    ))
    .expect("loads")
    .resolve_with_registry(
        &Layouts::new().with(Arc::new(Holding)),
        &TransportKinds::default(),
        &added,
    )
    .expect_err("the layout holds another document under the id");
    assert!(conflict.to_string().starts_with("registry: "), "{conflict}");
    assert!(
        conflict.to_string().contains("demo.greeting@1"),
        "{conflict}"
    );
}

/// A transport held open is what two binds share: a record one bus appended over
/// a memory transport is the other's to claim, and a schema registered between
/// the two binds is one the second validates by.
#[test]
fn resolve_over_binds_each_bus_over_the_transport_held_open() {
    let config = Config::parse(
        "version: 1\ntransport: {kind: memory}\nprofile: ledger\nqueues:\n  greetings: {schema: demo.greeting@1}\n",
    )
    .expect("loads");
    let transport = TransportKinds::default()
        .open(&config.transport)
        .expect("a memory transport");
    let refused = config
        .resolve_over(&layouts(), Arc::clone(&transport), &Registry::new())
        .expect_err("nothing registers demo.greeting@1 yet");
    assert!(
        refused.to_string().starts_with("queues.greetings.schema: "),
        "{refused}"
    );
    let mut added = Registry::new();
    added
        .register_schema(
            "demo.greeting@1".parse().expect("an id"),
            json!({"type": "object", "required": ["text"]}),
        )
        .expect("a schema");
    let first = config
        .resolve_over(&layouts(), Arc::clone(&transport), &added)
        .expect("binds");
    let greetings: QueueName = "greetings".parse().expect("a queue");
    first
        .send(&greetings, json!({"text": "hello"}))
        .expect("a greeting");
    let second = config
        .resolve_over(&layouts(), Arc::clone(&transport), &added)
        .expect("binds again");
    let status = second
        .queue(&greetings)
        .expect("declared")
        .status()
        .expect("a status");
    assert_eq!(status.records, 1, "the held transport keeps what was sent");
    assert!(Arc::ptr_eq(first.transport(), second.transport()));
}

/// Every policy key and every declaration key a configuration sets reaches the
/// queue it names, a layout that shapes nothing offers each record as it is, and
/// a key a built-in transport does not take is refused.
#[test]
fn a_configuration_sets_every_policy_and_declaration_key_on_a_queue() {
    use onemessagebus::{Delivery, Ordering, Retention};
    let dir = tempfile::tempdir().expect("a scratch directory");
    let text = format!(
        "version: 1\ntransport: {{kind: local, dir: {}}}\nprofile: ledger\nqueues:\n  beats:\n    policy: {{delivery: at-least-once, ordering: per-queue, supersede_on: {{key: kind, when: {{field: kind, equals: beat}}}}, hold_pending: true, blocking_first: true, retention: keep, projection: beats.json}}\n    claims: {{field: kind, present: true}}\n    consumers: [default, auditor]\n  notes: {{numbered: true}}\n",
        dir.path().display()
    );
    let bus = Config::parse(&text)
        .expect("loads")
        .resolve(&layouts(), &TransportKinds::default())
        .expect("resolves");
    assert!(format!("{bus:?}").contains("beats"), "{bus:?}");
    assert!(format!("{:?}", layouts()).contains("ledger"));
    assert!(format!("{:?}", TransportKinds::default()).contains("local"));
    let beats = bus
        .queue(&"beats".parse().expect("a queue"))
        .expect("declared");
    let policy = &beats.spec().policy;
    assert_eq!(
        (policy.delivery, policy.ordering, policy.retention),
        (Delivery::AtLeastOnce, Ordering::PerQueue, Retention::Keep)
    );
    assert!(policy.hold_pending && policy.blocking_first);
    assert_eq!(
        policy
            .supersede_on
            .as_ref()
            .map(|supersede| supersede.key.to_string()),
        Some("kind".to_owned())
    );
    assert_eq!(
        policy.projection.as_ref().map(|name| name.as_str()),
        Some("beats.json")
    );
    assert!(beats.spec().claims.is_some());
    let notes = bus
        .queue(&"notes".parse().expect("a queue"))
        .expect("declared");
    assert!(notes.spec().numbered);
    assert!(bus.registry().ids().is_empty());
    assert!(Arc::ptr_eq(bus.transport(), beats.transport()));

    let entries: QueueName = "entries".parse().expect("a queue");
    let sent = bus
        .send(&entries, json!({"blocking": true, "what": "an entry"}))
        .expect("the ledger shapes nothing");
    assert_eq!(sent[0].0, entries);
    assert_eq!(sent[0].1.record["what"], json!("an entry"));

    let refused = Config::parse("version: 1\ntransport: {kind: memory, dir: somewhere}\n")
        .expect("loads")
        .resolve(&layouts(), &TransportKinds::default())
        .expect_err("the memory transport takes no directory");
    assert_eq!(
        refused.to_string(),
        "transport: transport.dir is not a key the memory transport takes"
    );
}
