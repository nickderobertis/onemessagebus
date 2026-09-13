//! Every library entry the capability manifest names, exercised.
//!
//! A capability says which entry point a Rust consumer calls instead of
//! spawning the binary. This holds the two together in both directions: each
//! named entry is called here, through the path the manifest spells, and an
//! entry called here that the manifest does not name fails too — so the
//! manifest cannot name a function that does not exist, or drift away from
//! the library it describes.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use onemessagebus::sdk_schema::{self, Lang};
use onemessagebus::{
    Carried, Carry, Changed, Config, ConsumerName, Disposition, Emitter, Inbox, Layouts,
    MemoryTransport, Merge, Message, Open, Policy, Position, QueueConfig, QueueName, QueueSpec,
    RawQueue, Reader, Reading, Redactor, Registry, SchemaId, Source, Spool, TransportKinds,
    CAPABILITIES,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Ping {
    n: u64,
}

impl Message for Ping {
    const SCHEMA: SchemaId = SchemaId::literal("test", "ping", 1);
}

/// What a ping is answered with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Pong(u64);

impl Disposition for Pong {}

impl Carried for Pong {
    fn carried() -> Self {
        Pong(0)
    }
}

/// One library entry as the manifest spells it, and the call that exercises it.
type Exercise = (&'static str, Box<dyn Fn()>);

/// Each entry the manifest may name, and the call that exercises it.
fn exercised() -> Vec<Exercise> {
    vec![
        (
            "onemessagebus::Registry::ids",
            Box::new(|| {
                let mut registry = Registry::new();
                registry.register::<Ping>().expect("registers");
                assert_eq!(registry.ids(), vec![Ping::SCHEMA]);
            }),
        ),
        (
            "onemessagebus::Registry::check",
            Box::new(|| {
                let mut registry = Registry::new();
                registry.register::<Ping>().expect("registers");
                registry
                    .check(&Ping::SCHEMA, &json!({ "n": 1 }))
                    .expect("conforms");
                registry
                    .check(&Ping::SCHEMA, &json!({ "n": "one" }))
                    .expect_err("violates");
            }),
        ),
        (
            "onemessagebus::sdk_schema::generate",
            Box::new(|| {
                let document = schemars::schema_for!(Ping).to_value();
                let rendered =
                    sdk_schema::generate(Lang::Rust, &Ping::SCHEMA, &document).expect("renders");
                assert!(rendered.contains("pub struct Ping"));
                assert!(rendered.contains("pub n: u64,"));
            }),
        ),
        (
            "onemessagebus::Registry::register_schema",
            Box::new(|| {
                let mut registry = Registry::new();
                registry
                    .register_schema(Ping::SCHEMA, json!({ "type": "object" }))
                    .expect("registers");
                registry
                    .register_schema(Ping::SCHEMA, json!({ "type": "string" }))
                    .expect_err("a different document is refused");
            }),
        ),
        (
            "onemessagebus::Merge::open",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let path = dir.path().join("s.ndjson");
                Emitter::<Open>::shared("s", Source::from("cli"), &path)
                    .with_redactor(Redactor::new())
                    .emit("tick", Map::new());
                let merged = Merge::<Open>::open([&path]).expect("merges");
                assert_eq!(merged.records().len(), 1);
            }),
        ),
        (
            "onemessagebus::Emitter::shared",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let path = dir.path().join("s.ndjson");
                let emitter = Emitter::<Open>::shared("s", Source::from("cli"), &path)
                    .with_redactor(Redactor::new());
                assert_eq!(emitter.emit("tick", Map::new()).seq, 1);
                assert_eq!(emitter.emit("tock", Map::new()).seq, 2);
                let read: Vec<Reading<Open>> = Reader::open(&path).expect("opens").collect();
                assert_eq!(read.len(), 2);
            }),
        ),
        (
            "onemessagebus::Spool::deliver",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let inbox = Inbox::<Ping, Pong>::new();
                let spool = Spool::bind(dir.path(), &inbox).expect("binds");
                let address = spool.address().to_path_buf();
                let delivering = std::thread::spawn(move || {
                    Spool::deliver(&address, &json!({ "n": 4 }), Duration::from_secs(10))
                });
                let delivered = inbox
                    .take_within(Duration::from_secs(10))
                    .expect("the ping arrives");
                let n = delivered.message().n;
                delivered.answer(Pong(n * 2));
                assert_eq!(delivering.join().expect("finishes"), Ok(json!(8)));
            }),
        ),
        (
            "onemessagebus::Carry::read",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let store = dir.path().join("carried.ndjson");
                assert_eq!(
                    Carry::sender::<Ping, Pong>(&store).send(Ping { n: 1 }),
                    Ok(Pong(0))
                );
                let entries = Carry::read(&store).expect("a store");
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].message, json!({ "n": 1 }));
            }),
        ),
        (
            "onemessagebus::Bus::send",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let mut config = Config::local(dir.path(), None);
                config
                    .queues
                    .insert(queue("findings"), QueueConfig::default());
                let bus = config
                    .resolve(&Layouts::new(), &TransportKinds::builtin())
                    .expect("resolves");
                let sent = bus
                    .send(&queue("findings"), json!({ "what": "a finding" }))
                    .expect("sent");
                assert_eq!(sent.len(), 1);
                assert!(dir.path().join("findings.jsonl").is_file());
            }),
        ),
        (
            "onemessagebus::RawQueue::claim",
            Box::new(|| {
                let notes = plain("notes");
                notes.push(json!({ "n": 0 })).expect("appended");
                let claimed = notes
                    .claim(&ConsumerName::default_consumer())
                    .expect("a claim")
                    .expect("a record");
                assert_eq!(claimed.record, json!({ "n": 0 }));
            }),
        ),
        (
            "onemessagebus::RawQueue::answer_at",
            Box::new(|| {
                let policy = Policy {
                    hold_pending: true,
                    ..Policy::default()
                };
                let transport: Arc<dyn onemessagebus::Transport> = Arc::new(MemoryTransport::new());
                let questions = RawQueue::open(
                    Arc::clone(&transport),
                    QueueSpec {
                        answers: Some(queue("replies")),
                        ..QueueSpec::new(queue("questions"), policy)
                    },
                    Arc::new(Registry::new()),
                );
                questions
                    .push(json!({ "blocking": true, "text": "go on?" }))
                    .expect("queued");
                let claimed = questions
                    .claim(&ConsumerName::default_consumer())
                    .expect("a claim")
                    .expect("a record");
                let refused = questions
                    .answer_at(&claimed.position, &Position::from_token(0))
                    .expect_err("a position no reply ends at");
                assert!(
                    matches!(refused, onemessagebus::QueueError::NoReply { .. }),
                    "{refused}"
                );
                assert!(questions.held().expect("a read").is_some());
                let reply = transport
                    .append(&queue("replies"), br#"{"text":"go on"}"#)
                    .expect("a reply is appended");
                let answered = questions
                    .answer_at(&claimed.position, &reply)
                    .expect("answered");
                assert_eq!(answered.id, Some(0));
                assert!(questions.held().expect("a read").is_none());
            }),
        ),
        (
            "onemessagebus::RawQueue::wait_for_change",
            Box::new(|| {
                let notes = plain("notes");
                let since = notes.fingerprint().expect("a fingerprint");
                let writer = notes.clone();
                let appending = std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(20));
                    writer.push(json!({ "n": 1 })).expect("appended");
                });
                let changed = notes
                    .wait_for_change(&since, Duration::from_secs(10))
                    .expect("a wait");
                appending.join().expect("the writer finishes");
                assert!(matches!(changed, Changed::Moved(_)));
            }),
        ),
        (
            "onemessagebus::RawQueue::status",
            Box::new(|| {
                let notes = plain("notes");
                notes.push(json!({ "n": 0 })).expect("appended");
                let status = notes.status().expect("a status");
                assert_eq!((status.records, status.unread), (1, 1));
            }),
        ),
        (
            "onemessagebus::TransportKinds::kinds",
            Box::new(|| {
                let kinds: Vec<String> = TransportKinds::builtin()
                    .searching(Vec::new())
                    .kinds()
                    .into_iter()
                    .map(|entry| entry.kind.to_string())
                    .collect();
                assert_eq!(kinds, vec!["local", "memory"]);
            }),
        ),
        (
            "onemessagebus::Bus::validate",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let mut config = Config::local(dir.path(), None);
                config
                    .queues
                    .insert(queue("findings"), QueueConfig::default());
                let bus = config
                    .resolve(&Layouts::new(), &TransportKinds::builtin())
                    .expect("resolves")
                    .with_validator(&queue("findings"), SaysSomething)
                    .expect("a declared queue");
                assert!(bus
                    .validate(&queue("findings"), json!({ "what": "the base moved" }))
                    .expect("judged")
                    .passes());
                let refused = bus
                    .validate(&queue("findings"), json!({ "what": "" }))
                    .expect("judged");
                assert_eq!(refused.reason(), Some("a finding says what it found"));
                assert!(
                    !dir.path().join("findings.jsonl").exists(),
                    "validate appended"
                );
            }),
        ),
    ]
}

