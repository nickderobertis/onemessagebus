//! The committed contract drives the core's types: what `docs/contract.md`
//! says of the wire shape, the bounds, the redaction tables, the glob dialect
//! and the verbs, checked here against the crate that has no agent word in it.
//! The profile's `tests/contract.rs` drives the same document through the
//! agent types.

use onemessagebus::sdk_schema::{self, Lang};
use onemessagebus::{
    glob, Envelope, Filter, Open, Registry, Source, CAPABILITIES, CREDENTIAL_PREFIXES,
    CREDENTIAL_WORDS, MAX_ACTIVITY_DETAIL_CHARS, MAX_PAYLOAD_TEXT_BYTES, REDACTED,
};
use serde_json::{json, Value};

const CONTRACT: &str = include_str!("../../../docs/contract.md");

/// The fenced block tagged `<!-- fixture: name -->`.
fn fixture(name: &str) -> String {
    let tag = format!("<!-- fixture: {name} -->");
    let mut lines = CONTRACT.lines();
    lines
        .by_ref()
        .find(|line| line.trim() == tag)
        .unwrap_or_else(|| panic!("docs/contract.md has no fixture tagged {name:?}"));
    assert!(lines.next().is_some_and(|line| line.starts_with("```")));
    lines
        .take_while(|line| !line.starts_with("```"))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Every fixture a contract test drives: `envelope`, `filter` and `verbs` here
/// and in the profile's `tests/contract.rs`; the transport's `transport-layout`,
/// `transport-kinds` and `plugin-protocol`, the queue `policy` and the `config`
/// file here, where the core's types read them; and `read-sets`, the inbox's
/// `spool-documents` and `carry-store`, the note contract's `note`, `accepted`
/// and `note-undelivered`, and the planner channel's `planner-channel` and
/// `planner-channel-grants` there alone, since each names the agent profile's
/// message or layout. The validators' `verdicts` and `validators-config` and
/// the ask's `asked` are driven here. The configured codec fixture lives in
/// `docs/codecs.md` and is driven by `tests/serve.rs`.
/// A fixture added to the document is added here beside the test that drives
/// it.
const DRIVEN_FIXTURES: &[&str] = &[
    "accepted",
    "asked",
    "carry-store",
    "config",
    "envelope",
    "filter",
    "note",
    "note-undelivered",
    "planner-channel",
    "planner-channel-grants",
    "plugin-protocol",
    "policy",
    "read-sets",
    "spool-documents",
    "transport-kinds",
    "transport-layout",
    "validators-config",
    "verbs",
    "verdicts",
];

#[test]
fn the_documented_answers_are_what_ask_prints_and_only_a_reply_carries_a_reply() {
    use onemessagebus::sdk_schema::Asked;
    use onemessagebus::{Answer, AskRefusal, RefusalKind};
    let documented: Value = serde_json::from_str(&fixture("asked")).expect("JSON");
    let read: Vec<Asked> =
        serde_json::from_value(documented.clone()).expect("the documented answers read");
    assert!(
        matches!(
            read.as_slice(),
            [
                Asked::Reply { .. },
                Asked::Timeout { .. },
                Asked::Abandoned { .. },
                Asked::Refused { .. }
            ]
        ),
        "{read:?}"
    );
    assert_eq!(serde_json::to_value(&read).expect("JSON"), documented);
    let words = [
        Answer::<Value>::Reply(json!({})).word(),
        Answer::<Value>::Timeout.word(),
        Answer::<Value>::Abandoned.word(),
        Answer::<Value>::Refused(AskRefusal {
            kind: RefusalKind::Schema,
            reason: String::new(),
        })
        .word(),
    ];
    for (answer, word) in documented.as_array().expect("a list").iter().zip(words) {
        assert_eq!(answer["answer"], json!(word));
        assert_eq!(
            answer.get("reply").is_some(),
            word == "reply",
            "only a reply carries a reply member: {answer}"
        );
    }
}

fn fixture_tag(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("<!-- fixture: ")
        .and_then(|rest| rest.strip_suffix(" -->"))
}

/// A fenced block nobody drives is a shape the document states and no test
/// holds the crates to, so every block carries a tag, every tag sits on a
/// block, and every name is one a contract test reads.
#[test]
fn every_fenced_block_in_the_contract_is_a_fixture_a_contract_test_drives() {
    let lines: Vec<&str> = CONTRACT.lines().collect();
    let mut fenced = Vec::new();
    let mut inside = false;
    for (at, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with("```") {
            continue;
        }
        inside = !inside;
        if !inside {
            continue;
        }
        let name = at
            .checked_sub(1)
            .and_then(|previous| fixture_tag(lines[previous]))
            .unwrap_or_else(|| {
                panic!(
                    "the fenced block at docs/contract.md:{} has no `<!-- fixture: name -->` tag \
                     on the line before it; tag it and drive it from a contract test",
                    at + 1
                )
            });
        assert!(
            DRIVEN_FIXTURES.contains(&name),
            "docs/contract.md:{} tags a fixture {name:?} no contract test drives; drive it and \
             add it to DRIVEN_FIXTURES",
            at + 1
        );
        fenced.push(name);
    }
    assert!(!inside, "a fence in docs/contract.md never closes");
    let tagged: Vec<&str> = lines.iter().filter_map(|line| fixture_tag(line)).collect();
    assert_eq!(
        tagged, fenced,
        "a fixture tag is not on the line before a fence"
    );
    let mut names = fenced.clone();
    names.sort_unstable();
    assert_eq!(
        names, DRIVEN_FIXTURES,
        "each driven fixture is tagged exactly once"
    );
    for name in DRIVEN_FIXTURES {
        assert!(
            !fixture(name).trim().is_empty(),
            "the {name} fixture is empty"
        );
    }
}

/// Every `` `backticked` `` token in the contract.
fn backticked() -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = CONTRACT;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else { break };
        out.push(rest[..close].to_owned());
        rest = &rest[close + 1..];
    }
    out
}

