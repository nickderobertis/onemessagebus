//! Contract E for the emitter, over the open vocabulary: the filter decides
//! what is emitted and never what `seq` numbers; the emitter's rule is
//! `bound_payload`; redaction happens before the line is written; two shared
//! emitters over one file number one series.

use std::sync::{Arc, Mutex};

use onemessagebus::{
    Emitter, Envelope, Filter, Labels, Matcher, Open, Reader, Reading, Redactor, Source,
    MAX_PAYLOAD_TEXT_BYTES, REDACTED, TRUNCATED_KEY,
};
use serde_json::{json, Map, Value};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("sink").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("sink").clone()).expect("UTF-8")
    }
    fn envelopes(&self) -> Vec<Envelope<Open>> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str(line).expect("an envelope"))
            .collect()
    }
}

fn emitter(sink: &Captured) -> Emitter<Open> {
    Emitter::new("s", Source::from("billing"), Box::new(sink.clone()))
        .with_redactor(Redactor::new())
}

fn payload(entries: &[(&str, Value)]) -> Map<String, Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn ascii(n: usize) -> String {
    "abcdefghij".chars().cycle().take(n).collect()
}

/// Over a sequence of emits in which the filter suppresses some: every
/// suppressed envelope is returned, the sink never receives it, the sink's
/// envelopes carry `seq` exactly `1..=k`, and a suppressed envelope carries the
/// number the next admitted one takes.
#[test]
fn a_filter_decides_what_is_emitted_and_never_what_seq_numbers() {
    let sink = Captured::default();
    let filter = Filter::<Open> {
        include: Vec::new(),
        exclude: vec![Matcher::new().kind("heartbeat")],
    };
    let emitter = emitter(&sink).with_filter(filter);
    let kinds = [
        "started",
        "heartbeat",
        "heartbeat",
        "progress",
        "heartbeat",
        "finished",
    ];
    let mut returned = Vec::new();
    for kind in kinds {
        returned.push(emitter.emit(kind, Map::new()));
    }
    assert_eq!(
        returned.len(),
        6,
        "emit returns every envelope, suppressed or not"
    );
    let admitted: Vec<&Envelope<Open>> = returned
        .iter()
        .filter(|envelope| envelope.kind.as_str() != "heartbeat")
        .collect();
    let seqs: Vec<u64> = admitted.iter().map(|envelope| envelope.seq).collect();
    assert_eq!(seqs, [1, 2, 3]);
    // A suppressed envelope's seq is the number the next admitted one takes.
    assert_eq!(returned[1].seq, 2);
    assert_eq!(returned[2].seq, 2);
    assert_eq!(returned[3].seq, 2);
    assert_eq!(returned[4].seq, 3);
    assert_eq!(returned[5].seq, 3);

    let written = sink.envelopes();
    assert_eq!(written.len(), 3);
    assert!(written
        .iter()
        .all(|envelope| envelope.kind.as_str() != "heartbeat"));
    assert_eq!(
        written
            .iter()
            .map(|envelope| envelope.seq)
            .collect::<Vec<_>>(),
        [1, 2, 3],
        "no gap where a suppressed envelope fell"
    );
}

/// The emitter's rule is `bound_payload`, applied before the envelope is
/// stamped: the first 4096 bytes of a top-level text value and a `truncated`
/// stamp, the value whole and unstamped when inside the bound, cut back to a
/// character boundary inside a multi-byte character, and nested and non-text
/// values untouched.
#[test]
fn the_emitter_applies_bound_payload_before_stamping() {
    let sink = Captured::default();
    let emitter = emitter(&sink);

    let over = format!("{}Z", ascii(MAX_PAYLOAD_TEXT_BYTES));
    let written = emitter.emit(
        "ran",
        payload(&[
            ("output", json!(over)),
            ("count", json!(3)),
            (
                "nested",
                json!({ "inner": ascii(MAX_PAYLOAD_TEXT_BYTES + 5) }),
            ),
        ]),
    );
    assert_eq!(
        written.payload["output"],
        json!(ascii(MAX_PAYLOAD_TEXT_BYTES))
    );
    assert_eq!(written.payload[TRUNCATED_KEY], json!(true));
    assert_eq!(written.payload["count"], json!(3));
    assert_eq!(
        written.payload["nested"]["inner"].as_str().map(str::len),
        Some(MAX_PAYLOAD_TEXT_BYTES + 5)
    );

    let exact = ascii(MAX_PAYLOAD_TEXT_BYTES);
    let whole = emitter.emit("ran", payload(&[("output", json!(exact))]));
    assert_eq!(whole.payload["output"], json!(exact));
    assert!(!whole.payload.contains_key(TRUNCATED_KEY));

    let straddling = format!("{}€{}", ascii(MAX_PAYLOAD_TEXT_BYTES - 2), ascii(4));
    let cut = emitter.emit("ran", payload(&[("output", json!(straddling))]));
    assert_eq!(
        cut.payload["output"],
        json!(ascii(MAX_PAYLOAD_TEXT_BYTES - 2))
    );
    assert_eq!(cut.payload[TRUNCATED_KEY], json!(true));

    // And the file carries the same: what was returned is what was written.
    let on_disk = sink.envelopes();
    assert_eq!(on_disk, vec![written, whole, cut]);
}

