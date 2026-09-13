//! The transport a queue is kept on: the one seam a distributed backend plugs
//! into.
//!
//! Everything durable the bus does — a queue's records, where a consumer has
//! read up to, the small documents kept beside a queue, the exclusive section a
//! claim needs, and change detection — goes through [`Transport`], and nothing
//! above it names a file. [`LocalTransport`] keeps a queue as files in one
//! directory, byte-compatible with the channel directory `onepipeline` writes;
//! [`MemoryTransport`] keeps it in memory for tests; a transport of your own is a
//! crate implementing the trait, registered under its kind
//! ([`TransportKinds`](crate::TransportKinds)), or an executable serving it over
//! the plugin protocol ([`serve`]).
//!
//! The trait is object-safe, because a transport is chosen at runtime from
//! configuration, and every type it names can be built outside this crate, so a
//! transport written elsewhere is a peer of the two here rather than a guest.
//! `docs/transport.md` states the contract and how a NATS JetStream transport
//! maps onto it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use crate::plugin::{
    register_protocol, serve, PluginAnswer, PluginError, PluginErrorKind, PluginHello, PluginReply,
    PluginRequest, PluginStored, PluginTorn, ProcessTransport, HELLO_SCHEMA, PLUGIN_PREFIX,
    PROTOCOL, PROTOCOL_VERSION, REPLY_SCHEMA, REQUEST_SCHEMA,
};

/// A durable, totally ordered log per queue, with named cursors, small named
/// documents, an exclusive section and change detection.
///
/// The method names and what each is responsible for are the contract a
/// transport implements; `docs/transport.md` states it and maps each method onto
/// a NATS JetStream primitive.
pub trait Transport: Send + Sync + 'static {
    /// Append one record to a queue; total order per queue; returns where it
    /// landed — the position just after it.
    ///
    /// # Errors
    ///
    /// A record that is empty or spans more than one line is refused, and a
    /// backend that could not keep it says why.
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError>;

    /// Records after `from` (exclusive; `None` = from the start), oldest first,
    /// at most `limit`, each with the position after it. A torn or unreadable
    /// trailing record is reported in [`Batch::torn`], never silently dropped and
    /// never fatal.
    ///
    /// # Errors
    ///
    /// [`TransportError::PastEnd`] for a position the queue no longer reaches —
    /// the log was replaced or truncated — and a backend failure otherwise.
    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError>;

    /// Where a named consumer has read up to, or `None` for one that has read
    /// nothing.
    ///
    /// # Errors
    ///
    /// A backend failure.
    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError>;

    /// Record where a named consumer has read up to.
    ///
    /// # Errors
    ///
    /// A position this queue does not have, or a backend failure.
    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError>;

    /// An exclusive section over one queue — the check-then-append a claim
    /// needs. `body` is handed the transport to use inside the section; nothing
    /// else appends to `queue` until it returns.
    ///
    /// # Errors
    ///
    /// Whatever `body` returned, or the section could not be taken.
    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError>;

    /// A cheap change token for a queue: its records and its cursors.
    ///
    /// # Errors
    ///
    /// A backend failure.
    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError>;

    /// Wait up to `timeout` for a queue's fingerprint to move from `since`.
    ///
    /// # Errors
    ///
    /// A backend failure.
    fn wait_for_change(
        &self,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError>;

    /// A small named document kept beside a queue (a projection, a
    /// checkpoint), read whole; `None` where there is none.
    ///
    /// # Errors
    ///
    /// A document that exists and cannot be read.
    fn document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError>;

    /// Replace a named document atomically: a reader sees the old bytes or the
    /// new ones, never part of either.
    ///
    /// # Errors
    ///
    /// A backend failure.
    fn replace_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError>;
}

