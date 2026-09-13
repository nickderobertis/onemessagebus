//! The `schema` verbs, through the built binary.

use serde_json::{json, Value};

use crate::support::{fixture, run, run_in};

/// The ids the profile registers, which `schema list` prints with nothing
/// else.
const PROFILE_IDS: &[&str] = &[
    "agent.artifact-ref@1",
    "agent.event-envelope@1",
    "agent.event-envelope@2",
    "agent.event-filter@1",
    "agent.labels@1",
    "agent.reply-envelope@2",
    "agent.reply-envelope@3",
];

/// The committed Rust declaration `schema gen --lang rust agent.artifact-ref@1`
/// printed, compiled here: `generated_regenerates_the_document` holds it to the
/// binary's current output and to the registered document.
mod generated {
    include!("../generated/artifact_ref.rs");
}

#[test]
fn schema_list_prints_every_registered_id_and_nothing_else() {
    let listed = run(&["schema", "list"], None);
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    let entries: Vec<Value> = serde_json::from_str(&listed.stdout).expect("a JSON list");
    let ids: Vec<&str> = entries
        .iter()
        .map(|entry| entry["id"].as_str().expect("an id"))
        .collect();
    assert_eq!(ids, PROFILE_IDS);
    assert_eq!(entries[1]["family"], json!("agent.event-envelope"));
    assert_eq!(entries[1]["version"], json!(1));
    assert!(listed.stderr.is_empty(), "{}", listed.stderr);

    let text = run(&["schema", "list", "--format", "text"], None);
    assert_eq!(text.code, 0);
    assert_eq!(text.stdout.lines().collect::<Vec<_>>(), PROFILE_IDS);
}

#[test]
fn schema_check_accepts_a_payload_on_stdin_and_on_file_alike() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let payload = std::fs::read_to_string(fixture("golden/envelope-v2.json")).expect("golden");
    let file = dir.path().join("payload.json");
    std::fs::write(&file, &payload).expect("written");

    let on_stdin = run_in(
        dir.path(),
        &["schema", "check", "agent.event-envelope@2"],
        Some(&payload),
        &[],
    );
    assert_eq!(on_stdin.code, 0, "{}", on_stdin.stderr);
    assert!(on_stdin.stdout.is_empty() && on_stdin.stderr.is_empty());

    let on_file = run_in(
        dir.path(),
        &[
            "schema",
            "check",
            "agent.event-envelope@2",
            "--file",
            "payload.json",
        ],
        None,
        &[],
    );
    assert_eq!(on_file.code, 0, "{}", on_file.stderr);
}

#[test]
fn schema_check_refuses_a_violating_payload_naming_the_id_and_the_pointer() {
    let violating = json!({ "run_id": "R", "round": "two" }).to_string();
    let refused = run(&["schema", "check", "agent.labels@1"], Some(&violating));
    assert_eq!(refused.code, 1);
    assert!(refused.stdout.is_empty());
    assert!(
        refused.stderr.contains("agent.labels@1"),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains("/round"), "{}", refused.stderr);

    let unknown = run(&["schema", "check", "agent.nothing@1"], Some("{}"));
    assert_eq!(unknown.code, 2);
    assert!(
        unknown.stderr.contains("agent.nothing@1"),
        "{}",
        unknown.stderr
    );

    let malformed = run(&["schema", "check", "agent.labels"], Some("{}"));
    assert_eq!(malformed.code, 2);
    assert!(
        malformed.stderr.contains("names no version"),
        "{}",
        malformed.stderr
    );

    let not_json = run(&["schema", "check", "agent.labels@1"], Some("not json"));
    assert_eq!(not_json.code, 2);
    assert!(not_json.stderr.contains("not JSON"), "{}", not_json.stderr);
}