/// The documented envelope, with its placeholders made concrete and its
/// profile-declared `phase` removed — the core has no phase, and over the open
/// vocabulary a top-level key nobody declared is refused rather than carried.
fn open_envelope_example() -> Value {
    let mut example: Value = serde_json::from_str(&fixture("envelope")).expect("JSON");
    example["ts"] = json!("2026-08-07T12:34:56.789Z");
    example["stream"] = json!("billing-4f2a");
    example["source"] = json!("billing");
    example["kind"] = json!("invoice-issued");
    example.as_object_mut().expect("an object").remove("phase");
    example
}

#[test]
fn the_documented_envelope_reads_over_the_open_vocabulary_with_labels_as_an_open_map() {
    let example = open_envelope_example();
    let envelope: Envelope<Open> = serde_json::from_value(example.clone()).expect("parses");
    assert_eq!(envelope.v, 1);
    assert_eq!(envelope.seq, 42);
    assert_eq!(envelope.source, Source::from("billing"));
    assert_eq!(envelope.kind.as_str(), "invoice-issued");
    assert_eq!(envelope.labels.get_str("run_id"), Some("R"));
    assert_eq!(envelope.labels.0["round"], json!(2));
    assert_eq!(envelope.labels.get_str("extra"), Some("carried"));
    assert_eq!(
        serde_json::to_value(&envelope).expect("serializes"),
        example
    );

    let mut with_phase: Value = serde_json::from_str(&fixture("envelope")).expect("JSON");
    with_phase["source"] = json!("billing");
    let refusal = serde_json::from_value::<Envelope<Open>>(with_phase)
        .expect_err("the core has no phase: an undeclared top-level key is refused");
    assert!(refusal.to_string().contains("phase"), "{refusal}");
}

#[test]
fn the_documented_filter_reads_over_the_open_vocabulary_as_label_asks() {
    let filter: Filter<Open> =
        serde_json::from_str(&fixture("filter")).expect("the documented filter parses");
    filter.validate().expect("valid");
    assert_eq!(filter.include.len(), 2);
    assert_eq!(filter.include[0].kind.as_deref(), Some("member-*"));
    assert_eq!(filter.include[1].fields.0["member"], json!("worker"));
    assert_eq!(filter.exclude[1].source, Some(Source::from("vcs")));
    assert_eq!(filter.exclude[1].fields.0["phase"], json!("release"));
    assert_eq!(
        serde_json::to_value(&filter).expect("serializes"),
        serde_json::from_str::<Value>(&fixture("filter")).expect("JSON")
    );
}

