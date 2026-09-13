//! Stamping, numbering, bounding, redacting and writing envelopes.
//!
//! One emitter per producing process and stream, because `seq` is monotonic
//! per `stream` and `stream` is a unique id per producing process — the two are
//! one fact. It is cheap to clone and safe to share across threads: the counter
//! is atomic and the sink is behind one lock, so a line is written whole.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use crate::bounds::bound_payload;
use crate::clock::now_rfc3339;
use crate::envelope::{ArtifactRef, Envelope, Kind};
use crate::filter::Filter;
use crate::redact::Redactor;
use crate::vocabulary::Vocabulary;

/// Why an envelope could not be recorded.
#[derive(Debug, thiserror::Error)]
pub enum EmitterError {
    /// The sink refused the write.
    #[error("cannot record a {kind} event on stream {stream}: {source}")]
    Write {
        /// The stream the envelope belongs to.
        stream: String,
        /// The kind that was being recorded.
        kind: Kind,
        /// What the sink said.
        #[source]
        source: std::io::Error,
    },
    /// The envelope would not serialize to JSON, so there was no line to
    /// write. The core's own fields always serialize; this is a vocabulary's
    /// `Serialize` impl refusing a value it was handed.
    #[error("cannot record a {kind} event on stream {stream} at seq {seq}: it does not serialize to JSON: {source}")]
    Serialize {
        /// The stream the envelope belongs to.
        stream: String,
        /// The number the envelope was stamped with.
        seq: u64,
        /// The kind that was being recorded.
        kind: Kind,
        /// What the serializer said.
        #[source]
        source: serde_json::Error,
    },
    /// The shared stream file could not be locked, so the envelope could not
    /// be numbered.
    #[error("cannot order a {kind} event in {path}: {source}")]
    Lock {
        /// The file that would have ordered it.
        path: PathBuf,
        /// The kind that was being recorded.
        kind: Kind,
        /// What the lock said.
        #[source]
        source: std::io::Error,
    },
}

/// An envelope that was stamped but could not be recorded, and why.
#[derive(Debug)]
pub struct Unrecorded<V: Vocabulary> {
    /// The envelope as it was stamped.
    pub envelope: Envelope<V>,
    /// Why it was not written.
    pub error: EmitterError,
}

impl<V: Vocabulary> std::fmt::Display for Unrecorded<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl<V: Vocabulary> std::error::Error for Unrecorded<V> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Where the envelopes go, and where `seq` comes from.
enum Sink {
    /// One writer: the sequence is counted in memory.
    Single {
        seq: Arc<AtomicU64>,
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
    },
    /// Several processes appending to one file: the sequence is read from the
    /// file under its lock, so it is one series over the file rather than one
    /// per process.
    Shared { path: PathBuf },
}

/// Stamps and numbers one stream's envelopes and writes them where the
/// producer said.
///
/// Every envelope leaves through [`emit`](Self::emit) or one of its widenings,
/// which apply the emitter's rule — redaction, then [`bound_payload`] — before
/// the envelope is stamped. What the filter does not admit is returned but
/// never written, and takes no number: `seq` numbers what the stream carries.
pub struct Emitter<V: Vocabulary> {
    stream: String,
    source: V::Source,
    version: u32,
    sink: Arc<Sink>,
    labels: V::Labels,
    dimensions: V::Dimensions,
    filter: Arc<Filter<V>>,
    redactor: Arc<Redactor>,
}

impl<V: Vocabulary> Clone for Emitter<V> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
            source: self.source.clone(),
            version: self.version,
            sink: Arc::clone(&self.sink),
            labels: self.labels.clone(),
            dimensions: self.dimensions.clone(),
            filter: Arc::clone(&self.filter),
            redactor: Arc::clone(&self.redactor),
        }
    }
}

