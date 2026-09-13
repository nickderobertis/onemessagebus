//! A transport in another process: the plugin protocol, both of its ends.
//!
//! A transport kind that is neither built in nor registered in-process is an
//! executable named [`PLUGIN_PREFIX`]`<kind>` on `PATH`. [`ProcessTransport`]
//! spawns it and speaks this protocol to it over its stdin and stdout; the
//! plugin's `main` hands its [`Transport`] to [`serve`], which answers. So a
//! transport written in any crate — or any language — serves the shipped
//! `onemessagebus` binary with no consumer change.
//!
//! One JSON object per line in each direction. The client's first line is a
//! [`PluginHello`] naming the protocol, its version and the configuration the
//! transport is opened with; the plugin answers it, and then every
//! [`PluginRequest`] with one [`PluginReply`]. `exclusive` is the one method that
//! is not a single exchange: the client opens the section with `begin_exclusive`,
//! every request until the matching `end_exclusive` is served inside it, and the
//! plugin answers the end once the section has been let go. `docs/transport.md`
//! states the protocol, and the registry records the three shapes as
//! `onemessagebus.transport-hello@1`, `onemessagebus.transport-request@1` and
//! `onemessagebus.transport-reply@1` ([`register_protocol`]).

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kinds::TransportConfig;
use crate::schema::{Registry, RegistryError, SchemaId};
use crate::transport::{
    Batch, Changed, ConsumerName, DocumentName, Fingerprint, Position, QueueName, Stored,
    TornRecord, Transport, TransportError,
};

/// The protocol's name, in the first line of each direction.
pub const PROTOCOL: &str = "onemessagebus-transport";

/// The protocol version this build speaks. A plugin answering a hello at
/// another version is refused, naming both.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest one wait request holds a plugin's pipe: a longer wait is a
/// succession of these, so another handle's request is never held behind one.
const REMOTE_WAIT: Duration = Duration::from_millis(50);

/// What a plugin executable's name starts with: `onemessagebus-transport-nats`
/// serves the kind `nats`.
pub const PLUGIN_PREFIX: &str = "onemessagebus-transport-";

/// `onemessagebus.transport-hello@1`: [`PluginHello`].
pub const HELLO_SCHEMA: SchemaId = SchemaId::literal("onemessagebus", "transport-hello", 1);

/// `onemessagebus.transport-request@1`: [`PluginRequest`].
pub const REQUEST_SCHEMA: SchemaId = SchemaId::literal("onemessagebus", "transport-request", 1);

/// `onemessagebus.transport-reply@1`: [`PluginReply`].
pub const REPLY_SCHEMA: SchemaId = SchemaId::literal("onemessagebus", "transport-reply", 1);

/// Record the protocol's three shapes in `registry`, so a client in another
/// language validates what it sends and reads against the documents this build
/// speaks.
///
/// # Errors
///
/// [`RegistryError::Conflict`] when one of the ids already holds another
/// document.
pub fn register_protocol(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_schema(HELLO_SCHEMA, schemars::schema_for!(PluginHello).to_value())?;
    registry.register_schema(
        REQUEST_SCHEMA,
        schemars::schema_for!(PluginRequest).to_value(),
    )?;
    registry.register_schema(REPLY_SCHEMA, schemars::schema_for!(PluginReply).to_value())
}

/// The client's first line: which protocol, at which version, opening which
/// transport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginHello {
    /// Always [`PROTOCOL`].
    pub protocol: String,
    /// The version the client speaks: [`PROTOCOL_VERSION`].
    pub version: u32,
    /// The `transport` block of the configuration, with `dir` resolved.
    pub config: TransportConfig,
}

