//! `docs/hooks.md`, held to the crate: every fenced block in it is a fixture a
//! test here drives, so the document cannot state a shape the code does not
//! read and write.

use std::collections::BTreeSet;

use onemessagebus::{
    CommandHook, Config, Dropped, HookCache, HookContext, HookOutcome, HookRef, Layouts,
    QueueConfig, QueueName, RawRecord, TransportKinds, When, HOOK_RECORD_VERSION,
};
use serde_json::Value;

const HOOKS: &str = include_str!("../../../docs/hooks.md");

/// The fenced block tagged `<!-- fixture: name -->`.
fn fixture(name: &str) -> String {
    let tag = format!("<!-- fixture: {name} -->");
    let after = HOOKS
        .split_once(&tag)
        .unwrap_or_else(|| panic!("docs/hooks.md has no {tag}"))
        .1;
    let mut lines = after.lines().skip_while(|line| line.trim().is_empty());
    assert!(
        lines.next().is_some_and(|line| line.starts_with("```")),
        "{tag} is not on the line before a fence"
    );
    lines
        .take_while(|line| !line.starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

#[test]
fn every_fenced_block_is_a_fixture_a_test_here_drives() {
    let mut fenced = Vec::new();
    let mut inside = false;
    let mut previous = "";
    for line in HOOKS.lines() {
        if line.trim_start().starts_with("```") {
            if !inside {
                let name = previous
                    .trim()
                    .strip_prefix("<!-- fixture: ")
                    .and_then(|rest| rest.strip_suffix(" -->"))
                    .unwrap_or_else(|| panic!("a fenced block has no fixture tag: {line}"));
                fenced.push(name.to_owned());
            }
            inside = !inside;
        }
        previous = line;
    }
    assert!(!inside, "a fence in docs/hooks.md never closes");
    assert_eq!(
        fenced.iter().cloned().collect::<BTreeSet<_>>(),
        ["drop-line", "hook-record", "hooks-config"]
            .map(str::to_owned)
            .into_iter()
            .collect(),
        "a fenced block nobody drives, or a driven one the document dropped"
    );
}

#[test]
fn the_documented_hooks_block_loads_resolves_and_refuses_an_unknown_key() {
    let text = format!(
        "version: 1\ntransport: {{kind: local, dir: bus}}\nqueues: {{reports: {{}}, manager: {{}}, archive: {{}}}}\n{}",
        fixture("hooks-config")
    );
    let config = Config::parse(&text).expect("the documented block loads");
    let [triage, strip] = config.hooks.as_slice() else {
        panic!("the documented block declares two hooks: {config:?}");
    };
    assert_eq!(triage.on, queue("reports"));
    assert_eq!(
        triage.when,
        Some(When::Carries("verdict".parse().expect("a field path")))
    );
    assert_eq!(triage.command, ["python3", "-m", "triage_reports"]);
    assert_eq!(
        triage.routes,
        Some(vec![queue("manager"), queue("archive")])
    );
    let cache = triage.cache.as_ref().expect("the documented cache");
    assert_eq!(cache.dir, std::path::Path::new(".hook-outcomes"));
    assert_eq!(cache.bar_fingerprint, ["scripts/bar-fingerprint.sh"]);
    assert_eq!(strip.command, ["scripts/strip-noise.sh"]);
    assert!(strip.when.is_none() && strip.routes.is_none() && strip.cache.is_none());

    let dir = tempfile::tempdir().expect("a temp dir");
    config
        .clone()
        .with_transport_dir(dir.path())
        .resolve(&Layouts::new(), &TransportKinds::builtin())
        .expect("the documented hooks resolve over the queues they name");

    let mut narrowed = config.with_transport_dir(dir.path());
    narrowed.queues.remove(&queue("archive"));
    let refused = narrowed
        .resolve(&Layouts::new(), &TransportKinds::builtin())
        .expect_err("a route naming no declared queue");
    assert!(
        refused
            .to_string()
            .starts_with("hooks[0].routes[1]: `archive`"),
        "{refused}"
    );

    let refused =
        Config::parse(&text.replace("command: [scripts", "kind: command, command: [scripts"))
            .expect_err("a hook takes no kind");
    assert!(
        refused.to_string().contains("unknown field `kind`"),
        "{refused}"
    );
    let mut declared = Config::local(dir.path(), None);
    declared
        .queues
        .insert(queue("reports-dropped"), QueueConfig::default());
    let reserved = declared
        .resolve(&Layouts::new(), &TransportKinds::builtin())
        .expect_err("the suffix is reserved");
    assert!(
        reserved.to_string().starts_with("queues.reports-dropped: "),
        "{reserved}"
    );
}

#[test]
fn the_documented_drop_line_is_what_a_drop_reads_and_writes() {
    let line = fixture("drop-line");
    let dropped = Dropped::parse(line.as_bytes()).expect("the documented line reads");
    assert_eq!(dropped.queue, queue("reports"));
    assert_eq!(
        dropped.hook,
        HookRef {
            index: 1,
            command: Some(vec!["scripts/strip-noise.sh".to_owned()]),
        }
    );
    assert_eq!(
        dropped.message.bytes(),
        r#"{"kind":"report","text":"all quiet"}"#
    );
    assert_eq!(
        dropped.correlation.as_ref().map(|c| c.as_str()),
        Some("c-5f0e8a2b9c4d4e1f8a7b6c5d4e3f2a1b")
    );
    assert_eq!(dropped.line(), line, "a drop is not written as documented");

    let mut spelled = dropped.clone();
    spelled.message = RawRecord::parse("{ \"kind\" : \"report\" }").expect("JSON");
    spelled.correlation = None;
    spelled.hook.command = None;
    let written = spelled.line();
    assert!(
        written.contains(r#""hook":{"index":1},"message":{ "kind" : "report" }}"#),
        "a code hook's drop names no command, an uncorrelated one no correlation, and the message keeps its bytes: {written}"
    );
    assert_eq!(Dropped::parse(written.as_bytes()), Ok(spelled));
}

#[test]
fn the_documented_hook_record_is_what_a_cached_change_writes() {
    let documented: Value = serde_json::from_str(&fixture("hook-record")).expect("JSON");
    let keys = |record: &Value| -> Vec<String> {
        record
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(
        documented["schema_version"],
        Value::from(HOOK_RECORD_VERSION)
    );

    let dir = tempfile::tempdir().expect("a temp dir");
    let cache = HookCache::new(dir.path().join("outcomes"), ["echo", "bar-1"]).expect("a bar");
    // A script file, not `sh -c`: Windows quotes an argument's own double
    // quotes for its own C runtime, not for `sh`.
    let script = dir.path().join("report.sh");
    std::fs::write(
        &script,
        "cat > /dev/null\necho '{\"kind\":\"report\",\"text\":\"all quiet\"}'\n",
    )
    .expect("written");
    let hook = CommandHook::new(["sh", script.to_str().expect("a UTF-8 path")])
        .expect("an argv")
        .with_cache(cache);
    let outcome = hook.run(br#"{"kind":"report"}"#, &HookContext::new(queue("reports")));
    assert!(matches!(outcome, HookOutcome::Change(_)), "{outcome:?}");
    let records: Vec<_> = std::fs::read_dir(dir.path().join("outcomes"))
        .expect("the cache")
        .map(|entry| entry.expect("an entry").path())
        .collect();
    let [path] = records.as_slice() else {
        panic!("one record: {records:?}");
    };
    assert!(
        path.to_str()
            .is_some_and(|path| path.ends_with(".pass.json")),
        "{path:?}"
    );
    let written: Value = serde_json::from_slice(&std::fs::read(path).expect("read")).expect("JSON");
    assert_eq!(keys(&written), keys(&documented));
    assert_eq!(written["outcome"], documented["outcome"]);
    assert_eq!(written["output"], documented["output"]);
    assert_eq!(written["fingerprint"], Value::from("bar-1"));
}