#[test]
fn the_documented_bounds_are_the_constants() {
    let tokens = backticked();
    assert!(tokens.contains(&"MAX_PAYLOAD_TEXT_BYTES".to_owned()));
    assert!(tokens.contains(&"MAX_ACTIVITY_DETAIL_CHARS".to_owned()));
    assert!(CONTRACT.contains(&format!("**{MAX_PAYLOAD_TEXT_BYTES} bytes**")));
    assert!(CONTRACT.contains(&format!("**{MAX_ACTIVITY_DETAIL_CHARS}\n  characters**")));
}

#[test]
fn the_documented_redaction_tables_are_the_crates() {
    let tokens = backticked();
    for word in CREDENTIAL_WORDS.iter().chain(CREDENTIAL_PREFIXES) {
        assert!(
            tokens.contains(&(*word).to_owned()),
            "{word} is not in the contract"
        );
    }
    assert!(tokens.contains(&REDACTED.to_owned()));
    // And nothing the contract lists as a table entry is missing from the crate.
    let listed =
        "TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `APIKEY`, `API_KEY`, `PRIVATE_KEY";
    let words: Vec<&str> = listed.split("`, `").collect();
    assert_eq!(words, CREDENTIAL_WORDS);
}

#[test]
fn the_glob_dialect_is_star_alone() {
    assert!(glob("member-*", "member-started"));
    assert!(glob("member-*", "member-"));
    assert!(!glob("member-*", "turn-started"));
    assert!(glob("*", "anything"));
    assert!(glob("*-started", "member-started"));
    assert!(glob("*a*b*", "xaxbx"));
    assert!(!glob("*a*b*", "xbxax"));
    assert!(
        !glob("member-?", "member-x"),
        "`?` is itself, not a wildcard"
    );
    assert!(!glob("[a-z]", "a"), "a class is itself, not a class");
    assert!(glob("", ""));
    assert!(!glob("", "x"));
}

#[test]
fn the_documented_verbs_are_exactly_the_capabilities() {
    let documented: Vec<String> = serde_json::from_str(&fixture("verbs")).expect("JSON");
    let declared: Vec<String> = CAPABILITIES.iter().map(|c| c.verb.join(" ")).collect();
    assert_eq!(documented, declared);
}

#[test]
fn the_bundle_emits_the_manifest_and_every_documented_root() {
    let bundle = sdk_schema::bundle::<Open>(&Registry::new());
    let document: Value = serde_json::from_str(&bundle.to_json()).expect("the bundle is JSON");
    for root in [
        "envelope",
        "filter",
        "schema_id",
        "registry_document",
        "config",
        "capabilities",
    ] {
        assert!(document.get(root).is_some(), "the bundle has no {root}");
    }
    assert_eq!(
        document["capabilities"].as_array().map(Vec::len),
        Some(CAPABILITIES.len())
    );
    assert_eq!(document["vocabulary"]["name"], json!("open"));
    for capability in CAPABILITIES {
        if let Some(root) = capability.options {
            assert!(
                document["options"].get(root).is_some(),
                "{root} has no schema"
            );
        }
        assert_eq!(
            capability.python_method(),
            capability
                .method
                .chars()
                .fold(String::new(), |mut out, ch| {
                    if ch.is_ascii_uppercase() {
                        out.push('_');
                        out.push(ch.to_ascii_lowercase());
                    } else {
                        out.push(ch);
                    }
                    out
                })
        );
    }
    let entries: Vec<&Value> = document["capabilities"]
        .as_array()
        .expect("array")
        .iter()
        .collect();
    for entry in entries {
        for binding in entry["bindings"].as_array().expect("bindings") {
            assert!(binding.get("option").is_some() && binding.get("flag").is_some());
        }
    }
}

