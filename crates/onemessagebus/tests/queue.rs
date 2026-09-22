//! Contract Q, row by row: every row of the queue, subscription and author
//! table as a test of its own, over the memory transport and over the local
//! transport in a scratch directory. The same table runs over a transport
//! written outside this crate in `crates/onemessagebus-e2e/tests/plugin_transport.rs`.

use std::sync::{Arc, Mutex};

use onemessagebus::conformance::{self, QUEUE_TABLE};
use onemessagebus::{
    Asker, ConsumerName, DocumentName, LocalTransport, MemoryTransport, Policy, QueueSpec,
    RawQueue, Registry, Transport,
};
use serde_json::{json, Value};

fn memory() -> Arc<dyn Transport> {
    Arc::new(MemoryTransport::new())
}

/// Scratch directories the local rows keep their queues in, removed when the
/// test binary exits.
static SCRATCH: Mutex<Vec<tempfile::TempDir>> = Mutex::new(Vec::new());

fn local() -> Arc<dyn Transport> {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let transport = LocalTransport::open(dir.path().join("channel")).expect("the transport opens");
    SCRATCH.lock().expect("the scratch list").push(dir);
    Arc::new(transport)
}

macro_rules! rows {
    ($($row:ident),* $(,)?) => {
        mod over_memory {
            $(
                #[test]
                fn $row() {
                    onemessagebus::conformance::$row(&super::memory);
                }
            )*
        }

        mod over_local {
            $(
                #[test]
                fn $row() {
                    onemessagebus::conformance::$row(&super::local);
                }
            )*
        }

        #[test]
        fn every_row_of_the_table_is_a_test_here() {
            let mut here = vec![$(stringify!($row)),*];
            here.sort_unstable();
            let mut table: Vec<&str> = QUEUE_TABLE.iter().map(|(name, _)| *name).collect();
            table.sort_unstable();
            assert_eq!(here, table, "a row of the queue table has no test here, or a test names no row");
        }
    };
}

rows!(
    blocking_first_claims_a_blocking_record_before_an_older_one,
    hold_pending_keeps_one_claimed_blocking_record_pending_until_answered,
    a_newer_record_supersedes_a_waiting_one_with_its_key_and_never_another,
    a_claim_is_recorded_so_a_crashed_claimants_record_is_not_handed_out_twice,
    claimants_at_once_receive_distinct_records,
    abandon_marks_and_a_later_listener_of_the_same_asker_takes_back,
    a_different_asker_or_a_session_takes_nothing_back,
    a_blank_or_non_unicode_asker_is_refused,
    a_stamped_projection_that_does_not_seal_is_read_as_no_document,
    a_plain_queue_claims_through_each_consumers_cursor,
    a_record_its_schema_refuses_is_not_appended,
    a_numbered_queue_numbers_each_record_by_the_count_before_it,
    a_fingerprint_moves_when_the_queue_does_and_a_wait_sees_it,
    every_method_works_through_the_transport_a_section_lends,
    a_document_name_is_one_document_across_the_transport,
    an_allowlist_refuses_by_omission_naming_the_author_the_op_and_the_reason,
    a_configuration_narrows_an_author_and_is_refused_widening_one,
);

/// The whole table in one pass, as a consumer proving a transport runs it.
#[test]
fn the_whole_table_runs_over_the_memory_transport_in_one_pass() {
    conformance::queue_table(&memory);
}

