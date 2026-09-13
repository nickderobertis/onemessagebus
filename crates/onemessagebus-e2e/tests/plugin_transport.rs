//! Constraint 7, proven rather than asserted: a transport written only in this
//! crate, outside the core, serving every queue the local one does.
//!
//! [`DirFiles`] keeps a queue as a directory of files with a layout of its own —
//! one file per record, one per consumer's cursor, one per document, and a
//! generation counter its fingerprint reads — and implements
//! [`onemessagebus::Transport`] with nothing the core lends it but the trait and
//! its types. The same file is the `onemessagebus-transport-dirfiles` plugin
//! executable (`src/bin/`), which serves it over the plugin protocol.
//!
//! The journeys below run the core's queue, subscription and author table over
//! it — registered in-process under its own kind, and served by the plugin
//! executable — beside the local and memory transports, with one call that does
//! not differ between them; and they point the `onemessagebus` binary at the
//! plugin through a configuration file and hold `send`, `next` and `status` to
//! behaving as they do over the local transport.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use onemessagebus::transport::poll_for_change;
use onemessagebus::{
    Batch, Changed, ConsumerName, DocumentName, Fingerprint, Position, QueueName, Stored,
    Transport, TransportConfig, TransportError,
};

/// The kind the directory-of-files transport registers and serves under.
pub const KIND: &str = "dirfiles";

/// A queue per directory: `<root>/<queue>/records/<n>.json` holds the record
/// after position `n`, `<root>/<queue>/cursors/<consumer>` a consumer's position,
/// `<root>/<queue>/documents/<name>` a document, `<root>/<queue>/generation`
/// how many times the queue has changed, and `<root>/<queue>/lock` its
/// exclusive section. A position's token is the number of records before it.
#[derive(Debug, Clone)]
pub struct DirFiles {
    root: PathBuf,
}

/// A counter making each staging file of this process its own.
static STAGING: AtomicU64 = AtomicU64::new(0);

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> TransportError {
    let path = path.to_path_buf();
    move |source| TransportError::Io {
        action,
        path,
        source,
    }
}

