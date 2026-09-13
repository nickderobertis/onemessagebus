//! Contract T: the transport trait, the local layout on disk, the memory
//! transport, and the plugin protocol's serving end.
//!
//! The local transport is held to the files Contract T lays out — by reading
//! the directory, not by asking the transport — because those files are what
//! `onepipeline` reads.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use onemessagebus::transport::{self, PluginHello, PluginReply, PROTOCOL, PROTOCOL_VERSION};
use onemessagebus::{
    Batch, Changed, ConsumerName, DocumentName, LocalTransport, MemoryTransport, Position,
    QueueName, Registry, Transport, TransportConfig, TransportError, TransportKinds,
};
use serde_json::{json, Value};

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

fn consumer(name: &str) -> ConsumerName {
    name.parse().expect("a consumer name")
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("a directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn records(batch: &Batch) -> Vec<String> {
    batch
        .records
        .iter()
        .map(|stored| String::from_utf8(stored.bytes.clone()).expect("UTF-8"))
        .collect()
}

/// The trait is object-safe: a transport is chosen at runtime and held as
/// `Arc<dyn Transport>`, and every method is callable through it.
#[test]
fn the_transport_trait_is_object_safe_and_held_behind_an_arc() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let chosen: Vec<Arc<dyn Transport>> = vec![
        Arc::new(MemoryTransport::new()),
        Arc::new(LocalTransport::open(dir.path()).expect("opens")),
    ];
    for transport in chosen {
        let q = queue("things");
        let at = transport.append(&q, b"{\"n\":1}").expect("appends");
        assert_eq!(
            records(&transport.read(&q, None, 10).expect("reads")),
            vec![r#"{"n":1}"#]
        );
        transport
            .commit(&q, &consumer("default"), &at)
            .expect("commits");
        assert_eq!(
            transport.cursor(&q, &consumer("default")).expect("reads"),
            Some(at)
        );
        let doc: DocumentName = "things.json".parse().expect("a document");
        transport
            .replace_document(&q, &doc, b"{}")
            .expect("replaces");
        assert_eq!(
            transport.document(&q, &doc).expect("reads"),
            Some(b"{}".to_vec())
        );
        let mut ran = false;
        transport
            .exclusive(&q, &mut |inner| {
                inner.append(&q, b"{\"n\":2}")?;
                ran = true;
                Ok(())
            })
            .expect("the section runs");
        assert!(ran);
        let print = transport.fingerprint(&q).expect("a fingerprint");
        assert!(matches!(
            transport
                .wait_for_change(&q, &print, Duration::from_millis(10))
                .expect("waits"),
            Changed::Unchanged(_)
        ));
    }
}

/// The local transport writes `<queue>.jsonl`, `<queue>-cursor.json` for the
/// default consumer, `<queue>-cursor.<consumer>.json` for another, documents at
/// `<dir>/<name>`, and its lock under `.lock/` — and nothing else.
#[test]
fn the_local_transport_lays_its_files_out_as_contract_t_states() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let channel = dir.path().join("channel");
    let local = LocalTransport::open(&channel).expect("opens");
    let replies = queue("replies");

    let first = local.append(&replies, br#"{"id":0}"#).expect("appends");
    let second = local.append(&replies, br#"{"id":1}"#).expect("appends");
    assert_eq!(
        first,
        Position::from_token(9),
        "a position is not the byte offset after the record"
    );
    assert_eq!(second, Position::from_token(18));
    assert_eq!(
        std::fs::read_to_string(channel.join("replies.jsonl")).expect("the records file"),
        "{\"id\":0}\n{\"id\":1}\n"
    );

    local
        .commit(&replies, &consumer("default"), &first)
        .expect("commits");
    local
        .commit(&replies, &consumer("watcher"), &second)
        .expect("commits");
    assert_eq!(
        std::fs::read_to_string(channel.join("replies-cursor.json")).expect("the cursor"),
        "1",
        "a cursor file does not hold the number of records before the position"
    );
    assert_eq!(
        std::fs::read_to_string(channel.join("replies-cursor.watcher.json")).expect("the cursor"),
        "2"
    );
    assert_eq!(
        local.cursor(&replies, &consumer("default")).expect("reads"),
        Some(first)
    );
    assert_eq!(
        local.cursor(&replies, &consumer("watcher")).expect("reads"),
        Some(second)
    );
    assert_eq!(
        local.cursor(&replies, &consumer("nobody")).expect("reads"),
        None
    );

    let projection: DocumentName = "queue.json".parse().expect("a document");
    local
        .replace_document(&queue("surfaces"), &projection, b"{\n  \"waiting\": []\n}")
        .expect("replaces");
    assert_eq!(
        std::fs::read(channel.join("queue.json")).expect("the document"),
        b"{\n  \"waiting\": []\n}"
    );
    local
        .exclusive(&replies, &mut |_| Ok(()))
        .expect("the section runs");
    assert_eq!(
        files(&channel),
        vec![
            ".lock",
            "queue.json",
            "replies-cursor.json",
            "replies-cursor.watcher.json",
            "replies.jsonl"
        ],
        "the local transport left a file Contract T does not lay out"
    );
    assert_eq!(files(&channel.join(".lock")), vec!["replies.lock"]);

    // A cursor file 0.28.2 wrote is read as the position after that many records.
    std::fs::write(channel.join("replies-cursor.json"), "2").expect("a recorded cursor");
    assert_eq!(
        local.cursor(&replies, &consumer("default")).expect("reads"),
        Some(second)
    );
    std::fs::write(channel.join("replies-cursor.json"), "not a number").expect("a broken cursor");
    assert_eq!(
        local.cursor(&replies, &consumer("default")).expect("reads"),
        None,
        "a cursor nobody can read is not a consumer that has read nothing"
    );
}

/// A torn trailing record is reported, never dropped and never fatal; the next
/// append heals it back to the record boundary and records what it discarded.
#[test]
fn a_torn_tail_is_reported_on_read_and_healed_by_the_next_append() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let local = LocalTransport::open(dir.path()).expect("opens");
    let q = queue("surfaces");
    local.append(&q, br#"{"id":0}"#).expect("appends");
    let path = dir.path().join("surfaces.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(b"\n{\"id\":1,\"half"))
        .expect("a torn record");

    let batch = local
        .read(&q, None, usize::MAX)
        .expect("a torn tail is not fatal");
    assert_eq!(
        records(&batch),
        vec![r#"{"id":0}"#],
        "a whole record was lost, or a torn one read"
    );
    let torn = batch.torn.expect("the torn record is reported");
    assert_eq!(torn.at, Position::from_token(10));
    assert_eq!(torn.bytes, 13);

    let healed = local.append(&q, br#"{"id":1}"#).expect("appends");
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file"),
        "{\"id\":0}\n\n{\"id\":1}\n",
        "the fragment was glued onto, or the whole records were cut"
    );
    assert_eq!(healed, Position::from_token(19));
    let loss: Value = serde_json::from_str(
        std::fs::read_to_string(dir.path().join("surfaces.jsonl.torn"))
            .expect("the loss is recorded")
            .trim(),
    )
    .expect("a loss record");
    assert_eq!(loss["offset"], json!(10));
    assert_eq!(loss["bytes"], json!(13));
    assert_eq!(loss["healed_by"], json!(std::process::id()));

    let resumed = local
        .read(&q, Some(&Position::from_token(9)), usize::MAX)
        .expect("reads from a boundary");
    assert_eq!(records(&resumed), vec![r#"{"id":1}"#]);
    assert!(matches!(
        local.read(&q, Some(&Position::from_token(5)), 1),
        Err(TransportError::NotABoundary { .. })
    ));
    assert!(matches!(
        local.read(&q, Some(&Position::from_token(500)), 1),
        Err(TransportError::PastEnd { .. })
    ));
    assert!(matches!(
        local.commit(&q, &consumer("default"), &Position::from_token(3)),
        Err(TransportError::NotABoundary { .. })
    ));
}

/// Names are validated where they enter, refused naming what is wrong.
#[test]
fn a_name_that_would_escape_or_clobber_a_queue_is_refused() {
    for bad in [
        "",
        "../escape",
        "with space",
        "-leading",
        "a/b",
        "surfaces.jsonl",
    ] {
        let refusal = bad.parse::<QueueName>().expect_err(bad);
        assert!(
            refusal.to_string().contains("is not a queue name"),
            "{refusal}"
        );
    }
    for bad in [
        "surfaces.jsonl",
        "replies-cursor.json",
        "x.torn",
        ".hidden",
        "queue.json.staging",
    ] {
        let refusal = bad.parse::<DocumentName>().expect_err(bad);
        assert!(
            refusal.to_string().contains("is not a document name"),
            "{refusal}"
        );
    }
    assert_eq!(
        "queue.json"
            .parse::<DocumentName>()
            .expect("a document")
            .as_str(),
        "queue.json"
    );
    assert_eq!(
        "command-outcomes"
            .parse::<QueueName>()
            .expect("a queue")
            .to_string(),
        "command-outcomes"
    );
    assert!(consumer("default").is_default());
    let refusal = serde_json::from_value::<QueueName>(json!("a/b")).expect_err("refused on read");
    assert!(refusal.to_string().contains("a/b"), "{refusal}");
}

/// A record is one non-empty line.
#[test]
fn a_record_that_is_empty_or_spans_lines_is_refused_on_every_transport() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let transports: Vec<Arc<dyn Transport>> = vec![
        Arc::new(MemoryTransport::new()),
        Arc::new(LocalTransport::open(dir.path()).expect("opens")),
    ];
    for transport in transports {
        for bad in [&b""[..], b"  ", b"{\"a\":1}\n{\"b\":2}"] {
            let refusal = transport.append(&queue("q"), bad).expect_err("refused");
            assert!(
                matches!(refusal, TransportError::NotARecord { .. }),
                "{refusal:?}"
            );
        }
        assert!(transport
            .read(&queue("q"), None, 10)
            .expect("reads")
            .records
            .is_empty());
    }
}

/// An exclusive section excludes another writer to the same queue for as long
/// as its body runs, in another thread of the same process.
#[test]
fn an_exclusive_section_holds_other_writers_off_until_its_body_returns() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let transports: Vec<Arc<dyn Transport>> = vec![
        Arc::new(MemoryTransport::new()),
        Arc::new(LocalTransport::open(dir.path()).expect("opens")),
    ];
    for transport in transports {
        let q = queue("claims");
        let other = Arc::clone(&transport);
        let (entered, entered_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let transport = Arc::clone(&transport);
            let q = q.clone();
            std::thread::spawn(move || {
                transport
                    .exclusive(&q, &mut |inner| {
                        inner.append(&q, b"\"inside\"")?;
                        entered.send(()).expect("signals");
                        release_rx.recv().expect("released");
                        inner.append(&q, b"\"still inside\"")?;
                        // A nested section over the same queue is the same section.
                        inner.exclusive(&q, &mut |nested| {
                            nested.append(&q, b"\"nested\"").map(|_| ())
                        })
                    })
                    .expect("the section runs");
            })
        };
        entered_rx.recv().expect("the section was entered");
        let writer = std::thread::spawn(move || {
            other
                .append(&queue("claims"), b"\"outside\"")
                .expect("appends");
        });
        std::thread::sleep(Duration::from_millis(80));
        release.send(()).expect("releases");
        holder.join().expect("the holder finishes");
        writer.join().expect("the writer finishes");
        assert_eq!(
            records(&transport.read(&q, None, 10).expect("reads")),
            vec![
                "\"inside\"",
                "\"still inside\"",
                "\"nested\"",
                "\"outside\""
            ],
            "a writer landed inside another's exclusive section"
        );
    }
}

/// The built-in kinds open from configuration, a kind registered in-process
/// opens after them, and a kind nothing serves is refused naming the kinds
/// there are.
#[test]
fn transport_kinds_resolve_built_in_then_registered_then_plugin() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let mut kinds = TransportKinds::builtin().searching(vec![dir.path().to_path_buf()]);
    let local = kinds
        .open(&TransportConfig::local(dir.path().join("channel")))
        .expect("the local kind opens");
    local.append(&queue("q"), b"1").expect("appends");
    assert!(dir.path().join("channel/q.jsonl").is_file());

    let refusal = kinds
        .open(&TransportConfig {
            kind: "local".parse().expect("a transport kind"),
            dir: Some(dir.path().to_path_buf()),
            options: serde_json::from_value(json!({"url": "nats://x"})).expect("options"),
        })
        .err()
        .expect("a key the local kind does not take");
    assert_eq!(
        refusal.to_string(),
        "transport.url is not a key the local transport takes"
    );
    let refusal = kinds
        .open(&TransportConfig {
            kind: "local".parse().expect("a transport kind"),
            dir: None,
            options: serde_json::Map::new(),
        })
        .err()
        .expect("the local kind needs a directory");
    assert!(refusal.to_string().contains("transport.dir"), "{refusal}");

    let shared: Arc<dyn Transport> = Arc::new(MemoryTransport::new());
    let registered = Arc::clone(&shared);
    kinds
        .register("shared", Arc::new(move |_| Ok(Arc::clone(&registered))))
        .expect("registers");
    assert!(kinds
        .register(
            "shared",
            Arc::new(|_| Ok(Arc::new(MemoryTransport::new()) as Arc<dyn Transport>))
        )
        .is_err());
    assert!(kinds
        .register(
            "Not A Kind",
            Arc::new(|_| Ok(Arc::new(MemoryTransport::new()) as Arc<dyn Transport>))
        )
        .is_err());
    let opened = kinds
        .open(&TransportConfig {
            kind: "shared".parse().expect("a transport kind"),
            dir: None,
            options: serde_json::Map::new(),
        })
        .expect("the registered kind opens");
    opened.append(&queue("q"), b"2").expect("appends");
    assert_eq!(
        records(&shared.read(&queue("q"), None, 10).expect("reads")),
        vec!["2"]
    );

    let refusal = kinds
        .open(&TransportConfig {
            kind: "nats".parse().expect("a transport kind"),
            dir: None,
            options: serde_json::Map::new(),
        })
        .err()
        .expect("nothing serves nats");
    assert!(
        refusal.to_string().starts_with("\"nats\" is not a transport kind this build can open; the kinds there are: local, memory, shared"),
        "{refusal}"
    );
    let listed: Vec<String> = kinds
        .kinds()
        .into_iter()
        .map(|entry| entry.kind.to_string())
        .collect();
    assert_eq!(listed, vec!["local", "memory", "shared"]);
}

/// A plugin executable on the search path is listed as a kind, shadowed by a
/// built-in of the same name, and a non-executable file is not a plugin.
#[cfg(unix)]
#[test]
fn plugins_on_the_search_path_are_listed_and_a_shadowed_one_is_not() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("a scratch directory");
    for (name, mode) in [
        ("onemessagebus-transport-nats", 0o755),
        ("onemessagebus-transport-local", 0o755),
        ("onemessagebus-transport-idle", 0o644),
        ("unrelated", 0o755),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").expect("a file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("its mode");
    }
    let kinds = TransportKinds::builtin().searching(vec![dir.path().to_path_buf()]);
    let listed: Vec<(String, Option<String>)> = kinds
        .kinds()
        .into_iter()
        .map(|entry| {
            (
                entry.kind.to_string(),
                entry.path.map(|path| {
                    path.file_name()
                        .expect("a name")
                        .to_string_lossy()
                        .into_owned()
                }),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            ("local".to_owned(), None),
            ("memory".to_owned(), None),
            (
                "nats".to_owned(),
                Some("onemessagebus-transport-nats".to_owned())
            ),
        ]
    );
    assert_eq!(
        kinds.plugin("nats").and_then(|path| path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())),
        Some("onemessagebus-transport-nats".to_owned())
    );
    assert!(
        kinds.plugin("idle").is_none(),
        "a file that is not executable is a plugin"
    );
}

/// Drive `serve` over in-memory pipes, one request line at a time.
struct Served {
    replies: Vec<Value>,
    result: Result<(), TransportError>,
}

fn serve_lines(lines: &[Value], transport: Arc<dyn Transport>) -> Served {
    let input: String = lines.iter().map(|line| format!("{line}\n")).collect();
    let mut output = Vec::new();
    let result = transport::serve(move |_| Ok(transport), input.as_bytes(), &mut output);
    let replies = BufReader::new(output.as_slice())
        .lines()
        .map(|line| serde_json::from_str(&line.expect("a line")).expect("a reply is JSON"))
        .collect();
    Served { replies, result }
}

fn hello() -> Value {
    serde_json::to_value(PluginHello {
        protocol: PROTOCOL.to_owned(),
        version: PROTOCOL_VERSION,
        config: TransportConfig {
            kind: "memory".parse().expect("a transport kind"),
            dir: None,
            options: serde_json::Map::new(),
        },
    })
    .expect("a hello")
}

/// The protocol carries records and documents as UTF-8 text, so the serving
/// end refuses bytes that are not, rather than handing them on altered.
#[test]
fn the_serving_end_refuses_a_record_or_document_that_is_not_utf8() {
    let memory: Arc<dyn Transport> = Arc::new(MemoryTransport::new());
    memory
        .append(&queue("q"), b"{\"n\":\"\xff\"}")
        .expect("a transport holds bytes");
    let name: DocumentName = "q.json".parse().expect("a document name");
    memory
        .replace_document(&queue("q"), &name, b"\xfe")
        .expect("a transport holds a document of bytes");
    let served = serve_lines(
        &[
            hello(),
            json!({"op": "read", "queue": "q", "limit": 10}),
            json!({"op": "document", "queue": "q", "name": "q.json"}),
        ],
        memory,
    );
    served.result.expect("serving goes on after a refusal");
    for (reply, what) in [
        (&served.replies[1], "a record"),
        (&served.replies[2], "document q.json"),
    ] {
        assert_eq!(reply["error"]["kind"], json!("refused"), "{reply}");
        let message = reply["error"]["message"].as_str().expect("a message");
        assert!(
            message.contains(what) && message.contains("is not UTF-8"),
            "{message}"
        );
    }
}

/// The serving end answers the hello with its protocol and version, each
/// request with one reply, a section's requests inside it, and a refusal the
/// client can turn back into the transport error it was.
#[test]
fn the_plugin_protocol_serves_every_method_and_names_its_version_first() {
    let memory: Arc<dyn Transport> = Arc::new(MemoryTransport::new());
    let served = serve_lines(
        &[
            hello(),
            json!({"op": "append", "queue": "q", "record": "{\"n\":1}"}),
            json!({"op": "begin_exclusive", "queue": "q"}),
            json!({"op": "append", "queue": "q", "record": "{\"n\":2}"}),
            json!({"op": "end_exclusive", "queue": "q", "failed": false}),
            json!({"op": "read", "queue": "q", "limit": 10}),
            json!({"op": "commit", "queue": "q", "consumer": "default", "at": 1}),
            json!({"op": "cursor", "queue": "q", "consumer": "default"}),
            json!({"op": "read", "queue": "q", "from": 9, "limit": 1}),
            json!({"op": "replace_document", "queue": "q", "name": "q.json", "bytes": "{}"}),
            json!({"op": "document", "queue": "q", "name": "q.json"}),
            json!({"op": "fingerprint", "queue": "q"}),
            json!({"op": "wait_for_change", "queue": "q", "since": [0], "timeout_ms": 5}),
            json!({"op": "teleport"}),
            json!({"op": "end_exclusive", "queue": "q", "failed": false}),
        ],
        Arc::clone(&memory),
    );
    served.result.expect("serving ends when the input does");
    let replies = served.replies;
    assert_eq!(
        replies[0],
        json!({"ok": {"hello": {"protocol": PROTOCOL, "version": PROTOCOL_VERSION}}})
    );
    assert_eq!(replies[1], json!({"ok": {"position": 1}}));
    assert_eq!(
        replies[2],
        json!({"ok": "done"}),
        "the section was not opened"
    );
    assert_eq!(replies[3], json!({"ok": {"position": 2}}));
    assert_eq!(
        replies[4],
        json!({"ok": "done"}),
        "the section was not ended"
    );
    assert_eq!(
        replies[5],
        json!({"ok": {"batch": {"records": [{"record": "{\"n\":1}", "after": 1}, {"record": "{\"n\":2}", "after": 2}]}}})
    );
    assert_eq!(replies[6], json!({"ok": "done"}));
    assert_eq!(replies[7], json!({"ok": {"cursor": 1}}));
    assert_eq!(
        replies[8]["error"]["kind"],
        json!("past_end"),
        "{}",
        replies[8]
    );
    assert_eq!(replies[8]["error"]["end"], json!(2));
    assert_eq!(replies[9], json!({"ok": "done"}));
    assert_eq!(replies[10], json!({"ok": {"document": "{}"}}));
    assert!(
        replies[11]["ok"]["fingerprint"].is_array(),
        "{}",
        replies[11]
    );
    assert_eq!(
        replies[12]["ok"]["changed"]["moved"],
        json!(true),
        "{}",
        replies[12]
    );
    assert_eq!(
        replies[13]["error"]["kind"],
        json!("protocol"),
        "{}",
        replies[13]
    );
    assert_eq!(
        replies[14]["error"]["kind"],
        json!("protocol"),
        "an end with no section was accepted"
    );
    for reply in &replies {
        serde_json::from_value::<PluginReply>(reply.clone()).expect("every reply is a PluginReply");
    }
    assert_eq!(
        records(&memory.read(&queue("q"), None, 10).expect("reads")).len(),
        2
    );
}

/// A hello at another version, or a first line that is no hello, is refused
/// before a transport is opened.
#[test]
fn a_hello_at_another_version_is_refused_naming_both() {
    let mut other = hello();
    other["version"] = json!(PROTOCOL_VERSION + 1);
    let served = serve_lines(&[other], Arc::new(MemoryTransport::new()));
    let refusal = served.result.expect_err("another version is refused");
    assert!(
        refusal
            .to_string()
            .contains(&format!("version {}", PROTOCOL_VERSION + 1)),
        "{refusal}"
    );
    assert_eq!(served.replies[0]["error"]["kind"], json!("protocol"));

    let served = serve_lines(
        &[json!({"op": "read", "queue": "q", "limit": 1})],
        Arc::new(MemoryTransport::new()),
    );
    assert!(served.result.is_err());
    assert!(served.replies[0]["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("hello")));

    // A client that goes away inside its section fails the section.
    let served = serve_lines(
        &[hello(), json!({"op": "begin_exclusive", "queue": "q"})],
        Arc::new(MemoryTransport::new()),
    );
    served
        .result
        .expect("the section's failure is answered, not fatal");
    assert_eq!(
        served.replies.last().expect("a reply")["error"]["kind"],
        json!("refused")
    );
}

/// The protocol's three shapes are registered under their ids, so a client in
/// another language validates against the documents this build speaks.
#[test]
fn the_plugin_protocol_shapes_are_registered_schemas() {
    let mut registry = Registry::new();
    transport::register_protocol(&mut registry).expect("registers");
    for id in [
        transport::HELLO_SCHEMA,
        transport::REQUEST_SCHEMA,
        transport::REPLY_SCHEMA,
    ] {
        assert!(registry.schema(&id).is_some(), "{id} is not registered");
    }
    registry
        .check(
            &transport::REQUEST_SCHEMA,
            &json!({"op": "append", "queue": "q", "record": "x"}),
        )
        .expect("a request conforms");
    registry
        .check(
            &transport::REQUEST_SCHEMA,
            &json!({"op": "append", "queue": "q"}),
        )
        .expect_err("a request missing its record");
    registry
        .check(&transport::HELLO_SCHEMA, &hello())
        .expect("the hello conforms");
    registry
        .check(&transport::REPLY_SCHEMA, &json!({"ok": {"position": 3}}))
        .expect("a reply conforms");
}

/// A local transport reports what the host refused, naming the path, and reads
/// the boundaries of its own files exactly.
#[test]
fn a_local_transport_reports_what_the_host_refused_naming_the_path() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let file = dir.path().join("a-file");
    std::fs::write(&file, "").expect("a file");
    let refusal = LocalTransport::open(file.join("channel")).expect_err("a directory under a file");
    assert!(
        refusal.to_string().starts_with("cannot create"),
        "{refusal}"
    );

    let channel = dir.path().join("channel");
    let local = LocalTransport::open(&channel).expect("opens");
    let q = queue("q");
    std::fs::create_dir_all(local.records_path(&q)).expect("a directory where the records go");
    let refusal = local
        .read(&q, None, 1)
        .expect_err("the records file is a directory");
    assert!(refusal.to_string().starts_with("cannot read"), "{refusal}");
    std::fs::remove_dir(local.records_path(&q)).expect("removed");

    let default = consumer("default");
    std::fs::create_dir_all(local.cursor_path(&q, &default))
        .expect("a directory where the cursor goes");
    let refusal = local
        .cursor(&q, &default)
        .expect_err("the cursor file is a directory");
    assert!(refusal.to_string().starts_with("cannot read"), "{refusal}");
    std::fs::remove_dir(local.cursor_path(&q, &default)).expect("removed");

    let name: DocumentName = "d.json".parse().expect("a document");
    std::fs::create_dir_all(channel.join("d.json/inside"))
        .expect("a directory where the document goes");
    let refusal = local
        .document(&q, &name)
        .expect_err("the document is a directory");
    assert!(refusal.to_string().starts_with("cannot read"), "{refusal}");
    let refusal = local
        .replace_document(&q, &name, b"{}")
        .expect_err("a document cannot replace a directory");
    assert!(
        refusal.to_string().starts_with("cannot rename onto"),
        "{refusal}"
    );
    assert!(
        std::fs::read_dir(&channel)
            .expect("the channel")
            .all(|entry| !entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".staging")),
        "a failed replacement left its staging file behind"
    );

    std::fs::write(channel.join(".lock"), "").expect("a file where the locks go");
    let refusal = local.append(&q, b"1").expect_err("no lock directory");
    assert!(
        refusal.to_string().starts_with("cannot create"),
        "{refusal}"
    );
    std::fs::remove_file(channel.join(".lock")).expect("removed");

    let first = local.append(&q, b"1").expect("appends");
    local.append(&q, b"2").expect("appends");
    std::fs::OpenOptions::new()
        .append(true)
        .open(local.records_path(&q))
        .and_then(|mut records| records.write_all(b"3-torn"))
        .expect("a torn tail");
    let batch = local.read(&q, None, 1).expect("reads");
    assert_eq!(records(&batch), vec!["1"]);
    assert_eq!(
        batch.torn, None,
        "a read that stopped short reported the tail as torn"
    );
    std::fs::write(local.cursor_path(&q, &default), "0").expect("a cursor at the start");
    assert_eq!(
        local.cursor(&q, &default).expect("reads"),
        Some(Position::from_token(0))
    );
    assert_ne!(first, Position::from_token(0));

    let refusal = "a b".parse::<DocumentName>().expect_err("a space");
    assert!(refusal.to_string().contains("`.`"), "{refusal}");

    let gone = LocalTransport::open(dir.path().join("gone")).expect("opens");
    std::fs::remove_dir(gone.dir()).expect("removed");
    let refusal = gone.fingerprint(&q).expect_err("no directory to list");
    assert!(refusal.to_string().starts_with("cannot list"), "{refusal}");
}

/// Serving ends quietly on no input, passes over a blank line, answers a
/// section its client failed with that failure, and reports a torn record and a
/// position between records as a local transport does.
#[test]
fn serving_passes_over_blank_lines_and_answers_a_failed_section_and_a_torn_read() {
    let mut output = Vec::new();
    transport::serve(
        |_| Ok(Arc::new(MemoryTransport::new()) as Arc<dyn Transport>),
        &b""[..],
        &mut output,
    )
    .expect("nothing to serve");
    assert!(output.is_empty());

    let dir = tempfile::tempdir().expect("a scratch directory");
    let local = LocalTransport::open(dir.path()).expect("opens");
    let q = queue("q");
    local.append(&q, b"{}").expect("appends");
    std::fs::OpenOptions::new()
        .append(true)
        .open(local.records_path(&q))
        .and_then(|mut records| records.write_all(b"{\"torn"))
        .expect("a torn tail");
    let input = format!(
        "{}\n\n{}\n{}\n{}\n{}\n",
        hello(),
        // Read before the section: entering a section heals the torn tail.
        json!({"op": "read", "queue": "q", "limit": 10}),
        json!({"op": "read", "queue": "q", "from": 1, "limit": 10}),
        json!({"op": "begin_exclusive", "queue": "q"}),
        json!({"op": "end_exclusive", "queue": "q", "failed": true}),
    );
    let mut output = Vec::new();
    transport::serve(
        move |_| Ok(Arc::new(local) as Arc<dyn Transport>),
        input.as_bytes(),
        &mut output,
    )
    .expect("serves");
    let replies: Vec<Value> = String::from_utf8(output)
        .expect("UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a reply"))
        .collect();
    assert_eq!(replies.len(), 5, "{replies:?}");
    assert_eq!(
        replies[1]["ok"]["batch"]["torn"],
        json!({"at": 3, "bytes": 6}),
        "{}",
        replies[1]
    );
    assert_eq!(
        replies[2]["error"]["kind"],
        json!("not_a_boundary"),
        "{}",
        replies[2]
    );
    assert_eq!(replies[2]["error"]["position"], json!(1));
    assert_eq!(replies[3], json!({"ok": "done"}));
    assert_eq!(
        replies[4]["error"]["kind"],
        json!("refused"),
        "{}",
        replies[4]
    );
    assert!(replies[4]["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("section over q failed")));
}

#[cfg(unix)]
fn plugin_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("a plugin script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    path
}

/// A plugin that does not speak the protocol is refused saying what it did: it
/// could not be started, exited, answered with a line that is no reply, greeted
/// at another version, or answered a request with the wrong answer.
#[cfg(unix)]
#[test]
fn a_plugin_that_does_not_speak_the_protocol_is_refused_saying_what_it_did() {
    use onemessagebus::{Fingerprint, ProcessTransport};
    let dir = tempfile::tempdir().expect("a scratch directory");
    let config = TransportConfig {
        kind: "broken".parse().expect("a transport kind"),
        dir: None,
        options: serde_json::Map::new(),
    };
    let greeting = r#"{"ok":{"hello":{"protocol":"onemessagebus-transport","version":1}}}"#;
    for (name, body, says) in [
        ("exits", "read line; exit 0".to_owned(), "exited without answering".to_owned()),
        ("garbage", "read line; echo 'not a reply'".to_owned(), "a line that is not a onemessagebus-transport reply".to_owned()),
        (
            "older",
            r#"read line; echo '{"ok":{"hello":{"protocol":"onemessagebus-transport","version":0}}}'"#.to_owned(),
            "speaks onemessagebus-transport version 0, and this build speaks onemessagebus-transport version 1".to_owned(),
        ),
        ("rude", r#"read line; echo '{"ok":"done"}'"#.to_owned(), "answered the hello with Done".to_owned()),
    ] {
        let path = plugin_script(dir.path(), name, &body);
        let refusal = ProcessTransport::spawn(&path, &config)
            .err()
            .unwrap_or_else(|| panic!("the {name} plugin was accepted"));
        assert!(refusal.to_string().contains(&says), "{name}: {refusal}");
    }
    let refusal = ProcessTransport::spawn(&dir.path().join("absent"), &config)
        .expect_err("a plugin that is not there");
    assert!(
        refusal.to_string().contains("cannot start the plugin"),
        "{refusal}"
    );

    let q = queue("q");
    let default = consumer("default");
    let name: DocumentName = "d.json".parse().expect("a document");
    let done = plugin_script(
        dir.path(),
        "done",
        &format!(
            r#"read line; echo '{greeting}'; while read line; do echo '{{"ok":"done"}}'; done"#
        ),
    );
    let answers_done = ProcessTransport::spawn(&done, &config).expect("greets");
    assert_eq!(answers_done.kind(), "broken");
    assert!(format!("{answers_done:?}").contains("broken"));
    for (asked, refusal) in [
        ("append", answers_done.append(&q, b"x").err()),
        ("read", answers_done.read(&q, None, 1).err()),
        ("cursor", answers_done.cursor(&q, &default).err()),
        ("fingerprint", answers_done.fingerprint(&q).err()),
        (
            "wait_for_change",
            answers_done
                .wait_for_change(&q, &Fingerprint::default(), Duration::from_millis(10))
                .err(),
        ),
        ("document", answers_done.document(&q, &name).err()),
    ] {
        let refusal = refusal.unwrap_or_else(|| panic!("a wrong answer to {asked} was accepted"));
        assert!(
            refusal
                .to_string()
                .contains(&format!("the plugin answered {asked} with")),
            "{refusal}"
        );
    }
    let refusal = answers_done.append(&q, &[0xff]).expect_err("not UTF-8");
    assert!(refusal.to_string().contains("is not UTF-8"), "{refusal}");
    let refusal = answers_done
        .replace_document(&q, &name, &[0xff])
        .expect_err("not UTF-8");
    assert!(refusal.to_string().contains("is not UTF-8"), "{refusal}");
    let failed = answers_done
        .exclusive(&q, &mut |_| {
            Err(TransportError::Backend {
                transport: "body".to_owned(),
                detail: "the body failed".to_owned(),
            })
        })
        .expect_err("the body's failure is the section's");
    assert!(failed.to_string().contains("the body failed"), "{failed}");

    let positioned = plugin_script(
        dir.path(),
        "positioned",
        &format!(
            r#"read line; echo '{greeting}'; while read line; do echo '{{"ok":{{"position":1}}}}'; done"#
        ),
    );
    let answers_position = ProcessTransport::spawn(&positioned, &config).expect("greets");
    for (asked, refusal) in [
        (
            "commit",
            answers_position
                .commit(&q, &default, &Position::from_token(1))
                .err(),
        ),
        (
            "replace_document",
            answers_position.replace_document(&q, &name, b"{}").err(),
        ),
        (
            "begin_exclusive",
            answers_position.exclusive(&q, &mut |_| Ok(())).err(),
        ),
    ] {
        let refusal = refusal.unwrap_or_else(|| panic!("a wrong answer to {asked} was accepted"));
        assert!(
            refusal
                .to_string()
                .contains(&format!("the plugin answered {asked} with")),
            "{refusal}"
        );
    }
    let ends_wrong = plugin_script(
        dir.path(),
        "ends-wrong",
        &format!(
            r#"read line; echo '{greeting}'; read line; echo '{{"ok":"done"}}'; read line; echo '{{"ok":{{"position":1}}}}'"#
        ),
    );
    let refusal = ProcessTransport::spawn(&ends_wrong, &config)
        .expect("greets")
        .exclusive(&q, &mut |_| Ok(()))
        .expect_err("a wrong answer to the section's end");
    assert!(
        refusal
            .to_string()
            .contains("the plugin answered end_exclusive with"),
        "{refusal}"
    );
}

/// A wait whose timeout has no deadline `Instant` can represent is a wait with
/// no bound, not a panic: it still returns the moment the queue moves.
#[test]
fn a_wait_with_no_representable_deadline_returns_when_the_queue_moves() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let transports: Vec<Arc<dyn Transport>> = vec![
        Arc::new(MemoryTransport::new()),
        Arc::new(LocalTransport::open(dir.path()).expect("opens")),
    ];
    for transport in transports {
        let q = queue("moves");
        let since = transport.fingerprint(&q).expect("a fingerprint");
        let writer = Arc::clone(&transport);
        let appending = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            writer.append(&queue("moves"), b"1").expect("appends");
        });
        let changed = transport
            .wait_for_change(&q, &since, Duration::MAX)
            .expect("a wait with no bound");
        appending.join().expect("the writer finishes");
        assert!(matches!(changed, Changed::Moved(_)), "{changed:?}");
    }
}
