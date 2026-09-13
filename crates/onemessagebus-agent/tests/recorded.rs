//! Byte-for-byte fidelity, proven rather than asserted: a stream each of the
//! three producers wrote on a real host round-trips through `Reader` and
//! `serde_json::to_string` with no byte changed. `tests/recorded/README.md`
//! says where each came from.

use onemessagebus::{Reader, Reading};
use onemessagebus_agent::{AgentEnvelope, Envelope, Phase, Source};

const RECORDED: &[(&str, &str, Source)] = &[
    (
        "oneagentgraph-run.ndjson",
        include_str!("recorded/oneagentgraph-run.ndjson"),
        Source::Agentgraph,
    ),
    (
        "onevcs-session.ndjson",
        include_str!("recorded/onevcs-session.ndjson"),
        Source::Vcs,
    ),
    (
        "onepipeline-events.jsonl",
        include_str!("recorded/onepipeline-events.jsonl"),
        Source::Pipeline,
    ),
];

fn recorded_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/recorded")
        .join(name)
}

/// Every line of every recorded stream reads as an agent envelope and
/// serializes back to exactly the line its producer wrote.
#[test]
fn every_recorded_line_round_trips_with_no_byte_changed() {
    for (name, contents, _) in RECORDED {
        for (number, line) in contents.lines().enumerate() {
            let envelope: Envelope = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{name} line {}: not an envelope: {e}", number + 1));
            let written = serde_json::to_string(&envelope).expect("serializes");
            assert_eq!(
                written,
                line,
                "{name} line {}: re-serializing changed the bytes",
                number + 1
            );
        }
    }
}

/// The same, through `Reader` over the file: every line is a whole record,
/// nothing is torn or refused, and the positions chain from the first byte to
/// the last.
#[test]
fn every_recorded_stream_reads_whole_through_the_reader() {
    for (name, contents, _) in RECORDED {
        let reader =
            Reader::<onemessagebus_agent::Agent>::open(recorded_path(name)).expect("opens");
        let mut lines = contents.lines();
        let mut position = 0u64;
        for reading in reader {
            let Reading::Record(record) = reading else {
                panic!("{name}: a recorded stream read as {reading:?}");
            };
            let line = lines.next().expect("a line per record");
            assert_eq!(
                serde_json::to_string(&record.envelope).expect("serializes"),
                line
            );
            assert_eq!(record.position, position + line.len() as u64 + 1);
            position = record.position;
        }
        assert!(lines.next().is_none(), "{name}: a line was not yielded");
        assert_eq!(position, contents.len() as u64);
    }
}

/// What the three streams carry is what the profile says each producer
/// stamps: its source word, its write version on its own envelopes, and a
/// phase on `vcs` envelopes alone.
#[test]
fn each_recorded_stream_carries_its_producers_vocabulary() {
    for (name, contents, source) in RECORDED {
        let envelopes: Vec<Envelope> = contents
            .lines()
            .map(|line| serde_json::from_str(line).expect("an envelope"))
            .collect();
        assert!(
            envelopes.iter().any(|envelope| envelope.source == *source),
            "{name}"
        );
        for envelope in &envelopes {
            if envelope.source == *source && envelope.stream == envelopes[0].stream {
                assert_eq!(
                    envelope.v,
                    source.write_version(),
                    "{name}: {}",
                    envelope.kind
                );
            }
            match envelope.source {
                Source::Vcs if envelope.v == 1 => {
                    assert!(
                        Phase::every().contains(&envelope.phase().expect("vcs stamps a phase")),
                        "{name}"
                    );
                }
                Source::Agentgraph => assert_eq!(envelope.phase(), None, "{name}"),
                _ => {}
            }
        }
    }
}

/// The three relayed lines `onepipeline` wrote with `member` after `persona`
/// — its copy of `Labels` had no `member` field — read whole and come back out
/// in the contract's order: the drift reconciled by decision, not preserved.
#[test]
fn relayed_lines_with_drifted_label_order_read_whole_and_reserialize_in_contract_order() {
    const DRIFTED: &str = include_str!("recorded/onepipeline-relayed-drift.jsonl");
    for (number, line) in DRIFTED.lines().enumerate() {
        let envelope: Envelope = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("drifted line {}: not an envelope: {e}", number + 1));
        assert_eq!(envelope.source, Source::Agentgraph);
        assert_eq!(envelope.labels.member.as_deref(), Some("worker"));
        assert_eq!(envelope.labels.persona.as_deref(), Some("design-doc"));
        let written = serde_json::to_string(&envelope).expect("serializes");
        assert_ne!(
            written, line,
            "the drifted order was preserved rather than reconciled"
        );
        let member = written.find("\"member\"").expect("member is written");
        let persona = written.find("\"persona\"").expect("persona is written");
        assert!(member < persona, "member must precede persona: {written}");
        // Same content, different key order: the document is the same document.
        let as_written: serde_json::Value = serde_json::from_str(&written).expect("JSON");
        let as_relayed: serde_json::Value = serde_json::from_str(line).expect("JSON");
        assert_eq!(as_written, as_relayed);
    }
}
