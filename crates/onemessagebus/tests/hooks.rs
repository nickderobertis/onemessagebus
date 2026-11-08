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