impl DirFiles {
    /// The transport over `root`, creating it when missing.
    ///
    /// # Errors
    ///
    /// The directory cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, TransportError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(io_error("create", &root))?;
        Ok(Self { root })
    }

    /// The transport a configuration's `transport` block names: `dir` and no
    /// other key.
    ///
    /// # Errors
    ///
    /// A configuration with no `dir`, or a key this kind does not take.
    pub fn from_config(config: &TransportConfig) -> Result<Arc<dyn Transport>, TransportError> {
        if let Some(key) = config.options.keys().next() {
            return Err(TransportError::Config {
                kind: config.kind.clone(),
                why: format!("transport.{key} is not a key the {KIND} transport takes"),
            });
        }
        let dir = config.dir.clone().ok_or_else(|| TransportError::Config {
            kind: config.kind.clone(),
            why: format!("the {KIND} transport needs transport.dir"),
        })?;
        Ok(Arc::new(Self::open(dir)?))
    }

    fn queue_dir(&self, queue: &QueueName) -> PathBuf {
        self.root.join(queue.as_str())
    }

    fn records_dir(&self, queue: &QueueName) -> PathBuf {
        self.queue_dir(queue).join("records")
    }

    fn generation_path(&self, queue: &QueueName) -> PathBuf {
        self.queue_dir(queue).join("generation")
    }

    fn lock(&self, queue: &QueueName) -> Result<File, TransportError> {
        let dir = self.queue_dir(queue);
        fs::create_dir_all(&dir).map_err(io_error("create", &dir))?;
        let path = dir.join("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_error("open", &path))?;
        file.lock().map_err(io_error("lock", &path))?;
        Ok(file)
    }

    fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), TransportError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error("create", parent))?;
        }
        let staging = path.with_extension(format!(
            "{}.{}.staging",
            std::process::id(),
            STAGING.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&staging, bytes).map_err(io_error("write", &staging))?;
        fs::rename(&staging, path).map_err(io_error("rename onto", path))
    }

    fn count(&self, queue: &QueueName) -> Result<u64, TransportError> {
        let dir = self.records_dir(queue);
        match fs::read_dir(&dir) {
            Ok(entries) => Ok(entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
                .count() as u64),
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(failure) => Err(io_error("list", &dir)(failure)),
        }
    }

    fn generation(&self, queue: &QueueName) -> u64 {
        fs::read_to_string(self.generation_path(queue))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    /// Record that the queue changed. Called with the queue's lock held.
    fn bump(&self, queue: &QueueName) -> Result<(), TransportError> {
        let next = self.generation(queue) + 1;
        Self::write_atomic(&self.generation_path(queue), next.to_string().as_bytes())
    }

    fn append_held(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        if record.iter().all(u8::is_ascii_whitespace) {
            return Err(TransportError::NotARecord {
                queue: queue.clone(),
                why: "is empty",
            });
        }
        if record.contains(&b'\n') {
            return Err(TransportError::NotARecord {
                queue: queue.clone(),
                why: "holds a newline",
            });
        }
        let before = self.count(queue)?;
        let path = self.records_dir(queue).join(format!("{before:020}.json"));
        Self::write_atomic(&path, record)?;
        self.bump(queue)?;
        Ok(Position::from_token(before + 1))
    }

    fn commit_held(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        let end = self.count(queue)?;
        if at.token() > end {
            return Err(TransportError::PastEnd {
                queue: queue.clone(),
                position: *at,
                end: Position::from_token(end),
            });
        }
        let path = self
            .queue_dir(queue)
            .join("cursors")
            .join(consumer.as_str());
        Self::write_atomic(&path, at.token().to_string().as_bytes())?;
        self.bump(queue)
    }

    fn replace_held(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        let path = self.queue_dir(queue).join("documents").join(name.as_str());
        Self::write_atomic(&path, bytes)
    }

    fn read_records(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        let end = self.count(queue)?;
        let start = from.map_or(0, |position| position.token());
        if start > end {
            return Err(TransportError::PastEnd {
                queue: queue.clone(),
                position: Position::from_token(start),
                end: Position::from_token(end),
            });
        }
        let mut records = Vec::new();
        for n in start..end {
            if records.len() == limit {
                break;
            }
            let path = self.records_dir(queue).join(format!("{n:020}.json"));
            records.push(Stored {
                bytes: fs::read(&path).map_err(io_error("read", &path))?,
                after: Position::from_token(n + 1),
            });
        }
        Ok(Batch {
            records,
            torn: None,
        })
    }

    fn read_cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        let path = self
            .queue_dir(queue)
            .join("cursors")
            .join(consumer.as_str());
        match fs::read_to_string(&path) {
            Ok(text) => Ok(text.trim().parse().ok().map(Position::from_token)),
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(failure) => Err(io_error("read", &path)(failure)),
        }
    }

    fn read_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        let path = self.queue_dir(queue).join("documents").join(name.as_str());
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(failure) => Err(io_error("read", &path)(failure)),
        }
    }
}

impl Transport for DirFiles {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        let _lock = self.lock(queue)?;
        self.append_held(queue, record)
    }

    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        self.read_records(queue, from, limit)
    }

    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        self.read_cursor(queue, consumer)
    }

    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        let _lock = self.lock(queue)?;
        self.commit_held(queue, consumer, at)
    }

    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        let lock = self.lock(queue)?;
        let held = Held {
            files: self.clone(),
            held: BTreeSet::from([queue.clone()]),
        };
        let result = body(&held);
        drop(lock);
        result
    }

    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError> {
        Ok(Fingerprint::from_parts([
            self.count(queue)?,
            self.generation(queue),
        ]))
    }

    fn wait_for_change(
        &self,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError> {
        poll_for_change(self, queue, since, timeout)
    }

    fn document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        self.read_document(queue, name)
    }

    fn replace_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        let _lock = self.lock(queue)?;
        self.replace_held(queue, name, bytes)
    }
}

/// The transport as an exclusive section lends it to its body: a queue whose
/// lock the section holds is written without taking it again.
struct Held {
    files: DirFiles,
    held: BTreeSet<QueueName>,
}

impl Transport for Held {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        if self.held.contains(queue) {
            self.files.append_held(queue, record)
        } else {
            self.files.append(queue, record)
        }
    }

    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        self.files.read_records(queue, from, limit)
    }

    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        self.files.read_cursor(queue, consumer)
    }

    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        if self.held.contains(queue) {
            self.files.commit_held(queue, consumer, at)
        } else {
            self.files.commit(queue, consumer, at)
        }
    }

    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        if self.held.contains(queue) {
            return body(self);
        }
        let lock = self.files.lock(queue)?;
        let mut held = self.held.clone();
        held.insert(queue.clone());
        let nested = Held {
            files: self.files.clone(),
            held,
        };
        let result = body(&nested);
        drop(lock);
        result
    }

    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError> {
        self.files.fingerprint(queue)
    }

    fn wait_for_change(
        &self,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError> {
        poll_for_change(self, queue, since, timeout)
    }

    fn document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        self.files.read_document(queue, name)
    }

    fn replace_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        if self.held.contains(queue) {
            self.files.replace_held(queue, name, bytes)
        } else {
            self.files.replace_document(queue, name, bytes)
        }
    }
}

