//! What the infallible emit does with an envelope it cannot record: the
//! envelope is still returned, no line (and no part of one) reaches the sink,
//! the next envelope is written whole, and the failure is said on stderr.
//!
//! The stderr warning is printed with `eprintln!`, which a test cannot capture
//! in-process, so the tests that assert it re-run this test binary as a child
//! (the same test, selected by name, with an environment variable choosing the
//! child's half) and read the child's stderr through a pipe.

use std::process::{Command, Output};

use onemessagebus::{
    Emitter, EmitterError, Envelope, LabelMatch, Labels, Open, Redactor, Reserved, Source,
    Vocabulary,
};
use schemars::JsonSchema;
use serde::ser::{Error as _, SerializeMap as _};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Map;

/// The value the till dimension refuses to serialize.
const JAMMED: &str = "jammed";
/// What the refusing `Serialize` impl says, which the warning must carry.
const JAMMED_SAYS: &str = "the till is jammed";

/// Set in a child run to the name of the test whose child half it runs.
const CHILD: &str = "ONEMESSAGEBUS_TEST_UNRECORDED_CHILD";
/// The stream file a child writes to.
const CHILD_FILE: &str = "ONEMESSAGEBUS_TEST_UNRECORDED_FILE";

/// A vocabulary of this test's own whose one dimension is written through a
/// hand-rolled `Serialize` — the kind of impl a consumer supplies, and one the
/// core cannot promise succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shop;

#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
struct Till {
    #[serde(default)]
    till: Option<String>,
}

impl Serialize for Till {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.till.as_deref() == Some(JAMMED) {
            return Err(S::Error::custom(JAMMED_SAYS));
        }
        let mut map = serializer.serialize_map(None)?;
        if let Some(till) = &self.till {
            map.serialize_entry("till", till)?;
        }
        map.end()
    }
}

impl Vocabulary for Shop {
    type Source = Source;
    type Dimensions = Till;
    type Labels = Labels;
    type Fields = LabelMatch;

    const NAME: &'static str = "shop";
    const RESERVED: &'static [Reserved] = &[];
    const DIMENSIONS: &'static [Reserved] = &[Reserved::text("till")];
    const DEFAULT_SOURCE: &'static str = "register";

    fn write_version(_: &Self::Source) -> u32 {
        1
    }
}

fn till(name: &str) -> Till {
    Till {
        till: Some(name.to_owned()),
    }
}

/// Whether this process is the child half of `test`.
fn is_child_of(test: &str) -> bool {
    std::env::var(CHILD).is_ok_and(|role| role == test)
}

/// Re-run this test binary as the child half of `test`, with `env`, and hand
/// back what it said.
fn child(test: &str, env: &[(&str, &std::ffi::OsStr)]) -> Output {
    let mut command = Command::new(std::env::current_exe().expect("the test binary's own path"));
    command
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, test);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("the test binary re-runs")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[derive(Clone, Default)]
struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("sink").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("sink").clone()).expect("UTF-8")
    }
}

fn seqs_and_kinds(text: &str) -> Vec<(u64, String)> {
    text.lines()
        .map(|line| {
            let envelope: Envelope<Shop> = serde_json::from_str(line).expect("a whole envelope");
            (envelope.seq, envelope.kind.as_str().to_owned())
        })
        .collect()
}