/// Why a transport could not do what it was asked.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The host refused a file operation.
    #[error("cannot {action} {}: {source}", path.display())]
    Io {
        /// What was being done, as a verb phrase.
        action: &'static str,
        /// The path it was done to.
        path: PathBuf,
        /// What the host said.
        source: io::Error,
    },
    /// A record that is not exactly one non-empty line.
    #[error("{queue}: a record is one non-empty line of bytes, and this one {why}")]
    NotARecord {
        /// The queue it was offered to.
        queue: QueueName,
        /// What is wrong with it.
        why: &'static str,
    },
    /// A position past the end of the queue: the log was replaced or truncated.
    #[error("{queue}: position {position} is past the end of the queue, which ends at {end}; the log was replaced or truncated")]
    PastEnd {
        /// The queue.
        queue: QueueName,
        /// The position asked for.
        position: Position,
        /// Where the queue ends now.
        end: Position,
    },
    /// A position that does not fall on a record boundary.
    #[error("{queue}: position {position} is not a record boundary")]
    NotABoundary {
        /// The queue.
        queue: QueueName,
        /// The position asked for.
        position: Position,
    },
    /// A backend of another kind — a plugin, a remote service — failed.
    #[error("the {transport} transport: {detail}")]
    Backend {
        /// The transport's kind.
        transport: String,
        /// What it said.
        detail: String,
    },
    /// A transport's configuration that its kind refuses.
    #[error("{why}")]
    Config {
        /// The kind the configuration names.
        kind: String,
        /// What is wrong with it, naming the key.
        why: String,
    },
    /// A kind no built-in, registered or plugin transport serves.
    #[error("{kind:?} is not a transport kind this build can open; the kinds there are: {known}. A plugin serves a kind as an executable named onemessagebus-transport-<kind> on PATH")]
    UnknownKind {
        /// The kind asked for.
        kind: String,
        /// Every kind there is, comma-separated.
        known: String,
    },
}