#[cfg(test)]
mod journeys {
    use std::collections::BTreeMap;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    use onemessagebus::conformance::{queue_table, QUEUE_TABLE};
    use onemessagebus::{KindOrigin, Transport, TransportConfig, TransportKinds};
    use serde_json::{json, Value};

    use super::{DirFiles, KIND};

    /// The plugin executable Cargo built beside this test.
    fn plugin_executable() -> PathBuf {
        // `option_env!`, because this file is also compiled into the plugin
        // executable itself, where Cargo names no binary.
        option_env!("CARGO_BIN_EXE_onemessagebus-transport-dirfiles")
            .map(PathBuf::from)
            .expect("Cargo names the plugin executable to this test")
    }

    /// A directory holding only the plugin, named as `PATH` finds a kind's
    /// plugin.
    fn plugin_dir(scratch: &Path) -> PathBuf {
        let dir = scratch.join("plugins");
        std::fs::create_dir_all(&dir).expect("a plugin directory");
        let name = format!(
            "onemessagebus-transport-{KIND}{}",
            std::env::consts::EXE_SUFFIX
        );
        std::fs::copy(plugin_executable(), dir.join(name)).expect("the plugin is placed");
        dir
    }

    /// Scratch directories the transports keep their queues in, removed when the
    /// test binary exits.
    static SCRATCH: Mutex<Vec<tempfile::TempDir>> = Mutex::new(Vec::new());

    fn scratch_dir() -> PathBuf {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let path = dir.path().to_path_buf();
        SCRATCH.lock().expect("the scratch list").push(dir);
        path
    }

    /// Every row of the table over `fresh`, collecting each row that failed
    /// rather than stopping at the first.
    fn failed_rows(fresh: &dyn Fn() -> Arc<dyn Transport>) -> Vec<String> {
        QUEUE_TABLE
            .iter()
            .filter_map(|(name, row)| {
                catch_unwind(AssertUnwindSafe(|| row(fresh)))
                    .err()
                    .map(|panic| {
                        let why = panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                            .unwrap_or_default();
                        format!("{name}: {why}")
                    })
            })
            .collect()
    }

    /// The same table, called the same way, passes over the transport written in
    /// this crate — registered in-process under its own kind — and over the local
    /// and memory transports.
    #[test]
    fn the_queue_table_passes_over_a_transport_written_outside_the_core_as_over_local_and_memory() {
        let mut kinds = TransportKinds::builtin().searching(Vec::new());
        kinds
            .register(KIND, Arc::new(DirFiles::from_config))
            .expect("the kind registers");
        assert!(
            kinds
                .kinds()
                .iter()
                .any(|entry| entry.kind == KIND && entry.origin == KindOrigin::Registered),
            "the kind is not listed as registered"
        );
        let open = |kind: &str, dir: Option<PathBuf>| -> Arc<dyn Transport> {
            kinds
                .open(&TransportConfig {
                    kind: kind.to_owned(),
                    dir,
                    options: serde_json::Map::new(),
                })
                .unwrap_or_else(|failure| panic!("the {kind} transport opens: {failure}"))
        };
        let dirfiles = || open(KIND, Some(scratch_dir().join("queues")));
        let local = || open("local", Some(scratch_dir().join("channel")));
        let memory = || open("memory", None);

        let mut outcomes = BTreeMap::new();
        for (name, fresh) in [
            ("dirfiles", &dirfiles as &dyn Fn() -> Arc<dyn Transport>),
            ("local", &local),
            ("memory", &memory),
        ] {
            outcomes.insert(name, failed_rows(fresh));
        }
        for (name, failures) in &outcomes {
            assert!(
                failures.is_empty(),
                "the queue table failed over the {name} transport:\n{}",
                failures.join("\n")
            );
        }

        // And the whole table in one call, exactly as a consumer proving a
        // transport would make it.
        queue_table(&dirfiles);
    }