/// Over a single-writer sink: the refusal is handed back naming the stream,
/// the seq and the serializer's words; nothing reaches the sink for it; and,
/// as after a failed write, the in-memory number stays consumed.
#[test]
fn an_envelope_that_will_not_serialize_is_handed_back_and_never_written() {
    let sink = Captured::default();
    let emitter = Emitter::<Shop>::new("till-1", Source::from("register"), Box::new(sink.clone()))
        .with_redactor(Redactor::new());
    emitter.emit_stamped("opened", till("front"), Map::new(), Vec::new());

    let unrecorded = emitter
        .try_emit_stamped("rang-up", till(JAMMED), Map::new(), Vec::new())
        .expect_err("a dimension that will not serialize is not recorded");
    assert_eq!(unrecorded.envelope.kind.as_str(), "rang-up");
    assert_eq!(unrecorded.envelope.dimensions, till(JAMMED));
    match &unrecorded.error {
        EmitterError::Serialize { stream, seq, .. } => {
            assert_eq!(stream, "till-1");
            assert_eq!(*seq, 2);
        }
        other => panic!("not a serialization refusal: {other}"),
    }
    let said = unrecorded.to_string();
    assert!(said.contains("stream till-1 at seq 2"), "{said}");
    assert!(said.contains(JAMMED_SAYS), "{said}");
    assert_eq!(
        sink.text().lines().count(),
        1,
        "nothing written for the refused envelope"
    );

    let infallible = emitter.emit_stamped("rang-up", till(JAMMED), Map::new(), Vec::new());
    assert_eq!(
        infallible.seq, 3,
        "the infallible spelling hands it back too"
    );

    let closed = emitter.emit_stamped("closed", till("front"), Map::new(), Vec::new());
    assert_eq!(closed.seq, 4);
    let text = sink.text();
    assert!(text.ends_with('\n'), "{text}");
    assert_eq!(
        seqs_and_kinds(&text),
        [(1, "opened".to_owned()), (4, "closed".to_owned())]
    );
}

/// Over a shared file, through the infallible emit: the warning names the
/// stream, the seq and what the serializer said; the file holds only whole
/// lines; the lock is released, so the next emit — which takes the file's lock
/// afresh — writes, and takes the number the refused envelope did not use.
#[test]
fn emit_warns_on_stderr_when_an_envelope_will_not_serialize() {
    const TEST: &str = "emit_warns_on_stderr_when_an_envelope_will_not_serialize";
    if is_child_of(TEST) {
        let path = std::env::var_os(CHILD_FILE).expect("the parent names the file");
        let emitter = Emitter::<Shop>::shared("till-1", Source::from("register"), &path)
            .with_redactor(Redactor::new());
        emitter.emit_stamped("opened", till("front"), Map::new(), Vec::new());
        let refused = emitter.emit_stamped("rang-up", till(JAMMED), Map::new(), Vec::new());
        assert_eq!(refused.kind.as_str(), "rang-up");
        assert_eq!(refused.seq, 2);
        assert_eq!(refused.dimensions, till(JAMMED));
        let next = emitter.emit_stamped("closed", till("front"), Map::new(), Vec::new());
        assert_eq!(next.seq, 2, "the file numbers only what it holds");
        return;
    }
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("till.ndjson");
    let output = child(TEST, &[(CHILD_FILE, path.as_os_str())]);
    let stderr = stderr_of(&output);
    assert!(output.status.success(), "the child failed: {stderr}");
    assert!(
        stderr.contains(&format!(
            "onemessagebus: warning: cannot record a rang-up event on stream till-1 at seq 2: \
             it does not serialize to JSON: {JAMMED_SAYS}"
        )),
        "{stderr}"
    );
    let text = std::fs::read_to_string(&path).expect("the stream file");
    assert!(text.ends_with('\n'), "no partial line: {text}");
    assert_eq!(
        seqs_and_kinds(&text),
        [(1, "opened".to_owned()), (2, "closed".to_owned())]
    );
}

/// Through the infallible emit over a sink that refuses the write: the
/// envelope comes back and the warning names the kind, the stream and what the
/// sink said.
#[test]
fn emit_warns_on_stderr_when_the_sink_refuses_the_write() {
    const TEST: &str = "emit_warns_on_stderr_when_the_sink_refuses_the_write";
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("the sink is closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    if is_child_of(TEST) {
        let emitter = Emitter::<Open>::new("s-1", Source::from("billing"), Box::new(Broken))
            .with_redactor(Redactor::new());
        let envelope = emitter.emit("payment-taken", Map::new());
        assert_eq!(envelope.kind.as_str(), "payment-taken");
        assert_eq!(envelope.seq, 1);
        return;
    }
    let output = child(TEST, &[]);
    let stderr = stderr_of(&output);
    assert!(output.status.success(), "the child failed: {stderr}");
    assert!(
        stderr.contains(
            "onemessagebus: warning: cannot record a payment-taken event on stream s-1: \
             the sink is closed"
        ),
        "{stderr}"
    );
}
