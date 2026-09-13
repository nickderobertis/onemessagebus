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
    "agent.note@1",
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

    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(dir.path().join("labels.json"), &violating).expect("written");
    let on_file = run_in(
        dir.path(),
        &["schema", "check", "agent.labels@1", "--file", "labels.json"],
        None,
        &[],
    );
    assert_eq!(on_file.code, 1, "{}", on_file.stderr);
    assert!(on_file.stdout.is_empty());
    assert!(
        on_file.stderr.contains("agent.labels@1"),
        "{}",
        on_file.stderr
    );
    assert!(on_file.stderr.contains("/round"), "{}", on_file.stderr);

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
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    let mut expected: Vec<&str> = PROFILE_IDS.to_vec();
    expected.push("agent.finding@1");
    expected.sort_unstable();
    let mut printed: Vec<&str> = listed.stdout.lines().collect();
    printed.sort_unstable();
    assert_eq!(
        printed, expected,
        "the profile's ids and the registered one, each once"
    );

    let governed = run_in(
        dir.path(),
        &[
            "schema",
            "check",
            "agent.finding@1",
            "--registry",
            registry_arg,
        ],
        Some(r#"{"severity":"high","line":"three"}"#),
        &[],
    );
    assert_eq!(governed.code, 1, "{}", governed.stderr);
    assert!(
        governed.stderr.contains("agent.finding@1: at /line"),
        "{}",
        governed.stderr
    );
    let admitted = run_in(
        dir.path(),
        &[
            "schema",
            "check",
            "agent.finding@1",
            "--registry",
            registry_arg,
        ],
        Some(r#"{"severity":"high","line":3}"#),
        &[],
    );
    assert_eq!(admitted.code, 0, "{}", admitted.stderr);

    let env = [("ONEMESSAGEBUS_REGISTRY", registry_arg)];
    let listed_by_env = run_in(
        dir.path(),
        &["schema", "list", "--format", "text"],
        None,
        &env,
    );
    assert_eq!(listed_by_env.code, 0, "{}", listed_by_env.stderr);
    assert_eq!(listed_by_env.stdout, listed.stdout);
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

/// The committed declaration for `rich::Rich`'s schema, compiled here.
mod generated_rich {
    include!("../generated/rich.rs");
}

/// The renderer over every construct it covers: a schema registered from a
/// file, rendered through the binary, held to the committed declaration, which
/// compiles and regenerates the document the hand-written type emits.
#[test]
fn schema_gen_rust_covers_every_construct_the_renderer_declares() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let registry = dir.path().join("registry");
    let document = schemars::schema_for!(crate::rich::Rich).to_value();
    std::fs::write(dir.path().join("rich.json"), document.to_string()).expect("written");
    let registry_arg = registry.to_str().expect("UTF-8");
    let registered = run_in(
        dir.path(),
        &[
            "schema",
            "register",
            "test.rich@1",
            "--file",
            "rich.json",
            "--registry",
            registry_arg,
        ],
        None,
        &[],
    );
    assert_eq!(registered.code, 0, "{}", registered.stderr);
    let printed = run_in(
        dir.path(),
        &[
            "schema",
            "gen",
            "--lang",
            "rust",
            "test.rich@1",
            "--registry",
            registry_arg,
        ],
        None,
        &[],
    );
    assert_eq!(printed.code, 0, "{}", printed.stderr);
    let committed = include_str!("../generated/rich.rs");
    assert_eq!(
        printed.stdout, committed,
        "tests/generated/rich.rs is not what the binary prints; regenerate it by registering \
         rich::Rich's schema and running `onemessagebus schema gen --lang rust test.rich@1`"
    );
    let regenerated = schemars::schema_for!(generated_rich::Rich).to_value();
    assert_eq!(
        regenerated, document,
        "the generated declaration does not regenerate the document"
    );
    let value: generated_rich::Rich = serde_json::from_value(json!({
        "name": "n", "count": 1, "total": 2, "delta": -3, "ratio": 0.5, "enabled": true,
        "tags": ["a"], "level": "low", "inner": { "id": "i", "more": 1 }, "extra": null,
        "bag": { "k": [1] }, "counts": { "x": 9 }, "kebab-key": "k"
    }))
    .expect("the declaration reads a conforming document");
    assert_eq!(value.level, generated_rich::Level::Low);
    assert_eq!(value.label, "");
    assert_eq!(value.inner.id, "i");
}

/// Register `document` under `id` in `registry`, from a file in `dir`, and
/// hand back what `schema gen --lang rust` then prints for it.
fn register_and_gen_rust(
    dir: &std::path::Path,
    registry: &str,
    id: &str,
    document: &Value,
) -> crate::support::Run {
    let file = format!("{id}.json");
    std::fs::write(dir.join(&file), document.to_string()).expect("written");
    let registered = run_in(
        dir,
        &[
            "schema",
            "register",
            id,
            "--file",
            &file,
            "--registry",
            registry,
        ],
        None,
        &[],
    );
    assert_eq!(registered.code, 0, "{id}: {}", registered.stderr);
    run_in(
        dir,
        &[
            "schema",
            "gen",
            "--lang",
            "rust",
            id,
            "--registry",
            registry,
        ],
        None,
        &[],
    )
}

/// A registered document is anyone's JSON, and its names are spliced into the
/// Rust `schema gen` prints: one that cannot become an identifier is refused
/// as input, naming the id, the pointer and the name, and nothing is printed.
#[test]
fn schema_gen_rust_refuses_a_name_that_cannot_become_an_identifier() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let registry = dir.path().join("registry");
    let registry_arg = registry.to_str().expect("UTF-8");
    let cases = [
        (
            "test.bad-title@1",
            json!({ "title": "9Lives", "type": "object", "properties": { "a": { "type": "string" } } }),
            "/title",
            "the title \"9Lives\" is not a Rust identifier: it starts with a digit",
        ),
        (
            "test.bad-property@1",
            json!({ "title": "Finding", "type": "object", "properties": { "a.b": { "type": "string" } } }),
            "/properties/a.b",
            "the property name \"a.b\" is not a Rust identifier: '.' is not an ASCII letter, digit or underscore",
        ),
        (
            "test.bad-word@1",
            json!({
                "title": "Finding",
                "type": "object",
                "properties": { "level": { "$ref": "#/$defs/Level" } },
                "$defs": { "Level": { "enum": ["low", "9"] } }
            }),
            "/$defs/Level/enum/1",
            "the enum value \"9\" is not a Rust identifier: it starts with a digit",
        ),
        (
            "test.bad-keyword@1",
            json!({ "title": "Finding", "type": "object", "properties": { "self": { "type": "string" } } }),
            "/properties/self",
            "the property name \"self\" is not a Rust identifier: `self` is a keyword Rust does not take even as a raw identifier",
        ),
        (
            "test.colliding@1",
            json!({
                "title": "Finding",
                "type": "object",
                "properties": { "line-no": { "type": "integer" }, "line_no": { "type": "integer" } }
            }),
            "/properties/line_no",
            "the property name \"line-no\" and the property name \"line_no\" both become the field `line_no`",
        ),
    ];
    for (id, document, at, why) in cases {
        let refused = register_and_gen_rust(dir.path(), registry_arg, id, &document);
        assert_eq!(refused.code, 2, "{id}: {}", refused.stderr);
        assert!(refused.stdout.is_empty(), "{id}: {}", refused.stdout);
        assert_eq!(
            refused.stderr.trim_end(),
            format!("onemessagebus: {id}: cannot render {at} as Rust: {why}"),
            "{id}"
        );
    }
}