    /// The directory-of-files layout is what the transport kept: nothing of the
    /// local transport's files appears under it.
    #[test]
    fn the_transport_keeps_its_own_layout_and_refuses_a_key_it_does_not_take() {
        let root = scratch_dir();
        let files = DirFiles::open(&root).expect("opens");
        let queue = "surfaces".parse().expect("a queue");
        files.append(&queue, br#"{"n":0}"#).expect("appends");
        files
            .commit(
                &queue,
                &"default".parse().expect("a consumer"),
                &onemessagebus::Position::from_token(1),
            )
            .expect("commits");
        assert!(root
            .join("surfaces/records/00000000000000000000.json")
            .is_file());
        assert_eq!(
            std::fs::read_to_string(root.join("surfaces/cursors/default")).expect("the cursor"),
            "1"
        );
        assert!(!root.join("surfaces.jsonl").exists());
        let refused = DirFiles::from_config(&TransportConfig {
            kind: KIND.to_owned(),
            dir: Some(root),
            options: serde_json::from_value(json!({"url": "nats://x"})).expect("options"),
        })
        .err()
        .expect("an unknown key is refused");
        assert_eq!(
            refused.to_string(),
            "transport.url is not a key the dirfiles transport takes"
        );
    }

    /// The table passes over the plugin executable too: the transport in another
    /// process, found on the search path under its kind, spoken to over the
    /// plugin protocol by the core's client.
    #[test]
    fn the_queue_table_passes_over_the_plugin_executable_through_the_protocol() {
        let scratch = scratch_dir();
        let kinds = TransportKinds::builtin().searching(vec![plugin_dir(&scratch)]);
        let listed = kinds
            .kinds()
            .into_iter()
            .find(|entry| entry.kind == KIND)
            .expect("the plugin is listed");
        assert_eq!(listed.origin, KindOrigin::Plugin);
        let fresh = || -> Arc<dyn Transport> {
            kinds
                .open(&TransportConfig {
                    kind: KIND.to_owned(),
                    dir: Some(scratch_dir().join("queues")),
                    options: serde_json::Map::new(),
                })
                .expect("the plugin opens")
        };
        let failures = failed_rows(&fresh);
        assert!(
            failures.is_empty(),
            "the queue table failed over the plugin executable:\n{}",
            failures.join("\n")
        );

        let refused = kinds
            .open(&TransportConfig {
                kind: KIND.to_owned(),
                dir: None,
                options: serde_json::Map::new(),
            })
            .err()
            .expect("a plugin refusing its configuration is refused");
        assert!(
            refused.to_string().contains("needs transport.dir"),
            "{refused}"
        );
    }

    /// The `onemessagebus` binary the journeys drive.
    fn binary() -> PathBuf {
        std::env::var_os("ONEMESSAGEBUS_BIN").map_or_else(
            || assert_cmd::cargo::cargo_bin("onemessagebus"),
            PathBuf::from,
        )
    }

    struct Ran {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    }

    fn cli(cwd: &Path, path: &str, args: &[&str], stdin: Option<&str>) -> Ran {
        use std::io::Write as _;
        let mut child = Command::new(binary())
            .args(args)
            .current_dir(cwd)
            .env("PATH", path)
            .env_remove("ONEMESSAGEBUS_CONFIG")
            .env_remove("ONEMESSAGEBUS_TRANSPORT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the binary spawns");
        if let (Some(text), Some(mut handle)) = (stdin, child.stdin.take()) {
            handle.write_all(text.as_bytes()).expect("stdin is written");
        }
        let output = child.wait_with_output().expect("the binary exits");
        Ran {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).expect("UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("UTF-8"),
        }
    }

    /// What a verb answered, with the one thing a transport decides removed: a
    /// position's token, which is a byte offset over the local transport and a
    /// record count over the plugin.
    fn without_positions(value: Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| match key.as_str() {
                        "position" | "pending_position" => (key, json!("<position>")),
                        "cursors" => (
                            key,
                            match value {
                                Value::Object(cursors) => Value::Object(
                                    cursors
                                        .into_iter()
                                        .map(|(consumer, cursor)| {
                                            (
                                                consumer,
                                                if cursor.is_null() {
                                                    cursor
                                                } else {
                                                    json!("<position>")
                                                },
                                            )
                                        })
                                        .collect(),
                                ),
                                other => other,
                            },
                        ),
                        _ => (key, without_positions(value)),
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(without_positions).collect()),
            other => other,
        }
    }