/// One request after the hello, discriminated by `op`: one per [`Transport`]
/// method, with `exclusive` split into its opening and its end.
///
/// Records and documents travel as text, so a plugin carries UTF-8 records; a
/// record that is not UTF-8 is refused by the client before it is sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginRequest {
    /// [`Transport::append`].
    Append {
        /// The queue.
        queue: QueueName,
        /// The record.
        record: String,
    },
    /// [`Transport::read`].
    Read {
        /// The queue.
        queue: QueueName,
        /// Read after this position; from the start when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Position>,
        /// At most this many records.
        limit: u64,
    },
    /// [`Transport::cursor`].
    Cursor {
        /// The queue.
        queue: QueueName,
        /// The consumer.
        consumer: ConsumerName,
    },
    /// [`Transport::commit`].
    Commit {
        /// The queue.
        queue: QueueName,
        /// The consumer.
        consumer: ConsumerName,
        /// Where it has read up to.
        at: Position,
    },
    /// Open [`Transport::exclusive`]'s section over a queue.
    BeginExclusive {
        /// The queue.
        queue: QueueName,
    },
    /// End the section the matching `begin_exclusive` opened.
    EndExclusive {
        /// The queue.
        queue: QueueName,
        /// Whether the body failed, so the section ends with its failure.
        failed: bool,
    },
    /// [`Transport::fingerprint`].
    Fingerprint {
        /// The queue.
        queue: QueueName,
    },
    /// [`Transport::wait_for_change`].
    WaitForChange {
        /// The queue.
        queue: QueueName,
        /// The fingerprint to wait for it to move from.
        since: Fingerprint,
        /// How long to wait, in milliseconds.
        timeout_ms: u64,
    },
    /// [`Transport::document`].
    Document {
        /// The queue.
        queue: QueueName,
        /// The document.
        name: DocumentName,
    },
    /// [`Transport::replace_document`].
    ReplaceDocument {
        /// The queue.
        queue: QueueName,
        /// The document.
        name: DocumentName,
        /// Its new contents.
        bytes: String,
    },
}

/// One reply: what was asked for, or why not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginReply {
    /// The request was done.
    Ok(PluginAnswer),
    /// It was not.
    Error(PluginError),
}

/// What a request that was done answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginAnswer {
    /// The answer to a hello: the plugin speaks this protocol at this version.
    Hello {
        /// Always [`PROTOCOL`].
        protocol: String,
        /// The version the plugin speaks.
        version: u32,
    },
    /// Where an appended record landed.
    Position(Position),
    /// The records a read found.
    Batch {
        /// The whole records, oldest first.
        records: Vec<PluginStored>,
        /// A torn record after them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        torn: Option<PluginTorn>,
    },
    /// A consumer's cursor, or `null` for one that has read nothing.
    Cursor(Option<Position>),
    /// A fingerprint.
    Fingerprint(Fingerprint),
    /// What a wait for a change found.
    Changed {
        /// Whether the queue moved.
        moved: bool,
        /// Its fingerprint now.
        fingerprint: Fingerprint,
    },
    /// A document's contents, or `null` where there is none.
    Document(Option<String>),
    /// Done, with nothing to hand back.
    Done,
}

/// One record of a [`PluginAnswer::Batch`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginStored {
    /// The record.
    pub record: String,
    /// The position after it.
    pub after: Position,
}

/// The torn record of a [`PluginAnswer::Batch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginTorn {
    /// Where it starts.
    pub at: Position,
    /// How many bytes of it there are.
    pub bytes: u64,
}

/// Why a request was not done, in words the client turns back into a
/// [`TransportError`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginError {
    /// Which refusal: `past_end` and `not_a_boundary` carry the queue and the
    /// positions, so a queue reading the reply can fold its log again; every
    /// other refusal is the plugin's words.
    pub kind: PluginErrorKind,
    /// What the plugin said.
    pub message: String,
    /// The queue, for `past_end` and `not_a_boundary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueName>,
    /// The position asked for, for `past_end` and `not_a_boundary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    /// Where the queue ends, for `past_end`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<Position>,
}

/// The refusals the protocol tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginErrorKind {
    /// [`TransportError::PastEnd`].
    PastEnd,
    /// [`TransportError::NotABoundary`].
    NotABoundary,
    /// Anything else the transport refused.
    Refused,
    /// A request the plugin could not read, or one out of its place.
    Protocol,
}

