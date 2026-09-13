//! The one journey every vocabulary is driven through.
//!
//! A vocabulary is proven by use, not by inspection: [`drive`] builds
//! envelopes over it, serializes and reads them back, filters on its reserved
//! keys, emits and merges streams, and registers and checks a message under a
//! schema id of its own namespace — through the public API and nothing else.
//! The core's own tests drive it over a vocabulary with no agent word in it;
//! the agent profile drives the same table over its vocabulary; and a
//! vocabulary of your own is proven the same way, differing only in the
//! [`Fixture`] handed in.
//!
//! Every check here panics on failure, the way a test does: this module is
//! test support, published so a profile crate can run the table without
//! copying it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::author::{Allowlist, Author, OpWord, NOT_GRANTED};
use crate::config::{AuthorConfig, Bus, Config, ConfigError, Layout, Layouts, NARROWED};
use crate::emit::Emitter;
use crate::envelope::{Envelope, Kind};
use crate::filter::{Filter, Matcher};
use crate::kinds::{TransportConfig, TransportKinds};
use crate::queue::{
    Asker, AskerRefused, FieldPath, Lifetime, Policy, Predicate, Queue, QueueError, QueueSpec,
    RawQueue, Subscription, Supersede,
};
use crate::read::{Merge, Reader, Reading};
use crate::redact::Redactor;
use crate::schema::{CheckError, Message, Registry, SchemaId};
use crate::transport::{Changed, ConsumerName, DocumentName, Position, QueueName, Transport};
use crate::vocabulary::Vocabulary;

/// What a vocabulary hands the journey: two sources, a label set, one that
/// differs from it on a reserved key, and the matcher fields that tell them
/// apart.
#[derive(Debug, Clone)]
pub struct Fixture<V: Vocabulary> {
    /// The namespace the vocabulary's schema ids live under.
    pub namespace: &'static str,
    /// Two distinct source words.
    pub sources: [V::Source; 2],
    /// A label set with at least one reserved key stamped.
    pub labels: V::Labels,
    /// A label set differing from [`labels`](Self::labels) on a reserved key.
    pub other_labels: V::Labels,
    /// Matcher fields that hold of [`labels`](Self::labels) and not of
    /// [`other_labels`](Self::other_labels).
    pub matching: V::Fields,
    /// The dimensions envelopes are stamped with.
    pub dimensions: V::Dimensions,
    /// A JSON document that is not [`labels`](Self::labels)'s shape — an
    /// unknown top-level envelope key, say — to prove refusal names the key.
    pub unknown_key: &'static str,
}

/// One message type registered under the vocabulary's namespace: a conforming
/// instance, and a document that violates its schema.
#[derive(Debug, Clone)]
pub struct Sample<M: Message> {
    /// An instance that conforms.
    pub conforming: M,
    /// A document that does not, and the JSON pointer the violation is at.
    pub violating: (Value, &'static str),
}

/// A sink the journey can read back: every line written, shared.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the captured sink")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn payload(n: u64) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("n".to_owned(), json!(n));
    payload
}

static SCRATCH: AtomicU64 = AtomicU64::new(0);

/// A directory of this journey's own, removed when dropped.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "onemessagebus-conformance-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Drive the whole table over `V`.
///
/// # Panics
///
/// On the first property of the vocabulary that does not hold.
pub fn drive<V: Vocabulary, M: Message + PartialEq + std::fmt::Debug>(
    fixture: &Fixture<V>,
    sample: &Sample<M>,
) {
    envelopes_round_trip(fixture);
    filters_read_the_reserved_keys(fixture);
    streams_emit_read_and_merge(fixture);
    messages_register_and_check(fixture, sample);
}

fn envelope<V: Vocabulary>(fixture: &Fixture<V>, which: usize, seq: u64) -> Envelope<V> {
    Envelope {
        v: V::write_version(&fixture.sources[which]),
        ts: format!("2026-09-13T00:00:0{seq}.000Z"),
        stream: format!("stream-{which}"),
        seq,
        source: fixture.sources[which].clone(),
        kind: Kind::from("thing-happened"),
        dimensions: fixture.dimensions.clone(),
        labels: fixture.labels.clone(),
        payload: payload(seq),
        artifacts: Vec::new(),
    }
}

/// An envelope over the vocabulary serializes and reads back equal, and an
/// unknown top-level key is refused by name.
pub fn envelopes_round_trip<V: Vocabulary>(fixture: &Fixture<V>) {
    let original = envelope(fixture, 0, 1);
    let line = serde_json::to_string(&original).expect("an envelope serializes");
    let read: Envelope<V> = serde_json::from_str(&line).expect("an envelope reads back");
    assert_eq!(read, original, "an envelope did not round-trip");
    assert_eq!(
        serde_json::to_string(&read).expect("serializes"),
        line,
        "re-serializing changed the bytes"
    );

    let mut document: Value = serde_json::from_str(&line).expect("JSON");
    document[fixture.unknown_key] = json!("stray");
    let refusal = serde_json::from_value::<Envelope<V>>(document)
        .expect_err("an unknown top-level key must be refused");
    assert!(
        refusal.to_string().contains(fixture.unknown_key),
        "the refusal does not name the key: {refusal}"
    );

    let mut document: Value = serde_json::from_str(&line).expect("JSON");
    document["seq"] = json!(1.5);
    serde_json::from_value::<Envelope<V>>(document).expect_err("a non-integer seq must be refused");

    let mut document: Value = serde_json::from_str(&line).expect("JSON");
    document.as_object_mut().expect("an object").remove("ts");
    let refusal = serde_json::from_value::<Envelope<V>>(document)
        .expect_err("a missing required field must be refused");
    assert!(
        refusal.to_string().contains("ts"),
        "the refusal does not name the field: {refusal}"
    );
}

