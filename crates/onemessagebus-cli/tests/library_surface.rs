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
    drop_log, dropped_from, BusError, Carried, Carry, Changed, CommandHook, Config, ConsumerName,
    Disposition, Dropped, Emitter, Freshness, Hook, HookCache, HookContext, HookError, HookOutcome,
    HookRef, Hooked, HookedOutcome, Hooks, Inbox, Layouts, LinkResolver, MemoryTransport, Merge,
    Message, Open, Outcome, Policy, QueueConfig, QueueName, QueueSpec, RawQueue, RawRecord,
    RawRecordError, Reader, Reading, Redactor, Registry, SchemaId, SchemaLink, Source, Spool,
    TransportKinds, CAPABILITIES, DROPPED_SUFFIX,
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
                    "version: 1\ntransport: {kind: memory}\nqueues:\n  questions: {policy: {hold_pending: true}, answers: answers}\n  answers: {}\n",
                )
                .expect("loads")
                .resolve(&Layouts::new(), &TransportKinds::builtin())
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
                        &queue("questions"),
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
            "onemessagebus::Bus::hooked",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let mut config = Config::local(dir.path(), None);
                config
                    .queues
                    .insert(queue("findings"), QueueConfig::default());
                let bus = config
                    .resolve(&Layouts::new(), &TransportKinds::builtin())
                    .expect("resolves")
                    .with_hook(&queue("findings"), RefusesSilence)
                    .expect("a declared queue");
                let kept = bus
                    .hooked(
                        &queue("findings"),
                        RawRecord::from_value(&json!({ "what": "the base moved" })),
                    )
                    .expect("decided");
                assert_eq!(kept.outcome, HookedOutcome::Keep);
                assert_eq!(kept.records.len(), 1);
                let refused = bus
                    .hooked(
                        &queue("findings"),
                        RawRecord::from_value(&json!({ "what": "" })),
                    )
                    .expect("decided");
                assert_eq!(
                    refused.outcome,
                    HookedOutcome::Refuse {
                        reason: "a finding says what it found".to_owned()
                    }
                );
                assert!(refused.records.is_empty());
                assert!(
                    !dir.path().join("findings.jsonl").exists(),
                    "hooked appended"
                );
            }),
        ),
    ]
}

/// A hook refusing a finding that says nothing.
struct RefusesSilence;

