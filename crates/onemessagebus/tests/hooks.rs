//! Hooks a linking consumer registers in code, over a real local transport:
//! what a typed hook's answers become, and how the drop log is read back.

use onemessagebus::{
    drop_log, Bus, Config, Hook, HookContext, HookOutcome, HookedOutcome, Layouts, Message,
    QueueConfig, QueueName, RawRecord, Registry, SchemaId, TransportKinds,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Report {
    text: String,
}

impl Message for Report {
    const SCHEMA: SchemaId = SchemaId::literal("test", "report", 1);
}

/// A typed hook: a report saying `quiet` is dropped, one saying `escalate` is
/// sent on to the manager, shouting, and any other kept.
struct Triage;

impl Hook<Report> for Triage {
    fn run(&self, report: &Report, _: &HookContext) -> HookOutcome<Report> {
        match report.text.as_str() {
            "quiet" => HookOutcome::Drop,
            "escalate" => HookOutcome::Reroute {
                queue: queue("manager"),
                message: Report {
                    text: "ESCALATE".to_owned(),
                },
            },
            _ => HookOutcome::Keep,
        }
    }
}

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

fn bus(dir: &std::path::Path) -> Bus {
    let mut config = Config::local(dir, None);
    for name in ["reports", "manager"] {
        config.queues.insert(queue(name), QueueConfig::default());
    }
    config
        .resolve_with_registry(
            &Layouts::new(),
            &TransportKinds::builtin(),
            &Registry::new(),
        )
        .expect("resolves")
        .with_hook(&queue("reports"), Triage)
        .expect("a declared queue")
}

#[test]
fn a_typed_hooks_reroute_lands_its_message_on_the_queue_it_names() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let bus = bus(dir.path());
    let hooked = bus
        .hooked(
            &queue("reports"),
            RawRecord::from_value(&json!({"text": "escalate"})),
        )
        .expect("decided");
    assert_eq!(hooked.outcome, HookedOutcome::Change);
    assert_eq!(
        hooked.records,
        [(
            queue("manager"),
            RawRecord::from_value(&json!({"text": "ESCALATE"}))
        )]
    );
    let sent = bus
        .send(&queue("reports"), json!({"text": "escalate"}))
        .expect("sent");
    assert_eq!(sent[0].0, queue("manager"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("manager.jsonl")).expect("landed"),
        "{\"text\":\"ESCALATE\"}\n"
    );
}

#[test]
fn the_drop_log_is_read_back_passing_over_a_line_this_build_cannot_read() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let bus = bus(dir.path());
    let log = drop_log(&queue("reports"));
    bus.send(&queue("reports"), json!({"text": "quiet"}))
        .expect("dropped");
    for unreadable in [
        "{ not a drop".to_owned(),
        json!({"schema_version": 9, "at": 1, "queue": "reports", "hook": {"index": 0}, "message": {}})
            .to_string(),
        json!({"schema_version": 1, "at": 1, "queue": "reports", "hook": {"index": 0}, "message": {}, "extra": true})
            .to_string(),
    ] {
        bus.transport()
            .append(&log, unreadable.as_bytes())
            .expect("appended");
    }
    let offered = bus
        .offer(&queue("reports"), json!({"text": "quiet"}))
        .expect("dropped");
    assert!(offered.pushed.is_empty());
    assert_eq!(offered.dropped.len(), 1);

    let dropped = bus.dropped(&queue("reports")).expect("read");
    assert_eq!(dropped.len(), 2, "{dropped:?}");
    for drop in &dropped {
        assert_eq!(drop.queue, queue("reports"));
        assert_eq!(drop.message.bytes(), r#"{"text":"quiet"}"#);
        assert!(drop.hook.command.is_none(), "a code hook names no command");
    }
    assert!(bus.dropped(&queue("manager")).expect("read").is_empty());
    assert!(
        bus.dropped(&queue("nowhere")).is_err(),
        "an undeclared queue's drop log was read"
    );
    // The drop log is a log of its own: read as one, it holds every line.
    let raw = bus
        .queue(&log)
        .expect("the drop log opens")
        .log(None)
        .expect("read");
    assert_eq!(raw.len(), 4);
}

/// A hook over records sending every message to a queue nobody declared.
struct Astray;

impl Hook<RawRecord> for Astray {
    fn run(&self, message: &RawRecord, _: &HookContext) -> HookOutcome<RawRecord> {
        HookOutcome::Reroute {
            queue: queue("nowhere"),
            message: message.clone(),
        }
    }
}