impl TransportError {
    fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Why text is not a name a transport keeps a queue, a consumer or a document
/// under.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not a {what} name: {why}")]
pub struct NameError {
    /// Which kind of name.
    pub what: &'static str,
    /// What was offered.
    pub text: String,
    /// What is wrong with it.
    pub why: String,
}

/// Whether `text` is a non-empty run of ASCII letters, digits, `-` and `_`, plus
/// `.` where `dots` allows it.
fn check_name(what: &'static str, text: &str, dots: bool) -> Result<(), NameError> {
    let refuse = |why: String| NameError {
        what,
        text: text.to_owned(),
        why,
    };
    if text.is_empty() {
        return Err(refuse("it is empty".to_owned()));
    }
    if let Some(ch) = text
        .chars()
        .find(|ch| !(ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_' || dots && *ch == '.'))
    {
        let allowed = if dots {
            "ASCII letters, digits, `-`, `_` and `.`"
        } else {
            "ASCII letters, digits, `-` and `_`"
        };
        return Err(refuse(format!("{ch:?} is not one of {allowed}")));
    }
    if !text.starts_with(|ch: char| ch.is_ascii_alphanumeric()) {
        return Err(refuse(
            "it does not start with a letter or digit".to_owned(),
        ));
    }
    Ok(())
}

macro_rules! name_type {
    ($(#[$doc:meta])* $name:ident, $what:literal, $check:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
        #[schemars(transparent)]
        pub struct $name(String);

        impl $name {
            /// The name as text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = NameError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                let check: fn(&str) -> Result<(), NameError> = $check;
                check(text).map(|()| Self(text.to_owned()))
            }
        }

        impl TryFrom<&str> for $name {
            type Error = NameError;

            fn try_from(text: &str) -> Result<Self, Self::Error> {
                text.parse()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

name_type!(
    /// A queue's name: a non-empty run of ASCII letters, digits, `-` and `_`,
    /// starting with a letter or digit — `surfaces`, `command-outcomes`.
    QueueName,
    "queue",
    |text| check_name("queue", text, false)
);

name_type!(
    /// A consumer's name, held to the same rule as a queue's. The consumer named
    /// [`ConsumerName::DEFAULT`] is the one a queue with a single reader has.
    ConsumerName,
    "consumer",
    |text| check_name("consumer", text, false)
);

name_type!(
    /// A document's name: ASCII letters, digits, `-`, `_` and `.`, starting with
    /// a letter or digit — `queue.json`. A name a queue's own files are kept
    /// under locally (one ending `.jsonl`, `.torn`, `.staging` or `.lock`, or
    /// holding `-cursor.`) is refused, so a document never overwrites a queue.
    DocumentName,
    "document",
    |text| {
        check_name("document", text, true)?;
        let reserved = [".jsonl", ".torn", ".staging", ".lock"]
            .iter()
            .find(|suffix| text.ends_with(*suffix))
            .map(|suffix| format!("it ends in `{suffix}`, which a queue's own files are kept under"))
            .or_else(|| {
                text.contains("-cursor.")
                    .then(|| "it holds `-cursor.`, which a consumer's cursor file is named with".to_owned())
            });
        match reserved {
            Some(why) => Err(NameError {
                what: "document",
                text: text.to_owned(),
                why,
            }),
            None => Ok(()),
        }
    }
);

impl ConsumerName {
    /// The consumer a queue with a single reader has: `default`.
    pub const DEFAULT: &'static str = "default";

    /// The consumer named [`DEFAULT`](Self::DEFAULT).
    #[must_use]
    pub fn default_consumer() -> Self {
        Self(Self::DEFAULT.to_owned())
    }

    /// Whether this is the default consumer.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.0 == Self::DEFAULT
    }
}

/// Where in a queue a record ends: opaque to consumers, and meaningful only to
/// the transport that handed it out.
///
/// A consumer receives positions from [`Transport::append`] and
/// [`Transport::read`] and hands them back; it never builds one from a number
/// it made up. It serializes as its token so a cursor survives a process.
/// [`from_token`](Self::from_token) and [`token`](Self::token) exist for a
/// transport's own implementation: [`LocalTransport`]'s token is the byte offset
/// at a record boundary, [`MemoryTransport`]'s the count of records before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Position(u64);

impl Position {
    /// The position a transport's token names.
    #[must_use]
    pub const fn from_token(token: u64) -> Self {
        Self(token)
    }

    /// The transport's token for this position.
    #[must_use]
    pub const fn token(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A cheap change token for a queue: two fingerprints of one queue are equal
/// when nothing observable about it moved.
///
/// Opaque, like [`Position`]: a transport builds one from whatever it can
/// observe cheaply ([`LocalTransport`] from each file's length and modification
/// time, as `onepipeline` does), and a consumer only compares them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Fingerprint(Vec<u64>);

impl Fingerprint {
    /// A fingerprint from a transport's observations.
    #[must_use]
    pub fn from_parts(parts: impl IntoIterator<Item = u64>) -> Self {
        Self(parts.into_iter().collect())
    }

    /// The observations, for a transport's own implementation.
    #[must_use]
    pub fn parts(&self) -> &[u64] {
        &self.0
    }
}

/// One record read off a queue, and the position just after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The record's bytes, without the transport's own framing.
    pub bytes: Vec<u8>,
    /// The position after it: what a consumer resumes from, and commits.
    pub after: Position,
}

/// A trailing record whose writer has not finished it — still writing, or died
/// on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TornRecord {
    /// Where it starts, which is also where the whole records before it end.
    pub at: Position,
    /// How many bytes of it there are so far.
    pub bytes: u64,
}

/// What one [`Transport::read`] handed back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Batch {
    /// The whole records read, oldest first.
    pub records: Vec<Stored>,
    /// A torn record after them, when the read reached one.
    pub torn: Option<TornRecord>,
}

/// What a bounded wait for a queue to change found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changed {
    /// The queue moved; its fingerprint now.
    Moved(Fingerprint),
    /// The wait ran out with the queue as it was.
    Unchanged(Fingerprint),
}

/// How often a transport with no push notification looks again while waiting
/// for a change.
const CHANGE_POLL: Duration = Duration::from_millis(20);

/// How long one wait on the memory transport's condition lasts when the
/// caller's timeout has no representable deadline: a change still wakes it first.
const UNBOUNDED_WAIT: Duration = Duration::from_secs(3600);

/// A poisoned lock is state some other thread panicked while holding; what it
/// guards is plain data with no invariant a panic half-applies, so carry on.
fn unpoisoned<'a, T>(
    guard: Result<MutexGuard<'a, T>, PoisonError<MutexGuard<'a, T>>>,
) -> MutexGuard<'a, T> {
    guard.unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// The local transport.
// ---------------------------------------------------------------------------

/// A transport over one directory, laid out as `onepipeline` lays out a run's
/// channel directory.
///
/// | what | file |
/// | --- | --- |
/// | a queue's records, one JSON line each | `<dir>/<queue>.jsonl` |
/// | the `default` consumer's cursor | `<dir>/<queue>-cursor.json` |
/// | another consumer's cursor | `<dir>/<queue>-cursor.<consumer>.json` |
/// | a named document | `<dir>/<name>` |
/// | a queue's exclusive section | `<dir>/.lock/<queue>.lock` |
/// | the fragments an append healed away | `<dir>/<queue>.jsonl.torn` |
///
/// A [`Position`] is the byte offset at a record boundary. A cursor file holds
/// the **number of records** before the position rather than the offset,
/// pretty-printed as one JSON number, because that is what `onepipeline` writes
/// in `replies-cursor.json` and `commands-cursor.json`; the transport converts
/// between the two. A [`Fingerprint`] is each file's length and modification
/// time.
///
/// An append takes the queue's lock, heals a torn tail a dead writer left —
/// truncating it back to the last record boundary and recording what it
/// discarded in `<queue>.jsonl.torn` — and writes the record and its newline in
/// one write, rolled back if the write fails. Reads take no lock.
#[derive(Debug, Clone)]
pub struct LocalTransport {
    dir: PathBuf,
}

/// A byte range of a queue's file: where a line starts, and where it ends.
type Span = (usize, usize);

/// A counter making each staging file of this process its own.
static STAGING: AtomicU64 = AtomicU64::new(0);

impl LocalTransport {
    /// The transport over `dir`, creating the directory when it is missing.
    ///
    /// # Errors
    ///
    /// [`TransportError::Io`] when the directory cannot be created.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, TransportError> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|failure| TransportError::io("create", &dir, failure))?;
        Ok(Self { dir })
    }

    /// The directory this transport keeps its queues in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file a queue's records are kept in.
    #[must_use]
    pub fn records_path(&self, queue: &QueueName) -> PathBuf {
        self.dir.join(format!("{queue}.jsonl"))
    }

    /// The file a consumer's cursor on a queue is kept in.
    #[must_use]
    pub fn cursor_path(&self, queue: &QueueName, consumer: &ConsumerName) -> PathBuf {
        if consumer.is_default() {
            self.dir.join(format!("{queue}-cursor.json"))
        } else {
            self.dir.join(format!("{queue}-cursor.{consumer}.json"))
        }
    }

    fn lock_path(&self, queue: &QueueName) -> PathBuf {
        self.dir.join(".lock").join(format!("{queue}.lock"))
    }

    /// Take a queue's lock, blocking until it is free. Held until the handle is
    /// dropped — including when the process holding it dies.
    fn lock(&self, queue: &QueueName) -> Result<File, TransportError> {
        let path = self.lock_path(queue);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|failure| TransportError::io("create", parent, failure))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|failure| TransportError::io("open", &path, failure))?;
        file.lock()
            .map_err(|failure| TransportError::io("lock", &path, failure))?;
        Ok(file)
    }