impl<V: Vocabulary> std::fmt::Debug for Emitter<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter")
            .field("stream", &self.stream)
            .field("source", &self.source)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl<V: Vocabulary> Emitter<V> {
    /// An emitter for `stream`, speaking as `source`, writing one envelope per
    /// line to `sink`. The sequence is counted in memory: this is the
    /// single-writer stream.
    ///
    /// It stamps the version the vocabulary says `source` writes against, and
    /// redacts what this process's environment says is a credential
    /// ([`Redactor::from_env`]).
    #[must_use]
    pub fn new(stream: impl Into<String>, source: V::Source, sink: Box<dyn Write + Send>) -> Self {
        Self::build(
            stream.into(),
            source,
            Sink::Single {
                seq: Arc::new(AtomicU64::new(0)),
                writer: Arc::new(Mutex::new(sink)),
            },
        )
    }

    /// An emitter appending to the file at `path`, which other processes may
    /// append to at the same time.
    ///
    /// Each envelope is numbered from what the file already holds, under an
    /// exclusive lock on the file, and written inside that same turn — so two
    /// processes emitting together leave one gapless series.
    #[must_use]
    pub fn shared(stream: impl Into<String>, source: V::Source, path: impl AsRef<Path>) -> Self {
        Self::build(
            stream.into(),
            source,
            Sink::Shared {
                path: path.as_ref().to_path_buf(),
            },
        )
    }

    fn build(stream: String, source: V::Source, sink: Sink) -> Self {
        let version = V::write_version(&source);
        Self {
            stream,
            source,
            version,
            sink: Arc::new(sink),
            labels: V::Labels::default(),
            dimensions: V::Dimensions::default(),
            filter: Arc::new(Filter::default()),
            redactor: Arc::new(Redactor::from_env()),
        }
    }

    /// The same emitter, stamping `version` rather than the one the
    /// vocabulary declares for its source.
    #[must_use]
    pub fn with_version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }

    /// The same emitter, putting only what `filter` admits on the stream.
    #[must_use]
    pub fn with_filter(mut self, filter: Filter<V>) -> Self {
        self.filter = Arc::new(filter);
        self
    }

    /// The same emitter, redacting through `redactor` rather than the one
    /// built from this process's environment.
    #[must_use]
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = Arc::new(redactor);
        self
    }

    /// The same emitter with `labels` stamped on everything it writes next.
    ///
    /// The derived emitter's own stamp wins and this emitter's fills in what it
    /// left absent — so a child emitter derived from its parent's carries the
    /// keys it did not name and its own value for the ones it did, and a stamp
    /// added at the parent never rewrites what the child stamped for itself.
    #[must_use]
    pub fn with_labels(mut self, labels: V::Labels) -> Self {
        self.labels = merged(&labels, self.labels.clone());
        self
    }

    /// The same emitter with `dimensions` stamped on everything it writes next,
    /// unless an emit names its own.
    #[must_use]
    pub fn with_dimensions(mut self, dimensions: V::Dimensions) -> Self {
        self.dimensions = dimensions;
        self
    }

    /// The stream id every envelope this emitter writes carries.
    #[must_use]
    pub fn stream(&self) -> &str {
        &self.stream
    }

    /// The source every envelope this emitter writes carries.
    #[must_use]
    pub fn source(&self) -> &V::Source {
        &self.source
    }

    /// The envelope schema version this emitter stamps.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The labels this emitter stamps.
    #[must_use]
    pub fn labels(&self) -> &V::Labels {
        &self.labels
    }

    /// Write one event and return the envelope as it was written — or, when
    /// the filter did not admit it, as it would have been, carrying the number
    /// the next admitted envelope takes.
    ///
    /// A sink that cannot be written to, or an envelope the vocabulary's types
    /// will not serialize, is reported on stderr rather than returned: the
    /// envelope is the producer's own record of what it did, and a failed write
    /// of that record must not become a failed command. A
    /// caller that wants the failure calls [`try_emit`](Self::try_emit).
    pub fn emit(&self, kind: impl Into<Kind>, payload: Map<String, Value>) -> Envelope<V> {
        self.emit_with(kind, payload, Vec::new())
    }

    /// [`emit`](Self::emit), with artifacts attached.
    pub fn emit_with(
        &self,
        kind: impl Into<Kind>,
        payload: Map<String, Value>,
        artifacts: Vec<ArtifactRef>,
    ) -> Envelope<V> {
        self.emit_stamped(kind, self.dimensions.clone(), payload, artifacts)
    }

    /// [`emit_with`](Self::emit_with), at `dimensions` of the event's own
    /// rather than the emitter's default.
    pub fn emit_stamped(
        &self,
        kind: impl Into<Kind>,
        dimensions: V::Dimensions,
        payload: Map<String, Value>,
        artifacts: Vec<ArtifactRef>,
    ) -> Envelope<V> {
        match self.try_emit_stamped(kind, dimensions, payload, artifacts) {
            Ok(envelope) => envelope,
            Err(unrecorded) => {
                eprintln!("onemessagebus: warning: {}", unrecorded.error);
                unrecorded.envelope
            }
        }
    }

    /// [`emit_with`](Self::emit_with), handing back the failure to record the
    /// envelope beside the envelope itself.
    ///
    /// # Errors
    ///
    /// The envelope as it was stamped, and why it could not be written.
    pub fn try_emit(
        &self,
        kind: impl Into<Kind>,
        payload: Map<String, Value>,
        artifacts: Vec<ArtifactRef>,
    ) -> Result<Envelope<V>, Box<Unrecorded<V>>> {
        self.try_emit_stamped(kind, self.dimensions.clone(), payload, artifacts)
    }

    /// [`emit_stamped`](Self::emit_stamped), handing back the failure to
    /// record the envelope beside the envelope itself.
    ///
    /// # Errors
    ///
    /// The envelope as it was stamped, and why it could not be written.
    pub fn try_emit_stamped(
        &self,
        kind: impl Into<Kind>,
        dimensions: V::Dimensions,
        payload: Map<String, Value>,
        artifacts: Vec<ArtifactRef>,
    ) -> Result<Envelope<V>, Box<Unrecorded<V>>> {
        let kind = kind.into();
        // The emitter's rule, applied to the untyped map before the envelope is
        // stamped: redact, then the head of every top-level text value.
        let payload = match self.redactor.redact_value(Value::Object(payload)) {
            Value::Object(clean) => bound_payload(clean),
            _ => Map::new(),
        };
        let admitted = self
            .filter
            .allows(&self.source, kind.as_str(), &dimensions, &self.labels);
        let mut envelope = Envelope {
            v: self.version,
            ts: now_rfc3339(),
            stream: self.stream.clone(),
            seq: 0,
            source: self.source.clone(),
            kind,
            dimensions,
            labels: self.labels.clone(),
            payload,
            artifacts,
        };
        match self.sink.as_ref() {
            Sink::Single { seq, writer } => {
                // `seq` numbers what the stream carries: a suppressed envelope is
                // returned carrying the number the next admitted one takes.
                envelope.seq = if admitted {
                    seq.fetch_add(1, Ordering::SeqCst) + 1
                } else {
                    seq.load(Ordering::SeqCst) + 1
                };
                if !admitted {
                    return Ok(envelope);
                }
                // Serialized before the lock is taken, so a refusal leaves the
                // sink untouched and unheld. The number stays consumed, as it
                // does when the write itself fails: the in-memory counter
                // numbers what was attempted, and a consumer sees the gap.
                let line = match line_of(&envelope) {
                    Ok(line) => line,
                    Err(failure) => return Err(self.unserializable(envelope, failure)),
                };
                // A poisoned lock is a sink some other writer panicked while
                // holding; the sink itself is still there to write to.
                let mut sink = writer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match sink.write_all(line.as_bytes()).and_then(|()| sink.flush()) {
                    Ok(()) => Ok(envelope),
                    Err(failure) => Err(self.failed(envelope, failure)),
                }
            }
            Sink::Shared { path } => {
                let mut file = match open_locked(path) {
                    Ok(file) => file,
                    Err(failure) => {
                        let error = EmitterError::Lock {
                            path: path.clone(),
                            kind: envelope.kind.clone(),
                            source: failure,
                        };
                        return Err(Box::new(Unrecorded { envelope, error }));
                    }
                };
                let held = match recorded(&mut file, path) {
                    Ok(held) => held,
                    Err(failure) => return Err(self.failed(envelope, failure)),
                };
                envelope.seq = held + 1;
                if !admitted {
                    return Ok(envelope);
                }
                // Returning drops `file`, which releases the lock; nothing was
                // written, so the number is not consumed — the next envelope
                // takes it, as it does after a failed write, because here the
                // file is what numbers the series.
                let line = match line_of(&envelope) {
                    Ok(line) => line,
                    Err(failure) => return Err(self.unserializable(envelope, failure)),
                };
                // One whole line in one call, at the end `recorded` left the
                // handle at, under the lock: two writes per line is how a writer
                // dying between them leaves a line nobody finished.
                match file.write_all(line.as_bytes()).and_then(|()| file.flush()) {
                    Ok(()) => Ok(envelope),
                    Err(failure) => Err(self.failed(envelope, failure)),
                }
            }
        }
    }

    fn failed(&self, envelope: Envelope<V>, failure: std::io::Error) -> Box<Unrecorded<V>> {
        let error = EmitterError::Write {
            stream: self.stream.clone(),
            kind: envelope.kind.clone(),
            source: failure,
        };
        Box::new(Unrecorded { envelope, error })
    }

    fn unserializable(
        &self,
        envelope: Envelope<V>,
        failure: serde_json::Error,
    ) -> Box<Unrecorded<V>> {
        let error = EmitterError::Serialize {
            stream: self.stream.clone(),
            seq: envelope.seq,
            kind: envelope.kind.clone(),
            source: failure,
        };
        Box::new(Unrecorded { envelope, error })
    }
}