#[test]
fn a_reroute_to_a_queue_nobody_declared_is_unjudged_and_appends_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let bus = bus(dir.path())
        .with_hook(&queue("manager"), Astray)
        .expect("a declared queue");
    let hooked = bus
        .hooked(
            &queue("manager"),
            RawRecord::from_value(&json!({"text": "x"})),
        )
        .expect("decided");
    let HookedOutcome::Unjudged { reason } = &hooked.outcome else {
        panic!("{hooked:?}");
    };
    assert!(
        reason.contains("`nowhere`, which is not a queue this configuration declares"),
        "{reason}"
    );
    assert!(hooked.records.is_empty() && hooked.dropped.is_empty());
    let refused = bus
        .send(&queue("manager"), json!({"text": "x"}))
        .expect_err("unjudged");
    assert!(
        matches!(refused, onemessagebus::BusError::HookUnjudged { .. }),
        "{refused}"
    );
    assert!(!dir.path().join("manager.jsonl").exists());
}

#[test]
fn a_chain_of_typed_hooks_is_run_cloned_and_described_as_one() {
    let chain = onemessagebus::Hooks::<Report>::default().with(Triage);
    let copy = chain.clone();
    assert_eq!(format!("{copy:?}"), "Hooks { count: 1 }");
    let context = HookContext::new(queue("reports"));
    assert_eq!(
        copy.run(
            &Report {
                text: "quiet".to_owned()
            },
            &context
        ),
        HookOutcome::Drop
    );
    assert_eq!(
        chain.run(
            &Report {
                text: "fine".to_owned()
            },
            &context
        ),
        HookOutcome::Keep
    );
    // A record read from JSON is spelled compactly; one written is its bytes.
    let read: RawRecord = serde_json::from_str("{ \"a\" : 1 }").expect("a record");
    assert_eq!(read.bytes(), r#"{"a":1}"#);
    let spelled = RawRecord::parse("{ \"a\" : 1 }").expect("a record");
    assert_eq!(
        serde_json::to_string(&spelled).expect("JSON"),
        "{ \"a\" : 1 }"
    );
}

#[test]
fn append_hooked_refuses_a_drop_naming_an_undeclared_queue_appending_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let bus = bus(dir.path());
    let record = RawRecord::from_value(&json!({"text": "kept"}));
    let made = onemessagebus::Hooked {
        outcome: HookedOutcome::Change,
        records: vec![(queue("reports"), record.clone())],
        dropped: vec![onemessagebus::Dropped {
            at: 1,
            queue: queue("nowhere"),
            hook: onemessagebus::HookRef {
                index: 0,
                command: None,
            },
            message: record,
            correlation: None,
        }],
    };
    let refused = bus
        .append_hooked(&made)
        .expect_err("an undeclared drop log");
    assert!(
        matches!(refused, onemessagebus::BusError::UnknownQueue { .. }),
        "{refused}"
    );
    assert!(
        !dir.path().join("reports.jsonl").exists(),
        "a record was appended"
    );
    assert!(!dir.path().join("nowhere-dropped.jsonl").exists());
}

/// A [`onemessagebus::Hooked`] change of `records`, recording `dropped`.
fn made(
    records: Vec<(QueueName, RawRecord)>,
    dropped: Vec<onemessagebus::Dropped>,
) -> onemessagebus::Hooked {
    onemessagebus::Hooked {
        outcome: HookedOutcome::Change,
        records,
        dropped,
    }
}

#[test]
fn append_hooked_stops_at_the_first_append_that_fails_keeping_what_came_before() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let bus = bus(dir.path());
    let first = RawRecord::parse("{ \"first\" : 1 }").expect("JSON");
    let second = RawRecord::from_value(&json!({"second": 2}));
    // A directory where the manager's log would be: that append fails.
    std::fs::create_dir(dir.path().join("manager.jsonl")).expect("in the way");
    let drop = onemessagebus::Dropped {
        at: 1,
        queue: queue("reports"),
        hook: onemessagebus::HookRef {
            index: 0,
            command: None,
        },
        message: second.clone(),
        correlation: None,
    };
    let failed = bus.append_hooked(&made(
        vec![
            (queue("reports"), first.clone()),
            (queue("manager"), second.clone()),
        ],
        vec![drop.clone()],
    ));
    assert!(failed.is_err(), "{failed:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("reports.jsonl")).expect("the first landed"),
        "{ \"first\" : 1 }\n"
    );
    assert!(
        !dir.path().join("reports-dropped.jsonl").exists(),
        "a drop after the failed append was recorded"
    );

    // A drop log that cannot be written fails after every record landed.
    std::fs::create_dir(dir.path().join("reports-dropped.jsonl")).expect("in the way");
    let failed = bus.append_hooked(&made(vec![(queue("reports"), first)], vec![drop]));
    assert!(failed.is_err(), "{failed:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("reports.jsonl"))
            .expect("read")
            .lines()
            .count(),
        2
    );
}

/// A note whose serialization fails for the text `unwritable`.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
struct Fragile {
    text: String,
}

