//! The `events` verbs, through the built binary.

use std::path::Path;
use std::process::Stdio;

use onemessagebus::{
    Open, Source, CREDENTIAL_PREFIXES, CREDENTIAL_WORDS, MAX_PAYLOAD_TEXT_BYTES, REDACTED,
};
use serde_json::{json, Value};

use crate::support::{ascii, fixture, onemessagebus, run, run_in, Run};

/// The streams the journeys merge: three producers of one commerce run under
/// the open profile, their timestamps interleaved and two of them tied across
/// streams. One producer stamps an integer label and an artifact, which
/// `events emit` does not write but `events merge` carries.
const RECORDED: &[&str] = &[
    "billing-run.ndjson",
    "shipping-run.ndjson",
    "ledger-events.ndjson",
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
        r#"{"include":[{"source":"billing","kind":"invoice-*"}]}"#,
    ]);
    assert_eq!(filtered.code, 0, "{}", filtered.stderr);
    let printed = filtered.lines();
    assert_eq!(printed.len(), 3, "{}", filtered.stdout);
    assert!(printed.iter().all(|envelope| {
        envelope["source"] == json!("billing")
            && envelope["kind"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("invoice-"))
    }));

    // A label the open profile reserves nothing about is matched as text, and a
    // matcher naming a label an envelope did not stamp does not match it.
    let by_label = merge(&[
        "--filter",
        r#"{"include":[{"tenant":"globex"}],"exclude":[{"kind":"heartbeat"}]}"#,
    ]);
    assert_eq!(by_label.code, 0, "{}", by_label.stderr);
    let globex: Vec<(String, u64)> = by_label
        .lines()
        .iter()
        .map(|envelope| {
            (
                envelope["stream"].as_str().expect("stream").to_owned(),
                envelope["seq"].as_u64().expect("seq"),
            )
        })
        .collect();
    assert_eq!(
        globex,
        [("ledger-7".to_owned(), 2), ("billing-1".to_owned(), 4)]
    );

    // A YAML file spelling of the same filter.
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(
        dir.path().join("filter.yaml"),
        "include:\n  - source: billing\n    kind: \"invoice-*\"\n",
    )
    .expect("written");
    let files = recorded_args();
    let mut args: Vec<&str> = vec!["events", "merge"];
    args.extend(files.iter().map(String::as_str));
    args.extend(["--filter", "filter.yaml"]);
    let from_file = run_in(dir.path(), &args, None, &[]);
    assert_eq!(from_file.code, 0, "{}", from_file.stderr);
    assert_eq!(from_file.stdout, filtered.stdout);

    // `open` is the default: naming it reads the same stream the same way.
    let explicit = merge(&["--profile", "open"]);
    assert_eq!(explicit.code, 0, "{}", explicit.stderr);
    assert_eq!(
        explicit.stdout,
        merge(&[]).stdout,
        "the default profile is the open one"
    );

    // A profile this build does not link — the retired agent one among them —
    // is refused by the one generic refusal, which names `open` alone.
    for name in ["agent", "billing"] {
        let unknown = merge(&["--profile", name]);
        assert_eq!(unknown.code, 2, "{name}: {}", unknown.stderr);
        assert!(unknown.stdout.is_empty(), "{name}: {}", unknown.stdout);
        assert_eq!(
            unknown.stderr,
            format!(
                "onemessagebus: `{name}` is not a profile this build links; choose one of: open\n"
            ),
            "{name}"
        );
    }
}