impl PluginError {
    fn of(failure: &TransportError) -> Self {
        let mut error = Self {
            kind: PluginErrorKind::Refused,
            message: failure.to_string(),
            queue: None,
            position: None,
            end: None,
        };
        match failure {
            TransportError::PastEnd {
                queue,
                position,
                end,
            } => {
                error.kind = PluginErrorKind::PastEnd;
                error.queue = Some(queue.clone());
                error.position = Some(*position);
                error.end = Some(*end);
            }
            TransportError::NotABoundary { queue, position } => {
                error.kind = PluginErrorKind::NotABoundary;
                error.queue = Some(queue.clone());
                error.position = Some(*position);
            }
            _ => {}
        }
        error
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: PluginErrorKind::Protocol,
            message: message.into(),
            queue: None,
            position: None,
            end: None,
        }
    }

    fn into_transport_error(self, kind: &str) -> TransportError {
        match (self.kind, self.queue, self.position, self.end) {
            (PluginErrorKind::PastEnd, Some(queue), Some(position), Some(end)) => {
                TransportError::PastEnd {
                    queue,
                    position,
                    end,
                }
            }
            (PluginErrorKind::NotABoundary, Some(queue), Some(position), _) => {
                TransportError::NotABoundary { queue, position }
            }
            _ => TransportError::Backend {
                transport: kind.to_owned(),
                detail: self.message,
            },
        }
    }
}