/// A filter naming a reserved key admits the envelope stamped with it and not
/// the one stamped otherwise; `exclude` wins; globs and sources conjoin.
pub fn filters_read_the_reserved_keys<V: Vocabulary>(fixture: &Fixture<V>) {
    let stamped = envelope(fixture, 0, 1);
    let other = Envelope {
        labels: fixture.other_labels.clone(),
        ..envelope(fixture, 1, 2)
    };

    let by_label = Filter::<V> {
        include: vec![Matcher::new().fields(fixture.matching.clone())],
        exclude: Vec::new(),
    };
    by_label
        .validate()
        .expect("a matcher naming a reserved key is valid");
    assert!(by_label.matches(&stamped), "the reserved key did not match");
    assert!(
        !by_label.matches(&other),
        "a different reserved value matched"
    );

    let by_source = Filter::<V> {
        include: vec![Matcher::new().source(fixture.sources[1].clone())],
        exclude: Vec::new(),
    };
    assert!(!by_source.matches(&stamped));
    assert!(by_source.matches(&other));

    let excluded = Filter::<V> {
        include: vec![Matcher::new().kind("thing-*")],
        exclude: vec![Matcher::new().fields(fixture.matching.clone())],
    };
    assert!(!excluded.matches(&stamped), "exclude did not win");
    assert!(excluded.matches(&other));

    assert!(Filter::<V>::everything().matches(&stamped));

    let empty = Filter::<V> {
        include: vec![Matcher::new()],
        exclude: Vec::new(),
    };
    let refusal = empty
        .validate()
        .expect_err("a field-less matcher is refused");
    assert!(refusal.starts_with("include[0]"), "{refusal}");

    let spec = serde_json::to_string(&by_label).expect("a filter serializes");
    let parsed = Filter::<V>::parse(&spec).expect("a filter reads back");
    assert_eq!(parsed, by_label);
}