/// An untyped event queue writes each record in the order it was given, and a
/// fold of its log — every event record read back, the projection lost — hands
/// it back in that same order under the same seal.
#[test]
fn an_untyped_event_queue_keeps_each_records_field_order_through_a_fold() {
    let transport: Arc<dyn Transport> = Arc::new(MemoryTransport::new());
    let projection: DocumentName = "questions.json".parse().expect("a document name");
    let queue = RawQueue::open(
        Arc::clone(&transport),
        QueueSpec::new(
            "questions".parse().expect("a queue name"),
            Policy {
                hold_pending: true,
                projection: Some(projection.clone()),
                ..Policy::default()
            },
        ),
        Arc::new(Registry::new()),
    );
    queue
        .push(json!({"zeta": 1, "blocking": true, "asker": "a", "alpha": 2}))
        .expect("queued");
    queue.push(json!({"omega": 3, "alpha": 4})).expect("queued");
    queue
        .claim(&ConsumerName::default_consumer())
        .expect("a claim")
        .expect("a record");
    queue.abandon(&[0]).expect("abandoned");
    queue
        .attend(&Asker::new("a", "the test").expect("an asker"))
        .expect("attended");
    let written = transport
        .document(queue.name(), &projection)
        .expect("a read")
        .expect("a projection");

    transport
        .replace_document(queue.name(), &projection, b"{}")
        .expect("the projection is lost");
    let status = queue.status().expect("a status");
    assert_eq!(
        serde_json::to_string(&status.pending).expect("JSON"),
        r#"{"id":0,"zeta":1,"blocking":true,"asker":"a","alpha":2}"#,
        "a fold reordered the pending record's fields"
    );
    assert_eq!(
        serde_json::to_string(&status.waiting).expect("JSON"),
        r#"[{"id":1,"omega":3,"alpha":4}]"#,
        "a fold reordered a waiting record's fields"
    );
    let refolded: Value = serde_json::from_slice(
        &transport
            .document(queue.name(), &projection)
            .expect("a read")
            .expect("the repaired projection"),
    )
    .expect("JSON");
    let written: Value = serde_json::from_slice(&written).expect("JSON");
    assert_eq!(
        serde_json::to_string(&refolded).expect("JSON"),
        serde_json::to_string(&written).expect("JSON"),
        "the fold's projection differs from the one the writer sealed"
    );
}

fn memory_queue(name: &str, policy: Policy) -> (Arc<dyn Transport>, RawQueue) {
    let transport: Arc<dyn Transport> = Arc::new(MemoryTransport::new());
    let queue = RawQueue::open(
        Arc::clone(&transport),
        QueueSpec::new(name.parse().expect("a queue name"), policy),
        Arc::new(Registry::new()),
    );
    (transport, queue)
}

fn held_policy() -> Policy {
    Policy {
        hold_pending: true,
        blocking_first: true,
        projection: Some("events.json".parse().expect("a document name")),
        ..Policy::default()
    }
}

fn ids(records: &[Value]) -> Vec<u64> {
    records
        .iter()
        .map(|record| record["id"].as_u64().expect("an id"))
        .collect()
}

