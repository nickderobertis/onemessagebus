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
    Carried, Carry, Changed, Config, ConsumerName, Disposition, Emitter, Freshness, Inbox, Layouts,
    LinkResolver, MemoryTransport, Merge, Message, Open, Outcome, Policy, QueueConfig, QueueName,
    QueueSpec, RawQueue, Reader, Reading, Redactor, Registry, SchemaId, SchemaLink, Source, Spool,
    TransportKinds, CAPABILITIES,
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
            "onemessagebus::Bus::serve",
            Box::new(|| {
                let bus = Config::parse(
                    "version: 1\ntransport: {kind: memory}\nprofile: planner-channel\n",
                )
                .expect("loads")
                .resolve(
                    &Layouts::new().with(Arc::new(onemessagebus_agent::channel::PlannerChannel)),
                    &TransportKinds::builtin(),
                )
                .expect("resolves");
                let codec_config = onemessagebus::Config::parse(
                    "version: 1\ntransport: {kind: memory}\ncodecs:\n  example:\n    select: op\n    frames:\n      hello:\n        schema: example.hello@1\n        bindings:\n          - do: answer\n            response: {ok: true}\n",
                )
                .expect("codec config loads");
                let name: onemessagebus::CodecName = "example".parse().expect("a codec name");
                let mut codec = onemessagebus::ConfiguredCodec::new(
                    name.clone(),
                    codec_config.codecs[&name].clone(),
                )
                .expect("configured codec");
                let mut output = Vec::new();
                let refused = bus
                    .serve(
                        &queue("surfaces"),
                        &mut codec,
                        &onemessagebus::ServeOptions::default(),
                        Box::new(std::io::Cursor::new("[]\n")),
                        &mut output,
                    )
                    .expect_err("a non-object frame is refused");
                assert!(matches!(refused, onemessagebus::ServeError::Refused(_)));
                assert!(output.is_empty());
            }),
        ),
        (
            "onemessagebus::Bus::ask",
            Box::new(|| {
                let (_dir, bus) = asking_bus();
                let pending = bus
                    .ask::<serde_json::Value, serde_json::Value>(
                        &queue("questions"),
                        json!({ "text": "go on?" }),
                        onemessagebus::AskOptions::default(),
                    )
                    .expect("asked");
                assert!(pending.correlation().as_str().starts_with("c-"));
                assert_eq!(
                    pending.wait(Duration::from_millis(50)),
                    onemessagebus::Answer::Timeout
                );
            }),
        ),
        (
            "onemessagebus::Bus::reply",
            Box::new(|| {
                let (_dir, bus) = asking_bus();
                let pending = bus
                    .ask::<serde_json::Value, serde_json::Value>(
                        &queue("questions"),
                        json!({ "text": "go on?" }),
                        onemessagebus::AskOptions::default(),
                    )
                    .expect("asked");
                let bound = bus
                    .reply(
                        &queue("questions"),
                        Some(pending.correlation()),
                        json!({ "text": "go on" }),
                    )
                    .expect("bound");
                assert!(bound.answered);
                match pending.wait(Duration::from_secs(10)) {
                    onemessagebus::Answer::Reply(reply) => {
                        assert_eq!(reply["text"], json!("go on"))
                    }
                    other => panic!("not the reply: {other:?}"),
                }
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
            "onemessagebus::LinkResolver::resolve",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let path = dir.path().join("frames.json");
                std::fs::write(
                    &path,
                    json!({"version": "8.1", "schemas": [{"id": "test.frame@1", "schema": {"type": "object"}}]})
                        .to_string(),
                )
                .expect("written");
                let link = SchemaLink::parse(&format!("{}@8", path.display())).expect("a link");
                let resolved = LinkResolver::new(Some(dir.path().join("cache")))
                    .resolve(&link, Freshness::Window)
                    .expect("resolves");
                assert_eq!(resolved.outcome(), &Outcome::Read);
                let mut registry = Registry::new();
                resolved.register_into(&mut registry).expect("registers");
                assert_eq!(registry.ids(), vec!["test.frame@1".parse().expect("an id")]);
            }),
        ),
        (
            "onemessagebus::LinkResolver::cached",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let resolver = LinkResolver::new(Some(dir.path().join("absent")));
                assert!(resolver
                    .cached()
                    .expect("an absent cache is empty")
                    .is_empty());
                assert!(LinkResolver::new(None).cached().is_err());
            }),
        ),
        (
            "onemessagebus::LinkResolver::clear",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let resolver = LinkResolver::new(Some(dir.path().to_path_buf()));
                assert_eq!(resolver.clear().expect("clears"), 0);
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

/// A bus over a local transport with a queue questions are asked on and the
/// queue its answers land on.
fn asking_bus() -> (tempfile::TempDir, onemessagebus::Bus) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut config = Config::local(dir.path(), None);
    config.queues.insert(
        queue("questions"),
        QueueConfig {
            policy: onemessagebus::PolicyConfig {
                hold_pending: Some(true),
                ..onemessagebus::PolicyConfig::default()
            },
            answers: Some(queue("replies")),
            ..QueueConfig::default()
        },
    );
    config
        .queues
        .insert(queue("replies"), QueueConfig::default());
    let bus = config
        .resolve(&Layouts::new(), &TransportKinds::builtin())
        .expect("resolves");
    (dir, bus)
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