/// Serve `open`'s transport over the plugin protocol: the whole of a plugin's
/// `main`.
///
/// Reads the hello from `input`, opens the transport with the configuration it
/// names, answers, and then answers every request until `input` ends. A request
/// the transport refuses is answered with its refusal and serving goes on; a
/// line that is not a request is answered with a protocol refusal.
///
/// ```no_run
/// use std::sync::Arc;
/// use onemessagebus::{LocalTransport, Transport};
///
/// fn main() {
///     let stdin = std::io::stdin().lock();
///     let stdout = std::io::stdout().lock();
///     let served = onemessagebus::transport::serve(
///         |config| {
///             let dir = config.dir.clone().unwrap_or_else(|| ".".into());
///             Ok(Arc::new(LocalTransport::open(dir)?) as Arc<dyn Transport>)
///         },
///         stdin,
///         stdout,
///     );
///     if let Err(failure) = served {
///         eprintln!("{failure}");
///         std::process::exit(1);
///     }
/// }
/// ```
///
/// # Errors
///
/// A hello at another protocol or version, a transport that could not be
/// opened, or input and output that could not be read and written.
pub fn serve(
    open: impl FnOnce(&TransportConfig) -> Result<Arc<dyn Transport>, TransportError>,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<(), TransportError> {
    let mut lines = input.lines();
    let backend = |detail: String| TransportError::Backend {
        transport: "plugin".to_owned(),
        detail,
    };
    let Some(first) = lines.next() else {
        return Ok(());
    };
    let first = first.map_err(|failure| backend(format!("cannot read the hello: {failure}")))?;
    let hello: PluginHello = match serde_json::from_str(&first) {
        Ok(hello) => hello,
        Err(failure) => {
            let why = format!("the first line is not a {PROTOCOL} hello: {failure}");
            write_reply(
                &mut output,
                &PluginReply::Error(PluginError::protocol(why.clone())),
            )?;
            return Err(backend(why));
        }
    };
    if hello.protocol != PROTOCOL || hello.version != PROTOCOL_VERSION {
        let why = format!(
            "the client speaks {} version {}, and this plugin speaks {PROTOCOL} version {PROTOCOL_VERSION}",
            hello.protocol, hello.version
        );
        write_reply(
            &mut output,
            &PluginReply::Error(PluginError::protocol(why.clone())),
        )?;
        return Err(backend(why));
    }
    let transport = match open(&hello.config) {
        Ok(transport) => transport,
        Err(failure) => {
            write_reply(&mut output, &PluginReply::Error(PluginError::of(&failure)))?;
            return Err(failure);
        }
    };
    write_reply(
        &mut output,
        &PluginReply::Ok(PluginAnswer::Hello {
            protocol: PROTOCOL.to_owned(),
            version: PROTOCOL_VERSION,
        }),
    )?;
    let mut next_line = || lines.next();
    serve_until(transport.as_ref(), &mut next_line, &mut output, None)
}

type NextLine<'a> = dyn FnMut() -> Option<std::io::Result<String>> + 'a;

/// Serve requests against `transport` until `input` ends, or — inside a
/// section — until the `end_exclusive` for `section` arrives.
fn serve_until(
    transport: &dyn Transport,
    next_line: &mut NextLine<'_>,
    output: &mut dyn Write,
    section: Option<&QueueName>,
) -> Result<(), TransportError> {
    let backend = |detail: String| TransportError::Backend {
        transport: "plugin".to_owned(),
        detail,
    };
    while let Some(line) = next_line() {
        let line = line.map_err(|failure| backend(format!("cannot read a request: {failure}")))?;
        if line.trim().is_empty() {
            continue;
        }
        let request: PluginRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(failure) => {
                write_reply(
                    output,
                    &PluginReply::Error(PluginError::protocol(format!(
                        "not a {PROTOCOL} request: {failure}"
                    ))),
                )?;
                continue;
            }
        };
        let answer = match request {
            PluginRequest::EndExclusive { queue, failed } => {
                if section == Some(&queue) {
                    return if failed {
                        Err(backend(format!("the client's section over {queue} failed")))
                    } else {
                        Ok(())
                    };
                }
                Err(PluginError::protocol(format!(
                    "end_exclusive for {queue}, which no open section holds"
                )))
            }
            PluginRequest::BeginExclusive { queue } => {
                write_reply(output, &PluginReply::Ok(PluginAnswer::Done))?;
                let ended = transport.exclusive(&queue, &mut |inner| {
                    serve_until(inner, next_line, output, Some(&queue))
                });
                match ended {
                    Ok(()) => Ok(PluginAnswer::Done),
                    Err(failure) => Err(PluginError::of(&failure)),
                }
            }
            other => answer(transport, other).map_err(|failure| PluginError::of(&failure)),
        };
        let reply = match answer {
            Ok(answer) => PluginReply::Ok(answer),
            Err(error) => PluginReply::Error(error),
        };
        write_reply(output, &reply)?;
    }
    match section {
        None => Ok(()),
        Some(queue) => Err(backend(format!(
            "the client went away inside its section over {queue}"
        ))),
    }
}

/// Answer one request that is a single exchange.
fn answer(
    transport: &dyn Transport,
    request: PluginRequest,
) -> Result<PluginAnswer, TransportError> {
    Ok(match request {
        PluginRequest::Append { queue, record } => {
            PluginAnswer::Position(transport.append(&queue, record.as_bytes())?)
        }
        PluginRequest::Read { queue, from, limit } => {
            let batch = transport.read(
                &queue,
                from.as_ref(),
                usize::try_from(limit).unwrap_or(usize::MAX),
            )?;
            PluginAnswer::Batch {
                records: batch
                    .records
                    .into_iter()
                    .map(|stored| PluginStored {
                        record: String::from_utf8_lossy(&stored.bytes).into_owned(),
                        after: stored.after,
                    })
                    .collect(),
                torn: batch.torn.map(|torn| PluginTorn {
                    at: torn.at,
                    bytes: torn.bytes,
                }),
            }
        }
        PluginRequest::Cursor { queue, consumer } => {
            PluginAnswer::Cursor(transport.cursor(&queue, &consumer)?)
        }
        PluginRequest::Commit {
            queue,
            consumer,
            at,
        } => {
            transport.commit(&queue, &consumer, &at)?;
            PluginAnswer::Done
        }
        PluginRequest::Fingerprint { queue } => {
            PluginAnswer::Fingerprint(transport.fingerprint(&queue)?)
        }
        PluginRequest::WaitForChange {
            queue,
            since,
            timeout_ms,
        } => match transport.wait_for_change(&queue, &since, Duration::from_millis(timeout_ms))? {
            Changed::Moved(fingerprint) => PluginAnswer::Changed {
                moved: true,
                fingerprint,
            },
            Changed::Unchanged(fingerprint) => PluginAnswer::Changed {
                moved: false,
                fingerprint,
            },
        },
        PluginRequest::Document { queue, name } => PluginAnswer::Document(
            transport
                .document(&queue, &name)?
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        ),
        PluginRequest::ReplaceDocument { queue, name, bytes } => {
            transport.replace_document(&queue, &name, bytes.as_bytes())?;
            PluginAnswer::Done
        }
        PluginRequest::BeginExclusive { .. } | PluginRequest::EndExclusive { .. } => {
            return Err(TransportError::Backend {
                transport: "plugin".to_owned(),
                detail: "a section request is not a single exchange".to_owned(),
            })
        }
    })
}

