//! The documents the harness runs, by where they come from:
//!
//! - `docs`: every ```yaml block in docs/ and the SDK READMEs (as a
//!   configuration; a fragment without `version:` gets the two-line head the
//!   contract tests give it), the contract's `filter` fixture, and every inline
//!   `--until '…'` / `--filter '…'` spec in docs/.
//! - `captured`: every text the core, CLI and e2e suites handed to
//!   `Config::parse`, `Filter::parse` and `Predicate::read` (capture.sh).
//! - `sdk`: each JSON filter and predicate above re-spelled the way each SDK
//!   sends it — `JSON.stringify` (TypeScript) and `json.dumps` (Python, whose
//!   `ensure_ascii` escapes non-ASCII as `\uXXXX`, surrogate pairs included) —
//!   plus non-ASCII texts in both spellings.
//! - `asserted`: the error cases tests/config.rs, tests/contract.rs,
//!   tests/serve.rs, tests/validators.rs, tests/queue.rs and the e2e journeys
//!   assert, each with the substring the test checks.
//! - `onevcs`: the filter error cases onevcs's tests assert, read through a
//!   mirror of its vocabulary.
//! - `probe`: synthetic texts (not real documents) for YAML features the real
//!   documents do not exercise; reported, but apart from the real kinds.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use onemessagebus::Config;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Config,
    Filter,
    Predicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Asserted,
    Onevcs,
    Docs,
    Captured,
    Sdk,
    Probe,
}

pub struct Case {
    pub kind: Kind,
    pub origin: Origin,
    pub label: String,
    pub text: String,
    /// The substring a test asserts the refusal names.
    pub expect: Option<String>,
    /// For an asserted filter, the vocabulary its test reads it under.
    pub vocab: Option<&'static str>,
}

const HEAD: &str = "version: 1\ntransport: {kind: local, dir: runs/r1/channel}\n";
const LOCAL: &str = "version: 1\ntransport: {kind: local, dir: x}\n";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn here() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn markdown_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root().join("docs")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("docs/ reads") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                files.push(path);
            }
        }
    }
    for extra in ["README.md", "python/onemessagebus-sdk/README.md", "npm/onemessagebus-sdk/README.md"] {
        files.push(root().join(extra));
    }
    files.sort();
    files
}

fn relative(path: &Path) -> String {
    path.strip_prefix(root()).unwrap_or(path).display().to_string()
}

fn case(kind: Kind, origin: Origin, label: impl Into<String>, text: impl Into<String>, expect: Option<&str>) -> Case {
    let vocab = match (kind, origin, expect) {
        (Kind::Filter, Origin::Onevcs, Some(_)) => Some("onevcs"),
        (Kind::Filter, _, Some(_)) => Some("Open"),
        _ => None,
    };
    Case { kind, origin, label: label.into(), text: text.into(), expect: expect.map(str::to_owned), vocab }
}

fn docs(cases: &mut Vec<Case>) {
    for file in markdown_files() {
        let document = std::fs::read_to_string(&file).expect("a markdown file reads");
        let lines: Vec<&str> = document.lines().collect();
        let mut at = 0;
        while at < lines.len() {
            let fence = lines[at].trim_start();
            if fence.starts_with("```yaml") || fence.starts_with("```yml") {
                let start = at + 1;
                let mut end = start;
                while end < lines.len() && !lines[end].trim_start().starts_with("```") {
                    end += 1;
                }
                let block: String = lines[start..end].iter().map(|l| format!("{l}\n")).collect();
                let text = if block.contains("version:") { block } else { format!("{HEAD}{block}") };
                cases.push(case(Kind::Config, Origin::Docs, format!("docs {}:{}", relative(&file), at + 1), text, None));
                at = end;
            }
            at += 1;
        }
        for (flag, kind) in [("--until '", Kind::Predicate), ("--filter '", Kind::Filter)] {
            for (offset, _) in document.match_indices(flag) {
                let rest = &document[offset + flag.len()..];
                if let Some(end) = rest.find('\'') {
                    let spec = &rest[..end];
                    if spec.starts_with('{') {
                        let line = document[..offset].lines().count();
                        cases.push(case(kind, Origin::Docs, format!("docs {}:{line} {flag}…'", relative(&file)), spec, None));
                    }
                }
            }
        }
    }
    let contract = std::fs::read_to_string(root().join("docs/contract.md")).expect("the contract reads");
    cases.push(case(Kind::Filter, Origin::Docs, "docs docs/contract.md fixture `filter`", fixture(&contract, "filter"), None));
}