/// A deterministic validator: a finding says what it found.
struct SaysSomething;

impl onemessagebus::Validator<serde_json::Value> for SaysSomething {
    fn validate(
        &self,
        message: &serde_json::Value,
        _: &onemessagebus::ValidationContext,
    ) -> onemessagebus::Verdict {
        if message["what"]
            .as_str()
            .is_some_and(|what| !what.is_empty())
        {
            onemessagebus::Verdict::Pass
        } else {
            onemessagebus::Verdict::Refuse {
                reason: "a finding says what it found".to_owned(),
            }
        }
    }
}

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

fn plain(name: &str) -> RawQueue {
    RawQueue::open(
        Arc::new(MemoryTransport::new()),
        QueueSpec::new(queue(name), Policy::default()),
        Arc::new(Registry::new()),
    )
}

#[test]
fn every_named_library_entry_is_exercised_and_every_exercised_entry_is_named() {
    let named: BTreeSet<&str> = CAPABILITIES.iter().map(|c| c.library_entry).collect();
    let exercised = exercised();
    let covered: BTreeSet<&str> = exercised.iter().map(|(entry, _)| *entry).collect();
    assert_eq!(
        named, covered,
        "the manifest's library entries and the exercised entries differ"
    );
    for (entry, exercise) in exercised {
        exercise();
        assert!(
            entry.starts_with("onemessagebus::"),
            "{entry} is not a path into the core"
        );
    }
}