impl Hook<serde_json::Value> for RefusesSilence {
    fn run(&self, message: &serde_json::Value, _: &HookContext) -> HookOutcome<serde_json::Value> {
        if message["what"]
            .as_str()
            .is_some_and(|what| !what.is_empty())
        {
            HookOutcome::Keep
        } else {
            HookOutcome::Refuse {
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

/// A note on a typed queue, as a Rust consumer declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Note {
    text: String,
    n: u64,
}

impl Message for Note {
    const SCHEMA: SchemaId = SchemaId::literal("test", "note", 1);
}

/// A typed hook: a note saying `drop me` is dropped, and any other is shouted.
struct Shouts;

impl Hook<Note> for Shouts {
    fn run(&self, note: &Note, _: &HookContext) -> HookOutcome<Note> {
        if note.text == "drop me" {
            HookOutcome::Drop
        } else {
            HookOutcome::Change(Note {
                text: note.text.to_uppercase(),
                n: note.n + 1,
            })
        }
    }
}

/// A hook over records, answering one fixed outcome.
struct Answers(HookOutcome<RawRecord>);

impl Hook<RawRecord> for Answers {
    fn run(&self, _: &RawRecord, _: &HookContext) -> HookOutcome<RawRecord> {
        self.0.clone()
    }
}

/// A bus over a local transport in `dir` declaring `queues`, plain unless
/// named `typed` (whose schema is [`Note`]'s) or `numbered`.
fn hooked_bus(dir: &std::path::Path, queues: &[&str]) -> onemessagebus::Bus {
    let mut config = Config::local(dir, None);
    for name in queues {
        let declared = match *name {
            "typed" => QueueConfig {
                schema: Some(Note::SCHEMA),
                ..QueueConfig::default()
            },
            "numbered" => QueueConfig {
                numbered: Some(true),
                ..QueueConfig::default()
            },
            _ => QueueConfig::default(),
        };
        config.queues.insert(queue(name), declared);
    }
    let mut registry = Registry::new();
    registry.register::<Note>().expect("registers");
    config
        .resolve_with_registry(&Layouts::new(), &TransportKinds::builtin(), &registry)
        .expect("resolves")
}

/// The raw bytes of `queue`'s file under `dir`.
fn file(dir: &std::path::Path, queue: &str) -> String {
    std::fs::read_to_string(dir.join(format!("{queue}.jsonl"))).unwrap_or_default()
}

/// An `sh` script in `dir`, run as `sh <path>` because a shebang launches
/// nothing on Windows. Its path has forward slashes, which a Windows `sh` reads
/// as separators.
fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("{body}\n")).expect("written");
    path.to_str().expect("a UTF-8 path").replace('\\', "/")
}

/// Each entry of the hook surface `docs/hooks.md` states, and the call that
/// exercises it.
fn hook_surface() -> Vec<Exercise> {
    vec![
        (
            "onemessagebus::RawRecord::parse",
            Box::new(|| {
                let record = RawRecord::parse("{ \"a\" : 1.0 }\n").expect("one record");
                assert_eq!(record.bytes(), "{ \"a\" : 1.0 }");
                assert_eq!(record.value(), &json!({"a": 1.0}));
                for (bytes, refused) in [
                    ("{\n\"a\": 1\n}", RawRecordError::MultiLine { lines: 3 }),
                    ("  ", RawRecordError::Empty),
                ] {
                    assert_eq!(RawRecord::parse(bytes), Err(refused));
                }
                assert!(matches!(
                    RawRecord::parse("{} {}"),
                    Err(RawRecordError::NotJson { .. })
                ));
            }),
        ),
        (
            "onemessagebus::RawRecord::from_value",
            Box::new(|| {
                let record = RawRecord::from_value(&json!({"a": 1, "b": [true]}));
                assert_eq!(record.bytes(), r#"{"a":1,"b":[true]}"#);
                assert_eq!(record.into_value(), json!({"a": 1, "b": [true]}));
            }),
        ),
        (
            "onemessagebus::Hooks::run",
            Box::new(|| {
                let context = HookContext::new(queue("notes"))
                    .with_correlation("c-1".parse().expect("a correlation"));
                assert_eq!(
                    context.correlation.as_ref().map(|c| c.as_str()),
                    Some("c-1")
                );
                let changed = RawRecord::from_value(&json!({"changed": true}));
                let mut hooks = Hooks::<RawRecord>::new()
                    .with(Answers(HookOutcome::Keep))
                    .with(Answers(HookOutcome::Change(changed.clone())));
                assert_eq!((hooks.len(), hooks.is_empty()), (2, false));
                let offered = RawRecord::from_value(&json!({}));
                assert_eq!(hooks.run(&offered, &context), HookOutcome::Change(changed));
                hooks.push(Arc::new(Answers(HookOutcome::Drop)));
                hooks.push(Arc::new(Answers(HookOutcome::Refuse {
                    reason: "never reached".to_owned(),
                })));
                assert_eq!(hooks.run(&offered, &context), HookOutcome::Drop);
                assert!(Hooks::<RawRecord>::new().is_empty());
            }),
        ),
        (
            "onemessagebus::CommandHook::run",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let context = HookContext::new(queue("notes"));
                let keep = CommandHook::new(["cat"]).expect("an argv");
                assert_eq!(keep.command(), ["cat"]);
                assert!(keep.cache().is_none() && keep.routes().is_none());
                assert_eq!(keep.run(br#"{"a":1}"#, &context), HookOutcome::Keep);
                let respell = script(
                    dir.path(),
                    "respell.sh",
                    "echo ran >> \"$(dirname \"$0\")/ran\"\ncat > /dev/null\necho '{ \"a\" : 1.0 }'",
                );
                let bar = script(dir.path(), "bar.sh", "echo bar-1");
                let cache =
                    HookCache::new(dir.path().join("cache"), ["sh", bar.as_str()]).expect("a bar");
                assert_eq!(cache.dir(), dir.path().join("cache"));
                assert_eq!(cache.bar_fingerprint(), ["sh", bar.as_str()]);
                assert_eq!(cache.fingerprint().as_deref(), Some("bar-1"));
                let cached = CommandHook::new(["sh", respell.as_str()])
                    .expect("an argv")
                    .with_cache(cache.clone());
                assert_eq!(cached.cache(), Some(&cache));
                for _ in 0..2 {
                    assert_eq!(
                        cached.run(br#"{"a":1}"#, &context),
                        HookOutcome::Change(RawRecord::parse("{ \"a\" : 1.0 }").expect("JSON"))
                    );
                }
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("ran"))
                        .expect("ran")
                        .lines()
                        .count(),
                    1,
                    "a cached change ran again"
                );
                let reroute = script(
                    dir.path(),
                    "reroute.sh",
                    "cat\nprintf there > \"$ONEMESSAGEBUS_HOOK_ROUTE\"",
                );
                let routed = CommandHook::new(["sh", reroute.as_str()])
                    .expect("an argv")
                    .with_routes([queue("there")]);
                assert_eq!(routed.routes(), Some([queue("there")].as_slice()));
                assert_eq!(
                    routed.run(br#"{"a":1}"#, &context),
                    HookOutcome::Reroute {
                        queue: queue("there"),
                        message: RawRecord::parse(r#"{"a":1}"#).expect("JSON"),
                    }
                );
                assert_eq!(
                    CommandHook::new(Vec::<String>::new()),
                    Err(HookError::NoCommand)
                );
                assert_eq!(
                    HookCache::new("cache", Vec::<String>::new()),
                    Err(HookError::NoBar)
                );
            }),
        ),
        (
            "onemessagebus::Bus::with_hook",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let bus = hooked_bus(dir.path(), &["typed"])
                    .with_hook(&queue("typed"), Shouts)
                    .expect("a declared queue");
                let sent = bus
                    .send(&queue("typed"), json!({"text": "hello", "n": 1}))
                    .expect("sent");
                assert_eq!(sent.len(), 1);
                assert_eq!(file(dir.path(), "typed"), "{\"text\":\"HELLO\",\"n\":2}\n");
                let dropped = bus
                    .send(&queue("typed"), json!({"text": "drop me", "n": 1}))
                    .expect("sent");
                assert!(dropped.is_empty(), "a dropped record was answered as sent");
                assert_eq!(file(dir.path(), "typed").lines().count(), 1);
                let refused = bus
                    .send(&queue("typed"), json!({"text": 3}))
                    .expect_err("not a note");
                assert!(
                    matches!(&refused, BusError::HookRefused { hook, .. } if hook.index == 0 && hook.command.is_none()),
                    "{refused}"
                );
                assert!(
                    hooked_bus(dir.path(), &["typed"])
                        .with_hook(&queue("nowhere"), Shouts)
                        .is_err(),
                    "a hook on an undeclared queue was registered"
                );
            }),
        ),
        (
            "onemessagebus::Bus::dropped",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let bus = hooked_bus(dir.path(), &["typed"])
                    .with_hook(&queue("typed"), Shouts)
                    .expect("a declared queue");
                let offered = bus
                    .offer(&queue("typed"), json!({"text": "drop me", "n": 7}))
                    .expect("offered");
                assert!(offered.pushed.is_empty());
                let [dropped]: [Dropped; 1] = bus
                    .dropped(&queue("typed"))
                    .expect("read")
                    .try_into()
                    .expect("one drop");
                assert_eq!(offered.dropped, std::slice::from_ref(&dropped));
                assert_eq!(dropped.queue, queue("typed"));
                assert_eq!(
                    dropped.hook,
                    HookRef {
                        index: 0,
                        command: None
                    }
                );
                assert_eq!(dropped.message.bytes(), r#"{"text":"drop me","n":7}"#);
                assert_eq!(Dropped::parse(dropped.line().as_bytes()), Ok(dropped));
                assert_eq!(drop_log(&queue("typed")), queue("typed-dropped"));
                assert_eq!(dropped_from(&queue("typed-dropped")), Some(queue("typed")));
                assert_eq!(DROPPED_SUFFIX, "-dropped");
                let status = bus
                    .queue(&queue("typed"))
                    .expect("declared")
                    .status()
                    .expect("status");
                assert_eq!(status.dropped, 1);
            }),
        ),
        (
            "onemessagebus::Bus::append_hooked",
            Box::new(|| {
                let dir = tempfile::tempdir().expect("a temp dir");
                let respelled = RawRecord::parse("{ \"a\" : 1.0 }").expect("JSON");
                let bus = hooked_bus(dir.path(), &["numbered", "plain"])
                    .with_hook(
                        &queue("numbered"),
                        Answers(HookOutcome::Change(respelled.clone())),
                    )
                    .expect("a declared queue")
                    .with_hook(
                        &queue("plain"),
                        Answers(HookOutcome::Reroute {
                            queue: queue("numbered"),
                            message: respelled.clone(),
                        }),
                    )
                    .expect("a declared queue");
                let hooked: Hooked = bus
                    .hooked(&queue("numbered"), RawRecord::from_value(&json!({"a": 1})))
                    .expect("decided");
                assert_eq!(hooked.outcome, HookedOutcome::Change);
                assert_eq!(hooked.records, [(queue("numbered"), respelled.clone())]);
                assert!(file(dir.path(), "numbered").is_empty(), "hooked appended");
                let pushed = bus.append_hooked(&hooked).expect("appended");
                assert_eq!(pushed[0].1.id, Some(0));
                // The queue's own id is spliced in front of the hook's bytes.
                assert_eq!(file(dir.path(), "numbered"), "{\"id\":0, \"a\" : 1.0 }\n");
                // A reroute lands where it was sent, through that queue's hooks.
                let rerouted = bus
                    .hooked(&queue("plain"), RawRecord::from_value(&json!({"b": 2})))
                    .expect("decided");
                assert_eq!(rerouted.outcome, HookedOutcome::Change);
                assert_eq!(rerouted.records[0].0, queue("numbered"));
                let refused = Hooked {
                    outcome: HookedOutcome::Refuse {
                        reason: "no".to_owned(),
                    },
                    records: Vec::new(),
                    dropped: Vec::new(),
                };
                assert!(bus.append_hooked(&refused).expect("nothing").is_empty());
                let unjudged = bus
                    .clone()
                    .with_hook(
                        &queue("numbered"),
                        Answers(HookOutcome::Unjudged {
                            reason: "?".to_owned(),
                        }),
                    )
                    .expect("declared")
                    .hooked(&queue("numbered"), RawRecord::from_value(&json!({})))
                    .expect("decided");
                // The change before it is handed on, and the unjudged answer
                // after it ends the chain with nothing to append.
                assert_eq!(
                    unjudged.outcome,
                    HookedOutcome::Unjudged {
                        reason: "?".to_owned()
                    }
                );
                assert!(unjudged.records.is_empty());
            }),
        ),
        (
            "onemessagebus::BusError::Dropped",
            Box::new(|| {
                let (_dir, bus) = asking_bus();
                let bus = bus
                    .with_hook(&queue("questions"), Answers(HookOutcome::Drop))
                    .expect("declared");
                let asked = bus
                    .ask::<serde_json::Value, serde_json::Value>(
                        &queue("questions"),
                        json!({"message": "anyone?"}),
                        onemessagebus::AskOptions::default(),
                    )
                    .expect_err("dropped");
                // No `..`: a field added to this variant stops this compiling.
                match &asked {
                    BusError::Dropped { queue: on } => assert_eq!(on, &queue("questions")),
                    other => panic!("not a drop: {other}"),
                }
                assert_eq!(
                    asked.to_string(),
                    "questions: dropped by a hook; it is recorded in questions-dropped and appended nowhere else"
                );
                let recorded = bus.dropped(&queue("questions")).expect("read");
                assert_eq!(recorded.len(), 1);
                assert_eq!(recorded[0].hook.index, 0);
                let (_dir, bus) = asking_bus();
                let pending = bus
                    .ask::<serde_json::Value, serde_json::Value>(
                        &queue("questions"),
                        json!({"message": "anyone?"}),
                        onemessagebus::AskOptions::default(),
                    )
                    .expect("asked");
                let bus = bus
                    .with_hook(&queue("replies"), Answers(HookOutcome::Drop))
                    .expect("declared");
                let replied = bus
                    .reply(
                        &queue("questions"),
                        Some(pending.correlation()),
                        json!({"message": "me"}),
                    )
                    .expect_err("dropped");
                match &replied {
                    BusError::Dropped { queue: on } => assert_eq!(on, &queue("replies")),
                    other => panic!("not a drop: {other}"),
                }
                assert_eq!(bus.dropped(&queue("replies")).expect("read").len(), 1);
            }),
        ),
        (
            "onemessagebus::HookConfig",
            Box::new(|| {
                let config = Config::parse(
                    "version: 1\ntransport: {kind: local, dir: bus}\nqueues: {a: {}, b: {}}\nhooks:\n  - {on: a, when: {carries: x}, command: [cat], routes: [b], cache: {dir: c, bar_fingerprint: [echo, one]}}\n",
                )
                .expect("loads");
                let [hook]: [onemessagebus::HookConfig; 1] =
                    config.hooks.clone().try_into().expect("one hook");
                assert_eq!(hook.on, queue("a"));
                assert_eq!(hook.routes, Some(vec![queue("b")]));
                assert_eq!(hook.command, ["cat"]);
                assert!(hook.when.is_some() && hook.cache.is_some());
                assert_eq!(onemessagebus::HOOK_RECORD_VERSION, 2);
                assert_eq!(onemessagebus::PASS_RECORD_VERSION, 1);
                assert_eq!(onemessagebus::DROP_LOG_VERSION, 1);
                assert_eq!(
                    [
                        onemessagebus::HOOK_QUEUE_ENV,
                        onemessagebus::HOOK_CORRELATION_ENV,
                        onemessagebus::HOOK_ROUTE_ENV
                    ],
                    [
                        "ONEMESSAGEBUS_HOOK_QUEUE",
                        "ONEMESSAGEBUS_HOOK_CORRELATION",
                        "ONEMESSAGEBUS_HOOK_ROUTE"
                    ]
                );
            }),
        ),
    ]
}

#[test]
fn every_entry_of_the_hook_surface_is_exercised() {
    let surface = hook_surface();
    let named: BTreeSet<&str> = surface.iter().map(|(entry, _)| *entry).collect();
    assert_eq!(named.len(), surface.len(), "an entry is named twice");
    for (entry, exercise) in surface {
        assert!(entry.starts_with("onemessagebus::"), "{entry}");
        exercise();
    }
}

#[test]
fn the_library_exports_no_validator_item() {
    let lib = include_str!("../../onemessagebus/src/lib.rs");
    let words: BTreeSet<&str> = lib
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .collect();
    for removed in [
        "Validator",
        "Validators",
        "OnRecords",
        "CommandValidator",
        "PassCache",
        "Verdict",
        "ValidationContext",
        "ValidatorConfig",
        "ValidatorKind",
        "ValidatorError",
        "VALIDATORS_DEPRECATED",
        "VALIDATE_QUEUE_ENV",
        "VALIDATE_CORRELATION_ENV",
    ] {
        assert!(!words.contains(removed), "lib.rs still names {removed}");
    }
}