#[test]
fn credential_values_are_redacted_before_the_line_is_written() {
    let sink = Captured::default();
    let emitter = Emitter::<Open>::new("s", Source::from("billing"), Box::new(sink.clone()))
        .with_redactor(Redactor::new().with_secret("hunter2hunter2"));
    let written = emitter.emit(
        "login",
        payload(&[
            ("password", json!("the password is hunter2hunter2, keep it")),
            (
                "token",
                json!("ghp_0123456789abcdef and AKIAIOSFODNN7EXAMPLE"),
            ),
        ]),
    );
    assert_eq!(
        written.payload["password"],
        json!(format!("the password is {REDACTED}, keep it"))
    );
    assert_eq!(
        written.payload["token"],
        json!(format!("{REDACTED} and {REDACTED}"))
    );
    assert!(!sink.text().contains("hunter2"));
    assert!(!sink.text().contains("ghp_"));
}

#[test]
fn the_environment_names_what_is_a_credential() {
    // A value under a credential-shaped name is redacted wherever it appears;
    // a short one is not, so prose is not eaten.
    std::env::set_var("ONEMESSAGEBUS_TEST_API_KEY", "sk-live-0123456789");
    std::env::set_var("ONEMESSAGEBUS_TEST_TOKEN_ENABLED", "yes");
    let redactor = Redactor::from_env();
    assert_eq!(
        redactor.redact("key sk-live-0123456789 yes"),
        format!("key {REDACTED} yes")
    );
    std::env::remove_var("ONEMESSAGEBUS_TEST_API_KEY");
    std::env::remove_var("ONEMESSAGEBUS_TEST_TOKEN_ENABLED");
}

#[test]
fn labels_are_stamped_on_every_envelope_and_a_derived_stamp_wins() {
    let sink = Captured::default();
    let base =
        emitter(&sink).with_labels(Labels::new().with("tenant", "acme").with("order", "o-1"));
    let derived = base.clone().with_labels(Labels::new().with("order", "o-2"));
    let written = derived.emit("shipped", Map::new());
    assert_eq!(written.labels.get_str("tenant"), Some("acme"));
    assert_eq!(written.labels.get_str("order"), Some("o-2"));
    assert_eq!(base.labels().get_str("order"), Some("o-1"));
}

#[test]
fn two_shared_emitters_in_one_process_number_one_gapless_series() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("shared.ndjson");
    let writers: Vec<Emitter<Open>> = ["a", "b"]
        .into_iter()
        .map(|stream| {
            Emitter::shared(stream, Source::from("billing"), &path).with_redactor(Redactor::new())
        })
        .collect();
    let handles: Vec<_> = writers
        .into_iter()
        .map(|emitter| {
            std::thread::spawn(move || {
                for n in 0..40 {
                    emitter.emit("tick", payload(&[("n", json!(n))]));
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("a writer thread");
    }
    let mut seqs: Vec<u64> = Reader::<Open>::open(&path)
        .expect("opens")
        .map(|reading| match reading {
            Reading::Record(record) => record.envelope.seq,
            other => panic!("{other:?}"),
        })
        .collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=80).collect::<Vec<u64>>());
}

#[test]
fn a_shared_emitter_heals_a_torn_tail_before_it_appends() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("torn.ndjson");
    let emitter =
        Emitter::<Open>::shared("a", Source::from("billing"), &path).with_redactor(Redactor::new());
    emitter.emit("first", Map::new());
    let whole = std::fs::read_to_string(&path).expect("read");
    std::fs::write(&path, format!("{whole}{{\"v\":1,\"ts\":\"20")).expect("torn tail written");
    let second = emitter.emit("second", Map::new());
    assert_eq!(second.seq, 2, "the torn tail is not a record");
    let readings: Vec<Reading<Open>> = Reader::open(&path).expect("opens").collect();
    assert_eq!(readings.len(), 2);
    assert!(readings
        .iter()
        .all(|reading| matches!(reading, Reading::Record(_))));
}

#[test]
fn a_sink_that_cannot_be_written_is_reported_and_the_envelope_still_returned() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let emitter = Emitter::<Open>::new("s", Source::from("billing"), Box::new(Broken))
        .with_redactor(Redactor::new());
    let unrecorded = emitter
        .try_emit("thing", Map::new(), Vec::new())
        .expect_err("a closed sink is reported");
    assert_eq!(unrecorded.envelope.kind.as_str(), "thing");
    assert!(unrecorded.to_string().contains("closed"), "{unrecorded}");
    // The infallible spelling still hands the envelope back.
    let envelope = emitter.emit("thing", Map::new());
    assert_eq!(envelope.seq, 2);
}