impl Serialize for Fragile {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.text == "unwritable" {
            return Err(serde::ser::Error::custom("this note cannot be written"));
        }
        use serde::ser::SerializeStruct as _;
        let mut fields = serializer.serialize_struct("Fragile", 1)?;
        fields.serialize_field("text", &self.text)?;
        fields.end()
    }
}

impl Message for Fragile {
    const SCHEMA: SchemaId = SchemaId::literal("test", "fragile", 1);
}

/// A typed hook answering a change it cannot write, or a reroute of one.
struct Unwritable {
    reroute: bool,
}

impl Hook<Fragile> for Unwritable {
    fn run(&self, _: &Fragile, _: &HookContext) -> HookOutcome<Fragile> {
        let message = Fragile {
            text: "unwritable".to_owned(),
        };
        if self.reroute {
            HookOutcome::Reroute {
                queue: queue("manager"),
                message,
            }
        } else {
            HookOutcome::Change(message)
        }
    }
}

#[test]
fn a_typed_change_or_reroute_that_cannot_be_written_is_unjudged_with_nothing_appended() {
    for reroute in [false, true] {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut config = Config::local(dir.path(), None);
        for name in ["notes", "manager"] {
            config.queues.insert(queue(name), QueueConfig::default());
        }
        let bus = config
            .resolve(&Layouts::new(), &TransportKinds::builtin())
            .expect("resolves")
            .with_hook(&queue("notes"), Unwritable { reroute })
            .expect("a declared queue");
        let refused = bus
            .send(&queue("notes"), json!({"text": "fine"}))
            .expect_err("unjudged");
        assert!(
            matches!(&refused, onemessagebus::BusError::HookUnjudged { reason, .. }
                if reason.contains("could not be written as JSON") && reason.contains("this note cannot be written")),
            "{refused}"
        );
        for name in ["notes", "manager", "notes-dropped"] {
            assert!(!dir.path().join(format!("{name}.jsonl")).exists(), "{name}");
        }
    }
}

/// A hook recording what it was handed, then keeping it.
struct Records(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl Hook<RawRecord> for Records {
    fn run(&self, message: &RawRecord, _: &HookContext) -> HookOutcome<RawRecord> {
        self.0
            .lock()
            .expect("the record")
            .push(message.bytes().to_owned());
        HookOutcome::Keep
    }
}

#[test]
fn registered_hooks_run_after_the_configured_ones_and_receive_their_output() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let config = Config::parse(&format!(
        "version: 1\ntransport: {{kind: local, dir: {}}}\nqueues: {{notes: {{}}}}\nhooks:\n  - {{on: notes, command: [sh, -c, \"cat > /dev/null; echo '{{ \\\"first\\\" : 1 }}'\"]}}\n",
        serde_json::to_string(dir.path()).expect("a path")
    ))
    .expect("loads");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let bus = config
        .resolve(&Layouts::new(), &TransportKinds::builtin())
        .expect("resolves")
        .with_hook(&queue("notes"), Records(std::sync::Arc::clone(&seen)))
        .expect("a declared queue")
        .with_hook(&queue("notes"), Records(std::sync::Arc::clone(&seen)))
        .expect("a declared queue");
    bus.send(&queue("notes"), json!({"text": "offered"}))
        .expect("sent");
    assert_eq!(
        *seen.lock().expect("the record"),
        ["{ \"first\" : 1 }", "{ \"first\" : 1 }"],
        "a registered hook did not receive the configured hook's output, in order"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.jsonl")).expect("landed"),
        "{ \"first\" : 1 }\n"
    );
}

#[test]
fn append_hooked_appends_nothing_for_a_refused_or_unjudged_offer_whatever_its_lists_hold() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let bus = bus(dir.path());
    let record = RawRecord::from_value(&json!({"text": "never"}));
    let drop = onemessagebus::Dropped {
        at: 1,
        queue: queue("reports"),
        hook: onemessagebus::HookRef {
            index: 0,
            command: None,
        },
        message: record.clone(),
        correlation: None,
    };
    for outcome in [
        HookedOutcome::Refuse {
            reason: "no".to_owned(),
        },
        HookedOutcome::Unjudged {
            reason: "?".to_owned(),
        },
    ] {
        let appended = bus
            .append_hooked(&onemessagebus::Hooked {
                outcome,
                records: vec![(queue("reports"), record.clone())],
                dropped: vec![drop.clone()],
            })
            .expect("nothing to append");
        assert!(appended.is_empty());
    }
    assert!(
        !dir.path().join("reports.jsonl").exists(),
        "a refused record was appended"
    );
    assert!(
        !dir.path().join("reports-dropped.jsonl").exists(),
        "a refused drop was recorded"
    );
}