fn write_reply(output: &mut dyn Write, reply: &PluginReply) -> Result<(), TransportError> {
    let mut line = serde_json::to_string(reply).map_err(|failure| TransportError::Backend {
        transport: "plugin".to_owned(),
        detail: format!("cannot render a reply: {failure}"),
    })?;
    line.push('\n');
    output
        .write_all(line.as_bytes())
        .and_then(|()| output.flush())
        .map_err(|failure| TransportError::Backend {
            transport: "plugin".to_owned(),
            detail: format!("cannot write a reply: {failure}"),
        })
}

// ---------------------------------------------------------------------------
// The client end.
// ---------------------------------------------------------------------------

/// The plugin process behind a [`ProcessTransport`], and the pipe to it.
struct PluginProcess {
    kind: String,
    path: PathBuf,
    pipe: Mutex<Pipe>,
    /// Signalled when a section ends.
    released: Condvar,
    holders: AtomicU64,
}

impl std::fmt::Debug for PluginProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginProcess")
            .field("kind", &self.kind)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

struct Pipe {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// The section holding the pipe, when one is.
    held_by: Option<u64>,
}

impl PluginProcess {
    fn spawn(path: &Path, config: &TransportConfig) -> Result<Self, TransportError> {
        let kind = config.kind.clone();
        let backend = |detail: String| TransportError::Backend {
            transport: kind.clone(),
            detail,
        };
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|failure| {
                backend(format!(
                    "cannot start the plugin {}: {failure}",
                    path.display()
                ))
            })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(backend("the plugin was started without pipes".to_owned()));
        };
        let transport = Self {
            kind: kind.clone(),
            path: path.to_path_buf(),
            pipe: Mutex::new(Pipe {
                child,
                stdin: Some(stdin),
                stdout: BufReader::new(stdout),
                held_by: None,
            }),
            released: Condvar::new(),
            holders: AtomicU64::new(0),
        };
        let hello = serde_json::to_string(&PluginHello {
            protocol: PROTOCOL.to_owned(),
            version: PROTOCOL_VERSION,
            config: config.clone(),
        })
        .map_err(|failure| backend(format!("cannot render the hello: {failure}")))?;
        match transport.exchange_line(None, &hello)? {
            PluginAnswer::Hello { protocol, version }
                if protocol == PROTOCOL && version == PROTOCOL_VERSION =>
            {
                Ok(transport)
            }
            PluginAnswer::Hello { protocol, version } => Err(backend(format!(
                "the plugin {} speaks {protocol} version {version}, and this build speaks {PROTOCOL} version {PROTOCOL_VERSION}",
                path.display()
            ))),
            other => Err(backend(format!(
                "the plugin {} answered the hello with {other:?}",
                path.display()
            ))),
        }
    }

    fn backend(&self, detail: String) -> TransportError {
        TransportError::Backend {
            transport: self.kind.clone(),
            detail,
        }
    }

    /// Send one line and read its reply, as `holder` — the section this request
    /// belongs to, or `None` for one outside every section.
    fn exchange_line(
        &self,
        holder: Option<u64>,
        line: &str,
    ) -> Result<PluginAnswer, TransportError> {
        let mut pipe = self
            .pipe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while let Some(owner) = pipe.held_by {
            if Some(owner) == holder {
                break;
            }
            pipe = self
                .released
                .wait(pipe)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        self.exchange_on(&mut pipe, line)
    }

    fn exchange_on(&self, pipe: &mut Pipe, line: &str) -> Result<PluginAnswer, TransportError> {
        let Some(stdin) = pipe.stdin.as_mut() else {
            return Err(self.backend("the plugin's input is closed".to_owned()));
        };
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|failure| self.backend(format!("cannot write to the plugin: {failure}")))?;
        let mut reply = String::new();
        let read = pipe
            .stdout
            .read_line(&mut reply)
            .map_err(|failure| self.backend(format!("cannot read from the plugin: {failure}")))?;
        if read == 0 {
            return Err(self.backend(format!(
                "the plugin {} exited without answering",
                self.path.display()
            )));
        }
        match serde_json::from_str::<PluginReply>(&reply) {
            Ok(PluginReply::Ok(answer)) => Ok(answer),
            Ok(PluginReply::Error(error)) => Err(error.into_transport_error(&self.kind)),
            Err(failure) => Err(self.backend(format!(
                "the plugin answered with a line that is not a {PROTOCOL} reply: {failure}: {}",
                reply.trim_end()
            ))),
        }
    }

    fn request(
        &self,
        holder: Option<u64>,
        request: &PluginRequest,
    ) -> Result<PluginAnswer, TransportError> {
        let line = serde_json::to_string(request)
            .map_err(|failure| self.backend(format!("cannot render a request: {failure}")))?;
        self.exchange_line(holder, &line)
    }

    fn unexpected(&self, answer: &PluginAnswer, asked: &str) -> TransportError {
        self.backend(format!("the plugin answered {asked} with {answer:?}"))
    }

    fn call_append(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        record: &[u8],
    ) -> Result<Position, TransportError> {
        let Ok(record) = std::str::from_utf8(record) else {
            return Err(TransportError::NotARecord {
                queue: queue.clone(),
                why: "is not UTF-8, which the plugin protocol carries records as",
            });
        };
        match self.request(
            holder,
            &PluginRequest::Append {
                queue: queue.clone(),
                record: record.to_owned(),
            },
        )? {
            PluginAnswer::Position(position) => Ok(position),
            other => Err(self.unexpected(&other, "append")),
        }
    }

    fn call_read(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        match self.request(
            holder,
            &PluginRequest::Read {
                queue: queue.clone(),
                from: from.copied(),
                limit: u64::try_from(limit).unwrap_or(u64::MAX),
            },
        )? {
            PluginAnswer::Batch { records, torn } => Ok(Batch {
                records: records
                    .into_iter()
                    .map(|stored| Stored {
                        bytes: stored.record.into_bytes(),
                        after: stored.after,
                    })
                    .collect(),
                torn: torn.map(|torn| TornRecord {
                    at: torn.at,
                    bytes: torn.bytes,
                }),
            }),
            other => Err(self.unexpected(&other, "read")),
        }
    }

    fn call_cursor(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        match self.request(
            holder,
            &PluginRequest::Cursor {
                queue: queue.clone(),
                consumer: consumer.clone(),
            },
        )? {
            PluginAnswer::Cursor(position) => Ok(position),
            other => Err(self.unexpected(&other, "cursor")),
        }
    }

    fn call_commit(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        match self.request(
            holder,
            &PluginRequest::Commit {
                queue: queue.clone(),
                consumer: consumer.clone(),
                at: *at,
            },
        )? {
            PluginAnswer::Done => Ok(()),
            other => Err(self.unexpected(&other, "commit")),
        }
    }

    fn call_exclusive(
        self: &Arc<Self>,
        holding: &BTreeSet<QueueName>,
        holder: u64,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        if holding.contains(queue) {
            let held = ProcessHeld {
                process: Arc::clone(self),
                holder,
                holding: holding.clone(),
            };
            return body(&held);
        }
        let outermost = holding.is_empty();
        {
            let mut pipe = self
                .pipe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while pipe.held_by.is_some_and(|owner| owner != holder) {
                pipe = self
                    .released
                    .wait(pipe)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            let line = serde_json::to_string(&PluginRequest::BeginExclusive {
                queue: queue.clone(),
            })
            .map_err(|failure| self.backend(format!("cannot render a request: {failure}")))?;
            match self.exchange_on(&mut pipe, &line)? {
                PluginAnswer::Done => pipe.held_by = Some(holder),
                other => return Err(self.unexpected(&other, "begin_exclusive")),
            }
        }
        let mut nested = holding.clone();
        nested.insert(queue.clone());
        let held = ProcessHeld {
            process: Arc::clone(self),
            holder,
            holding: nested,
        };
        let result = body(&held);
        let ended = self.request(
            Some(holder),
            &PluginRequest::EndExclusive {
                queue: queue.clone(),
                failed: result.is_err(),
            },
        );
        if outermost {
            let mut pipe = self
                .pipe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pipe.held_by = None;
            self.released.notify_all();
        }
        match (result, ended) {
            (Err(failure), _) => Err(failure),
            (Ok(()), Ok(PluginAnswer::Done)) => Ok(()),
            (Ok(()), Ok(other)) => Err(self.unexpected(&other, "end_exclusive")),
            (Ok(()), Err(failure)) => Err(failure),
        }
    }

    fn call_fingerprint(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
    ) -> Result<Fingerprint, TransportError> {
        match self.request(
            holder,
            &PluginRequest::Fingerprint {
                queue: queue.clone(),
            },
        )? {
            PluginAnswer::Fingerprint(fingerprint) => Ok(fingerprint),
            other => Err(self.unexpected(&other, "fingerprint")),
        }
    }

    /// One bounded wait on the plugin, no longer than [`REMOTE_WAIT`]: the pipe
    /// is held for a request, so a wait holds it no longer than that, and a
    /// request from another handle — the claim a waiter is waiting to see —
    /// lands between two of them.
    fn call_wait_once(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError> {
        match self.request(
            holder,
            &PluginRequest::WaitForChange {
                queue: queue.clone(),
                since: since.clone(),
                timeout_ms: u64::try_from(timeout.min(REMOTE_WAIT).as_millis()).unwrap_or(u64::MAX),
            },
        )? {
            PluginAnswer::Changed {
                moved: true,
                fingerprint,
            } => Ok(Changed::Moved(fingerprint)),
            PluginAnswer::Changed {
                moved: false,
                fingerprint,
            } => Ok(Changed::Unchanged(fingerprint)),
            other => Err(self.unexpected(&other, "wait_for_change")),
        }
    }

    fn call_wait(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.call_wait_once(holder, queue, since, left)? {
                Changed::Unchanged(now) if !left.is_zero() && left > REMOTE_WAIT => {
                    let _ = now;
                }
                answered => return Ok(answered),
            }
        }
    }

    fn call_document(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        match self.request(
            holder,
            &PluginRequest::Document {
                queue: queue.clone(),
                name: name.clone(),
            },
        )? {
            PluginAnswer::Document(text) => Ok(text.map(String::into_bytes)),
            other => Err(self.unexpected(&other, "document")),
        }
    }

    fn call_replace(
        &self,
        holder: Option<u64>,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Err(self.backend(format!(
                "document {name} is not UTF-8, which the plugin protocol carries documents as"
            )));
        };
        match self.request(
            holder,
            &PluginRequest::ReplaceDocument {
                queue: queue.clone(),
                name: name.clone(),
                bytes: text.to_owned(),
            },
        )? {
            PluginAnswer::Done => Ok(()),
            other => Err(self.unexpected(&other, "replace_document")),
        }
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        let pipe = self
            .pipe
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Closing its input is how the plugin learns it is done; waiting reaps
        // it, so no plugin outlives the transport that started it.
        drop(pipe.stdin.take());
        let _ = pipe.child.wait();
    }
}

