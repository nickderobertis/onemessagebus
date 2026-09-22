//! The committed contract drives the core's types: what `docs/contract.md`
//! says of the wire shape, the bounds, the redaction tables, the glob dialect,
//! the registry, the inbox's documents and the verbs, checked here against the
//! crate every consumer links. Every fenced block in the document is driven
//! here, so the document and the types cannot drift.

use onemessagebus::sdk_schema::{self, Lang};
use onemessagebus::{
    glob, Author, Envelope, Filter, LayoutDocument, Open, Registry, SchemaBundle, Source,
    CAPABILITIES, CREDENTIAL_PREFIXES, CREDENTIAL_WORDS, MAX_ACTIVITY_DETAIL_CHARS,
    MAX_PAYLOAD_TEXT_BYTES, REDACTED,
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

/// Every fixture this test drives: the wire's `envelope` and `filter`, the
/// registry's `read-sets`, the inbox's `spool-documents` and `carry-store`, the
/// transport's `transport-layout`, `transport-kinds` and `plugin-protocol`, the
/// queue `policy`, the `config` file, the `layout-document`, the validators'
/// `verdicts` and `validators-config`, the ask's `asked` and the command line's
/// `verbs`. The configured codec fixture lives in `docs/codecs.md` and is driven
/// by `tests/serve.rs`.
/// A fixture added to the document is added here beside the test that drives
/// it.
const DRIVEN_FIXTURES: &[&str] = &[
    "asked",
    "carry-store",
    "config",
    "envelope",
    "filter",
    "layout-document",
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

/// The documented envelope, with its placeholders made concrete: each is
/// asserted still there before it is replaced, so a doc edit that renames one
/// fails here rather than silently skipping the substitution.
fn open_envelope_example() -> Value {
    let mut example: Value = serde_json::from_str(&fixture("envelope")).expect("JSON");
    for (key, placeholder, concrete) in [
        (
            "ts",
            "<RFC 3339, millisecond precision, UTC>",
            "2026-08-07T12:34:56.789Z",
        ),
        (
            "stream",
            "<unique id per producing process>",
            "billing-4f2a",
        ),
        ("source", "<a source word the vocabulary admits>", "billing"),
        ("kind", "<kebab-case event kind>", "invoice-issued"),
    ] {
        assert_eq!(
            example[key],
            json!(placeholder),
            "the envelope's {key} placeholder moved; update this substitution"
        );
        example[key] = json!(concrete);
    }
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
    assert_eq!(envelope.labels.get_str("tenant"), Some("acme"));
    assert_eq!(envelope.labels.0["attempt"], json!(2));
    assert_eq!(envelope.labels.get_str("extra"), Some("carried"));
    assert_eq!(envelope.artifacts.len(), 1);
    assert_eq!(
        serde_json::to_value(&envelope).expect("serializes"),
        example
    );
    assert_eq!(
        serde_json::to_string(&envelope).expect("serializes"),
        serde_json::to_string(&example).expect("serializes"),
        "the wire order is the documented order"
    );

    let mut with_dimension = example;
    with_dimension["region"] = json!("eu");
    let refusal = serde_json::from_value::<Envelope<Open>>(with_dimension)
        .expect_err("the core declares no dimension: an undeclared top-level key is refused");
    assert!(refusal.to_string().contains("region"), "{refusal}");
}

#[test]
fn the_documented_filter_reads_over_the_open_vocabulary_as_label_asks() {
    let filter: Filter<Open> =
        serde_json::from_str(&fixture("filter")).expect("the documented filter parses");
    filter.validate().expect("valid");
    assert_eq!(filter.include.len(), 2);
    assert_eq!(filter.include[0].kind.as_deref(), Some("invoice-*"));
    assert_eq!(filter.include[1].fields.0["tenant"], json!("acme"));
    assert_eq!(filter.include[1].fields.0["region"], json!("eu"));
    assert_eq!(filter.exclude[1].source, Some(Source::from("ledger")));
    assert_eq!(filter.exclude[1].fields.0["tenant"], json!("internal"));
    assert_eq!(
        serde_json::to_value(&filter).expect("serializes"),
        serde_json::from_str::<Value>(&fixture("filter")).expect("JSON")
    );
}

#[test]
fn the_documented_read_sets_are_what_the_registry_answers() {
    let documented: std::collections::BTreeMap<String, Vec<u32>> =
        serde_json::from_str(&fixture("read-sets")).expect("the read sets are JSON");
    let mut registry = Registry::new();
    for version in [1, 2] {
        registry
            .register_schema(
                format!("shop.order@{version}").parse().expect("an id"),
                json!({"type": "object"}),
            )
            .expect("registers");
    }
    assert_eq!(
        documented.keys().cloned().collect::<Vec<_>>(),
        vec!["shop.order".to_owned()]
    );
    for (family, read_set) in documented {
        assert_eq!(registry.read_set(&family), read_set, "{family}");
    }
}

/// The message and disposition the inbox fixtures show: an order a shop's till
/// sends its stock room, answered with a receipt.
mod shop {
    use onemessagebus::{Carried, Disposition, Message, SchemaId};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct Order {
        pub sku: String,
        pub quantity: u32,
    }

    impl Message for Order {
        const SCHEMA: SchemaId = SchemaId::literal("shop", "order", 1);
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Receipt {
        Filled { by: String },
        Deferred,
    }

    impl Disposition for Receipt {}

    impl Carried for Receipt {
        fn carried() -> Self {
            Receipt::Deferred
        }
    }

    pub fn order(sku: &str, quantity: u32) -> Order {
        Order {
            sku: sku.to_owned(),
            quantity,
        }
    }
}

/// A document a spool holds, once something has written it.
fn document_at(path: &std::path::Path) -> Value {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(document) = serde_json::from_str(&text) {
                return document;
            }
        }
        assert!(
            std::time::Instant::now() < until,
            "{} was never written",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn offer_id(n: u32) -> String {
    format!("{:039}-1-{n:020}", 1)
}

#[test]
fn the_documented_spool_documents_are_what_a_bound_spool_reads_and_writes() {
    use onemessagebus::{Closed, Inbox, Spool};
    use shop::{order, Order, Receipt};
    use std::time::Duration;

    let documents: Value =
        serde_json::from_str(&fixture("spool-documents")).expect("the documents are JSON");
    let dir = tempfile::tempdir().expect("a temp dir");
    let spool_dir = dir.path().join("orders");
    let inbox: Inbox<Order, Receipt> = Inbox::new();
    let _spool = Spool::bind(&spool_dir, &inbox).expect("binds");
    assert_eq!(
        document_at(&spool_dir.join("spool.json")),
        documents["spool.json"]
    );

    // An offer in the documented shape is taken, and its answer is written in the
    // documented shape.
    let offered = |n: u32, offer: &Value| {
        let id = offer_id(n);
        std::fs::write(
            spool_dir.join(format!("{id}.offer.json")),
            offer.to_string(),
        )
        .expect("offered");
        spool_dir.join(format!("{id}.answer.json"))
    };
    let answer = offered(1, &documents["offer"]);
    let taken = inbox
        .take_within(Duration::from_secs(30))
        .expect("the documented offer is taken");
    assert_eq!(taken.message(), &order("A-1", 2));
    taken.answer(Receipt::Filled {
        by: "stock-room".to_owned(),
    });
    assert_eq!(document_at(&answer), documents["answer"]);

    // An offer the receiver cannot read as its message is answered refused.
    let mut unreadable = documents["offer"].clone();
    unreadable["message"]["quantity"] = json!("two");
    let refused = document_at(&offered(2, &unreadable));
    let mut expected = documents["answer-refused"].clone();
    assert_eq!(
        expected["answer"]["refused"]["reason"],
        json!("<why the receiver could not read the offer>"),
        "the refused answer's placeholder moved; update this substitution"
    );
    assert!(refused["answer"]["refused"]["reason"].is_string());
    expected["answer"]["refused"]["reason"] = refused["answer"]["refused"]["reason"].clone();
    assert_eq!(refused, expected);

    let closing = offered(3, &documents["offer"]);
    let held = inbox
        .take_within(Duration::from_secs(30))
        .expect("the third offer is taken");
    inbox.close(Closed::new("the till closed"));
    assert_eq!(document_at(&closing), documents["answer-closed"]);
    assert_eq!(
        document_at(&spool_dir.join("closed.json")),
        documents["closed.json"]
    );
    drop(held);

    // A sender writes the documented offer, and nothing else, while it waits.
    let unserviced = dir.path().join("unserviced");
    std::fs::create_dir(&unserviced).expect("made");
    let sending = {
        let unserviced = unserviced.clone();
        std::thread::spawn(move || {
            Spool::connect_within::<Order, Receipt>(&unserviced, Duration::from_secs(5))
                .send(order("A-1", 2))
        })
    };
    let offer = loop {
        let found = std::fs::read_dir(&unserviced)
            .expect("a directory")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().ends_with(".offer.json"));
        if let Some(path) = found {
            break path;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(document_at(&offer), documents["offer"]);
    std::fs::remove_file(&offer).expect("taken away from the sender");
    assert!(
        sending.join().expect("the sender finishes").is_err(),
        "a sender whose offer vanished was answered"
    );
}

#[test]
fn inbox_md_names_every_file_a_spool_holds() {
    let inbox_md = include_str!("../../../docs/inbox.md");
    let start = inbox_md
        .find("#### On disk")
        .expect("docs/inbox.md has an `On disk` section");
    let table: Vec<String> = inbox_md[start..]
        .lines()
        .skip_while(|line| !line.starts_with("| file"))
        .skip(2)
        .take_while(|line| line.starts_with("| `"))
        .map(|row| {
            row.split('`')
                .nth(1)
                .expect("a backticked file name")
                .to_owned()
        })
        .collect();
    assert_eq!(
        table,
        [
            "spool.json",
            "receiver.lock",
            "closed.json",
            "<id>.offer.json",
            "<id>.taken.json",
            "<id>.answer.json",
            "<id>.withdrawn",
        ],
        "docs/inbox.md's file table no longer names the files a spool holds"
    );
}

#[test]
fn the_documented_carry_store_is_what_the_carry_backend_writes() {
    use onemessagebus::{Carry, Sender};
    use shop::{order, Order, Receipt};

    let documented: Value =
        serde_json::from_str(&fixture("carry-store")).expect("the store is JSON");
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = dir.path().join("carried.ndjson");
    let carrier: Sender<Order, Receipt> = Carry::sender(&store);
    assert_eq!(carrier.send(order("B-2", 1)), Ok(Receipt::Deferred));
    let written = std::fs::read_to_string(&store).expect("the store");
    let lines: Vec<Value> = written
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    assert_eq!(lines.len(), 2, "{written}");
    assert_eq!(lines[0], documented["header"]);
    assert_eq!(
        written.lines().next(),
        Some(documented["header"].to_string().as_str()),
        "the header is not written in the documented key order"
    );
    let mut record = documented["record"].clone();
    assert_eq!(
        record["ts"],
        json!("<RFC 3339, millisecond precision, UTC>"),
        "the record's placeholder moved; update this substitution"
    );
    record["ts"] = lines[1]["ts"].clone();
    assert_eq!(lines[1], record);
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
        "schema_bundle",
        "layout",
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
        queue: "questions".parse().expect("a queue"),
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
        Some(std::path::Path::new("runs/r1/desk"))
    );
    assert_eq!(config.profile.as_deref(), Some("desk"));
    assert_eq!(
        config
            .schemas
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["https://example.org/desk.json@1"]
    );
    let findings = &config.queues[&"findings".parse().expect("a queue")];
    assert_eq!(findings.policy.hold_pending, Some(false));
    assert_eq!(
        config.authors[&onemessagebus::Author::from("sentinel")].capabilities,
        ["retry", "note"].map(|word| onemessagebus::OpWord(word.to_owned()))
    );
    assert_eq!(
        config.authors[&onemessagebus::Author::from("sentinel")].refusals
            [&onemessagebus::OpWord("complete".to_owned())]
            .as_str(),
        "whether the desk is done is the lead's verdict, not an observation"
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
    assert_eq!(validator.on.as_str(), "answers");
    assert_eq!(
        validator.when,
        Some(When::Carries("actions".parse().expect("a field path")))
    );
    assert_eq!(validator.kind, ValidatorKind::Command);
    assert_eq!(
        validator.command,
        ["python3", "-m", "review_actions", "--envelope"]
    );
    let cache = validator.cache.as_ref().expect("the documented cache");
    assert_eq!(cache.dir, std::path::Path::new(".validator-passes"));
    assert_eq!(cache.bar_fingerprint, ["scripts/bar-fingerprint.sh"]);
    for (from, to, named) in [
        ("bar_fingerprint:", "bar:", "unknown field `bar`"),
        (
            "kind: command",
            "kind: command, retries: 2",
            "unknown field `retries`",
        ),
        (
            "{carries: actions}",
            "{carries: actions, and: x}",
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

#[test]
fn the_documented_layout_document_reads_as_the_one_type_and_names_every_step() {
    let text = fixture("layout-document");
    let document: LayoutDocument =
        serde_json::from_str(&text).expect("the documented layout reads as a LayoutDocument");
    assert_eq!(*document.name(), "desk");
    let bundle = SchemaBundle::with_layouts(
        "1".parse().expect("a version"),
        None,
        Vec::new(),
        vec![document.clone()],
    )
    .expect("a bundle carries it");
    assert_eq!(bundle.layouts(), std::slice::from_ref(&document));
    let allowlist = document.allowlist();
    let words = |author: &str| -> Vec<String> {
        allowlist
            .granted(&Author::from(author))
            .into_iter()
            .map(|op| op.0)
            .collect()
    };
    assert_eq!(words("lead"), ["retry", "note", "complete"], "every_op");
    assert_eq!(words("bot"), ["note"]);
    let written = serde_json::to_string(&document).expect("it serializes");
    for step in ["rename", "stamp", "version", "grant", "route"] {
        assert!(
            written.contains(&format!("{{\"{step}\":")),
            "the {step} step is documented"
        );
    }
    let mut unknown: Value = serde_json::from_str(&text).expect("JSON");
    unknown["prepare"]["questions"][0] = json!({"shout": {"member": "x"}});
    let refused = serde_json::from_value::<LayoutDocument>(unknown).expect_err("an unknown step");
    assert!(
        refused.to_string().contains("unknown variant `shout`"),
        "{refused}"
    );
}

/// The documented configuration resolves against the documented layout — the
/// layout linked as data, as a bundle serving it would link it — narrowing the
/// fully granted author, declaring another, and refusing an op the layout does
/// not have by the key it is at.
#[test]
fn the_documented_configuration_resolves_against_the_documented_layout() {
    use std::sync::Arc;

    use onemessagebus::{Config, ConfigError, Layouts, LinkedLayout, OpWord, TransportKinds};

    let dir = tempfile::tempdir().expect("a scratch directory");
    let document: LayoutDocument =
        serde_json::from_str(&fixture("layout-document")).expect("the documented layout");
    let layouts = Layouts::new().with(Arc::new(
        LinkedLayout::new(document, &Registry::new()).expect("it links"),
    ));
    let text = fixture("config");
    let bus = Config::parse(&text)
        .expect("the documented configuration loads")
        .with_transport_dir(dir.path())
        .resolve(&layouts, &TransportKinds::builtin())
        .expect("the documented configuration resolves");
    let names: Vec<String> = bus.queues().iter().map(ToString::to_string).collect();
    assert_eq!(names, ["actions", "answers", "findings", "questions"]);
    let allows = |author: &str, op: &str| {
        bus.allowlist()
            .allows(&Author::from(author), &OpWord(op.to_owned()))
    };
    assert!(allows("lead", "retry").is_ok());
    assert_eq!(
        allows("lead", "complete")
            .expect_err("narrowed away")
            .reason,
        onemessagebus::NARROWED
    );
    assert!(allows("sentinel", "note").is_ok());
    assert_eq!(
        allows("sentinel", "complete")
            .expect_err("complete was not granted")
            .reason,
        "whether the desk is done is the lead's verdict, not an observation"
    );

    let with_unknown_op = text.replace(
        "lead: {capabilities: [retry, note]}",
        "lead: {capabilities: [retry, note, unknown]}",
    );
    assert_ne!(with_unknown_op, text, "the unknown op did not apply");
    match Config::parse(&with_unknown_op)
        .expect("the file alone cannot know the layout's grants, so it loads")
        .with_transport_dir(dir.path())
        .resolve(&layouts, &TransportKinds::builtin())
    {
        Err(ConfigError::Narrowing(refusal)) => {
            assert_eq!(refusal.key, "authors.lead.capabilities");
            assert!(refusal.why.contains("`unknown`"), "{refusal}");
        }
        other => panic!("an unknown operation was not refused by resolve: {other:?}"),
    }
}

/// `docs/queues.md` shows the configuration file's author keys as the contract
/// does.
#[test]
fn the_queues_pages_configuration_is_the_contracts() {
    let yaml = |document: &str| -> Value {
        document
            .split("```yaml")
            .skip(1)
            .filter_map(|rest| rest.split_once("```").map(|(block, _)| block))
            .find(|block| block.contains("version: 1") && block.contains("authors:"))
            .map(|block| serde_norway::from_str(block).expect("the documented block is YAML"))
            .expect("a configuration block with authors")
    };
    let queues = yaml(include_str!("../../../docs/queues.md"));
    let contract = yaml(CONTRACT);
    for key in ["profile", "authors", "queues", "schemas"] {
        assert_eq!(
            queues[key], contract[key],
            "docs/queues.md's `{key}` drifted from the contract"
        );
    }
}
