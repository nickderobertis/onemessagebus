//! The `events` verbs, through the built binary.

use std::path::Path;
use std::process::Stdio;

use onemessagebus::{CREDENTIAL_PREFIXES, CREDENTIAL_WORDS, MAX_PAYLOAD_TEXT_BYTES, REDACTED};
use serde_json::{json, Value};

use crate::support::{ascii, fixture, onemessagebus, run, run_in, Run};

const RECORDED: &[&str] = &[
    "recorded/oneagentgraph-run.ndjson",
    "recorded/onevcs-session.ndjson",
    "recorded/onepipeline-events.jsonl",
];

fn recorded_args() -> Vec<String> {
    RECORDED
        .iter()
        .map(|name| fixture(name).to_str().expect("UTF-8").to_owned())
        .collect()
}

fn merge(extra: &[&str]) -> Run {
    let files = recorded_args();
    let mut args: Vec<&str> = vec!["events", "merge"];
    args.extend(files.iter().map(String::as_str));
    args.extend(extra);
    run(&args, None)
}

fn order_key(envelope: &Value) -> (String, String, u64) {
    (
        envelope["ts"].as_str().expect("ts").to_owned(),
        envelope["stream"].as_str().expect("stream").to_owned(),
        envelope["seq"].as_u64().expect("seq"),
    )
}