/// The bindings restate the option structs by name, because an SDK builds its
/// argv from the manifest and cannot see a Rust field. Each capability's
/// bindings are held to the properties of the options schema it names — the
/// generated one, so serde's camelCase is what is compared — in both
/// directions: a binding naming no property renders a flag from nothing, and
/// a property no binding names is an option the SDK accepts and drops. The
/// manifest declares uncovered *flags* but no uncovered options, so every
/// property must be bound. Every disagreement is named at once.
#[test]
fn every_capabilitys_bindings_are_exactly_its_options_schemas_properties() {
    let bundle = sdk_schema::bundle::<Open>(&Registry::new());
    let options = serde_json::to_value(&bundle.options).expect("the options serialize");
    let mut named = std::collections::BTreeSet::new();
    let mut disagreements = Vec::new();
    for capability in CAPABILITIES {
        let bound: Vec<&str> = capability.bindings.iter().map(|b| b.option).collect();
        let Some(root) = capability.options else {
            if !bound.is_empty() {
                disagreements.push(format!(
                    "{}: names no options schema but binds {bound:?}",
                    capability.method
                ));
            }
            continue;
        };
        named.insert(root);
        let Some(properties) = options[root]["properties"].as_object() else {
            disagreements.push(format!(
                "{}: options schema {root} is not in the bundle, or has no properties",
                capability.method
            ));
            continue;
        };
        let unbound: Vec<&str> = properties
            .keys()
            .map(String::as_str)
            .filter(|property| !bound.contains(property))
            .collect();
        let unknown: Vec<&str> = bound
            .iter()
            .copied()
            .filter(|option| !properties.contains_key(*option))
            .collect();
        if !unknown.is_empty() {
            disagreements.push(format!(
                "{}: bindings name {unknown:?}, which options schema {root} has no property for",
                capability.method
            ));
        }
        if !unbound.is_empty() {
            disagreements.push(format!(
                "{}: options schema {root} has properties {unbound:?}, which no binding names",
                capability.method
            ));
        }
    }
    for root in bundle.options.keys() {
        if !named.contains(root) {
            disagreements.push(format!(
                "options schema {root} is in the bundle, but no capability names it"
            ));
        }
    }
    assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
}

#[test]
fn an_unsupported_language_is_refused_by_name() {
    let id = "test.thing@1".parse().expect("id");
    for lang in [Lang::Python, Lang::Typescript] {
        let refusal = sdk_schema::generate(lang, &id, &json!({})).expect_err("refused");
        assert!(refusal.to_string().contains(lang.as_str()), "{refusal}");
        assert!(refusal.to_string().contains("test.thing@1"), "{refusal}");
    }
    let json = sdk_schema::generate(Lang::Json, &id, &json!({ "type": "object" })).expect("json");
    assert_eq!(json, "{\n  \"type\": \"object\"\n}\n");
    let untitled = sdk_schema::generate(Lang::Rust, &id, &json!({ "type": "object" }))
        .expect_err("a document with no title cannot name a type");
    assert!(untitled.to_string().contains("title"), "{untitled}");
}

