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

use serde_json::{json, Map, Value};

use crate::emit::Emitter;
use crate::envelope::{Envelope, Kind};
use crate::filter::{Filter, Matcher};
use crate::read::{Merge, Reader, Reading};
use crate::redact::Redactor;
use crate::schema::{CheckError, Message, Registry, SchemaId};
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