    /// Append under a lock the caller already holds.
    fn append_locked(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        if record.is_empty() || record.iter().all(u8::is_ascii_whitespace) {
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
        let path = self.records_path(queue);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|failure| TransportError::io("open", &path, failure))?;
        self.heal(queue, &path, &mut file)?;
        let boundary = file
            .seek(SeekFrom::End(0))
            .map_err(|failure| TransportError::io("seek", &path, failure))?;
        let mut line = Vec::with_capacity(record.len() + 1);
        line.extend_from_slice(record);
        line.push(b'\n');
        if let Err(failure) = file.write_all(&line).and_then(|()| file.flush()) {
            // Whatever of the record reached the file goes back off it, so the
            // file stays on the boundary it started on; the write's own failure
            // is the one reported.
            let _ = file.set_len(boundary);
            return Err(TransportError::io("append to", &path, failure));
        }
        Ok(Position(boundary + line.len() as u64))
    }

    /// Truncate a fragment a dead writer left at the end of a queue's file, and
    /// record what was discarded beside it. Called with the queue's lock held.
    fn heal(&self, queue: &QueueName, path: &Path, file: &mut File) -> Result<(), TransportError> {
        let bytes = read_all(path, file, 0)?;
        let Some(last) = bytes.last() else {
            return Ok(());
        };
        if *last == b'\n' {
            return Ok(());
        }
        let boundary = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1);
        file.set_len(boundary as u64)
            .map_err(|failure| TransportError::io("truncate", path, failure))?;
        let loss = serde_json::json!({
            "at": crate::clock::now_rfc3339(),
            "offset": boundary,
            "bytes": bytes.len() - boundary,
            "healed_by": std::process::id(),
        });
        let torn_log = self.dir.join(format!("{queue}.jsonl.torn"));
        // The heal already happened and the append it clears the way for has not
        // failed; a report that could not be written must not fail it, or the
        // store a writer put back together could record nothing.
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&torn_log)
            .and_then(|mut log| log.write_all(format!("{loss}\n").as_bytes()));
        Ok(())
    }

    /// The whole records of a queue's file, as `(start, end)` byte ranges of the
    /// non-blank lines, and the torn tail after them.
    fn scan(bytes: &[u8]) -> (Vec<Span>, Option<Span>) {
        let mut lines = Vec::new();
        let mut start = 0;
        while start < bytes.len() {
            match bytes[start..].iter().position(|byte| *byte == b'\n') {
                Some(offset) => {
                    let end = start + offset;
                    if !bytes[start..end].iter().all(u8::is_ascii_whitespace) {
                        lines.push((start, end));
                    }
                    start = end + 1;
                }
                None => return (lines, Some((start, bytes.len() - start))),
            }
        }
        (lines, None)
    }

    fn file_bytes(&self, queue: &QueueName) -> Result<Vec<u8>, TransportError> {
        let path = self.records_path(queue);
        match fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(failure) => Err(TransportError::io("read", &path, failure)),
        }
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), TransportError> {
        let mut staging = path.as_os_str().to_owned();
        staging.push(format!(
            ".{}.{}.staging",
            std::process::id(),
            STAGING.fetch_add(1, Ordering::Relaxed)
        ));
        let staging = PathBuf::from(staging);
        fs::write(&staging, bytes)
            .map_err(|failure| TransportError::io("write", &staging, failure))?;
        fs::rename(&staging, path).map_err(|failure| {
            let _ = fs::remove_file(&staging);
            TransportError::io("rename onto", path, failure)
        })
    }

    fn mark(path: &Path, parts: &mut Vec<u64>) {
        match fs::metadata(path) {
            Ok(metadata) => {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                    .unwrap_or_default();
                parts.extend([
                    1,
                    metadata.len(),
                    modified.as_secs(),
                    u64::from(modified.subsec_nanos()),
                ]);
            }
            Err(_) => parts.push(0),
        }
    }

    fn read_in(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        let bytes = self.file_bytes(queue)?;
        let from = from.map_or(0, |position| position.0);
        let end = bytes.len() as u64;
        if from > end {
            return Err(TransportError::PastEnd {
                queue: queue.clone(),
                position: Position(from),
                end: Position(end),
            });
        }
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        if from > 0 && bytes[from - 1] != b'\n' {
            return Err(TransportError::NotABoundary {
                queue: queue.clone(),
                position: Position(from as u64),
            });
        }
        let (lines, torn) = Self::scan(&bytes[from..]);
        let mut batch = Batch::default();
        for (start, stop) in lines.iter().take(limit) {
            batch.records.push(Stored {
                bytes: bytes[from + start..from + stop].to_vec(),
                after: Position((from + stop + 1) as u64),
            });
        }
        if lines.len() <= limit {
            batch.torn = torn.map(|(start, length)| TornRecord {
                at: Position((from + start) as u64),
                bytes: length as u64,
            });
        }
        Ok(batch)
    }
}