/// A transport served by a plugin executable in another process: what
/// [`TransportKinds`](crate::TransportKinds) opens a kind no built-in or
/// registered transport serves as.
///
/// Spawned once and spoken to for as long as a clone of it lives; dropping the
/// last closes the plugin's stdin, which ends it. Requests from several threads
/// are serialized over the one pipe, and an exclusive section holds the pipe for
/// its body's requests alone: another thread's request waits for the section to
/// end rather than landing inside it.
#[derive(Debug, Clone)]
pub struct ProcessTransport(Arc<PluginProcess>);

impl ProcessTransport {
    /// Spawn the plugin at `path` serving `config.kind`, and greet it.
    ///
    /// # Errors
    ///
    /// [`TransportError::Backend`] when the executable cannot be started, answers
    /// the hello at another protocol or version, or refuses the configuration.
    pub fn spawn(path: &Path, config: &TransportConfig) -> Result<Self, TransportError> {
        PluginProcess::spawn(path, config).map(|process| Self(Arc::new(process)))
    }

    /// The kind this plugin serves.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.0.kind
    }
}

impl Transport for ProcessTransport {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        self.0.call_append(None, queue, record)
    }

    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        self.0.call_read(None, queue, from, limit)
    }

    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        self.0.call_cursor(None, queue, consumer)
    }

    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        self.0.call_commit(None, queue, consumer, at)
    }

    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        let holder = self.0.holders.fetch_add(1, Ordering::Relaxed) + 1;
        self.0.call_exclusive(&BTreeSet::new(), holder, queue, body)
    }

    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError> {
        self.0.call_fingerprint(None, queue)
    }

    fn wait_for_change(
        &self,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError> {
        self.0.call_wait(None, queue, since, timeout)
    }

    fn document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        self.0.call_document(None, queue, name)
    }

    fn replace_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        self.0.call_replace(None, queue, name, bytes)
    }
}