/// The names the renderer maps rather than refuses: a kebab-case or camelCase
/// property becomes a snake-case field, a keyword a raw identifier, an enum
/// word a Pascal-case variant, each renamed back to the wire spelling.
#[test]
fn schema_gen_rust_maps_names_that_need_it_and_renames_them_back() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let registry = dir.path().join("registry");
    let registry_arg = registry.to_str().expect("UTF-8");
    let document = json!({
        "title": "Finding",
        "type": "object",
        "properties": {
            "line-no": { "type": "integer", "format": "uint32" },
            "filePath": { "type": "string" },
            "type": { "$ref": "#/$defs/Severity" },
            "_note": { "type": "string" }
        },
        "required": ["line-no", "filePath", "type", "_note"],
        "additionalProperties": false,
        "$defs": { "Severity": { "enum": ["needs-review", "blocking_now"] } }
    });
    let printed = register_and_gen_rust(dir.path(), registry_arg, "test.mapped@1", &document);
    assert_eq!(printed.code, 0, "{}", printed.stderr);
    assert!(printed.stderr.is_empty(), "{}", printed.stderr);
    for expected in [
        "pub struct Finding {",
        "    #[serde(rename = \"line-no\")]\n    pub line_no: u32,",
        "    #[serde(rename = \"filePath\")]\n    pub file_path: String,",
        "    #[serde(rename = \"type\")]\n    pub r#type: Severity,",
        "    pub _note: String,",
        "pub enum Severity {",
        "    #[serde(rename = \"needs-review\")]\n    NeedsReview,",
        "    #[serde(rename = \"blocking_now\")]\n    BlockingNow,",
    ] {
        assert!(
            printed.stdout.contains(expected),
            "missing {expected:?} in:\n{}",
            printed.stdout
        );
    }
    assert!(
        !printed.stdout.contains("rename = \"_note\""),
        "a name that is already an identifier is not renamed:\n{}",
        printed.stdout
    );
}