/// The clap tree admits no positional a payload could be read as.
#[test]
fn schema_check_refuses_a_payload_passed_as_an_argument() {
    let refused = run(
        &["schema", "check", "agent.labels@1", r#"{"run_id":"R"}"#],
        None,
    );
    assert_eq!(refused.code, 2);
    assert!(
        refused.stderr.contains("unexpected argument"),
        "{}",
        refused.stderr
    );
}

#[test]
fn schema_gen_json_prints_the_registered_document_byte_for_byte() {
    let printed = run(
        &["schema", "gen", "--lang", "json", "agent.artifact-ref@1"],
        None,
    );
    assert_eq!(printed.code, 0, "{}", printed.stderr);
    let registry = onemessagebus_agent::registry();
    let id = "agent.artifact-ref@1".parse().expect("id");
    let document = registry.schema(&id).expect("registered");
    let expected = format!(
        "{}\n",
        serde_json::to_string_pretty(document).expect("JSON")
    );
    assert_eq!(printed.stdout, expected);
}

#[test]
fn schema_gen_rust_prints_a_declaration_that_compiles_and_regenerates_the_document() {
    let printed = run(
        &["schema", "gen", "--lang", "rust", "agent.artifact-ref@1"],
        None,
    );
    assert_eq!(printed.code, 0, "{}", printed.stderr);
    let committed = include_str!("../generated/artifact_ref.rs");
    assert_eq!(
        printed.stdout, committed,
        "tests/generated/artifact_ref.rs is not what the binary prints; regenerate it with \
         `onemessagebus schema gen --lang rust agent.artifact-ref@1`"
    );
    // The committed declaration compiled into this test binary regenerates the
    // registered document.
    let regenerated = schemars::schema_for!(generated::ArtifactRef).to_value();
    let registry = onemessagebus_agent::registry();
    let id = "agent.artifact-ref@1".parse().expect("id");
    assert_eq!(&regenerated, registry.schema(&id).expect("registered"));
    let value: generated::ArtifactRef =
        serde_json::from_value(json!({ "id": "a-91", "kind": "log", "bytes": 21400 }))
            .expect("the declaration reads the documented artifact");
    assert_eq!(value.bytes, 21400);
}

#[test]
fn schema_gen_refuses_an_unknown_id_and_an_unsupported_language_by_name() {
    let unknown = run(
        &["schema", "gen", "--lang", "json", "agent.nothing@1"],
        None,
    );
    assert_eq!(unknown.code, 2);
    assert!(
        unknown.stderr.contains("agent.nothing@1"),
        "{}",
        unknown.stderr
    );
    for lang in ["python", "typescript"] {
        let unsupported = run(
            &["schema", "gen", "--lang", lang, "agent.artifact-ref@1"],
            None,
        );
        assert_eq!(unsupported.code, 2, "{lang}");
        assert!(unsupported.stderr.contains(lang), "{}", unsupported.stderr);
        assert!(unsupported.stdout.is_empty());
    }
    let nonsense = run(
        &["schema", "gen", "--lang", "cobol", "agent.artifact-ref@1"],
        None,
    );
    assert_eq!(nonsense.code, 2);
    assert!(nonsense.stderr.contains("cobol"), "{}", nonsense.stderr);
}

#[test]
fn schema_register_makes_an_id_answer_in_list_and_govern_check_in_a_later_invocation() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let registry = dir.path().join("registry");
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Finding",
        "type": "object",
        "properties": { "severity": { "type": "string" }, "line": { "type": "integer" } },
        "required": ["severity", "line"],
        "additionalProperties": false
    });
    std::fs::write(dir.path().join("finding.json"), schema.to_string()).expect("written");
    let registry_arg = registry.to_str().expect("UTF-8 path");

    let registered = run_in(
        dir.path(),
        &[
            "schema",
            "register",
            "agent.finding@1",
            "--file",
            "finding.json",
            "--registry",
            registry_arg,
        ],
        None,
        &[],
    );
    assert_eq!(registered.code, 0, "{}", registered.stderr);
    let stored: Value = serde_json::from_str(
        &std::fs::read_to_string(registry.join("agent.finding@1.json")).expect("the file"),
    )
    .expect("a registry document");
    assert_eq!(stored["id"], json!("agent.finding@1"));
    assert_eq!(stored["schema"], schema);

    // A later invocation over the same directory, named by the flag.
    let listed = run_in(
        dir.path(),
        &[
            "schema",
            "list",
            "--registry",
            registry_arg,
            "--format",
            "text",
        ],
        None,
        &[],
    );
    assert!(listed.stdout.lines().any(|line| line == "agent.finding@1"));
    assert_eq!(listed.stdout.lines().count(), PROFILE_IDS.len() + 1);

    // And named by the environment variable, governing a check.
    let env = [("ONEMESSAGEBUS_REGISTRY", registry_arg)];
    let conforming = run_in(
        dir.path(),
        &["schema", "check", "agent.finding@1"],
        Some(r#"{"severity":"high","line":3}"#),
        &env,
    );
    assert_eq!(conforming.code, 0, "{}", conforming.stderr);
    let violating = run_in(
        dir.path(),
        &["schema", "check", "agent.finding@1"],
        Some(r#"{"severity":"high","line":"three"}"#),
        &env,
    );
    assert_eq!(violating.code, 1);
    assert!(
        violating.stderr.contains("agent.finding@1: at /line"),
        "{}",
        violating.stderr
    );

    // A second registration of the same document is fine; a different one is
    // refused naming the id, and the file is untouched.
    let again = run_in(
        dir.path(),
        &[
            "schema",
            "register",
            "agent.finding@1",
            "--file",
            "finding.json",
        ],
        None,
        &env,
    );
    assert_eq!(again.code, 0, "{}", again.stderr);
    std::fs::write(
        dir.path().join("other.json"),
        json!({ "type": "object", "properties": { "other": { "type": "string" } } }).to_string(),
    )
    .expect("written");
    let conflict = run_in(
        dir.path(),
        &[
            "schema",
            "register",
            "agent.finding@1",
            "--file",
            "other.json",
        ],
        None,
        &env,
    );
    assert_eq!(conflict.code, 2);
    assert!(
        conflict.stderr.contains("agent.finding@1"),
        "{}",
        conflict.stderr
    );
    assert!(
        conflict.stderr.contains("different document"),
        "{}",
        conflict.stderr
    );
    let untouched: Value = serde_json::from_str(
        &std::fs::read_to_string(registry.join("agent.finding@1.json")).expect("the file"),
    )
    .expect("JSON");
    assert_eq!(untouched["schema"], schema);

    // Nor may a directory contradict the profile.
    let profile_conflict = run_in(
        dir.path(),
        &[
            "schema",
            "register",
            "agent.labels@1",
            "--file",
            "other.json",
        ],
        None,
        &env,
    );
    assert_eq!(profile_conflict.code, 2);
    assert!(
        profile_conflict.stderr.contains("agent.labels@1"),
        "{}",
        profile_conflict.stderr
    );

    // With no registry directory at all, there is nowhere to write.
    let nowhere = run_in(
        dir.path(),
        &[
            "schema",
            "register",
            "agent.other@1",
            "--file",
            "other.json",
        ],
        None,
        &[],
    );
    assert_eq!(nowhere.code, 2);
    assert!(nowhere.stderr.contains("--registry"), "{}", nowhere.stderr);
}