/// Every byte of an open file from `from`.
fn read_all(path: &Path, file: &mut File, from: u64) -> Result<Vec<u8>, TransportError> {
    file.seek(SeekFrom::Start(from))
        .map_err(|failure| TransportError::io("seek", path, failure))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|failure| TransportError::io("read", path, failure))?;
    Ok(bytes)
}

impl Transport for LocalTransport {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        let _lock = self.lock(queue)?;
        self.append_locked(queue, record)
    }

    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        self.read_in(queue, from, limit)
    }

    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        let path = self.cursor_path(queue, consumer);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(failure) => return Err(TransportError::io("read", &path, failure)),
        };
        // A cursor this build cannot read is a consumer that has read nothing —
        // the reading `onepipeline` gives it — rather than a queue nobody can
        // claim from again.
        let Ok(count) = serde_json::from_str::<u64>(text.trim()) else {
            return Ok(None);
        };
        let bytes = self.file_bytes(queue)?;
        let (lines, _) = Self::scan(&bytes);
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        let offset = match count {
            0 => 0,
            n => lines
                .get(n - 1)
                .or(lines.last())
                .map_or(0, |(_, end)| end + 1),
        };
        Ok(Some(Position(offset as u64)))
    }

    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        let bytes = self.file_bytes(queue)?;
        let end = bytes.len() as u64;
        if at.0 > end {
            return Err(TransportError::PastEnd {
                queue: queue.clone(),
                position: *at,
                end: Position(end),
            });
        }
        let offset = usize::try_from(at.0).unwrap_or(usize::MAX);
        if offset > 0 && bytes[offset - 1] != b'\n' {
            return Err(TransportError::NotABoundary {
                queue: queue.clone(),
                position: *at,
            });
        }
        let (lines, _) = Self::scan(&bytes);
        let count = lines.iter().filter(|(_, stop)| *stop < offset).count();
        let path = self.cursor_path(queue, consumer);
        let text = serde_json::to_string_pretty(&count).unwrap_or_else(|_| count.to_string());
        self.write_atomic(&path, text.as_bytes())
    }

    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        let lock = self.lock(queue)?;
        let path = self.records_path(queue);
        // Healed on the way in, so what the section reads ends on a record
        // boundary and what it stamps is the length it appends from.
        if path.exists() {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|failure| TransportError::io("open", &path, failure))?;
            self.heal(queue, &path, &mut file)?;
        }
        let held = LocalHeld {
            local: self.clone(),
            held: BTreeSet::from([queue.clone()]),
        };
        let result = body(&held);
        drop(lock);
        result
    }

    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError> {
        let mut parts = Vec::new();
        Self::mark(&self.records_path(queue), &mut parts);
        let prefix = format!("{queue}-cursor.");
        let mut cursors: Vec<PathBuf> = match fs::read_dir(&self.dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(&prefix) && name.ends_with(".json")
                })
                .map(|entry| entry.path())
                .collect(),
            Err(failure) => return Err(TransportError::io("list", &self.dir, failure)),
        };
        cursors.sort();
        for cursor in cursors {
            Self::mark(&cursor, &mut parts);
        }
        Ok(Fingerprint(parts))
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
        _queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        let path = self.dir.join(name.as_str());
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(failure) => Err(TransportError::io("read", &path, failure)),
        }
    }

    fn replace_document(
        &self,
        _queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        self.write_atomic(&self.dir.join(name.as_str()), bytes)
    }
}