#[test]
fn a_registry_directory_that_is_not_one_is_refused_naming_the_file() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let registry = dir.path().join("registry");
    std::fs::create_dir(&registry).expect("created");
    let registry_arg = registry.to_str().expect("UTF-8");
    let env = [("ONEMESSAGEBUS_REGISTRY", registry_arg)];

    // A document filed under a name that is not its id.
    std::fs::write(
        registry.join("agent.other@1.json"),
        json!({ "id": "agent.finding@1", "schema": { "type": "object" } }).to_string(),
    )
    .expect("written");
    let misnamed = run_in(dir.path(), &["schema", "list"], None, &env);
    assert_eq!(misnamed.code, 2);
    assert!(
        misnamed.stderr.contains("agent.other@1.json"),
        "{}",
        misnamed.stderr
    );
    assert!(
        misnamed.stderr.contains("named by its id"),
        "{}",
        misnamed.stderr
    );
    std::fs::remove_file(registry.join("agent.other@1.json")).expect("removed");

    // A file that is not a registry document at all.
    std::fs::write(registry.join("agent.broken@1.json"), "not json").expect("written");
    let corrupt = run_in(dir.path(), &["schema", "list"], None, &env);
    assert_eq!(corrupt.code, 2);
    assert!(
        corrupt.stderr.contains("agent.broken@1.json"),
        "{}",
        corrupt.stderr
    );
    assert!(
        corrupt.stderr.contains("not a registry document"),
        "{}",
        corrupt.stderr
    );
    std::fs::remove_file(registry.join("agent.broken@1.json")).expect("removed");

    // A schema file that is not JSON.
    std::fs::write(dir.path().join("schema.txt"), "not json").expect("written");
    let not_json = run_in(
        dir.path(),
        &[
            "schema",
            "register",
            "agent.finding@1",
            "--file",
            "schema.txt",
        ],
        None,
        &env,
    );
    assert_eq!(not_json.code, 2);
    assert!(
        not_json.stderr.contains("schema.txt"),
        "{}",
        not_json.stderr
    );
    assert!(
        not_json.stderr.contains("not a JSON document"),
        "{}",
        not_json.stderr
    );
    let missing = run_in(
        dir.path(),
        &[
            "schema",
            "register",
            "agent.finding@1",
            "--file",
            "absent.json",
        ],
        None,
        &env,
    );
    assert_eq!(missing.code, 2);
    assert!(missing.stderr.contains("absent.json"), "{}", missing.stderr);
}

/// A `--registry` path that exists but is not a directory would otherwise read
/// as a registry holding nothing, so every `schema` verb refuses it by path —
/// named by the flag or by the environment — before answering or writing.
#[test]
fn every_schema_verb_refuses_a_registry_path_that_is_not_a_directory() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("registry.json");
    std::fs::write(&file, "{}").expect("written");
    std::fs::write(
        dir.path().join("ok.json"),
        json!({ "type": "object" }).to_string(),
    )
    .expect("written");
    let file_arg = file.to_str().expect("UTF-8");
    let verbs: [&[&str]; 4] = [
        &["schema", "list"],
        &["schema", "check", "agent.labels@1"],
        &["schema", "gen", "--lang", "json", "agent.labels@1"],
        &["schema", "register", "agent.finding@1", "--file", "ok.json"],
    ];
    for verb in verbs {
        let mut by_flag = verb.to_vec();
        by_flag.extend(["--registry", file_arg]);
        let flagged = run_in(dir.path(), &by_flag, Some("{}"), &[]);
        let by_env = run_in(
            dir.path(),
            verb,
            Some("{}"),
            &[("ONEMESSAGEBUS_REGISTRY", file_arg)],
        );
        for refused in [flagged, by_env] {
            assert_eq!(refused.code, 2, "{verb:?}: {}", refused.stderr);
            assert!(refused.stdout.is_empty(), "{verb:?}: {}", refused.stdout);
            assert!(
                refused
                    .stderr
                    .contains(&format!("the registry {file_arg} is not a directory")),
                "{verb:?}: {}",
                refused.stderr
            );
        }
    }
    assert_eq!(
        std::fs::read_to_string(&file).expect("the file"),
        "{}",
        "nothing was written"
    );

    // A path under a file is neither a directory nor absent: the filesystem's
    // own answer is the refusal.
    let under_file = file.join("nested");
    let beneath = run_in(
        dir.path(),
        &[
            "schema",
            "list",
            "--registry",
            under_file.to_str().expect("UTF-8"),
        ],
        None,
        &[],
    );
    assert_eq!(beneath.code, 2, "{}", beneath.stderr);
    assert!(beneath.stdout.is_empty(), "{}", beneath.stdout);
    assert!(
        beneath.stderr.contains("cannot use the registry directory"),
        "{}",
        beneath.stderr
    );
    assert!(beneath.stderr.contains("nested"), "{}", beneath.stderr);
}
