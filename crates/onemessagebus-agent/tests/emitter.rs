//! The emitter over the agent vocabulary: the per-source write version it
//! stamps, the redaction it applies, the phase it carries, and two shared
//! emitters over one file in one process.

use std::sync::{Arc, Mutex};

use onemessagebus::{Reading, Redactor, CREDENTIAL_PREFIXES, REDACTED};
use onemessagebus_agent::{
    AgentEnvelope, Dimensions, Emitter, Envelope, Labels, Phase, Reader, Source,
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
    fn lines(&self) -> Vec<Envelope> {
        let bytes = self.0.lock().expect("sink").clone();
        String::from_utf8(bytes)
            .expect("UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("an envelope"))
            .collect()
    }
}

fn payload(entries: &[(&str, Value)]) -> Map<String, Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

/// The version an emitter stamps is the profile's fact about its source:
/// `pipeline` writes 2, `agentgraph` and `vcs` write 1.
#[test]
fn an_emitter_stamps_its_sources_write_version() {
    for (source, expected) in [
        (Source::Pipeline, 2),
        (Source::Agentgraph, 1),
        (Source::Vcs, 1),
    ] {
        let sink = Captured::default();
        let emitter =
            Emitter::new("s", source, Box::new(sink.clone())).with_redactor(Redactor::new());
        let written = emitter.emit("thing", Map::new());
        assert_eq!(written.v, expected, "{source}");
        assert_eq!(emitter.version(), expected);
        assert_eq!(sink.lines()[0].v, expected, "{source} on the wire");
        assert_eq!(sink.lines()[0].source, source);
    }
}

#[test]
fn a_phase_is_stamped_per_emitter_or_per_event_and_omitted_when_none() {
    let sink = Captured::default();
    let emitter = Emitter::new("s", Source::Vcs, Box::new(sink.clone()))
        .with_redactor(Redactor::new())
        .with_dimensions(Dimensions::at(Phase::Development));
    let first = emitter.emit("fetch", Map::new());
    assert_eq!(first.phase(), Some(Phase::Development));
    let second = emitter.emit_stamped(
        "merge-queued",
        Dimensions::at(Phase::Integrate),
        Map::new(),
        Vec::new(),
    );
    assert_eq!(second.phase(), Some(Phase::Integrate));
    let bare = Emitter::new("t", Source::Agentgraph, Box::new(sink.clone()))
        .with_redactor(Redactor::new())
        .emit("graph-started", Map::new());
    assert_eq!(bare.phase(), None);
    let lines = sink.lines();
    assert_eq!(lines[0].phase(), Some(Phase::Development));
    assert_eq!(lines[1].phase(), Some(Phase::Integrate));
    assert_eq!(lines[2].phase(), None);
    let raw = String::from_utf8(sink.0.lock().expect("sink").clone()).expect("UTF-8");
    assert!(!raw.lines().nth(2).expect("a third line").contains("phase"));
}

#[test]
fn labels_stamped_on_a_derived_emitter_win_and_the_runs_fill_in() {
    let sink = Captured::default();
    let run = Emitter::new("s", Source::Agentgraph, Box::new(sink.clone()))
        .with_redactor(Redactor::new())
        .with_labels(Labels {
            run_id: Some("R".to_owned()),
            member: Some("graph".to_owned()),
            ..Labels::default()
        });
    let member = run.clone().with_labels(Labels {
        member: Some("worker".to_owned()),
        persona: Some("engineer".to_owned()),
        ..Labels::default()
    });
    let written = member.emit("turn-started", Map::new());
    assert_eq!(written.labels.run_id.as_deref(), Some("R"));
    assert_eq!(written.labels.member.as_deref(), Some("worker"));
    assert_eq!(written.labels.persona.as_deref(), Some("engineer"));
    // The run's own emitter is untouched by the derivation.
    let runs = run.emit("member-heartbeat", Map::new());
    assert_eq!(runs.labels.member.as_deref(), Some("graph"));
}

/// Every table entry, redacted before the line is written: a value under each
/// credential-shaped environment name, and a token with each prefix.
#[test]
fn credential_shaped_values_are_redacted_before_the_envelope_is_written() {
    let mut redactor = Redactor::new();
    let mut entries: Vec<(&str, Value)> = Vec::new();
    let words = onemessagebus::CREDENTIAL_WORDS;
    let secrets: Vec<String> = words
        .iter()
        .map(|word| format!("value-of-{}-1234567890", word.to_ascii_lowercase()))
        .collect();
    for secret in &secrets {
        redactor = redactor.with_secret(secret.clone());
    }
    for (word, secret) in words.iter().zip(&secrets) {
        entries.push((word, json!(format!("printed {secret} here"))));
    }
    let prefixed: Vec<String> = CREDENTIAL_PREFIXES
        .iter()
        .map(|prefix| format!("{prefix}abcdefghijklmnop"))
        .collect();
    entries.push(("tokens", json!(prefixed.join(" "))));
    entries.push(("nested", json!({ "deep": [prefixed[0].clone()] })));
    entries.push(("plain", json!("nothing to see")));
    entries.push(("count", json!(3)));

    let sink = Captured::default();
    let emitter = Emitter::new("s", Source::Vcs, Box::new(sink.clone())).with_redactor(redactor);
    let written = emitter.emit("push", payload(&entries));
    let line = String::from_utf8(sink.0.lock().expect("sink").clone()).expect("UTF-8");
    for secret in secrets.iter().chain(&prefixed) {
        assert!(!line.contains(secret.as_str()), "{secret} leaked: {line}");
    }
    for word in words {
        assert_eq!(
            written.payload[*word],
            json!(format!("printed {REDACTED} here"))
        );
    }
    assert_eq!(
        written.payload["tokens"],
        json!(vec![REDACTED; prefixed.len()].join(" "))
    );
    assert_eq!(written.payload["nested"]["deep"][0], json!(REDACTED));
    assert_eq!(written.payload["plain"], json!("nothing to see"));
    assert_eq!(written.payload["count"], json!(3));
}

/// The redactor a producer gets without asking for one: every value this
/// process's environment holds under a name carrying a credential word, read
/// by [`Emitter::new`] through `Redactor::from_env`, and a token with each
/// prefix — none of it on the returned envelope or on the written line.
#[test]
fn an_emitter_redacts_every_value_its_environment_holds_under_a_credential_word() {
    let words = onemessagebus::CREDENTIAL_WORDS;
    let secrets: Vec<String> = words
        .iter()
        .map(|word| format!("env-held-{}-0987654321", word.to_ascii_lowercase()))
        .collect();
    for (word, secret) in words.iter().zip(&secrets) {
        // nextest runs each test in a process of its own, so the environment
        // this sets is read by this test's emitter alone.
        std::env::set_var(format!("OMB_TEST_EMITTER_{word}"), secret);
    }
    let prefixed: Vec<String> = CREDENTIAL_PREFIXES
        .iter()
        .map(|prefix| format!("{prefix}qrstuvwxyz012345"))
        .collect();
    let mut entries: Vec<(&str, Value)> = words
        .iter()
        .zip(&secrets)
        .map(|(word, secret)| (*word, json!(secret)))
        .collect();
    for (prefix, token) in CREDENTIAL_PREFIXES.iter().zip(&prefixed) {
        entries.push((prefix, json!(token)));
    }

    let sink = Captured::default();
    let emitter = Emitter::new("s", Source::Vcs, Box::new(sink.clone()));
    let written = emitter.emit("push", payload(&entries));
    let line = String::from_utf8(sink.0.lock().expect("sink").clone()).expect("UTF-8");
    for (key, _) in &entries {
        assert_eq!(
            written.payload[*key],
            json!(REDACTED),
            "{key} was not redacted on the returned envelope"
        );
        assert_eq!(
            sink.lines()[0].payload[*key],
            json!(REDACTED),
            "{key} was not redacted on the written line"
        );
    }
    for secret in secrets.iter().chain(&prefixed) {
        assert!(!line.contains(secret.as_str()), "{secret} leaked: {line}");
    }
}

/// Two shared emitters in one process, on one file, leave one gapless series.
#[test]
fn two_shared_emitters_in_one_process_number_one_gapless_series() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("shared.ndjson");
    let first = Emitter::shared("a", Source::Pipeline, &path).with_redactor(Redactor::new());
    let second = Emitter::shared("b", Source::Vcs, &path).with_redactor(Redactor::new());
    let handles: Vec<_> = [first, second]
        .into_iter()
        .map(|emitter| {
            std::thread::spawn(move || {
                for n in 0..50 {
                    let written = emitter.emit("tick", payload(&[("n", json!(n))]));
                    assert!(written.seq >= 1);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("a writer thread");
    }
    let mut seqs: Vec<u64> = Reader::open(&path)
        .expect("opens")
        .map(|reading| match reading {
            Reading::Record(record) => record.envelope.seq,
            other => panic!("{other:?}"),
        })
        .collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=100).collect::<Vec<u64>>());
}