#[test]
fn events_merge_refuses_a_malformed_filter_naming_list_index_and_matcher() {
    let empty = merge(&["--filter", r#"{"include":[{"kind":"x"},{}]}"#]);
    assert_eq!(empty.code, 2);
    assert!(empty.stdout.is_empty());
    assert!(empty.stderr.contains("include[1] {}"), "{}", empty.stderr);

    let blank = merge(&["--filter", r#"{"exclude":[{"tenant":""}]}"#]);
    assert_eq!(blank.code, 2);
    assert!(
        blank.stderr.contains(r#"exclude[0] {"tenant":""}"#),
        "{}",
        blank.stderr
    );

    let unknown = merge(&["--filter", r#"{"only":[{"kind":"x"}]}"#]);
    assert_eq!(unknown.code, 2);
    assert!(unknown.stdout.is_empty());
    // Named by the field, with the lists a filter does take.
    for said in ["unknown field `only`", "expected `include` or `exclude`"] {
        assert!(unknown.stderr.contains(said), "{}", unknown.stderr);
    }

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
        let labels: String = envelope["labels"]
            .as_object()
            .expect("labels")
            .iter()
            .map(|(key, value)| match value {
                Value::String(text) => format!(" {key}={text}"),
                other => format!(" {key}={other}"),
            })
            .collect();
        assert!(text.contains(&labels), "{text}\n{labels}");
    }
}

#[test]
fn events_merge_over_a_torn_file_prints_the_whole_records_and_reports_the_tail() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let whole = std::fs::read_to_string(fixture("billing-run.ndjson")).expect("recorded");
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
        "billing",
        "--label",
        "tenant=acme",
        "--label",
        "attempt=2",
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
    assert_eq!(envelope["source"], json!("billing"));
    assert_eq!(envelope["seq"], json!(1));
    assert_eq!(envelope["v"], json!(1));
    // `open` reserves no key, so every label is the text it was given.
    assert_eq!(
        envelope["labels"],
        json!({ "tenant": "acme", "attempt": "2", "workstream": "w" })
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
    let printed = on_file.lines();
    assert_eq!(printed.len(), 1);
    assert_eq!(
        printed[0], written[1],
        "the envelope printed is the one appended"
    );
    for envelope in [&printed[0], &written[1]] {
        assert_eq!(envelope["kind"], json!("thing-done"));
        assert_eq!(envelope["stream"], json!("s-1"));
        assert_eq!(envelope["source"], json!("billing"));
        assert_eq!(envelope["seq"], json!(2));
        assert_eq!(
            envelope["labels"],
            json!({ "tenant": "acme", "attempt": "2", "workstream": "w" })
        );
        assert_eq!(envelope["payload"], json!({ "n": 2 }));
    }

    let text = emit_in(
        dir.path(),
        &[
            "--kind", "k", "--stream", "s-1", "--source", "billing", "--format", "text",
        ],
        Some("{}"),
        &[],
    );
    assert_eq!(text.code, 0, "{}", text.stderr);
    assert!(
        text.stdout.contains(" billing k stream=s-1 seq=3 v=1"),
        "{}",
        text.stdout
    );
}

#[test]
fn events_emit_refuses_bad_input_by_name() {
    let dir = tempfile::tempdir().expect("a temp dir");
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

/// `open` is the one profile this build links, and the default: with the
/// option omitted and with it named, any source word is written at version 1,
/// and with no word the profile's own default is stamped. Naming any other
/// profile — the retired agent one among them — is refused by the generic
/// refusal, which names `open` alone and no crate, and appends nothing.
#[test]
fn events_emit_writes_under_the_open_profile_by_default_and_refuses_any_other() {
    for source in ["billing", "shipping", "anything-at-all"] {
        for profile in [Some("open"), None] {
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
                json!(1),
                "{source} {profile:?} on the file"
            );
            assert_eq!(
                emitted.lines()[0],
                written[0],
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
    assert_eq!(lines_of(dir.path())[0]["source"], json!("onemessagebus"));
    assert_eq!(lines_of(dir.path())[0]["v"], json!(1));

    for name in ["agent", "billing"] {
        let refused = emit_in(
            dir.path(),
            &["--kind", "k", "--stream", "s", "--profile", name],
            Some("{}"),
            &[],
        );
        assert_eq!(refused.code, 2, "{name}: {}", refused.stderr);
        assert!(refused.stdout.is_empty(), "{name}: {}", refused.stdout);
        assert_eq!(
            refused.stderr,
            format!(
                "onemessagebus: `{name}` is not a profile this build links; choose one of: open\n"
            ),
            "{name}"
        );
        assert!(
            !refused.stderr.contains("onemessagebus-"),
            "the refusal names a crate: {}",
            refused.stderr
        );
    }
    assert_eq!(
        lines_of(dir.path()).len(),
        1,
        "a refused emit appended nothing"
    );
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
    // However deeply nested: a list of tokens, and an object inside it.
    payload.insert(
        "nested".to_owned(),
        json!([prefixed[0], { "deeper": [prefixed[0], 7] }]),
    );
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
        assert!(
            !emitted.stdout.contains(token.as_str()),
            "{token} leaked to stdout"
        );
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
    assert_eq!(
        envelope["payload"]["nested"],
        json!([REDACTED, { "deeper": [REDACTED, 7] }])
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
                        .args(["--source", "billing"])
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
        // And the core's own type reads every line whole, as a consumer does.
        let typed: onemessagebus::Envelope<Open> = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("a line is not an open envelope: {e}: {line}"));
        assert_eq!(typed.source, Source::from("billing"), "{line}");
        assert_eq!(typed.kind.as_str(), "tick", "{line}");
        assert!(typed.stream.starts_with("writer-"), "{line}");
        assert_eq!(Some(typed.seq), envelope["seq"].as_u64(), "{line}");
        seqs.push(typed.seq);
    }
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=per_writer * writers).collect::<Vec<u64>>());
}

/// A writer killed mid-line leaves a torn tail: the next `events emit` cuts it
/// away, says on stderr how many bytes it cut, where and from which file, and
/// appends its envelope as a whole line numbered after the whole records only.
#[test]
fn events_emit_onto_a_torn_file_heals_the_tail_and_says_so() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let args = ["--kind", "tick", "--stream", "s-1", "--source", "billing"];
    for n in 1..=2 {
        let emitted = emit_in(dir.path(), &args, Some(&json!({ "n": n }).to_string()), &[]);
        assert_eq!(emitted.code, 0, "{}", emitted.stderr);
        assert!(emitted.stderr.is_empty(), "{}", emitted.stderr);
    }
    let path = dir.path().join("stream.ndjson");
    let whole = std::fs::read_to_string(&path).expect("the stream file");
    let last = whole.lines().last().expect("a last line");
    let tail = &last[..last.len() / 2];
    std::fs::write(&path, format!("{whole}{tail}")).expect("the torn tail is written");

    let healed = emit_in(dir.path(), &args, Some(r#"{"n":3}"#), &[]);
    assert_eq!(healed.code, 0, "{}", healed.stderr);
    assert!(
        healed.stderr.contains(&format!(
            "onemessagebus: healed a torn record of {} bytes at byte {} of stream.ndjson",
            tail.len(),
            whole.len()
        )),
        "{}",
        healed.stderr
    );
    let after = std::fs::read_to_string(&path).expect("the stream file");
    assert!(
        after.starts_with(&whole),
        "the whole records are kept byte for byte: {after}"
    );
    assert!(
        after.ends_with('\n'),
        "the new record is a whole line: {after}"
    );
    let written = lines_of(dir.path());
    assert_eq!(written.len(), 3, "{after}");
    assert_eq!(
        written.iter().map(|e| e["seq"].clone()).collect::<Vec<_>>(),
        [json!(1), json!(2), json!(3)],
        "the torn tail is not a record, so the new envelope is the third"
    );
    assert_eq!(written[2]["payload"], json!({ "n": 3 }));
    assert_eq!(healed.lines(), [written[2].clone()], "printed is written");
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
            "--kind",
            "k",
            "--stream",
            "s",
            "--label",
            "attempt=2",
            "--label",
            "node=svc",
            "--format",
            "text",
        ],
        Some(r#"{"n":1}"#),
        &[],
    );
    assert_eq!(text.code, 0, "{}", text.stderr);
    assert!(
        text.stdout
            .contains(" attempt=2 node=svc payload={\"n\":1}"),
        "{}",
        text.stdout
    );
}

/// `events emit` authors a kind, so it refuses one that is not kebab-case by
/// name and appends nothing, while `events merge` still carries such a kind a
/// sibling wrote.
#[test]
fn events_emit_refuses_a_kind_that_is_not_kebab_case_and_merge_still_carries_one() {
    let dir = tempfile::tempdir().expect("a temp dir");
    for kind in [
        "Change-Merged",
        "change_merged",
        "change--merged",
        "-change",
        "change-",
        "change merged",
        "changé",
    ] {
        // Joined to its flag, so a kind with a leading hyphen reaches the verb
        // rather than being read as a flag of its own.
        let flag = format!("--kind={kind}");
        let refused = emit_in(dir.path(), &[&flag, "--stream", "s"], Some("{}"), &[]);
        assert_eq!(refused.code, 2, "{kind}: {}", refused.stderr);
        assert!(refused.stdout.is_empty(), "{kind}: {}", refused.stdout);
        assert!(
            refused.stderr.contains(&format!("--kind `{kind}`")),
            "{}",
            refused.stderr
        );
        assert!(
            refused.stderr.contains("not kebab-case"),
            "{}",
            refused.stderr
        );
    }
    assert!(
        !dir.path().join("stream.ndjson").exists(),
        "nothing was appended"
    );
    for kind in ["k", "change-merged", "round-2-done"] {
        let admitted = emit_in(
            dir.path(),
            &["--kind", kind, "--stream", "s"],
            Some("{}"),
            &[],
        );
        assert_eq!(admitted.code, 0, "{kind}: {}", admitted.stderr);
        assert_eq!(admitted.lines()[0]["kind"], json!(kind));
    }

    let relayed = dir.path().join("relayed.ndjson");
    std::fs::write(
        &relayed,
        "{\"v\":1,\"ts\":\"2026-09-13T00:00:00.000Z\",\"stream\":\"x\",\"seq\":1,\"source\":\"shipping\",\"kind\":\"Sibling_Kind\"}\n",
    )
    .expect("written");
    let merged = run(&["events", "merge", relayed.to_str().expect("UTF-8")], None);
    assert_eq!(merged.code, 0, "{}", merged.stderr);
    assert!(merged.stderr.is_empty(), "{}", merged.stderr);
    assert_eq!(merged.lines()[0]["kind"], json!("Sibling_Kind"));
}

/// A line carrying a top-level key the profile does not declare is not an
/// envelope of it: `open` declares no dimension, so a stream a vocabulary with
/// one wrote is reported line by line and left out, never read as something
/// else.
#[test]
fn events_merge_renders_artifacts_and_reports_a_line_of_another_profile() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let recorded = std::fs::read_to_string(fixture("shipping-run.ndjson")).expect("recorded");
    let packed = recorded.lines().next().expect("the first line");
    let stream = dir.path().join("with-artifacts.ndjson");
    std::fs::write(
        &stream,
        format!(
            "{packed}\n{{\"v\":1,\"ts\":\"2026-09-13T00:00:00.000Z\",\"stream\":\"x\",\"seq\":1,\"source\":\"billing\",\"kind\":\"k\",\"region\":\"eu\"}}\n"
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
            .contains(" artifacts=[{\"id\":\"label-pdf\",\"kind\":\"document\",\"bytes\":2048}]"),
        "{}",
        merged.stdout
    );
    assert!(merged.stdout.contains(" attempt=1"), "{}", merged.stdout);
    assert!(
        merged.stderr.contains("not an envelope"),
        "{}",
        merged.stderr
    );
    assert!(
        merged.stderr.contains("unknown field `region`"),
        "{}",
        merged.stderr
    );
}