/// Poll `transport`'s fingerprint of `queue` until it moves from `since` or
/// `timeout` passes: the wait of a transport with no change notification.
///
/// # Errors
///
/// Whatever reading the fingerprint failed with.
pub fn poll_for_change(
    transport: &dyn Transport,
    queue: &QueueName,
    since: &Fingerprint,
    timeout: Duration,
) -> Result<Changed, TransportError> {
    // A deadline past what `Instant` can represent is no deadline at all.
    let deadline = Instant::now().checked_add(timeout);
    loop {
        let now = transport.fingerprint(queue)?;
        if &now != since {
            return Ok(Changed::Moved(now));
        }
        let left = deadline.map_or(CHANGE_POLL, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        });
        if left.is_zero() {
            return Ok(Changed::Unchanged(now));
        }
        std::thread::sleep(left.min(CHANGE_POLL));
    }
}

/// The local transport as an exclusive section hands it to its body.
///
/// It owns what it needs rather than borrowing the transport, because a
/// [`Transport`] is `'static`. An append to a queue whose lock the section
/// holds goes straight to the file — taking the lock again from the same process
/// would wait on itself — and an append to any other queue takes that queue's
/// lock as usual. A section opened inside it over a queue it already holds is
/// the same section; over another queue it takes that queue's lock too.
struct LocalHeld {
    local: LocalTransport,
    held: BTreeSet<QueueName>,
}