/// A plugin transport as a section hands it to its body: its requests are the
/// section's own.
struct ProcessHeld {
    process: Arc<PluginProcess>,
    holder: u64,
    holding: BTreeSet<QueueName>,
}

impl Transport for ProcessHeld {
    fn append(&self, queue: &QueueName, record: &[u8]) -> Result<Position, TransportError> {
        self.process.call_append(Some(self.holder), queue, record)
    }

    fn read(
        &self,
        queue: &QueueName,
        from: Option<&Position>,
        limit: usize,
    ) -> Result<Batch, TransportError> {
        self.process
            .call_read(Some(self.holder), queue, from, limit)
    }

    fn cursor(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
    ) -> Result<Option<Position>, TransportError> {
        self.process.call_cursor(Some(self.holder), queue, consumer)
    }

    fn commit(
        &self,
        queue: &QueueName,
        consumer: &ConsumerName,
        at: &Position,
    ) -> Result<(), TransportError> {
        self.process
            .call_commit(Some(self.holder), queue, consumer, at)
    }

    fn exclusive(
        &self,
        queue: &QueueName,
        body: &mut dyn FnMut(&dyn Transport) -> Result<(), TransportError>,
    ) -> Result<(), TransportError> {
        self.process
            .call_exclusive(&self.holding, self.holder, queue, body)
    }

    fn fingerprint(&self, queue: &QueueName) -> Result<Fingerprint, TransportError> {
        self.process.call_fingerprint(Some(self.holder), queue)
    }

    fn wait_for_change(
        &self,
        queue: &QueueName,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, TransportError> {
        self.process
            .call_wait(Some(self.holder), queue, since, timeout)
    }

    fn document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        self.process.call_document(Some(self.holder), queue, name)
    }

    fn replace_document(
        &self,
        queue: &QueueName,
        name: &DocumentName,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        self.process
            .call_replace(Some(self.holder), queue, name, bytes)
    }
}