/// One envelope as its line: the JSON and the newline that ends the record.
///
/// Whole or not at all: the line is built in memory before any byte reaches a
/// sink, so a refusal leaves no partial record behind. The core's fields cannot
/// be refused, but the vocabulary's source, dimensions and labels serialize
/// through impls the consumer wrote.
fn line_of<V: Vocabulary>(envelope: &Envelope<V>) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(envelope)?;
    line.push('\n');
    Ok(line)
}

/// The shared stream file, opened for reading and writing and locked
/// exclusively. The OS lock is the one coordination that survives a holder
/// being killed, and it is released when the file is dropped, at the end of the
/// emit.
///
/// Not opened for appending: healing a torn tail truncates through this handle,
/// and Windows refuses `set_len` on an append-only handle. Every writer holds the
/// lock while it reads to the end and writes there, so the lock positions the
/// line where an append flag would have.
fn open_locked(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.lock()?;
    Ok(file)
}

/// How many whole records the file already holds, and — where a writer died
/// mid-line — the torn tail healed away, so the next record starts on a line
/// of its own rather than glued to half of somebody else's.
fn recorded(file: &mut File, path: &Path) -> std::io::Result<u64> {
    let mut contents = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut contents)?;
    let whole = contents.iter().filter(|byte| **byte == b'\n').count() as u64;
    if !contents.is_empty() && !contents.ends_with(b"\n") {
        let keep = contents
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1);
        file.set_len(keep as u64)?;
        file.seek(SeekFrom::End(0))?;
        eprintln!(
            "onemessagebus: healed a torn record of {} bytes at byte {keep} of {}",
            contents.len() - keep,
            path.display()
        );
    }
    Ok(whole)
}

/// `base` with every key of `fill` it did not carry, and nothing `base` holds
/// rewritten — applied through the label set's own serialization so it holds
/// for any vocabulary.
fn merged<L: serde::Serialize + serde::de::DeserializeOwned + Clone>(base: &L, fill: L) -> L {
    let (Ok(Value::Object(mut base_map)), Ok(Value::Object(fill_map))) =
        (serde_json::to_value(base), serde_json::to_value(&fill))
    else {
        return base.clone();
    };
    for (key, value) in fill_map {
        base_map.entry(key).or_insert(value);
    }
    serde_json::from_value(Value::Object(base_map)).unwrap_or_else(|_| base.clone())
}