impl Transport for LocalHeld {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        if self.held.contains(queue) {
            self.local.append_locked(queue, record)
        } else {
            self.local.append(queue, record)
        }
    }

    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        self.local.read_in(queue, from, limit)
    }

    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        self.local.cursor(queue, consumer)
    }

    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        self.local.commit(queue, consumer, at)
    }

    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        if self.held.contains(queue) {
            return body(self);
        }
        let lock = self.local.lock(queue)?;
        let mut held = self.held.clone();
        held.insert(queue.clone());
        let nested = LocalHeld {
            local: self.local.clone(),
            held,
        };
        let result = body(&nested);
        drop(lock);
        result
    }

    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError> {
        self.local.fingerprint(queue)
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
        self.local.document(queue, name)
    }

    fn replace_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        self.local.replace_document(queue, name, bytes)
    }
}

// ---------------------------------------------------------------------------
// The memory transport.
// ---------------------------------------------------------------------------

/// A transport in memory, for tests: every [`Transport`] promise the local one
/// keeps, with nothing on disk.
///
/// A [`Position`]'s token is the number of records before it. Cloning it shares
/// the same queues, so two handles in two threads see one store.
#[derive(Debug, Clone, Default)]
pub struct MemoryTransport {
    inner: Arc<MemoryInner>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    state: Mutex<MemoryState>,
    /// Signalled whenever a queue's lock is released or a queue changes.
    changed: Condvar,
    /// Hands each exclusive section its own holder token.
    holders: AtomicU64,
}

#[derive(Debug, Default)]
struct MemoryState {
    records: BTreeMap<QueueName, Vec<Vec<u8>>>,
    cursors: BTreeMap<(QueueName, ConsumerName), u64>,
    documents: BTreeMap<DocumentName, Vec<u8>>,
    /// Per queue, how many times anything about it changed.
    generations: BTreeMap<QueueName, u64>,
    /// Per queue, the section holding it.
    held: BTreeMap<QueueName, u64>,
}

impl MemoryTransport {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The state, once `queue` is not held by a section other than `holder`.
    fn state_for(&self, queue: &QueueName, holder: Option<u64>) -> MutexGuard<'_, MemoryState> {
        let mut state = unpoisoned(self.inner.state.lock());
        while let Some(owner) = state.held.get(queue) {
            if Some(*owner) == holder {
                break;
            }
            state = unpoisoned(self.inner.changed.wait(state));
        }
        state
    }

    fn bump(&self, state: &mut MemoryState, queue: &QueueName) {
        *state.generations.entry(queue.clone()).or_default() += 1;
        self.inner.changed.notify_all();
    }

    fn append_as(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        record: &[u8],
    ) -> Result<Position, TransportError> {
        if record.is_empty() || record.iter().all(u8::is_ascii_whitespace) {
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
        let mut state = self.state_for(queue, holder);
        let records = state.records.entry(queue.clone()).or_default();
        records.push(record.to_vec());
        let after = Position(records.len() as u64);
        self.bump(&mut state, queue);
        Ok(after)
    }

    fn read_as(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        let state = unpoisoned(self.inner.state.lock());
        let records = state.records.get(queue).map_or(&[][..], Vec::as_slice);
        let from = from.map_or(0, |position| position.0);
        let end = records.len() as u64;
        if from > end {
            return Err(TransportError::PastEnd {
                queue: queue.clone(),
                position: Position(from),
                end: Position(end),
            });
        }
        let start = usize::try_from(from).unwrap_or(usize::MAX);
        Ok(Batch {
            records: records[start..]
                .iter()
                .take(limit)
                .enumerate()
                .map(|(offset, bytes)| Stored {
                    bytes: bytes.clone(),
                    after: Position((start + offset + 1) as u64),
                })
                .collect(),
            torn: None,
        })
    }

    fn commit_as(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        let mut state = self.state_for(queue, holder);
        let end = state.records.get(queue).map_or(0, Vec::len) as u64;
        if at.0 > end {
            return Err(TransportError::PastEnd {
                queue: queue.clone(),
                position: *at,
                end: Position(end),
            });
        }
        state
            .cursors
            .insert((queue.clone(), consumer.clone()), at.0);
        self.bump(&mut state, queue);
        Ok(())
    }

    fn exclusive_as(
        &self,
        holding: &BTreeSet<QueueName>,
        holder: u64,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        if holding.contains(queue) {
            let held = MemoryHeld {
                memory: self.clone(),
                holder,
                holding: holding.clone(),
            };
            return body(&held);
        }
        {
            let mut state = self.state_for(queue, None);
            state.held.insert(queue.clone(), holder);
        }
        let mut holding = holding.clone();
        holding.insert(queue.clone());
        let held = MemoryHeld {
            memory: self.clone(),
            holder,
            holding,
        };
        let result = body(&held);
        let mut state = unpoisoned(self.inner.state.lock());
        state.held.remove(queue);
        self.inner.changed.notify_all();
        result
    }

    fn replace_as(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        let mut state = self.state_for(queue, holder);
        state.documents.insert(name.clone(), bytes.to_vec());
        Ok(())
    }

    fn fingerprint_of(&self, queue: &QueueName) -> Fingerprint {
        let state = unpoisoned(self.inner.state.lock());
        Fingerprint(vec![state.generations.get(queue).copied().unwrap_or(0)])
    }
}