#[test]
fn events_merge_prints_every_envelope_of_every_file_in_ts_stream_seq_order() {
    let merged = merge(&[]);
    assert_eq!(merged.code, 0, "{}", merged.stderr);
    assert!(merged.stderr.is_empty(), "{}", merged.stderr);
    let printed = merged.lines();
    let mut expected: Vec<Value> = RECORDED
        .iter()
        .flat_map(|name| {
            std::fs::read_to_string(fixture(name))
                .expect("recorded")
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).expect("an envelope"))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(printed.len(), expected.len());
    expected.sort_by_key(order_key);
    assert_eq!(printed, expected);
    let keys: Vec<_> = printed.iter().map(order_key).collect();
    assert!(keys.windows(2).all(|pair| pair[0] <= pair[1]));
    // Byte for byte: each printed line is the recorded line.
    let recorded_lines: std::collections::BTreeSet<String> = RECORDED
        .iter()
        .flat_map(|name| {
            std::fs::read_to_string(fixture(name))
                .expect("recorded")
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    for line in merged.stdout.lines() {
        assert!(
            recorded_lines.contains(line),
            "a printed line was not recorded: {line}"
        );
    }
}

#[test]
fn events_merge_applies_a_filter_and_a_profile() {
    let filtered = merge(&[
        "--filter",
        r#"{"include":[{"source":"vcs","kind":"change-*"}]}"#,
    ]);
    assert_eq!(filtered.code, 0, "{}", filtered.stderr);
    let printed = filtered.lines();
    assert!(!printed.is_empty());
    assert!(printed.iter().all(|envelope| {
        envelope["source"] == json!("vcs")
            && envelope["kind"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("change-"))
    }));

    let by_phase = merge(&["--filter", r#"{"include":[{"phase":"review"}]}"#]);
    assert_eq!(by_phase.code, 0, "{}", by_phase.stderr);
    assert!(by_phase
        .lines()
        .iter()
        .all(|envelope| envelope["phase"] == json!("review")));

    // A YAML file spelling of the same filter.
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(
        dir.path().join("filter.yaml"),
        "include:\n  - source: vcs\n    kind: \"change-*\"\n",
    )
    .expect("written");
    let files = recorded_args();
    let mut args: Vec<&str> = vec!["events", "merge"];
    args.extend(files.iter().map(String::as_str));
    args.extend(["--filter", "filter.yaml"]);
    let from_file = run_in(dir.path(), &args, None, &[]);
    assert_eq!(from_file.code, 0, "{}", from_file.stderr);
    assert_eq!(from_file.stdout, filtered.stdout);

    // Through the open profile, the agent keys are plain labels, and a source
    // word the agent profile has not is admitted rather than refused.
    let open = merge(&[
        "--profile",
        "open",
        "--filter",
        r#"{"include":[{"member":"corpus"}]}"#,
    ]);
    assert_eq!(open.code, 0, "{}", open.stderr);
    assert!(!open.lines().is_empty());
    assert!(open
        .lines()
        .iter()
        .all(|envelope| envelope["labels"]["member"] == json!("corpus")));
    let explicit = merge(&["--profile", "agent"]);
    assert_eq!(
        explicit.stdout,
        merge(&[]).stdout,
        "the default profile is the agent one"
    );

    let unknown = merge(&["--profile", "billing"]);
    assert_eq!(unknown.code, 2);
    assert!(unknown.stderr.contains("billing"), "{}", unknown.stderr);
    assert!(unknown.stderr.contains("agent, open"), "{}", unknown.stderr);
}

#[test]
fn events_merge_refuses_a_malformed_filter_naming_list_index_and_matcher() {
    let empty = merge(&["--filter", r#"{"include":[{"kind":"x"},{}]}"#]);
    assert_eq!(empty.code, 2);
    assert!(empty.stdout.is_empty());
    assert!(empty.stderr.contains("include[1] {}"), "{}", empty.stderr);

    let blank = merge(&["--filter", r#"{"exclude":[{"member":""}]}"#]);
    assert_eq!(blank.code, 2);
    assert!(
        blank.stderr.contains(r#"exclude[0] {"member":""}"#),
        "{}",
        blank.stderr
    );

    let unknown = merge(&["--filter", r#"{"include":[{"stream":"s"}]}"#]);
    assert_eq!(unknown.code, 2);
    assert!(unknown.stderr.contains("stream"), "{}", unknown.stderr);

    let missing = merge(&["--filter", "no-such-filter.yaml"]);
    assert_eq!(missing.code, 2);
    assert!(
        missing.stderr.contains("no-such-filter.yaml"),
        "{}",
        missing.stderr
    );

    let dir = tempfile::tempdir().expect("a temp dir");
    let absent = dir.path().join("absent.ndjson");
    let missing_file = run(&["events", "merge", absent.to_str().expect("UTF-8")], None);
    assert_eq!(missing_file.code, 2);
    assert!(
        missing_file.stderr.contains("absent.ndjson"),
        "{}",
        missing_file.stderr
    );
}

#[test]
fn events_merge_renders_text_deterministically() {
    let first = merge(&["--format", "text"]);
    let second = merge(&["--format", "text"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(first.stdout, second.stdout);
    let json = merge(&[]);
    assert_eq!(first.stdout.lines().count(), json.lines().len());
    // The same events: every line names the same ts, source, kind, stream and seq
    // as the JSON rendering, in the same order.
    for (text, envelope) in first.stdout.lines().zip(json.lines()) {
        let expected_head = format!(
            "{} {} {} stream={} seq={} v={}",
            envelope["ts"].as_str().expect("ts"),
            envelope["source"].as_str().expect("source"),
            envelope["kind"].as_str().expect("kind"),
            envelope["stream"].as_str().expect("stream"),
            envelope["seq"],
            envelope["v"]
        );
        assert!(text.starts_with(&expected_head), "{text}\n{expected_head}");
        if let Some(phase) = envelope["phase"].as_str() {
            assert!(text.contains(&format!(" phase={phase}")), "{text}");
        }
    }
}

#[test]
fn events_merge_over_a_torn_file_prints_the_whole_records_and_reports_the_tail() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let whole =
        std::fs::read_to_string(fixture("recorded/onevcs-session.ndjson")).expect("recorded");
    let last = whole.lines().last().expect("a last line");
    let torn = dir.path().join("torn.ndjson");
    std::fs::write(&torn, format!("{whole}{}", &last[..last.len() / 2])).expect("written");
    let merged = run(&["events", "merge", torn.to_str().expect("UTF-8")], None);
    assert_eq!(merged.code, 0, "{}", merged.stderr);
    assert_eq!(merged.lines().len(), whole.lines().count());
    assert!(merged.stderr.contains("torn record"), "{}", merged.stderr);
    assert!(
        merged.stderr.contains(&format!("at byte {}", whole.len())),
        "{}",
        merged.stderr
    );
}

fn emit_in(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Run {
    let mut full: Vec<&str> = vec!["events", "emit", "stream.ndjson"];
    full.extend(args);
    run_in(dir, &full, stdin, env)
}

fn lines_of(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("stream.ndjson"))
        .expect("the stream file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("an envelope"))
        .collect()
}

#[test]
fn events_emit_appends_one_envelope_stamped_from_stdin_and_from_file_alike() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let base = [
        "--kind",
        "thing-done",
        "--stream",
        "s-1",
        "--source",
        "vcs",
        "--label",
        "run_id=R",
        "--label",
        "round=2",
        "--label",
        "workstream=w",
    ];
    let on_stdin = emit_in(dir.path(), &base, Some(r#"{"note":"hi","n":1}"#), &[]);
    assert_eq!(on_stdin.code, 0, "{}", on_stdin.stderr);
    let printed = on_stdin.lines();
    assert_eq!(printed.len(), 1);
    let written = lines_of(dir.path());
    assert_eq!(written.len(), 1);
    assert_eq!(
        printed[0], written[0],
        "the envelope printed is the one written"
    );
    let envelope = &written[0];
    assert_eq!(envelope["kind"], json!("thing-done"));
    assert_eq!(envelope["stream"], json!("s-1"));
    assert_eq!(envelope["source"], json!("vcs"));
    assert_eq!(envelope["seq"], json!(1));
    assert_eq!(envelope["v"], json!(1));
    assert_eq!(
        envelope["labels"],
        json!({ "run_id": "R", "round": 2, "workstream": "w" })
    );
    assert_eq!(envelope["payload"], json!({ "note": "hi", "n": 1 }));
    assert_eq!(envelope["artifacts"], json!([]));
    assert!(envelope["ts"]
        .as_str()
        .is_some_and(|ts| ts.ends_with('Z') && ts.len() == 24));

    std::fs::write(dir.path().join("payload.json"), r#"{"n":2}"#).expect("written");
    let mut with_file = base.to_vec();
    with_file.extend(["--file", "payload.json"]);
    let on_file = emit_in(dir.path(), &with_file, None, &[]);
    assert_eq!(on_file.code, 0, "{}", on_file.stderr);
    let written = lines_of(dir.path());
    assert_eq!(written.len(), 2);
    assert_eq!(written[1]["seq"], json!(2));
    assert_eq!(written[1]["payload"], json!({ "n": 2 }));

    let text = emit_in(
        dir.path(),
        &[
            "--kind", "k", "--stream", "s-1", "--source", "vcs", "--format", "text",
        ],
        Some("{}"),
        &[],
    );
    assert_eq!(text.code, 0, "{}", text.stderr);
    assert!(
        text.stdout.contains(" vcs k stream=s-1 seq=3 v=1"),
        "{}",
        text.stdout
    );
}

#[test]
fn events_emit_refuses_bad_input_by_name() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let bad_source = emit_in(
        dir.path(),
        &["--kind", "k", "--stream", "s", "--source", "billing"],
        Some("{}"),
        &[],
    );
    assert_eq!(bad_source.code, 2);
    assert!(
        bad_source.stderr.contains("billing"),
        "{}",
        bad_source.stderr
    );
    assert!(
        bad_source.stderr.contains("agent profile"),
        "{}",
        bad_source.stderr
    );

    let bad_label = emit_in(
        dir.path(),
        &["--kind", "k", "--stream", "s", "--label", "round=two"],
        Some("{}"),
        &[],
    );
    assert_eq!(bad_label.code, 2);
    assert!(bad_label.stderr.contains("round"), "{}", bad_label.stderr);
    assert!(bad_label.stderr.contains("integer"), "{}", bad_label.stderr);

    let no_pair = emit_in(
        dir.path(),
        &["--kind", "k", "--stream", "s", "--label", "novalue"],
        Some("{}"),
        &[],
    );
    assert_eq!(no_pair.code, 2);
    assert!(no_pair.stderr.contains("key=value"), "{}", no_pair.stderr);

    let not_object = emit_in(
        dir.path(),
        &["--kind", "k", "--stream", "s"],
        Some("[1]"),
        &[],
    );
    assert_eq!(not_object.code, 2);
    assert!(
        not_object.stderr.contains("JSON object"),
        "{}",
        not_object.stderr
    );

    let positional = run_in(
        dir.path(),
        &[
            "events",
            "emit",
            "stream.ndjson",
            r#"{"n":1}"#,
            "--kind",
            "k",
            "--stream",
            "s",
        ],
        None,
        &[],
    );
    assert_eq!(positional.code, 2);
    assert!(
        positional.stderr.contains("unexpected argument"),
        "{}",
        positional.stderr
    );

    assert!(!dir.path().join("stream.ndjson").exists() || lines_of(dir.path()).is_empty());
}

/// The per-source write version is a profile fact the emitter stamps: through
/// the binary, for each of the three words, with `--profile agent` and with
/// the option omitted.
#[test]
fn events_emit_stamps_each_sources_write_version_under_the_agent_profile_and_its_default() {
    for (source, version) in [("pipeline", 2), ("agentgraph", 1), ("vcs", 1)] {
        for profile in [Some("agent"), None] {
            let dir = tempfile::tempdir().expect("a temp dir");
            let mut args = vec!["--kind", "k", "--stream", "s", "--source", source];
            if let Some(name) = profile {
                args.extend(["--profile", name]);
            }
            let emitted = emit_in(dir.path(), &args, Some("{}"), &[]);
            assert_eq!(emitted.code, 0, "{source} {profile:?}: {}", emitted.stderr);
            let written = lines_of(dir.path());
            assert_eq!(
                written[0]["v"],
                json!(version),
                "{source} {profile:?} on the file"
            );
            assert_eq!(
                emitted.lines()[0]["v"],
                json!(version),
                "{source} {profile:?} printed"
            );
            assert_eq!(written[0]["source"], json!(source));
        }
    }
    // The profile's default source when none is named.
    let dir = tempfile::tempdir().expect("a temp dir");
    let defaulted = emit_in(
        dir.path(),
        &["--kind", "k", "--stream", "s"],
        Some("{}"),
        &[],
    );
    assert_eq!(defaulted.code, 0, "{}", defaulted.stderr);
    assert_eq!(lines_of(dir.path())[0]["source"], json!("pipeline"));
    assert_eq!(lines_of(dir.path())[0]["v"], json!(2));
    // And the open profile takes any word at version 1.
    let dir = tempfile::tempdir().expect("a temp dir");
    let open = emit_in(
        dir.path(),
        &[
            "--kind",
            "k",
            "--stream",
            "s",
            "--profile",
            "open",
            "--source",
            "billing",
        ],
        Some("{}"),
        &[],
    );
    assert_eq!(open.code, 0, "{}", open.stderr);
    assert_eq!(lines_of(dir.path())[0]["source"], json!("billing"));
    assert_eq!(lines_of(dir.path())[0]["v"], json!(1));
}

/// The emitter's rule through the binary: the first 4096 bytes of an over-long
/// top-level text value and a `truncated` stamp; a value of exactly 4096 bytes
/// whole and unstamped; a multi-byte straddle cut back to a boundary; nested
/// and non-text values untouched — read off the file and off the printed line.
#[test]
fn events_emit_applies_bound_payload_before_stamping() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let args = ["--kind", "ran", "--stream", "s"];

    let over = json!({
        "output": format!("{}Z", ascii(MAX_PAYLOAD_TEXT_BYTES)),
        "count": 3,
        "nested": { "inner": ascii(MAX_PAYLOAD_TEXT_BYTES + 5) }
    })
    .to_string();
    let cut = emit_in(dir.path(), &args, Some(&over), &[]);
    assert_eq!(cut.code, 0, "{}", cut.stderr);
    for envelope in [&cut.lines()[0], &lines_of(dir.path())[0]] {
        assert_eq!(
            envelope["payload"]["output"],
            json!(ascii(MAX_PAYLOAD_TEXT_BYTES))
        );
        assert_eq!(envelope["payload"]["truncated"], json!(true));
        assert_eq!(envelope["payload"]["count"], json!(3));
        assert_eq!(
            envelope["payload"]["nested"]["inner"]
                .as_str()
                .map(str::len),
            Some(MAX_PAYLOAD_TEXT_BYTES + 5)
        );
    }

    let exact = json!({ "output": ascii(MAX_PAYLOAD_TEXT_BYTES) }).to_string();
    let whole = emit_in(dir.path(), &args, Some(&exact), &[]);
    assert_eq!(whole.code, 0, "{}", whole.stderr);
    for envelope in [&whole.lines()[0], &lines_of(dir.path())[1]] {
        assert_eq!(
            envelope["payload"]["output"],
            json!(ascii(MAX_PAYLOAD_TEXT_BYTES))
        );
        assert!(envelope["payload"].get("truncated").is_none());
    }

    let straddling = json!({
        "output": format!("{}€{}", ascii(MAX_PAYLOAD_TEXT_BYTES - 2), ascii(4))
    })
    .to_string();
    let boundary = emit_in(dir.path(), &args, Some(&straddling), &[]);
    assert_eq!(boundary.code, 0, "{}", boundary.stderr);
    for envelope in [&boundary.lines()[0], &lines_of(dir.path())[2]] {
        assert_eq!(
            envelope["payload"]["output"],
            json!(ascii(MAX_PAYLOAD_TEXT_BYTES - 2))
        );
        assert_eq!(envelope["payload"]["truncated"], json!(true));
    }
    // The file is valid UTF-8 line by line, which reading it as text proved.
    assert_eq!(lines_of(dir.path()).len(), 3);
}

/// Every table entry, redacted before the line is written: a value under each
/// credential-shaped environment name, and a token with each prefix.
#[test]
fn events_emit_redacts_credential_shaped_values_before_writing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let secrets: Vec<(String, String)> = CREDENTIAL_WORDS
        .iter()
        .map(|word| {
            (
                format!("ONEMESSAGEBUS_E2E_{word}"),
                format!("secret-under-{}-0123456789", word.to_ascii_lowercase()),
            )
        })
        .collect();
    let env: Vec<(&str, &str)> = secrets
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let prefixed: Vec<String> = CREDENTIAL_PREFIXES
        .iter()
        .map(|prefix| format!("{prefix}abcdefghijklmnop"))
        .collect();
    let mut payload = serde_json::Map::new();
    for (word, (_, value)) in CREDENTIAL_WORDS.iter().zip(&secrets) {
        payload.insert((*word).to_owned(), json!(format!("printed {value} here")));
    }
    payload.insert("tokens".to_owned(), json!(prefixed.join(" ")));
    payload.insert("plain".to_owned(), json!("nothing to see"));
    let emitted = emit_in(
        dir.path(),
        &["--kind", "push", "--stream", "s"],
        Some(&Value::Object(payload).to_string()),
        &env,
    );
    assert_eq!(emitted.code, 0, "{}", emitted.stderr);
    let raw = std::fs::read_to_string(dir.path().join("stream.ndjson")).expect("the file");
    for (_, value) in &secrets {
        assert!(!raw.contains(value.as_str()), "{value} leaked to the file");
        assert!(
            !emitted.stdout.contains(value.as_str()),
            "{value} leaked to stdout"
        );
    }
    for token in &prefixed {
        assert!(!raw.contains(token.as_str()), "{token} leaked to the file");
    }
    let envelope = &lines_of(dir.path())[0];
    for word in CREDENTIAL_WORDS {
        assert_eq!(
            envelope["payload"][*word],
            json!(format!("printed {REDACTED} here"))
        );
    }
    assert_eq!(
        envelope["payload"]["tokens"],
        json!(vec![REDACTED; prefixed.len()].join(" "))
    );
    assert_eq!(envelope["payload"]["plain"], json!("nothing to see"));
}

/// Several processes appending to one file at once leave `seq` exactly
/// `1..=n`, no duplicate and no gap, every line an envelope: three writers,
/// each a thread that spawns the binary forty times in a row, so at any moment
/// up to three copies of it are appending to the file together.
#[test]
fn concurrent_emitters_leave_one_gapless_series() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let stream = dir.path().join("shared.ndjson");
    let per_writer = 40u64;
    let writers = 3u64;
    let handles: Vec<_> = (0..writers)
        .map(|which| {
            let stream = stream.clone();
            std::thread::spawn(move || {
                for n in 0..per_writer {
                    let mut child = onemessagebus()
                        .args(["events", "emit"])
                        .arg(&stream)
                        .args(["--kind", "tick", "--stream"])
                        .arg(format!("writer-{which}"))
                        .args(["--source", "vcs"])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::piped())
                        .spawn()
                        .expect("the binary spawns");
                    {
                        use std::io::Write as _;
                        let mut stdin = child.stdin.take().expect("a stdin pipe");
                        stdin
                            .write_all(json!({ "n": n }).to_string().as_bytes())
                            .expect("stdin is written");
                    }
                    let output = child.wait_with_output().expect("the binary exits");
                    assert!(
                        output.status.success(),
                        "writer {which} emit {n} failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("a writer thread");
    }
    let mut seqs = Vec::new();
    for line in std::fs::read_to_string(&stream).expect("the file").lines() {
        let envelope: Value = serde_json::from_str(line).expect("every line is an envelope");
        seqs.push(envelope["seq"].as_u64().expect("seq"));
    }
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=per_writer * writers).collect::<Vec<u64>>());
}

#[test]
fn events_emit_refuses_an_empty_kind_stream_or_label_key_and_renders_typed_labels() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let no_kind = emit_in(
        dir.path(),
        &["--kind", " ", "--stream", "s"],
        Some("{}"),
        &[],
    );
    assert_eq!(no_kind.code, 2);
    assert!(no_kind.stderr.contains("--kind"), "{}", no_kind.stderr);
    let no_stream = emit_in(
        dir.path(),
        &["--kind", "k", "--stream", ""],
        Some("{}"),
        &[],
    );
    assert_eq!(no_stream.code, 2);
    assert!(
        no_stream.stderr.contains("--stream"),
        "{}",
        no_stream.stderr
    );
    let no_key = emit_in(
        dir.path(),
        &["--kind", "k", "--stream", "s", "--label", "=v"],
        Some("{}"),
        &[],
    );
    assert_eq!(no_key.code, 2);
    assert!(no_key.stderr.contains("names no key"), "{}", no_key.stderr);
    assert!(
        !dir.path().join("stream.ndjson").exists(),
        "nothing was appended"
    );

    let text = emit_in(
        dir.path(),
        &[
            "--kind", "k", "--stream", "s", "--label", "round=2", "--label", "node=svc",
            "--format", "text",
        ],
        Some(r#"{"n":1}"#),
        &[],
    );
    assert_eq!(text.code, 0, "{}", text.stderr);
    assert!(
        text.stdout.contains(" round=2 node=svc payload={\"n\":1}"),
        "{}",
        text.stdout
    );
}

#[test]
fn events_merge_renders_artifacts_and_reports_a_line_of_another_profile() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let golden = std::fs::read_to_string(fixture("golden/envelope-v2.json")).expect("golden");
    let envelope: Value = serde_json::from_str(&golden).expect("JSON");
    let stream = dir.path().join("with-artifacts.ndjson");
    std::fs::write(
        &stream,
        format!(
            "{}\n{{\"v\":1,\"ts\":\"2026-09-13T00:00:00.000Z\",\"stream\":\"x\",\"seq\":1,\"source\":\"billing\",\"kind\":\"k\"}}\n",
            envelope
        ),
    )
    .expect("written");
    let merged = run(
        &[
            "events",
            "merge",
            stream.to_str().expect("UTF-8"),
            "--format",
            "text",
        ],
        None,
    );
    assert_eq!(merged.code, 0, "{}", merged.stderr);
    assert_eq!(merged.stdout.lines().count(), 1, "{}", merged.stdout);
    assert!(
        merged
            .stdout
            .contains(" artifacts=[{\"id\":\"gate-log\",\"kind\":\"log\",\"bytes\":8192}]"),
        "{}",
        merged.stdout
    );
    assert!(merged.stdout.contains(" attempt=1"), "{}", merged.stdout);
    assert!(
        merged.stderr.contains("not an envelope"),
        "{}",
        merged.stderr
    );
    assert!(merged.stderr.contains("billing"), "{}", merged.stderr);
}