/// Every predicate form reads — inline or from a YAML file — matches as the
/// grammar says, serializes back to the form it read, and a spec that is not one
/// predicate is refused saying why.
#[test]
fn every_predicate_form_reads_matches_and_refuses_what_is_not_one() {
    use onemessagebus::Predicate;
    let record = json!({"a": {"b": "x"}, "empty": "", "none": null, "list": [], "map": {}, "flag": false, "n": 0});
    let holds = |spec: &str| {
        Predicate::read(spec)
            .unwrap_or_else(|why| panic!("{spec}: {why}"))
            .matches(&record)
    };
    assert!(holds(r#"{"field": "a.b", "equals": "x"}"#));
    assert!(holds(r#"{"field": "none", "equals": null}"#));
    assert!(
        !holds(r#"{"field": "a.c", "equals": null}"#),
        "an absent field equals null"
    );
    assert!(holds(r#"{"field": "a", "present": true}"#));
    assert!(holds(r#"{"field": "none", "present": false}"#));
    for (field, non_empty) in [
        ("empty", false),
        ("none", false),
        ("list", false),
        ("map", false),
        ("missing", false),
        ("flag", true),
        ("n", true),
        ("a", true),
    ] {
        assert_eq!(
            holds(&format!(r#"{{"field": "{field}", "non_empty": true}}"#)),
            non_empty,
            "non_empty over {field}"
        );
    }
    assert!(holds(r#"{"field": "empty", "non_empty": false}"#));
    assert!(holds(r#"{"all": []}"#), "an empty all does not hold");
    assert!(!holds(r#"{"any": []}"#), "an empty any holds");
    assert!(holds(
        r#"{"not": {"any": [{"field": "flag", "equals": true}]}}"#
    ));
    assert!(holds(
        r#"{"all": [{"field": "flag", "equals": false}, {"any": [{"field": "n", "equals": 0}]}]}"#
    ));

    let dir = tempfile::tempdir().expect("a scratch directory");
    let file = dir.path().join("until.yaml");
    std::fs::write(&file, "any:\n  - field: a.b\n    equals: x\n").expect("a predicate file");
    assert!(Predicate::read(file.to_str().expect("a path"))
        .expect("a YAML predicate reads")
        .matches(&record));

    let spec = r#"{"not":{"all":[{"field":"a","non_empty":true},{"any":[{"field":"n","present":true}]}]}}"#;
    let predicate = Predicate::read(spec).expect("reads");
    assert_eq!(
        serde_json::to_value(&predicate).expect("JSON"),
        serde_json::from_str::<Value>(spec).expect("JSON"),
        "a predicate does not serialize to the form it read"
    );

    for (spec, says) in [
        (r#"{"field": "a"}"#, "this names none"),
        (
            r#"{"field": "a", "equals": 1, "present": true}"#,
            "this names equals and present",
        ),
        (r#"{"equals": 1}"#, "this names no `field`"),
        (r#"{"field": "a", "all": []}"#, "takes no `field`"),
        (r#"{"field": "", "equals": 1}"#, "it is empty"),
        (
            r#"{"field": "a..b", "equals": 1}"#,
            "one of its keys is empty",
        ),
        (r#"{"all": [{"field": "a"}]}"#, "this names none"),
        (r#"{"nope": 1}"#, "unknown field"),
    ] {
        let why = Predicate::read(spec).expect_err(spec);
        assert!(why.contains(says), "{spec}: {why}");
    }
    let missing = Predicate::read(&dir.path().join("absent.yaml").display().to_string())
        .expect_err("no such file");
    assert!(
        missing.contains("neither inline JSON nor a readable predicate file"),
        "{missing}"
    );
}

/// An event queue refuses a record it cannot keep naming what it is, and passes
/// over every line another writer left that it cannot read — reading a line with
/// no event as the older writer meant it.
#[test]
fn an_event_queue_refuses_what_it_cannot_keep_and_passes_over_lines_it_cannot_read() {
    let (transport, queue) = memory_queue("events", held_policy());
    for (value, shape) in [
        (json!(["a list"]), "an array"),
        (json!(null), "null"),
        (json!(true), "a boolean"),
        (json!(7), "a number"),
        (json!("text"), "a string"),
    ] {
        let refusal = queue.push(value).expect_err("not an object");
        assert_eq!(
            refusal.to_string(),
            format!("events: a record on this queue is a JSON object, and this is {shape}")
        );
    }
    let name = queue.name().clone();
    for line in [
        &b"not json"[..],
        br#"[1]"#,
        br#"{"event": 7, "id": 0}"#,
        br#"{"event": "teleported", "id": 0}"#,
        br#"{"event": "queued"}"#,
        br#"{"event": "claimed", "id": 40}"#,
        br#"{"id": 0, "blocking": true, "text": "an older writer queued this"}"#,
        br#"{"event": null, "id": 1, "text": "and this, with a null event"}"#,
        br#"{"id": 0, "blocking": true, "abandoned": true, "text": "then abandoned it"}"#,
        br#"{"id": 0, "blocking": true, "text": "and took it back"}"#,
    ] {
        transport.append(&name, line).expect("appended");
    }
    let status = queue.status().expect("a status");
    assert_eq!(ids(&status.waiting), vec![0, 1], "{status:?}");
    assert!(status.abandoned.is_empty(), "{status:?}");
    assert_eq!(
        queue
            .push(json!({"text": "after them"}))
            .expect("queued")
            .id,
        Some(2),
        "an id the older writer allocated was handed out again"
    );

    let mut spec = QueueSpec::new("checked".parse().expect("a queue"), Policy::default());
    spec.schema = Some("nowhere.nothing@1".parse().expect("an id"));
    let unregistered = RawQueue::open(Arc::clone(&transport), spec, Arc::new(Registry::new()));
    let refusal = unregistered
        .push(json!({}))
        .expect_err("an unregistered schema");
    assert!(
        refusal
            .to_string()
            .contains("nowhere.nothing@1 is not a registered schema"),
        "{refusal}"
    );
    assert!(
        unregistered.held().expect("a read").is_none(),
        "a plain queue has a pending slot"
    );
}

/// A live question takes the slot from an abandoned one, which goes back among
/// the waiting records rather than being written over.
#[test]
fn a_live_question_takes_the_slot_and_the_abandoned_one_goes_back_to_waiting() {
    let (_, queue) = memory_queue("events", held_policy());
    let anyone = ConsumerName::default_consumer();
    queue
        .push(json!({"blocking": true, "text": "nobody waits on this now"}))
        .expect("queued");
    queue.claim(&anyone).expect("a claim").expect("claimed");
    queue.abandon(&[0]).expect("abandoned");
    queue
        .push(json!({"blocking": true, "text": "somebody waits on this"}))
        .expect("queued");
    let live = queue.claim(&anyone).expect("a claim").expect("claimed");
    assert_eq!(live.id, Some(1));
    let status = queue.status().expect("a status");
    assert_eq!(
        status.pending.as_ref().and_then(|held| held["id"].as_u64()),
        Some(1)
    );
    assert_eq!(
        ids(&status.waiting),
        vec![0],
        "the displaced question was written over"
    );
    assert_eq!(status.waiting[0]["abandoned"], json!(true));
}

/// The last id there is is never allocated, and a line claiming an id with no
/// successor folds to nothing.
#[test]
fn the_last_id_there_is_is_never_allocated() {
    let (transport, queue) = memory_queue("events", held_policy());
    transport
        .append(
            queue.name(),
            format!(r#"{{"event":"queued","id":{}}}"#, u64::MAX).as_bytes(),
        )
        .expect("appended");
    assert!(
        queue.waiting().expect("waiting").is_empty(),
        "an id with no successor was folded"
    );
    transport
        .append(
            queue.name(),
            format!(r#"{{"event":"queued","id":{}}}"#, u64::MAX - 1).as_bytes(),
        )
        .expect("appended");
    let refusal = queue
        .push(json!({"text": "one too many"}))
        .expect_err("no id left");
    assert_eq!(
        refusal.to_string(),
        format!(
            "events: the queue has no id left to allocate; the last one, {}, has already been queued",
            u64::MAX - 1
        )
    );
}

/// A projection that is not one — the wrong shapes, a malformed seal, a stamp
/// past the end of its log — is read as no document, and the log is folded whole.
#[test]
fn a_projection_that_is_not_one_or_outruns_its_log_is_read_as_no_document() {
    let document: DocumentName = "events.json".parse().expect("a document name");
    let (longer, longer_queue) = memory_queue("events", held_policy());
    for n in 0..3 {
        longer_queue.push(json!({"n": n})).expect("queued");
    }
    let outran = longer
        .document(longer_queue.name(), &document)
        .expect("a read")
        .expect("a projection");

    for bad in [
        outran.clone(),
        b"[]".to_vec(),
        br#"{"waiting": 5}"#.to_vec(),
        br#"{"waiting": [], "next_id": "x"}"#.to_vec(),
        br#"{"waiting": [], "accounted": "x"}"#.to_vec(),
        br#"{"waiting": [], "accounted": 9, "seal": "XYZ"}"#.to_vec(),
        br#"{"waiting": [], "accounted": 9, "seal": 5}"#.to_vec(),
    ] {
        let (transport, queue) = memory_queue("events", held_policy());
        queue.push(json!({"n": "only"})).expect("queued");
        transport
            .replace_document(queue.name(), &document, &bad)
            .expect("replaced");
        assert_eq!(
            ids(&queue.waiting().expect("waiting")),
            vec![0],
            "a projection {} was trusted",
            String::from_utf8_lossy(&bad)
        );
    }

    // An older writer's projection, with no stamp: taken at its word below its
    // counter, and every logged id at or past it folded in.
    let (transport, queue) = memory_queue("events", held_policy());
    transport
        .append(
            queue.name(),
            br#"{"id": 0, "text": "read by the older writer"}"#,
        )
        .expect("appended");
    transport
        .append(
            queue.name(),
            br#"{"id": 1, "text": "a write-back it lost"}"#,
        )
        .expect("appended");
    transport
        .replace_document(
            queue.name(),
            &document,
            br#"{"waiting": [], "pending": null, "next_id": 1}"#,
        )
        .expect("replaced");
    assert_eq!(ids(&queue.waiting().expect("waiting")), vec![1]);
}

/// What a queue, a typed queue, a subscription and a transport were opened with
/// is what they name.
#[test]
fn a_handle_names_what_it_was_opened_with() {
    use onemessagebus::conformance::Ticket;
    use onemessagebus::{Lifetime, Queue, Subscription};
    let dir = tempfile::tempdir().expect("a scratch directory");
    let local = LocalTransport::open(dir.path()).expect("opens");
    assert_eq!(local.dir(), dir.path());
    let transport: Arc<dyn Transport> = Arc::new(local);
    let typed: Queue<Ticket> = Queue::open(
        Arc::clone(&transport),
        QueueSpec::new("tickets".parse().expect("a queue"), held_policy()),
    )
    .expect("opens");
    let copy = typed.clone();
    assert!(format!("{copy:?}").contains("\"ticket\""), "{copy:?}");
    assert!(format!("{:?}", copy.raw()).contains("RawQueue"));
    assert!(Arc::ptr_eq(copy.raw().transport(), &transport));
    let listener = Subscription::open(
        copy.raw().clone(),
        "reader".parse().expect("a consumer"),
        Lifetime::Durable(Asker::new("dispatch-a", "the test").expect("an asker")),
    )
    .expect("listens");
    assert_eq!(listener.queue().name().as_str(), "tickets");
    assert_eq!(listener.consumer().as_str(), "reader");
    assert!(
        matches!(listener.lifetime(), Lifetime::Durable(asker) if asker.as_str() == "dispatch-a")
    );
    let read: Asker = serde_json::from_value(json!("dispatch-a")).expect("an asker reads");
    assert_eq!(read.as_str(), "dispatch-a");
    let blank = serde_json::from_value::<Asker>(json!("  ")).expect_err("a blank asker");
    assert!(blank.to_string().contains("blank value"), "{blank}");
}

/// Answering the pending slot releases what it holds and hands it back; an
/// empty slot releases nothing, and a plain queue has no slot to answer.
#[test]
fn answering_the_pending_slot_releases_the_held_question_once() {
    let (_, queue) = memory_queue("events", held_policy());
    let anyone = ConsumerName::default_consumer();
    queue
        .push(json!({"blocking": true, "text": "which base?"}))
        .expect("queued");
    queue.claim(&anyone).expect("a claim").expect("claimed");
    assert!(queue.status().expect("a status").pending.is_some());

    let released = queue
        .answer_pending()
        .expect("answered")
        .expect("the held question");
    assert_eq!(released["id"], json!(0));
    assert_eq!(released["text"], json!("which base?"));
    let status = queue.status().expect("a status");
    assert!(status.pending.is_none(), "{status:?}");
    assert!(status.waiting.is_empty(), "{status:?}");
    assert_eq!(
        queue.answer_pending().expect("answered"),
        None,
        "an empty slot releases nothing"
    );

    let (_, plain) = memory_queue("plain", Policy::default());
    let refusal = plain
        .answer_pending()
        .expect_err("a plain queue has no pending slot");
    assert!(
        refusal.to_string().contains("plain"),
        "the refusal names the queue: {refusal}"
    );
}