    fn answered(ran: &Ran) -> (Option<i32>, Value, String) {
        let lines: Option<Vec<Value>> = ran
            .stdout
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).ok())
            .collect();
        let stdout = if ran.stdout.trim().is_empty() {
            Value::Null
        } else if let Ok(document) = serde_json::from_str::<Value>(&ran.stdout) {
            without_positions(document)
        } else if let Some(lines) = lines {
            Value::Array(lines.into_iter().map(without_positions).collect())
        } else {
            // A text rendering: compared as the words it printed.
            Value::String(ran.stdout.clone())
        };
        (ran.code, stdout, ran.stderr.clone())
    }

    /// The binary pointed at the plugin through a configuration file answers
    /// `send`, `next` and `status` exactly as it answers them over the local
    /// transport, and the plugin's own layout is what holds the queues.
    #[test]
    fn the_binary_pointed_at_the_plugin_through_configuration_behaves_as_over_local() {
        let scratch = scratch_dir();
        let plugins = plugin_dir(&scratch);
        let path = std::env::join_paths(
            std::iter::once(plugins.clone()).chain(
                std::env::var_os("PATH")
                    .iter()
                    .flat_map(std::env::split_paths),
            ),
        )
        .expect("a PATH");
        let path = path.to_str().expect("a UTF-8 PATH").to_owned();

        let listed = cli(&scratch, &path, &["transports"], None);
        assert_eq!(listed.code, Some(0), "{}", listed.stderr);
        let kinds: Value = serde_json::from_str(&listed.stdout).expect("JSON");
        assert!(
            kinds
                .as_array()
                .expect("a list")
                .iter()
                .any(|entry| entry["kind"] == json!(KIND) && entry["origin"] == json!("plugin")),
            "the binary does not list the plugin: {kinds}"
        );

        let config = |name: &str, kind: &str, dir: &Path| -> String {
            let file = scratch.join(name);
            std::fs::write(
                &file,
                format!(
                    "version: 1\ntransport: {{kind: {kind}, dir: {}}}\nprofile: planner-channel\nqueues:\n  findings: {{}}\n",
                    dir.display()
                ),
            )
            .expect("a configuration");
            file.to_str().expect("a path").to_owned()
        };
        let plugin_dir_queues = scratch.join("plugin-queues");
        let local_dir = scratch.join("local-channel");
        let over_plugin = config("plugin.yaml", KIND, &plugin_dir_queues);
        let over_local = config("local.yaml", "local", &local_dir);

        let surface = |message: &str, blocking: bool| {
            json!({"kind": "planner-question", "message": message, "source": "proposal",
                   "blocking": blocking, "queued_at": 1_789_000_000_000_u64})
            .to_string()
        };
        let script: Vec<(Vec<&str>, Option<String>)> = vec![
            (vec!["send", "surfaces"], Some(surface("narration", false))),
            (vec!["send", "surfaces"], Some(surface("a question", true))),
            (
                vec!["send", "findings"],
                Some(r#"{"what":"a finding"}"#.to_owned()),
            ),
            (vec!["send", "nowhere"], Some("{}".to_owned())),
            (vec!["status"], None),
            (vec!["next", "surfaces"], None),
            (vec!["status", "surfaces"], None),
            (vec!["next", "surfaces"], None),
            (vec!["next", "surfaces"], None),
            (vec!["next", "findings", "--consumer", "reader"], None),
            (vec!["status", "findings", "--format", "text"], None),
        ];
        for (args, stdin) in &script {
            let run = |config: &str| {
                let mut argv = args.clone();
                argv.extend(["--config", config]);
                answered(&cli(&scratch, &path, &argv, stdin.as_deref()))
            };
            let plugin = run(&over_plugin);
            let local = run(&over_local);
            let text_status = args.contains(&"text");
            if text_status {
                // The text rendering carries the cursor's token itself.
                assert_eq!(plugin.0, local.0, "`{}` exited differently", args.join(" "));
            } else {
                assert_eq!(
                    plugin,
                    local,
                    "`{}` answered differently over the plugin",
                    args.join(" ")
                );
            }
        }
        assert!(
            plugin_dir_queues.join("surfaces/records").is_dir(),
            "the binary did not keep its queues through the plugin"
        );
        assert!(!plugin_dir_queues.join("surfaces.jsonl").exists());
        assert!(local_dir.join("surfaces.jsonl").is_file());
    }
}