impl Transport for MemoryTransport {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        self.append_as(None, queue, record)
    }

    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        self.read_as(queue, from, limit)
    }

    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        let state = unpoisoned(self.inner.state.lock());
        Ok(state
            .cursors
            .get(&(queue.clone(), consumer.clone()))
            .map(|token| Position(*token)))
    }

    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        self.commit_as(None, queue, consumer, at)
    }

    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        let holder = self.inner.holders.fetch_add(1, Ordering::Relaxed) + 1;
        self.exclusive_as(&BTreeSet::new(), holder, queue, body)
    }

    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError> {
        Ok(self.fingerprint_of(queue))
    }

    fn wait_for_change(
        &self,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError> {
        // A deadline past what `Instant` can represent is no deadline at all.
        let deadline = Instant::now().checked_add(timeout);
        let mut state = unpoisoned(self.inner.state.lock());
        loop {
            let now = Fingerprint(vec![state.generations.get(queue).copied().unwrap_or(0)]);
            if &now != since {
                return Ok(Changed::Moved(now));
            }
            let left = deadline.map_or(UNBOUNDED_WAIT, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                return Ok(Changed::Unchanged(now));
            }
            state = self
                .inner
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn document(
        &self,
        _queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        let state = unpoisoned(self.inner.state.lock());
        Ok(state.documents.get(name).cloned())
    }

    fn replace_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        self.replace_as(None, queue, name, bytes)
    }
}

/// The memory transport as an exclusive section hands it to its body.
struct MemoryHeld {
    memory: MemoryTransport,
    holder: u64,
    holding: BTreeSet<QueueName>,
}

impl MemoryHeld {
    fn holder_for(&self, queue: &QueueName) -> Option<u64> {
        self.holding.contains(queue).then_some(self.holder)
    }
}

impl Transport for MemoryHeld {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        self.memory.append_as(self.holder_for(queue), queue, record)
    }

    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        self.memory.read_as(queue, from, limit)
    }

    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        self.memory.cursor(queue, consumer)
    }

    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        self.memory
            .commit_as(self.holder_for(queue), queue, consumer, at)
    }

    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        self.memory
            .exclusive_as(&self.holding, self.holder, queue, body)
    }

    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError> {
        self.memory.fingerprint(queue)
    }

    fn wait_for_change(
        &self,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError> {
        self.memory.wait_for_change(queue, since, timeout)
    }

    fn document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        self.memory.document(queue, name)
    }

    fn replace_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        self.memory
            .replace_as(self.holder_for(queue), queue, name, bytes)
    }
}