/// Every file under `dir`, as `/`-separated paths relative to it.
fn files_under(dir: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in std::fs::read_dir(&at).expect("a directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(dir).expect("under the directory");
                found.push(
                    relative
                        .components()
                        .map(|part| part.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
    }
    found.sort();
    found
}

#[test]
fn the_documented_local_layout_is_the_files_the_local_transport_writes() {
    use onemessagebus::{ConsumerName, DocumentName, LocalTransport, QueueName, Transport};
    let layout: Value = serde_json::from_str(&fixture("transport-layout")).expect("JSON");
    let dir = tempfile::tempdir().expect("a scratch directory");
    let local = LocalTransport::open(dir.path()).expect("opens");
    let queue: QueueName = "replies".parse().expect("a queue");
    let watcher: ConsumerName = "watcher".parse().expect("a consumer");
    let document: DocumentName = "queue.json".parse().expect("a document");
    let at = local.append(&queue, b"{}").expect("appends");
    local
        .commit(&queue, &ConsumerName::default_consumer(), &at)
        .expect("commits");
    local.commit(&queue, &watcher, &at).expect("commits");
    local
        .replace_document(&queue, &document, b"{}")
        .expect("replaces");
    local
        .exclusive(&queue, &mut |_| Ok(()))
        .expect("the section runs");
    let mut documented: Vec<String> = layout
        .as_object()
        .expect("an object")
        .values()
        .map(|pattern| {
            pattern
                .as_str()
                .expect("a path")
                .replace("<queue>", "replies")
                .replace("<consumer>", "watcher")
                .replace("<name>", "queue.json")
        })
        .collect();
    documented.sort();
    assert_eq!(
        files_under(dir.path()),
        documented,
        "the local transport's files are not the layout the contract states"
    );
}

#[test]
fn the_documented_transport_kinds_resolve_in_the_documented_order() {
    use onemessagebus::{MemoryTransport, Transport, TransportKinds};
    use std::sync::Arc;
    let documented: Value = serde_json::from_str(&fixture("transport-kinds")).expect("JSON");
    let dir = tempfile::tempdir().expect("a scratch directory");
    let executable = documented["plugin executable"]
        .as_str()
        .expect("a name")
        .replace("<kind>", "nats");
    assert_eq!(
        executable,
        format!("{}nats", onemessagebus::transport::PLUGIN_PREFIX)
    );
    let plugin = dir.path().join(&executable);
    std::fs::write(&plugin, "#!/bin/sh\n").expect("a plugin file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&plugin, std::fs::Permissions::from_mode(0o755))
            .expect("its mode");
    }
    let mut kinds = TransportKinds::builtin().searching(vec![dir.path().to_path_buf()]);
    kinds
        .register(
            "shared",
            Arc::new(|_| Ok(Arc::new(MemoryTransport::new()) as Arc<dyn Transport>)),
        )
        .expect("registers");
    let listed = kinds.kinds();
    let built_in: Vec<Value> = listed
        .iter()
        .filter(|entry| entry.origin == onemessagebus::KindOrigin::Builtin)
        .map(|entry| json!(entry.kind))
        .collect();
    assert_eq!(Value::Array(built_in), documented["built-in"]);
    let mut order: Vec<Value> = listed
        .iter()
        .map(|entry| serde_json::to_value(&entry.origin).expect("JSON"))
        .collect();
    order.dedup();
    assert_eq!(Value::Array(order), documented["order"]);
}

#[test]
fn the_documented_plugin_protocol_lines_are_the_protocols_own_shapes() {
    use onemessagebus::transport::{self, PluginHello, PluginReply, PluginRequest};
    let documented: Value = serde_json::from_str(&fixture("plugin-protocol")).expect("JSON");
    let hello: PluginHello =
        serde_json::from_value(documented["hello"].clone()).expect("the hello reads");
    assert_eq!(hello.protocol, transport::PROTOCOL);
    assert_eq!(hello.version, transport::PROTOCOL_VERSION);
    assert_eq!(
        serde_json::to_value(&hello).expect("JSON"),
        documented["hello"]
    );
    let mut registry = Registry::new();
    transport::register_protocol(&mut registry).expect("the protocol registers");
    registry
        .check(&transport::HELLO_SCHEMA, &documented["hello"])
        .expect("the hello conforms");
    for request in documented["requests"].as_array().expect("requests") {
        let read: PluginRequest = serde_json::from_value(request.clone()).expect("a request reads");
        assert_eq!(&serde_json::to_value(&read).expect("JSON"), request);
        registry
            .check(&transport::REQUEST_SCHEMA, request)
            .expect("a request conforms");
    }
    let mut replies = vec![documented["hello-answer"].clone()];
    replies.extend(
        documented["replies"]
            .as_array()
            .expect("replies")
            .iter()
            .cloned(),
    );
    for reply in &replies {
        let read: PluginReply = serde_json::from_value(reply.clone()).expect("a reply reads");
        assert_eq!(&serde_json::to_value(&read).expect("JSON"), reply);
        registry
            .check(&transport::REPLY_SCHEMA, reply)
            .expect("a reply conforms");
    }
    let past_end = onemessagebus::TransportError::PastEnd {
        queue: "surfaces".parse().expect("a queue"),
        position: onemessagebus::Position::from_token(12),
        end: onemessagebus::Position::from_token(9),
    };
    assert_eq!(
        documented["replies"][3]["error"]["message"],
        json!(past_end.to_string()),
        "the documented refusal is not the transport's own words"
    );
}

#[test]
fn the_documented_default_policy_is_a_plain_queue() {
    let documented: Value = serde_json::from_str(&fixture("policy")).expect("JSON");
    assert_eq!(
        serde_json::to_value(onemessagebus::Policy::default()).expect("JSON"),
        documented
    );
    let read: onemessagebus::Policy = serde_json::from_value(documented).expect("reads");
    assert!(!read.keeps_events());
}

#[test]
fn the_documented_configuration_loads_and_an_unknown_key_in_it_is_refused_by_name() {
    let text = fixture("config");
    let config = onemessagebus::Config::parse(&text).expect("the documented configuration loads");
    assert_eq!(config.version, onemessagebus::CONFIG_VERSION);
    assert_eq!(config.transport.kind.as_str(), "local");
    assert_eq!(
        config.transport.dir.as_deref(),
        Some(std::path::Path::new("runs/r1/channel"))
    );
    assert_eq!(config.profile.as_deref(), Some("planner-channel"));
    let findings = &config.queues[&"findings".parse().expect("a queue")];
    assert_eq!(findings.policy.hold_pending, Some(false));
    assert_eq!(
        config.authors[&onemessagebus::Author::from("sentinel")].capabilities,
        vec!["retry", "requeue", "cancel", "finding", "add"]
    );
    assert_eq!(
        config.authors[&onemessagebus::Author::from("sentinel")].refusals
            [&onemessagebus::OpWord("complete".to_owned())],
        "whether the run is finished is the planner's verdict, not an observation"
    );
    let refused = onemessagebus::Config::parse(&text.replace("queues:", "queus:"))
        .expect_err("an unknown key is refused by load");
    assert!(
        refused.to_string().contains("unknown field `queus`"),
        "{refused}"
    );
}

#[test]
fn the_documented_verdicts_are_the_verdict_type_on_the_wire() {
    use onemessagebus::Verdict;
    let documented: Value = serde_json::from_str(&fixture("verdicts")).expect("JSON");
    let read: Vec<Verdict> =
        serde_json::from_value(documented.clone()).expect("the documented verdicts read");
    assert!(
        matches!(
            read.as_slice(),
            [
                Verdict::Pass,
                Verdict::Refuse { .. },
                Verdict::Unjudged { .. }
            ]
        ),
        "{read:?}"
    );
    assert_eq!(serde_json::to_value(&read).expect("JSON"), documented);
    assert_eq!(
        read.iter().map(Verdict::passes).collect::<Vec<_>>(),
        [true, false, false],
        "an unjudged verdict passed"
    );
}

#[test]
fn the_documented_validators_block_loads_into_the_config_and_an_unknown_key_in_it_is_refused_by_name(
) {
    use onemessagebus::{Config, ValidatorKind, When};
    let text = format!(
        "version: 1\ntransport: {{kind: local, dir: runs/r1/channel}}\n{}",
        fixture("validators-config")
    );
    let config = Config::parse(&text).expect("the documented validators block loads");
    let [validator] = config.validators.as_slice() else {
        panic!("the documented block declares one validator: {config:?}");
    };
    assert_eq!(validator.on.as_str(), "replies");
    assert_eq!(
        validator.when,
        Some(When::Carries("commands".parse().expect("a field path")))
    );
    assert_eq!(validator.kind, ValidatorKind::Command);
    assert_eq!(
        validator.command,
        [
            "uv",
            "run",
            "python",
            "-m",
            "orchestrator.plan_review",
            "--envelope"
        ]
    );
    let cache = validator.cache.as_ref().expect("the documented cache");
    assert_eq!(cache.dir, std::path::Path::new(".validator-passes"));
    assert_eq!(cache.bar_fingerprint, ["scripts/llmlint-fingerprint.sh"]);
    for (from, to, named) in [
        ("bar_fingerprint:", "bar:", "unknown field `bar`"),
        (
            "kind: command",
            "kind: command, retries: 2",
            "unknown field `retries`",
        ),
        (
            "{carries: commands}",
            "{carries: commands, and: x}",
            "`carries` stands alone",
        ),
    ] {
        let refused = Config::parse(&text.replace(from, to)).expect_err(to);
        assert!(refused.to_string().contains(named), "{to}: {refused}");
    }
    let bundle = sdk_schema::bundle::<Open>(&Registry::new());
    let document: Value = serde_json::from_str(&bundle.to_json()).expect("the bundle is JSON");
    assert!(
        document["config"]["properties"]["validators"].is_object(),
        "the SDK bundle's config root has no validators block"
    );
}