/// Two emitters write two streams; a reader reads each back with positions;
/// a merge folds them in `(ts, stream, seq)` order.
pub fn streams_emit_read_and_merge<V: Vocabulary>(fixture: &Fixture<V>) {
    let scratch = Scratch::new();
    let mut paths = Vec::new();
    for which in 0..2 {
        let captured = Captured::default();
        let emitter = Emitter::<V>::new(
            format!("stream-{which}"),
            fixture.sources[which].clone(),
            Box::new(captured.clone()),
        )
        .with_redactor(Redactor::new())
        .with_labels(fixture.labels.clone())
        .with_dimensions(fixture.dimensions.clone());
        for n in 1..=3 {
            let written = emitter.emit("thing-happened", payload(n));
            assert_eq!(written.seq, n, "seq is not monotonic from 1");
            assert_eq!(written.v, V::write_version(&fixture.sources[which]));
            assert_eq!(written.labels, fixture.labels);
        }
        let path = scratch.0.join(format!("stream-{which}.ndjson"));
        std::fs::write(&path, captured.0.lock().expect("the sink").as_slice())
            .expect("the stream is written");
        paths.push(path);
    }

    let readings: Vec<Reading<V>> = Reader::open(&paths[0]).expect("opens").collect();
    assert_eq!(readings.len(), 3);
    let mut resume = 0;
    for (index, reading) in readings.iter().enumerate() {
        let Reading::Record(record) = reading else {
            panic!("a whole stream read as {reading:?}");
        };
        assert_eq!(record.envelope.seq, index as u64 + 1);
        assert!(record.position > resume);
        resume = record.position;
    }
    let after_first = match &readings[0] {
        Reading::Record(record) => record.position,
        other => panic!("{other:?}"),
    };
    let rest: Vec<Reading<V>> = Reader::open_at(&paths[0], after_first)
        .expect("resumes")
        .collect();
    assert_eq!(rest.len(), 2, "a resumed reader re-read the first record");

    let merged = Merge::<V>::open(&paths).expect("merges");
    assert!(merged.torn().is_empty() && merged.refused().is_empty());
    let keys: Vec<(String, String, u64)> = merged
        .records()
        .iter()
        .map(|envelope| {
            let (ts, stream, seq) = envelope.order_key();
            (ts.to_owned(), stream.to_owned(), seq)
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "the merge is not in (ts, stream, seq) order");
    assert_eq!(keys.len(), 6);
}

/// A message type of the vocabulary's namespace registers under its id, its
/// conforming instance passes, and a violating document is refused naming the
/// id and the pointer.
pub fn messages_register_and_check<V: Vocabulary, M: Message + PartialEq + std::fmt::Debug>(
    fixture: &Fixture<V>,
    sample: &Sample<M>,
) {
    assert_eq!(
        M::SCHEMA.namespace(),
        fixture.namespace,
        "the sample message is not in the vocabulary's namespace"
    );
    let mut registry = Registry::new();
    registry.register::<M>().expect("registers");
    assert_eq!(
        registry.read_set(&M::SCHEMA.family()),
        vec![M::SCHEMA.version()]
    );
    assert_eq!(
        registry.writes(&M::SCHEMA.family()),
        Some(M::SCHEMA.version())
    );
    registry
        .register::<M>()
        .expect("registering the same document twice is fine");

    let conforming = serde_json::to_value(&sample.conforming).expect("serializes");
    registry
        .check(&M::SCHEMA, &conforming)
        .expect("a conforming instance passes");
    let read: M = serde_json::from_value(conforming).expect("reads back");
    assert_eq!(read, sample.conforming);

    let (document, pointer) = &sample.violating;
    match registry.check(&M::SCHEMA, document) {
        Err(CheckError::Violation(violation)) => {
            assert_eq!(violation.id, M::SCHEMA);
            assert_eq!(violation.pointer, *pointer, "{violation}");
        }
        other => panic!("a violating document was not refused as one: {other:?}"),
    }

    let elsewhere: SchemaId = format!("{}.{}@{}", fixture.namespace, "nothing", 1)
        .parse()
        .expect("an id");
    match registry.check(&elsewhere, &json!({})) {
        Err(CheckError::Registry(refusal)) => {
            assert!(refusal.to_string().contains(&elsewhere.to_string()));
        }
        other => panic!("an unregistered id was not refused: {other:?}"),
    }
}

/// Hands a row a fresh, empty transport each time it is called.
pub type Fresh<'a> = &'a dyn Fn() -> Arc<dyn Transport>;

/// One row of the queue table.
pub type Row = fn(Fresh<'_>);

/// Every row of the queue, subscription and author table, by name: what a
/// transport is proven by. A suite runs each over its transport as a test of
/// its own, and holds that it ran every one.
pub const QUEUE_TABLE: &[(&str, Row)] = &[
    (
        "blocking_first_claims_a_blocking_record_before_an_older_one",
        blocking_first_claims_a_blocking_record_before_an_older_one,
    ),
    (
        "hold_pending_keeps_one_claimed_blocking_record_pending_until_answered",
        hold_pending_keeps_one_claimed_blocking_record_pending_until_answered,
    ),
    (
        "a_newer_record_supersedes_a_waiting_one_with_its_key_and_never_another",
        a_newer_record_supersedes_a_waiting_one_with_its_key_and_never_another,
    ),
    (
        "a_claim_is_recorded_so_a_crashed_claimants_record_is_not_handed_out_twice",
        a_claim_is_recorded_so_a_crashed_claimants_record_is_not_handed_out_twice,
    ),
    (
        "claimants_at_once_receive_distinct_records",
        claimants_at_once_receive_distinct_records,
    ),
    (
        "abandon_marks_and_a_later_listener_of_the_same_asker_takes_back",
        abandon_marks_and_a_later_listener_of_the_same_asker_takes_back,
    ),
    (
        "a_different_asker_or_a_session_takes_nothing_back",
        a_different_asker_or_a_session_takes_nothing_back,
    ),
    (
        "a_blank_or_non_unicode_asker_is_refused",
        a_blank_or_non_unicode_asker_is_refused,
    ),
    (
        "a_stamped_projection_that_does_not_seal_is_read_as_no_document",
        a_stamped_projection_that_does_not_seal_is_read_as_no_document,
    ),
    (
        "a_plain_queue_claims_through_each_consumers_cursor",
        a_plain_queue_claims_through_each_consumers_cursor,
    ),
    (
        "a_record_its_schema_refuses_is_not_appended",
        a_record_its_schema_refuses_is_not_appended,
    ),
    (
        "a_numbered_queue_numbers_each_record_by_the_count_before_it",
        a_numbered_queue_numbers_each_record_by_the_count_before_it,
    ),
    (
        "a_fingerprint_moves_when_the_queue_does_and_a_wait_sees_it",
        a_fingerprint_moves_when_the_queue_does_and_a_wait_sees_it,
    ),
    (
        "every_method_works_through_the_transport_a_section_lends",
        every_method_works_through_the_transport_a_section_lends,
    ),
    (
        "an_allowlist_refuses_by_omission_naming_the_author_the_op_and_the_reason",
        an_allowlist_refuses_by_omission_naming_the_author_the_op_and_the_reason,
    ),
    (
        "a_configuration_narrows_an_author_and_is_refused_widening_one",
        a_configuration_narrows_an_author_and_is_refused_widening_one,
    ),
];

/// Run every row of [`QUEUE_TABLE`] over `fresh`'s transports.
///
/// # Panics
///
/// On the first row that does not hold.
pub fn queue_table(fresh: Fresh<'_>) {
    for (_, row) in QUEUE_TABLE {
        row(fresh);
    }
}

/// The table's record: a ticket on a desk's queue. The queue owns its `id`,
/// `blocking`, `abandoned` and `asker`; `kind` and `text` are the desk's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ticket {
    /// Allocated by the queue.
    pub id: u64,
    /// What the ticket is about; a `heartbeat` supersedes a waiting one.
    pub kind: String,
    /// Its text.
    pub text: String,
    /// Whether whoever raised it waits on its answer.
    #[serde(default)]
    pub blocking: bool,
    /// Whether nobody is listening for its answer any more.
    #[serde(default, skip_serializing_if = "is_false")]
    pub abandoned: bool,
    /// Who raised it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Message for Ticket {
    const SCHEMA: SchemaId = SchemaId::literal("conformance", "ticket", 1);
}

fn queue_name(text: &str) -> QueueName {
    text.parse().expect("a queue name")
}

fn field(text: &str) -> FieldPath {
    text.parse().expect("a field path")
}

fn anyone() -> ConsumerName {
    ConsumerName::default_consumer()
}

fn asker(name: &str) -> Asker {
    Asker::new(name, "the table").expect("an asker")
}

fn ticket(kind: &str, text: &str, blocking: bool) -> Ticket {
    Ticket {
        id: 0,
        kind: kind.parse().expect("a transport kind"),
        text: text.to_owned(),
        blocking,
        abandoned: false,
        asker: None,
    }
}

/// The desk's event queue: held pending, blocking first, a waiting heartbeat
/// superseded by a newer one, and a projection.
fn tickets_spec() -> QueueSpec {
    QueueSpec::new(
        queue_name("tickets"),
        Policy {
            supersede_on: Some(Supersede {
                key: field("kind"),
                when: Some(Predicate::equals(field("kind"), "heartbeat")),
            }),
            hold_pending: true,
            blocking_first: true,
            projection: Some("tickets.json".parse().expect("a document name")),
            ..Policy::default()
        },
    )
}

fn tickets(transport: &Arc<dyn Transport>) -> Queue<Ticket> {
    Queue::open(Arc::clone(transport), tickets_spec()).expect("the tickets queue opens")
}

fn texts(records: &[Ticket]) -> Vec<&str> {
    records.iter().map(|record| record.text.as_str()).collect()
}

/// A claim hands out a blocking record before an older non-blocking one, and
/// arrival order within each.
pub fn blocking_first_claims_a_blocking_record_before_an_older_one(fresh: Fresh<'_>) {
    let transport = fresh();
    let queue = tickets(&transport);
    let narration = queue
        .push(&ticket("finding", "narration", false))
        .expect("queued");
    let question = queue
        .push(&ticket("question", "a question", true))
        .expect("queued");
    let later = queue
        .push(&ticket("question", "a later question", true))
        .expect("queued");
    assert_eq!(
        (narration.id, question.id, later.id),
        (Some(0), Some(1), Some(2)),
        "ids are not allocated one past the highest queued"
    );
    let order: Vec<String> = std::iter::from_fn(|| {
        queue
            .claim(&anyone())
            .expect("a claim")
            .map(|claimed| claimed.record.text)
    })
    .collect();
    assert_eq!(
        order,
        vec!["a question", "a later question", "narration"],
        "blocking records are not claimed first, in arrival order"
    );
}

/// A claimed blocking record is pending until answered, one at a time; reading
/// narration leaves it standing; an answer releases it.
pub fn hold_pending_keeps_one_claimed_blocking_record_pending_until_answered(fresh: Fresh<'_>) {
    let transport = fresh();
    let queue = tickets(&transport);
    queue
        .push(&ticket("question", "first", true))
        .expect("queued");
    queue
        .push(&ticket("question", "second", true))
        .expect("queued");
    queue
        .push(&ticket("finding", "narration", false))
        .expect("queued");

    let first = queue.claim(&anyone()).expect("a claim").expect("a record");
    assert_eq!(first.record.text, "first");
    let pending = queue.pending(&anyone()).expect("a read").expect("pending");
    assert_eq!(pending.record.text, "first");
    assert_eq!(
        pending.position, first.position,
        "the pending record names another claim"
    );

    let second = queue.claim(&anyone()).expect("a claim").expect("a record");
    assert_eq!(second.record.text, "second");
    let status = queue.raw().status().expect("a status");
    assert_eq!(
        status.pending.as_ref().and_then(|held| held.get("text")),
        Some(&json!("second")),
        "the slot does not hold the one claimed last: {status:?}"
    );
    assert_eq!(
        texts(&queue.waiting().expect("waiting")),
        vec!["narration"],
        "more than one record is pending"
    );

    let narration = queue.claim(&anyone()).expect("a claim").expect("a record");
    assert_eq!(narration.record.text, "narration");
    assert_eq!(
        queue
            .pending(&anyone())
            .expect("a read")
            .map(|held| held.record.text),
        Some("second".to_owned()),
        "reading narration answered the pending record"
    );

    assert!(
        !queue
            .answer(&first, &Position::from_token(0))
            .expect("answers"),
        "a record the slot no longer holds was answered"
    );
    assert!(queue
        .answer(&second, &Position::from_token(0))
        .expect("answers"));
    assert_eq!(queue.pending(&anyone()).expect("a read"), None);
    assert!(
        !queue
            .answer(&second, &Position::from_token(0))
            .expect("answers"),
        "a record was answered twice"
    );
}

/// A newer heartbeat replaces a waiting heartbeat, and never a finding; a
/// claimed heartbeat is not replaced.
pub fn a_newer_record_supersedes_a_waiting_one_with_its_key_and_never_another(fresh: Fresh<'_>) {
    let transport = fresh();
    let queue = tickets(&transport);
    queue
        .push(&ticket("heartbeat", "first beat", false))
        .expect("queued");
    queue
        .push(&ticket("finding", "a finding", false))
        .expect("queued");
    queue
        .push(&ticket("heartbeat", "second beat", false))
        .expect("queued");
    assert_eq!(
        texts(&queue.waiting().expect("waiting")),
        vec!["a finding", "second beat"],
        "a waiting heartbeat was not superseded, or a finding was"
    );
    queue
        .push(&ticket("finding", "another finding", false))
        .expect("queued");
    assert_eq!(
        texts(&queue.waiting().expect("waiting")),
        vec!["a finding", "second beat", "another finding"],
        "a finding superseded a finding"
    );
    // Claimed in arrival order: the finding, then the heartbeat.
    queue.claim(&anyone()).expect("a claim").expect("a record");
    let beat = queue.claim(&anyone()).expect("a claim").expect("a record");
    assert_eq!(beat.record.text, "second beat");
    queue
        .push(&ticket("heartbeat", "third beat", false))
        .expect("queued");
    assert_eq!(
        texts(&queue.waiting().expect("waiting")),
        vec!["another finding", "third beat"]
    );
    assert_eq!(queue.unread_count().expect("a count"), 2);
}

/// A claimant that takes a record and dies before answering leaves it claimed:
/// the next claimant, through another handle, is handed something else.
pub fn a_claim_is_recorded_so_a_crashed_claimants_record_is_not_handed_out_twice(fresh: Fresh<'_>) {
    let transport = fresh();
    {
        let queue = tickets(&transport);
        queue
            .push(&ticket("question", "taken, then the claimant died", true))
            .expect("queued");
        queue
            .push(&ticket("finding", "still waiting", false))
            .expect("queued");
        let crashed = queue.claim(&anyone()).expect("a claim").expect("a record");
        assert_eq!(crashed.record.text, "taken, then the claimant died");
    }
    let survivor = tickets(&transport);
    let next = survivor
        .claim(&anyone())
        .expect("a claim")
        .expect("a record");
    assert_eq!(
        next.record.text, "still waiting",
        "a claimed record was handed out again"
    );
    assert_eq!(
        survivor
            .pending(&anyone())
            .expect("a read")
            .map(|held| held.record.text),
        Some("taken, then the claimant died".to_owned())
    );
    assert!(survivor.claim(&anyone()).expect("a claim").is_none());

    let reader: ConsumerName = "reader".parse().expect("a consumer");
    let plain_spec = QueueSpec::new(queue_name("notes"), Policy::default());
    let registry = Arc::new(Registry::new());
    {
        let notes = RawQueue::open(
            Arc::clone(&transport),
            plain_spec.clone(),
            Arc::clone(&registry),
        );
        notes.push(json!({"n": 0})).expect("appended");
        notes.push(json!({"n": 1})).expect("appended");
        let crashed = notes.claim(&reader).expect("a claim").expect("a record");
        assert_eq!(crashed.record, json!({"n": 0}));
    }
    let notes = RawQueue::open(Arc::clone(&transport), plain_spec, registry);
    assert_eq!(
        notes
            .claim(&reader)
            .expect("a claim")
            .map(|claimed| claimed.record),
        Some(json!({"n": 1})),
        "a plain queue handed a claimed record to the same consumer again"
    );
    assert_eq!(
        notes
            .claim(&anyone())
            .expect("a claim")
            .map(|claimed| claimed.record),
        Some(json!({"n": 0})),
        "another consumer does not read through a cursor of its own"
    );
}

/// Several claimants claiming from one queue at once are each handed a
/// different record, and between them every record.
pub fn claimants_at_once_receive_distinct_records(fresh: Fresh<'_>) {
    const RECORDS: u64 = 24;
    const CLAIMANTS: usize = 4;
    let transport = fresh();
    let queue = tickets(&transport);
    let notes = RawQueue::open(
        Arc::clone(&transport),
        QueueSpec::new(queue_name("notes"), Policy::default()),
        Arc::new(Registry::new()),
    );
    for n in 0..RECORDS {
        queue
            .push(&ticket("finding", &format!("ticket {n}"), false))
            .expect("queued");
        notes.push(json!({ "n": n })).expect("appended");
    }
    let claimed: Vec<(Vec<u64>, Vec<u64>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..CLAIMANTS)
            .map(|_| {
                let queue = tickets(&transport);
                let notes = notes.clone();
                scope.spawn(move || {
                    let mut tickets_taken = Vec::new();
                    while let Some(claimed) = queue.claim(&anyone()).expect("a claim") {
                        tickets_taken.push(claimed.record.id);
                    }
                    let mut notes_taken = Vec::new();
                    while let Some(claimed) = notes.claim(&anyone()).expect("a claim") {
                        notes_taken.push(claimed.record["n"].as_u64().expect("a number"));
                    }
                    (tickets_taken, notes_taken)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("a claimant finishes"))
            .collect()
    });
    for (what, mut ids) in [
        (
            "tickets",
            claimed
                .iter()
                .flat_map(|(t, _)| t.clone())
                .collect::<Vec<u64>>(),
        ),
        (
            "notes",
            claimed
                .iter()
                .flat_map(|(_, n)| n.clone())
                .collect::<Vec<u64>>(),
        ),
    ] {
        ids.sort_unstable();
        assert_eq!(
            ids,
            (0..RECORDS).collect::<Vec<u64>>(),
            "the {what} claimed at once were not each handed out exactly once"
        );
    }
}

/// A durable listener that ends marks what it raised and claimed abandoned —
/// kept, uncounted, still readable — and a later listener of the same asker takes
/// those back.
pub fn abandon_marks_and_a_later_listener_of_the_same_asker_takes_back(fresh: Fresh<'_>) {
    let transport = fresh();
    let raw = tickets(&transport).raw().clone();
    let first = Subscription::open(
        raw.clone(),
        anyone(),
        Lifetime::Durable(asker("listener-a")),
    )
    .expect("a listener");
    let question = first
        .push(json!({"kind": "question", "text": "is the base still right?", "blocking": true}))
        .expect("raised");
    assert_eq!(question.record["asker"], json!("listener-a"));
    first
        .push(json!({"kind": "finding", "text": "unread narration", "blocking": false}))
        .expect("raised");
    let claimed = first.claim().expect("a claim").expect("a record");
    assert_eq!(claimed.record["text"], json!("is the base still right?"));

    let marked = first.abandon().expect("abandoned");
    assert_eq!(
        marked.len(),
        2,
        "not everything the listener raised was marked: {marked:?}"
    );
    assert!(marked
        .iter()
        .all(|record| record["abandoned"] == json!(true)));
    let status = raw.status().expect("a status");
    assert_eq!(
        status.unread, 0,
        "an abandoned record still counts: {status:?}"
    );
    assert_eq!(status.abandoned.len(), 2, "{status:?}");
    assert_eq!(
        status.pending.as_ref().map(|held| held["text"].clone()),
        Some(json!("is the base still right?")),
        "the slot gave up what it holds"
    );
    assert!(
        raw.pending(&anyone()).expect("a read").is_none(),
        "an abandoned record reads as pending"
    );
    assert!(
        first.abandon().expect("abandoned").is_empty(),
        "a record was abandoned twice"
    );
    // Still readable, and still claimable: the unread narration is handed out.
    assert_eq!(
        raw.claim(&anyone())
            .expect("a claim")
            .map(|claimed| claimed.record["text"].clone()),
        Some(json!("unread narration"))
    );

    let second = Subscription::open(
        raw.clone(),
        anyone(),
        Lifetime::Durable(asker("listener-a")),
    )
    .expect("a later listener");
    let status = raw.status().expect("a status");
    assert!(
        status.abandoned.is_empty(),
        "the same asker took nothing back: {status:?}"
    );
    assert_eq!(
        raw.pending(&anyone())
            .expect("a read")
            .map(|held| held.record["text"].clone()),
        Some(json!("is the base still right?")),
        "the question is not pending again"
    );
    second.abandon().expect("abandoned");
    assert_eq!(raw.status().expect("a status").abandoned.len(), 1);
}

/// A listener of a different asker takes nothing back, a session listener takes
/// nothing back, and nothing takes back what a session raised.
pub fn a_different_asker_or_a_session_takes_nothing_back(fresh: Fresh<'_>) {
    let transport = fresh();
    let raw = tickets(&transport).raw().clone();
    let durable = Subscription::open(
        raw.clone(),
        anyone(),
        Lifetime::Durable(asker("listener-a")),
    )
    .expect("a listener");
    durable
        .push(json!({"kind": "question", "text": "asked by a", "blocking": true}))
        .expect("raised");
    durable.abandon().expect("abandoned");

    let other = Subscription::open(
        raw.clone(),
        anyone(),
        Lifetime::Durable(asker("listener-b")),
    )
    .expect("another asker");
    assert_eq!(
        raw.status().expect("a status").abandoned.len(),
        1,
        "another asker took a's question"
    );
    let session = Subscription::open(raw.clone(), anyone(), Lifetime::Session).expect("a session");
    assert_eq!(
        raw.status().expect("a status").abandoned.len(),
        1,
        "a session took a's question"
    );

    let raised = session
        .push(json!({"kind": "question", "text": "asked by a session", "blocking": true}))
        .expect("raised");
    assert!(
        raised.record.get("asker").is_none(),
        "a session stamped an asker"
    );
    session.abandon().expect("abandoned");
    assert_eq!(raw.status().expect("a status").abandoned.len(), 2);
    Subscription::open(
        raw.clone(),
        anyone(),
        Lifetime::Durable(asker("listener-a")),
    )
    .expect("a's next listener");
    let left: Vec<Value> = raw
        .status()
        .expect("a status")
        .abandoned
        .into_iter()
        .map(|record| record["text"].clone())
        .collect();
    assert_eq!(
        left,
        vec![json!("asked by a session")],
        "a durable listener took back what a session raised"
    );
    drop(other);
}

/// A blank asker, and one that is not Unicode, name nobody and are refused
/// saying where the value came from.
pub fn a_blank_or_non_unicode_asker_is_refused(fresh: Fresh<'_>) {
    let _ = fresh;
    for blank in ["", "   ", "\t\n"] {
        let refusal = Asker::new(blank, "ASKER_SOURCE").expect_err("a blank asker");
        assert!(matches!(refusal, AskerRefused::Blank { .. }), "{refusal:?}");
        assert!(
            refusal
                .to_string()
                .starts_with("ASKER_SOURCE is set to a blank value"),
            "{refusal}"
        );
    }
    assert!(Asker::named(std::ffi::OsStr::new("dispatch-a"), "ASKER_SOURCE").is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let refusal = Asker::named(
            std::ffi::OsStr::from_bytes(b"dispatch-\xff"),
            "ASKER_SOURCE",
        )
        .expect_err("an asker that is not Unicode");
        assert!(
            matches!(refusal, AskerRefused::NotUnicode { .. }),
            "{refusal:?}"
        );
        assert!(
            refusal
                .to_string()
                .starts_with("ASKER_SOURCE is set to a value this host cannot read as text"),
            "{refusal}"
        );
    }
}

/// A projection whose stamp still matches the log but whose claims moved under
/// its seal is read as no document: the whole log is folded, and the repaired
/// projection is written back.
pub fn a_stamped_projection_that_does_not_seal_is_read_as_no_document(fresh: Fresh<'_>) {
    type Tamper = fn(&mut Value);
    let tamperings: [(&str, Tamper); 3] = [
        ("its waiting records emptied", |document| {
            document["waiting"] = json!([]);
        }),
        ("its counter reset", |document| {
            document["next_id"] = json!(0);
        }),
        ("its seal removed", |document| {
            document["waiting"] = json!([]);
            document.as_object_mut().expect("an object").remove("seal");
        }),
    ];
    for (how, tamper) in tamperings {
        let transport = fresh();
        let queue = tickets(&transport);
        queue
            .push(&ticket("question", "logged", true))
            .expect("queued");
        queue
            .push(&ticket("finding", "also logged", false))
            .expect("queued");
        let name = queue.raw().spec().name.clone();
        let document: DocumentName = "tickets.json".parse().expect("a document name");
        let read_back = |transport: &Arc<dyn Transport>| -> Value {
            serde_json::from_slice(
                &transport
                    .document(&name, &document)
                    .expect("a read")
                    .expect("the projection was written"),
            )
            .expect("the projection is JSON")
        };
        let original = read_back(&transport);
        assert!(
            original["accounted"].is_u64() && original["seal"].is_string(),
            "the projection is not stamped and sealed: {original}"
        );

        let mut tampered = original.clone();
        tamper(&mut tampered);
        assert_eq!(
            tampered["accounted"], original["accounted"],
            "the stamp moved"
        );
        transport
            .replace_document(
                &name,
                &document,
                serde_json::to_string_pretty(&tampered)
                    .expect("JSON")
                    .as_bytes(),
            )
            .expect("the projection is replaced");
        assert_eq!(
            texts(&queue.waiting().expect("waiting")),
            vec!["logged", "also logged"],
            "a projection with {how} under an intact stamp was trusted"
        );
        assert_eq!(
            read_back(&transport),
            original,
            "the fold over a projection with {how} was not written back"
        );

        tamper(&mut tampered);
        transport
            .replace_document(
                &name,
                &document,
                serde_json::to_string_pretty(&tampered)
                    .expect("JSON")
                    .as_bytes(),
            )
            .expect("the projection is replaced again");
        let after = queue
            .push(&ticket("finding", "after the repair", false))
            .expect("queued");
        assert_eq!(
            after.id,
            Some(2),
            "a push over a projection with {how} handed out an id the log had allocated"
        );
    }
}

/// A plain queue is read through each consumer's cursor: a claim takes the
/// first record after it that `claims` admits and moves the cursor just past it,
/// and a record passed over is not handed out afterwards.
pub fn a_plain_queue_claims_through_each_consumers_cursor(fresh: Fresh<'_>) {
    let transport = fresh();
    let mut spec = QueueSpec::new(queue_name("letters"), Policy::default());
    spec.claims = Some(Predicate::Present {
        field: field("verdict"),
        present: true,
    });
    let reader: ConsumerName = "reader".parse().expect("a consumer");
    spec.consumers = vec![anyone(), reader.clone()];
    let letters = RawQueue::open(Arc::clone(&transport), spec, Arc::new(Registry::new()));
    letters.push(json!({"edits": ["drop"]})).expect("appended");
    letters.push(json!({"verdict": "go on"})).expect("appended");
    letters.push(json!({"edits": ["add"]})).expect("appended");
    letters.push(json!({"verdict": "stop"})).expect("appended");
    assert_eq!(letters.unread_count().expect("a count"), 2);

    let first = letters
        .claim(&anyone())
        .expect("a claim")
        .expect("a record");
    assert_eq!(
        first.record,
        json!({"verdict": "go on"}),
        "a record claims do not admit was handed out"
    );
    let status = letters.status().expect("a status");
    assert_eq!(
        status.cursors[&anyone()],
        Some(first.position),
        "the cursor is not just past the claim"
    );
    assert_eq!(status.cursors[&reader], None);
    assert_eq!(status.unread, 1);
    assert_eq!(
        letters
            .claim(&anyone())
            .expect("a claim")
            .map(|claimed| claimed.record),
        Some(json!({"verdict": "stop"}))
    );
    assert!(letters.claim(&anyone()).expect("a claim").is_none());
    assert_eq!(
        letters
            .claim(&reader)
            .expect("a claim")
            .map(|claimed| claimed.record),
        Some(json!({"verdict": "go on"})),
        "consumers share a cursor"
    );
    assert!(matches!(
        letters.abandon(&[0]),
        Err(QueueError::NotAnEventQueue { .. })
    ));
    assert_eq!(letters.status().expect("a status").records, 4);
}

/// A record pushed onto a typed queue is validated against its schema before
/// it is appended, and one that does not conform leaves the log as it was.
pub fn a_record_its_schema_refuses_is_not_appended(fresh: Fresh<'_>) {
    let transport = fresh();
    let queue = tickets(&transport);
    queue
        .push(&ticket("finding", "conforms", false))
        .expect("queued");
    let before = queue.raw().log(None).expect("the log").len();
    let refusal = queue
        .raw()
        .push(json!({"kind": 7, "text": "a kind that is not a string"}))
        .expect_err("a non-conforming record is refused");
    assert!(
        matches!(
            refusal,
            QueueError::Shape { .. } | QueueError::Violation { .. }
        ),
        "{refusal:?}"
    );
    assert!(
        refusal.to_string().contains("tickets"),
        "the refusal does not name the queue: {refusal}"
    );
    assert_eq!(
        queue.raw().log(None).expect("the log").len(),
        before,
        "a refused record was appended"
    );

    let mut registry = Registry::new();
    registry.register::<Ticket>().expect("registers");
    let mut spec = QueueSpec::new(queue_name("checked"), Policy::default());
    spec.schema = Some(Ticket::SCHEMA);
    let checked = RawQueue::open(Arc::clone(&transport), spec, Arc::new(registry));
    let refusal = checked
        .push(json!({"id": 0, "kind": "finding"}))
        .expect_err("a record missing `text` is refused");
    match &refusal {
        QueueError::Violation { violation, .. } => assert_eq!(violation.id, Ticket::SCHEMA),
        other => panic!("not refused as a violation: {other:?}"),
    }
    assert!(checked.log(None).expect("the log").is_empty());
}

/// A numbered plain queue stamps each record's `id` with the number of records
/// before it, in place where the record has one and first where it has none.
pub fn a_numbered_queue_numbers_each_record_by_the_count_before_it(fresh: Fresh<'_>) {
    let transport = fresh();
    let mut spec = QueueSpec::new(queue_name("orders"), Policy::default());
    spec.numbered = true;
    let orders = RawQueue::open(Arc::clone(&transport), spec, Arc::new(Registry::new()));
    let first = orders
        .push(json!({"id": 99, "what": "first"}))
        .expect("appended");
    let second = orders.push(json!({"what": "second"})).expect("appended");
    assert_eq!((first.id, second.id), (Some(0), Some(1)));
    assert_eq!(
        serde_json::to_string(&second.record).expect("JSON"),
        r#"{"id":1,"what":"second"}"#,
        "an id was not put first"
    );
    let log: Vec<Value> = orders
        .log(None)
        .expect("the log")
        .into_iter()
        .map(|(record, _)| record)
        .collect();
    assert_eq!(
        log,
        vec![
            json!({"id": 0, "what": "first"}),
            json!({"id": 1, "what": "second"})
        ]
    );
}

/// A queue's fingerprint moves when the queue does, stays put when it does not,
/// and a bounded wait sees a change made while it waits.
pub fn a_fingerprint_moves_when_the_queue_does_and_a_wait_sees_it(fresh: Fresh<'_>) {
    let transport = fresh();
    let queue = tickets(&transport);
    let empty = queue.raw().fingerprint().expect("a fingerprint");
    assert_eq!(
        queue.raw().fingerprint().expect("a fingerprint"),
        empty,
        "a fingerprint moved by itself"
    );
    match queue
        .raw()
        .wait_for_change(&empty, std::time::Duration::from_millis(30))
        .expect("a wait")
    {
        Changed::Unchanged(now) => assert_eq!(now, empty),
        Changed::Moved(now) => panic!("an unchanged queue reported moving to {now:?}"),
    }
    queue
        .push(&ticket("finding", "moves it", false))
        .expect("queued");
    let pushed = queue.raw().fingerprint().expect("a fingerprint");
    assert_ne!(pushed, empty, "a push did not move the fingerprint");

    let writer = tickets(&transport);
    let waited = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| {
            queue
                .raw()
                .wait_for_change(&pushed, std::time::Duration::from_secs(20))
                .expect("a wait")
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        writer.claim(&anyone()).expect("a claim").expect("a record");
        waiting.join().expect("the wait returns")
    });
    assert!(
        matches!(waited, Changed::Moved(_)),
        "a claim made during the wait was not seen: {waited:?}"
    );
}

/// The desk: a layout with no agent word in it, whose `orders` queue checks
/// each order's author against its allowlist.
#[derive(Debug)]
struct Desk;

/// The desk's operation vocabulary.
const DESK_OPS: &[&str] = &["open", "close", "note"];

fn op(word: &str) -> OpWord {
    OpWord(word.to_owned())
}

impl Layout for Desk {
    fn name(&self) -> &str {
        "desk"
    }

    fn queues(&self) -> Vec<QueueSpec> {
        let mut orders = QueueSpec::new(queue_name("orders"), Policy::default());
        orders.numbered = true;
        vec![tickets_spec(), orders]
    }

    fn allowlist(&self) -> Allowlist<OpWord> {
        let mut allowlist = Allowlist::new(DESK_OPS.iter().map(|word| op(word)));
        for word in DESK_OPS {
            allowlist.grant(Author::from("clerk"), op(word));
        }
        allowlist.grant(Author::from("visitor"), op("note"));
        allowlist.refuse(
            Author::from("visitor"),
            &op("close"),
            "closing a ticket is the clerk's decision",
        );
        allowlist
    }

    fn registry(&self) -> Registry {
        let mut registry = Registry::new();
        registry.register::<Ticket>().expect("the ticket registers");
        registry
    }

    fn prepare(
        &self,
        queue: &QueueName,
        record: Value,
        allowlist: &Allowlist<OpWord>,
    ) -> Result<Vec<(QueueName, Value)>, String> {
        if queue.as_str() == "orders" {
            let author = record["author"].as_str().unwrap_or("clerk");
            let word = record["op"].as_str().ok_or("an order names its `op`")?;
            allowlist
                .allows(&Author::from(author), &op(word))
                .map_err(|refusal| refusal.to_string())?;
        }
        Ok(vec![(queue.clone(), record)])
    }
}

fn desk_bus(
    transport: &Arc<dyn Transport>,
    authors: &[(&str, &[&str])],
) -> Result<Bus, ConfigError> {
    let mut kinds = TransportKinds::builtin();
    let shared = Arc::clone(transport);
    kinds
        .register("table", Arc::new(move |_| Ok(Arc::clone(&shared))))
        .expect("the table's kind registers");
    let mut config = Config::local("unused", Some("desk"));
    config.transport = TransportConfig {
        kind: "table".parse().expect("a transport kind"),
        dir: None,
        options: Map::new(),
    };
    for (author, capabilities) in authors {
        config.authors.insert(
            Author::from(*author),
            AuthorConfig {
                capabilities: capabilities.iter().map(|word| (*word).to_owned()).collect(),
            },
        );
    }
    config.resolve(&Layouts::new().with(Arc::new(Desk)), &kinds)
}

/// An op not granted is refused by omission: the refusal names the author, the
/// op and the reason recorded for it — or that nothing grants it — and nothing
/// is appended.
pub fn an_allowlist_refuses_by_omission_naming_the_author_the_op_and_the_reason(fresh: Fresh<'_>) {
    let transport = fresh();
    let bus = desk_bus(&transport, &[]).expect("the desk resolves");
    let orders = queue_name("orders");
    let noted = bus
        .send(&orders, json!({"id": 0, "author": "visitor", "op": "note"}))
        .expect("a visitor may note");
    assert_eq!(noted[0].1.id, Some(0));

    let closing = bus
        .send(
            &orders,
            json!({"id": 0, "author": "visitor", "op": "close"}),
        )
        .expect_err("a visitor may not close");
    assert_eq!(
        closing.to_string(),
        "orders: 'close' is not an op visitor may issue: closing a ticket is the clerk's decision"
    );
    let opening = bus
        .send(&orders, json!({"id": 0, "author": "visitor", "op": "open"}))
        .expect_err("nothing grants a visitor open");
    assert_eq!(
        opening.to_string(),
        format!("orders: 'open' is not an op visitor may issue: {NOT_GRANTED}")
    );
    let stranger = bus
        .send(
            &orders,
            json!({"id": 0, "author": "stranger", "op": "note"}),
        )
        .expect_err("an undeclared author is granted nothing");
    assert!(
        stranger
            .to_string()
            .contains("'note' is not an op stranger may issue"),
        "{stranger}"
    );
    bus.send(&orders, json!({"id": 0, "author": "clerk", "op": "close"}))
        .expect("the clerk may close");
    assert_eq!(
        bus.queue(&orders)
            .expect("the queue")
            .log(None)
            .expect("the log")
            .len(),
        2,
        "a refused order was appended"
    );
    let unknown = bus
        .queue(&queue_name("elsewhere"))
        .expect_err("an undeclared queue is refused");
    assert_eq!(
        unknown.to_string(),
        "`elsewhere` is not a queue this configuration declares; it declares: orders, tickets"
    );
}

/// A configuration may narrow an author's grants, and what it narrows away is
/// refused with the configuration's reason; one widening them, naming an op
/// that does not exist or an author the layout does not declare is refused
/// naming the key.
pub fn a_configuration_narrows_an_author_and_is_refused_widening_one(fresh: Fresh<'_>) {
    let transport = fresh();
    let orders = queue_name("orders");
    let narrowed =
        desk_bus(&transport, &[("clerk", &["note", "open"])]).expect("narrowing resolves");
    let refusal = narrowed
        .send(&orders, json!({"id": 0, "author": "clerk", "op": "close"}))
        .expect_err("a narrowed-away op is refused");
    assert_eq!(
        refusal.to_string(),
        format!("orders: 'close' is not an op clerk may issue: {NARROWED}")
    );
    narrowed
        .send(&orders, json!({"id": 0, "author": "clerk", "op": "open"}))
        .expect("a kept op is allowed");

    for (authors, key, names) in [
        (
            vec![("visitor", &["note", "close"][..])],
            "authors.visitor.capabilities",
            "`close` is not granted to visitor",
        ),
        (
            vec![("visitor", &["fly"][..])],
            "authors.visitor.capabilities",
            "`fly` is not an op",
        ),
        (
            vec![("stranger", &["note"][..])],
            "authors.stranger.capabilities",
            "`stranger` is not an author",
        ),
    ] {
        match desk_bus(&transport, &authors) {
            Err(ConfigError::Narrowing(refusal)) => {
                assert_eq!(refusal.key, key, "{refusal}");
                let text = refusal.to_string();
                assert!(text.starts_with(key) && text.contains(names), "{text}");
            }
            other => panic!("{authors:?} was not refused as a widening: {other:?}"),
        }
    }
}

/// The transport an exclusive section lends its body is a whole transport: every
/// method works through it, a section over the queue it holds is the same
/// section, one over another queue is taken as well, and a position past the end
/// is refused inside a section as outside one.
pub fn every_method_works_through_the_transport_a_section_lends(fresh: Fresh<'_>) {
    use crate::transport::TransportError;
    let transport = fresh();
    let held = queue_name("held");
    let other = queue_name("other");
    let reader: ConsumerName = "reader".parse().expect("a consumer");
    let document: DocumentName = "held.json".parse().expect("a document name");
    let mut ran = false;
    transport
        .exclusive(&held, &mut |inner| {
            let first = inner.append(&held, br#"{"n":0}"#)?;
            inner.append(&other, br#"{"n":1}"#)?;
            assert_eq!(inner.read(&held, None, 10)?.records.len(), 1);
            inner.commit(&held, &reader, &first)?;
            assert_eq!(inner.cursor(&held, &reader)?, Some(first));
            inner.replace_document(&held, &document, b"{}")?;
            assert_eq!(inner.document(&held, &document)?, Some(b"{}".to_vec()));
            let print = inner.fingerprint(&held)?;
            assert!(
                matches!(
                    inner.wait_for_change(&held, &print, std::time::Duration::from_millis(10))?,
                    Changed::Unchanged(_)
                ),
                "a queue nothing moved reported moving inside a section"
            );
            inner.exclusive(&held, &mut |same| {
                same.append(&held, br#"{"n":2}"#).map(|_| ())
            })?;
            inner.exclusive(&other, &mut |both| {
                let landed = both.append(&other, br#"{"n":3}"#)?;
                both.append(&held, br#"{"n":4}"#)?;
                both.commit(&other, &reader, &landed)
            })?;
            let past = Position::from_token(u64::MAX);
            assert!(
                matches!(
                    inner.commit(&held, &reader, &past),
                    Err(TransportError::PastEnd { .. })
                ),
                "a commit past the end was not refused as one inside a section"
            );
            assert!(
                matches!(
                    inner.read(&held, Some(&past), 1),
                    Err(TransportError::PastEnd { .. })
                ),
                "a read past the end was not refused as one inside a section"
            );
            ran = true;
            Ok(())
        })
        .expect("the section runs");
    assert!(ran, "the section never ran its body");
    assert_eq!(
        transport
            .read(&held, None, 10)
            .expect("reads")
            .records
            .len(),
        3,
        "a record appended inside the section was lost"
    );
    let others = transport.read(&other, None, 10).expect("reads");
    assert_eq!(others.records.len(), 2);
    transport
        .commit(
            &other,
            &ConsumerName::default_consumer(),
            &others.records[0].after,
        )
        .expect("a commit outside every section");
    assert_eq!(
        transport
            .cursor(&other, &reader)
            .expect("reads")
            .map(|cursor| cursor == others.records[1].after),
        Some(true),
        "a commit inside a nested section was lost"
    );
    assert!(matches!(
        transport.commit(&other, &reader, &Position::from_token(u64::MAX)),
        Err(TransportError::PastEnd { .. })
    ));
    assert!(!transport
        .fingerprint(&held)
        .expect("a fingerprint")
        .parts()
        .is_empty());
}