/// tests/contract.rs's `fixture`, restated.
fn fixture(contract: &str, name: &str) -> String {
    let tag = format!("<!-- fixture: {name} -->");
    let mut lines = contract.lines();
    lines.by_ref().find(|line| line.trim() == tag).expect("the fixture is tagged");
    assert!(lines.next().is_some_and(|line| line.starts_with("```")));
    lines.take_while(|line| !line.starts_with("```")).map(|line| format!("{line}\n")).collect()
}

fn captured(cases: &mut Vec<Case>) {
    for (dir, kind) in [("config", Kind::Config), ("filter", Kind::Filter), ("predicate", Kind::Predicate)] {
        let path = here().join("corpus/captured").join(dir);
        let mut files: Vec<_> = std::fs::read_dir(&path)
            .expect("the captured corpus exists: run spikes/saphyr-diff/capture.sh")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        files.sort();
        for file in files {
            let text = std::fs::read_to_string(&file).expect("a captured text reads");
            cases.push(case(kind, Origin::Captured, format!("captured {dir}/{}", file.file_name().unwrap().to_string_lossy()), text, None));
        }
    }
}

/// Python's `json.dumps` with its defaults: `", "` and `": "` separators and
/// `ensure_ascii`, which escapes every non-ASCII character as `\uXXXX`.
fn py_dumps(value: &Value) -> String {
    match value {
        Value::String(text) => {
            let mut out = String::from("\"");
            for c in text.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                        let mut units = [0u16; 2];
                        for unit in c.encode_utf16(&mut units) {
                            out.push_str(&format!("\\u{unit:04x}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::Array(items) => format!("[{}]", items.iter().map(py_dumps).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", py_dumps(&Value::String(k.clone())), py_dumps(v))).collect::<Vec<_>>().join(", ")
        ),
        other => other.to_string(),
    }
}

fn sdk(cases: &mut Vec<Case>) {
    let mut spelled = Vec::new();
    for source in cases.iter().filter(|c| matches!(c.kind, Kind::Filter | Kind::Predicate)) {
        if let Ok(value) = serde_json::from_str::<Value>(&source.text) {
            spelled.push((source.kind, source.label.clone(), value));
        }
    }
    let extra = [
        (Kind::Predicate, r#"{"field": "text", "equals": "café ☕ 😀 — naïve"}"#),
        (Kind::Predicate, r#"{"field": "reply.message", "equals": "line one\nline \"two\"\ttab"}"#),
        (Kind::Predicate, r#"{"any": [{"field": "n", "equals": 3}, {"field": "f", "equals": 2.5}, {"field": "b", "equals": false}, {"field": "z", "equals": null}]}"#),
        (Kind::Filter, r#"{"include": [{"kind": "résumé-*", "tenant": "Zürich"}]}"#),
    ];
    for (kind, text) in extra {
        let value: Value = serde_json::from_str(text).expect("the synthetic SDK text is JSON");
        spelled.push((kind, format!("synthetic {text}"), value));
    }
    for (kind, label, value) in spelled {
        cases.push(case(kind, Origin::Sdk, format!("sdk TypeScript JSON.stringify of {label}"), serde_json::to_string(&value).expect("JSON"), None));
        cases.push(case(kind, Origin::Sdk, format!("sdk Python json.dumps of {label}"), py_dumps(&value), None));
    }
}

fn asserted(cases: &mut Vec<Case>) {
    let contract = std::fs::read_to_string(root().join("docs/contract.md")).expect("the contract reads");
    let mut config = |label: &str, text: String, expect: &str| {
        cases.push(case(Kind::Config, Origin::Asserted, format!("asserted {label}"), text, Some(expect)));
    };
    // tests/config.rs `load_refuses_an_unknown_key_a_version_and_a_malformed_value_by_name`.
    for (text, names) in [
        ("version: 1\ntransport: {kind: local, dir: x}\nqueus: {}\n", "unknown field `queus`"),
        ("version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  findings: {policy: {hold_pendin: false}}\n", "unknown field `hold_pendin`"),
        ("version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  findings: {retain: forever}\n", "unknown field `retain`"),
        ("version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  sentinel: {capabilities: [retry], extra: 1}\n", "unknown field `extra`"),
        ("version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  Bad_Name: {capabilities: []}\n", "authors.Bad_Name"),
        ("version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  '': {capabilities: []}\n", "authors."),
        ("version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  bad_name: {capabilities: []}\n", "authors.bad_name"),
        ("version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  sentinel: {capabilities: [], refusals: {post: '   '}}\n", "a refusal reason must be non-empty text"),
        ("version: 1\ntransport: {kind: local, dir: x}\nauthors:\n  sentinel: {}\n", "missing field `capabilities`"),
        ("version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  ../escape: {}\n", "is not a queue name"),
        ("version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  q: {schema: not-an-id}\n", "is not a schema id"),
        ("version: 1\ntransport: {kind: local, dir: x}\nqueues:\n  q: {claims: {field: a, equals: 1, present: true}}\n", "exactly one of"),
        ("transport: {kind: local, dir: x}\n", "missing field `version`"),
        ("version: 1\ntransport: {kind: NATS, dir: x}\n", "is not a transport kind name"),
        ("version: 2\ntransport: {kind: local, dir: x}\n", "version: 2"),
    ] {
        config("tests/config.rs load_refuses_…", text.to_owned(), names);
    }
    let long = format!("version: 1\ntransport: {{kind: local, dir: x}}\nauthors:\n  {}: {{capabilities: []}}\n", "a".repeat(65));
    config("tests/config.rs load_refuses_… (65-char author)", long, &format!("authors.{}", "a".repeat(65)));
    // tests/contract.rs: the documented configuration with `queues:` misspelt,
    // and the documented validators block with a key changed.
    let documented = fixture(&contract, "config");
    config("tests/contract.rs the_documented_configuration_loads_…", documented.replace("queues:", "queus:"), "unknown field `queus`");
    let validators = format!("version: 1\ntransport: {{kind: local, dir: runs/r1/channel}}\n{}", fixture(&contract, "validators-config"));
    for (from, to, named) in [
        ("bar_fingerprint:", "bar:", "unknown field `bar`"),
        ("kind: command", "kind: command, retries: 2", "unknown field `retries`"),
        ("{carries: actions}", "{carries: actions, and: x}", "`carries` stands alone"),
    ] {
        config("tests/contract.rs the_documented_validators_block_…", validators.replace(from, to), named);
    }
    // tests/serve.rs: the documented codecs block, and the codecs table.
    let codecs_doc = std::fs::read_to_string(root().join("docs/codecs.md")).expect("the codecs page reads");
    let codecs = format!("version: 1\ntransport: {{kind: local, dir: runs/r1/channel}}\n{}", fixture(&codecs_doc, "codecs-config"));
    config("tests/serve.rs the_documented_codecs_block_…", codecs.replace("about_env", "node_env"), "unknown field `node_env`");
    let text = |codecs: &str| format!("version: 1\ntransport: {{kind: memory}}\nqueues:\n  prompts: {{}}\ncodecs:\n{codecs}");
    for (codecs, names) in [
        ("  example: {window: 5}\n", "unknown field `window`"),
        ("  example: {reply_window_seconds: 0}\n", "nonzero"),
        ("  example: {run_env: RUN}\n", "unknown field `run_env`"),
        ("  example: {asker_env: \"CHANNEL-ASKER\"}\n", "ASCII letters, digits and `_`"),
        ("  Example: {}\n", "does not start with a lowercase letter"),
        ("  example: {queue: \"no queue\"}\n", "no queue"),
    ] {
        config("tests/serve.rs a_codecs_block_loads_by_name_…", text(codecs), names);
    }
    // tests/validators.rs `a_validators_block_is_refused_by_the_key_it_is_wrong_at`.
    let command = r#"["/repo/target/debug/deps/validators-0","--exact","scripted_validator","scripted-validator-script=/tmp/rig/script.json"]"#;
    let loaded = |validators: &str| format!("version: 1\ntransport: {{kind: local, dir: \"/tmp/rig/channel\"}}\nqueues:\n  findings: {{}}\nvalidators:\n{validators}");
    for (validators, names) in [
        (format!("  - {{on: findings, kind: command, command: {command}, timeout: 5}}\n"), "timeout"),
        (format!("  - {{on: findings, kind: script, command: {command}}}\n"), "script"),
        ("  - {on: findings, kind: command, command: []}\n".to_owned(), "validators[0].command"),
        (format!("  - {{on: findings, kind: command, command: {command}, cache: {{dir: passes}}}}\n"), "bar_fingerprint"),
        (format!("  - {{on: findings, kind: command, command: {command}, cache: {{dir: passes, bar_fingerprint: []}}}}\n"), "validators[0].cache.bar_fingerprint"),
        (format!("  - {{on: findings, kind: command, command: {command}, cache: {{dir: passes, bar_fingerprint: {command}, ttl: 5}}}}\n"), "ttl"),
        (format!("  - {{on: findings, when: {{carries: commands, field: x}}, kind: command, command: {command}}}\n"), "`carries` stands alone"),
        (format!("  - {{on: findings, when: {{carries: \"\"}}, kind: command, command: {command}}}\n"), "not a field path"),
        (format!("  - {{on: findings, when: {{field: x, matches: y}}, kind: command, command: {command}}}\n"), "matches"),
        (format!("  - {{on: \"not a queue\", kind: command, command: {command}}}\n"), "not a queue"),
    ] {
        config("tests/validators.rs a_validators_block_is_refused_…", loaded(&validators), names);
    }
    // e2e queues.rs: a configuration with `policy` misspelt (no version: the
    // binary's own default head is not part of the text it reads).
    config("e2e queues.rs a_configuration_declares_an_open_author_…", "queues:\n  findings: {polcy: {hold_pending: false}}\n".to_owned(), "unknown field `polcy`");
    // e2e events.rs: the `merge --filter` refusal (the CLI reads it under Open).
    cases.push(case(Kind::Filter, Origin::Asserted, "asserted e2e events.rs merge --filter", r#"{"only":[{"kind":"x"}]}"#, Some("unknown field `only`")));
    cases.push(case(Kind::Filter, Origin::Asserted, "asserted e2e events.rs merge --filter (lists named)", r#"{"only":[{"kind":"x"}]}"#, Some("expected `include` or `exclude`")));
    cases.push(case(Kind::Filter, Origin::Asserted, "asserted e2e events.rs merge --filter (blank tenant)", r#"{"exclude":[{"tenant":""}]}"#, Some(r#"exclude[0] {"tenant":""}"#)));
    // tests/queue.rs: Predicate::read's refusals.
    for (spec, says) in [
        (r#"{"field": "a"}"#, "this names none"),
        (r#"{"field": "a", "equals": 1, "present": true}"#, "this names equals and present"),
        (r#"{"equals": 1}"#, "this names no `field`"),
        (r#"{"field": "a", "all": []}"#, "takes no `field`"),
        (r#"{"field": "", "equals": 1}"#, "it is empty"),
        (r#"{"field": "a..b", "equals": 1}"#, "one of its keys is empty"),
        (r#"{"all": [{"field": "a"}]}"#, "this names none"),
        (r#"{"nope": 1}"#, "unknown field"),
    ] {
        cases.push(case(Kind::Predicate, Origin::Asserted, "asserted tests/queue.rs Predicate::read", spec, Some(says)));
    }
}

fn onevcs(cases: &mut Vec<Case>) {
    // onevcs crates/onevcs/tests/contract.rs
    // `a_filter_spec_the_grammar_does_not_name_is_refused_where_it_is_read`, and
    // tests/e2e/filter.rs `a_spec_the_grammar_does_not_name_is_refused_…`.
    for (spec, named) in [
        ("include: [{kind: fetch}, {kinds: push}]", "`kinds`"),
        ("exclude: [{payload: {}}]", "`payload`"),
        ("include: [{round: 2}]", "`round`"),
        ("include: [fetch]", "include[0]"),
        ("exclude: [[{kind: fetch}]]", "exclude[0]"),
        ("include: [{kind: 7}]", "kind: invalid type"),
        ("exclude: [{node: [service]}]", "expected a string"),
        ("include: {kind: fetch}", "expected a sequence"),
        ("include: [{}]", "naming no field"),
        ("include: [{kind: \"\"}]", "`kind` is empty"),
        ("- {kind: fetch}", "expected struct Filter"),
        ("includes: [{kind: fetch}]", "`includes`"),
        (r#"{"include": [{"kind": "fetch"}, {"kinds": "push"}]}"#, "`kinds`"),
        (r#"{"exclude": [["kind"]]}"#, "exclude[0]"),
        ("include: {kind: fetch}\n", "include: invalid type"),
    ] {
        cases.push(case(Kind::Filter, Origin::Onevcs, format!("onevcs asserted {spec:?}"), spec, Some(named)));
    }
    cases.push(case(Kind::Filter, Origin::Onevcs, "onevcs accepted \"include: [{source: onevcs}]\"", "include: [{source: onevcs}]", None));
}

fn probes(cases: &mut Vec<Case>) {
    let config = [
        ("typed bool spelt yes", "queues:\n  q: {numbered: yes}\n"),
        ("typed bool spelt on/off", "queues:\n  q: {policy: {hold_pending: off, blocking_first: on}}\n"),
        ("Value equals: yes", "queues:\n  q: {claims: {field: a, equals: yes}}\n"),
        ("Value equals: 0o17", "queues:\n  q: {claims: {field: a, equals: 0o17}}\n"),
        ("Value equals: 017", "queues:\n  q: {claims: {field: a, equals: 017}}\n"),
        ("Value equals: 0x1F", "queues:\n  q: {claims: {field: a, equals: 0x1F}}\n"),
        ("Value equals: ~", "queues:\n  q: {claims: {field: a, equals: ~}}\n"),
        ("Value equals: 1e3", "queues:\n  q: {claims: {field: a, equals: 1e3}}\n"),
        ("Value equals: .inf", "queues:\n  q: {claims: {field: a, equals: .inf}}\n"),
        ("Value equals: 2026-10-12", "queues:\n  q: {claims: {field: a, equals: 2026-10-12}}\n"),
        ("Value equals: 18446744073709551616", "queues:\n  q: {claims: {field: a, equals: 18446744073709551616}}\n"),
        ("Value equals: !!str 1", "queues:\n  q: {claims: {field: a, equals: !!str 1}}\n"),
        ("Value equals: \"\\ud83d\\ude00\"", "queues:\n  q: {claims: {field: a, equals: \"\\ud83d\\ude00\"}}\n"),
        ("duplicate top-level key", "queues: {}\nqueues: {}\n"),
        ("duplicate key in a codec (Value pass only)", "queues:\n  prompts: {}\ncodecs:\n  example: {queue: prompts, queue: prompts}\n"),
        ("anchor and alias", "authors:\n  lead: &grant {capabilities: [retry]}\n  bot: *grant\n"),
        ("merge key", "authors:\n  lead: &grant {capabilities: [retry]}\n  bot: {<<: *grant}\n"),
        ("non-string author key", "authors:\n  1: {capabilities: []}\n"),
        ("version as a string", "QUOTED"),
        ("version 1.0", "FLOAT"),
        ("byte-order mark", "BOM"),
        ("two documents", "MULTI"),
        ("empty text", "EMPTY"),
        ("tab-indented", "TAB"),
    ];
    for (label, body) in config {
        let text = match body {
            "QUOTED" => "version: \"1\"\ntransport: {kind: local, dir: x}\n".to_owned(),
            "FLOAT" => "version: 1.0\ntransport: {kind: local, dir: x}\n".to_owned(),
            "BOM" => format!("\u{feff}{LOCAL}"),
            "MULTI" => format!("{LOCAL}---\n{LOCAL}"),
            "EMPTY" => String::new(),
            "TAB" => "version: 1\ntransport:\n\tkind: local\n".to_owned(),
            body => format!("{LOCAL}{body}"),
        };
        cases.push(case(Kind::Config, Origin::Probe, format!("probe config: {label}"), text, None));
    }
    for (label, text) in [
        ("kind: yes", "include: [{kind: yes}]"),
        ("kind: 1", "include: [{kind: 1}]"),
        ("kind: null", "include: [{kind: null}]"),
        ("tenant: off", "include: [{tenant: off}]"),
        ("kind: !!str 7", "include: [{kind: !!str 7}]"),
        ("duplicate kind", "include: [{kind: a, kind: b}]"),
        ("empty text", ""),
    ] {
        cases.push(case(Kind::Filter, Origin::Probe, format!("probe filter: {label}"), text, None));
    }
    for (label, text) in [
        ("equals: yes", "{field: a, equals: yes}"),
        ("equals: no (YAML block)", "field: a\nequals: no\n"),
        ("equals: 0o17", "{field: a, equals: 0o17}"),
        ("equals: 1_000", "{field: a, equals: 1_000}"),
        ("present: yes", "{field: a, present: yes}"),
        ("duplicate field", "{field: a, field: b, present: true}"),
    ] {
        cases.push(case(Kind::Predicate, Origin::Probe, format!("probe predicate: {label}"), text, None));
    }
}

/// Every case, deduplicated by (kind, text): an asserted case supersedes the
/// same text captured without its expectation.
pub fn load() -> Vec<Case> {
    let mut cases = Vec::new();
    asserted(&mut cases);
    onevcs(&mut cases);
    docs(&mut cases);
    captured(&mut cases);
    sdk(&mut cases);
    probes(&mut cases);
    let mut seen = HashSet::new();
    cases.retain(|case| case.expect.is_some() || seen.insert((case.kind, case.text.clone())));
    // An asserted text also seen as captured: drop the captured twin.
    let asserted: HashSet<_> = cases.iter().filter(|c| c.expect.is_some()).map(|c| (c.kind, c.text.clone())).collect();
    cases.retain(|case| case.expect.is_some() || !asserted.contains(&(case.kind, case.text.clone())));
    cases
}

pub fn summary(cases: &[Case]) -> String {
    let mut lines = vec!["corpus (deduplicated by kind and text):".to_owned()];
    for kind in [Kind::Config, Kind::Filter, Kind::Predicate] {
        let mut parts = Vec::new();
        for origin in [Origin::Docs, Origin::Captured, Origin::Sdk, Origin::Asserted, Origin::Onevcs, Origin::Probe] {
            let n = cases.iter().filter(|c| c.kind == kind && c.origin == origin).count();
            if n > 0 {
                parts.push(format!("{origin:?} {n}").to_lowercase());
            }
        }
        lines.push(format!("  {kind:?}: {}", parts.join(", ")).to_lowercase());
    }
    lines.join("\n") + "\n"
}

/// The two configurations the core's tests write with serde_norway and read back.
pub fn round_trips() -> Vec<(String, Config)> {
    let serve = "version: 1\ntransport: {kind: memory}\nqueues:\n  prompts: {}\ncodecs:\n  example:\n    queue: prompts\n    reply_window_seconds: 5\n    session_env: SERVE_SESSION\n    asker_env: CHANNEL_ASKER\n    about_env: NODE\n    select: op\n    frames:\n      hello:\n        schema: example.hello@1\n        bindings:\n          - do: answer\n            response: {ok: true}\n";
    let command = r#"["/repo/target/debug/deps/validators-0","--exact","scripted_validator","scripted-validator-script=/tmp/rig/script.json"]"#;
    let validators = format!(
        "version: 1\ntransport: {{kind: local, dir: \"/tmp/rig/channel\"}}\nqueues:\n  findings: {{}}\nvalidators:\n  - {{on: findings, when: {{carries: reply.commands}}, kind: command, command: {command}, cache: {{dir: passes, bar_fingerprint: {command}}}}}\n  - {{on: findings, when: {{not: {{field: author, present: true}}}}, kind: command, command: {command}}}\n"
    );
    vec![
        ("round trip tests/serve.rs a_codecs_block_loads_by_name_…".to_owned(), Config::parse(serve).expect("the serve.rs configuration reads")),
        ("round trip tests/validators.rs a_validators_block_is_refused_…".to_owned(), Config::parse(&validators).expect("the validators.rs configuration reads")),
    ]
}
