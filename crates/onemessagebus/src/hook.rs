//! Hooks: what a queue makes of a message before anything is appended.
//!
//! A [`Hook`] answers one [`HookOutcome`] for one message: keep it, change it,
//! reroute it to another queue, drop it, refuse it, or say it could not be
//! decided at all — and an unjudged message is never sent. [`Hooks`] is an
//! ordered chain of them, each one's output the next one's input, ended by the
//! first drop, refusal or unjudged answer.
//!
//! [`CommandHook`] is the external one: the message as one line of JSON on its
//! stdin, and whatever it prints back is the message from then on. A
//! [`HookCache`] beside it records a keep, change or drop under the message's
//! content digest and the bar's fingerprint, so an unchanged message under an
//! unchanged bar is not run twice, and reads a pass the removed validators
//! recorded under the same key as a keep. A [`RawRecord`] is how a message travels
//! through a chain and into an append without losing the spelling a hook gave
//! it. Every drop is recorded in the queue's drop log, `<queue>-dropped`, as a
//! [`Dropped`] line. `docs/hooks.md` states the contract.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ask::Correlation;
use crate::config::{Bus, BusError};
use crate::queue::{Pushed, QueueError};
use crate::schema::{Message, SchemaId};
use crate::transport::QueueName;
use crate::when::When;

/// The environment variable a command hook is told the queue in.
pub const HOOK_QUEUE_ENV: &str = "ONEMESSAGEBUS_HOOK_QUEUE";

/// The environment variable a command hook is told the correlation of the
/// question the message asks or answers in, when it asks or answers one.
pub const HOOK_CORRELATION_ENV: &str = "ONEMESSAGEBUS_HOOK_CORRELATION";

/// The environment variable naming the empty file a command hook declaring
/// routes may write one queue name to, rerouting the message there.
pub const HOOK_ROUTE_ENV: &str = "ONEMESSAGEBUS_HOOK_ROUTE";

/// The version of the record a [`HookCache`] writes.
pub const HOOK_RECORD_VERSION: u32 = 2;

/// The version of the pass record the validators removed in 0.11.0 wrote,
/// which a [`HookCache`] still reads as a keep.
pub const PASS_RECORD_VERSION: u32 = 1;

/// The version of each line a drop log holds.
pub const DROP_LOG_VERSION: u32 = 1;

/// The suffix of a drop log's name, reserved: no queue a layout or a
/// configuration declares may end in it.
pub const DROPPED_SUFFIX: &str = "-dropped";

/// The drop log of `queue`: `<queue>-dropped`, the companion log every drop on
/// it is recorded in.
#[must_use]
pub fn drop_log(queue: &QueueName) -> QueueName {
    format!("{queue}{DROPPED_SUFFIX}")
        .parse()
        .unwrap_or_else(|_| queue.clone())
}

/// The queue whose drop log `name` is, when it is one.
#[must_use]
pub fn dropped_from(name: &QueueName) -> Option<QueueName> {
    name.as_str()
        .strip_suffix(DROPPED_SUFFIX)
        .and_then(|base| base.parse().ok())
}

/// One record as one line of JSON: the exact bytes, and the value parsed from
/// them.
///
/// It is how a message travels through a chain and into an append without
/// losing the spelling a hook gave it, while preparation and schema checks read
/// its value.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRecord {
    bytes: String,
    value: Value,
}

/// Why bytes are not one record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RawRecordError {
    /// Not UTF-8 text.
    #[error("it is not UTF-8 text")]
    NotUtf8,
    /// Nothing, or only whitespace.
    #[error("it is empty or only whitespace, which is no JSON value")]
    Empty,
    /// More than one line.
    #[error("a queue record is one line, and it is {lines} lines")]
    MultiLine {
        /// How many lines it is.
        lines: usize,
    },
    /// Not exactly one JSON value.
    #[error("it is not exactly one JSON value: {why}")]
    NotJson {
        /// What reading it said.
        why: String,
    },
}

impl RawRecord {
    /// The record `bytes` spell: exactly one JSON value on one line, its one
    /// trailing newline optional and not part of the record.
    ///
    /// # Errors
    ///
    /// [`RawRecordError`] for bytes that are not UTF-8, are empty or only
    /// whitespace, span more than one line, or are not exactly one JSON value.
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self, RawRecordError> {
        let bytes = bytes.as_ref();
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let text = std::str::from_utf8(bytes).map_err(|_| RawRecordError::NotUtf8)?;
        if text.trim().is_empty() {
            return Err(RawRecordError::Empty);
        }
        if text.contains('\n') {
            return Err(RawRecordError::MultiLine {
                lines: text.split('\n').count(),
            });
        }
        let value =
            serde_json::from_str::<Value>(text).map_err(|failure| RawRecordError::NotJson {
                why: failure.to_string(),
            })?;
        Ok(Self {
            bytes: text.to_owned(),
            value,
        })
    }

    /// The record `value` is, in its compact serialization.
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        Self {
            bytes: serde_json::to_string(value).unwrap_or_default(),
            value: value.clone(),
        }
    }

    /// The exact bytes, without a trailing newline.
    #[must_use]
    pub fn bytes(&self) -> &str {
        &self.bytes
    }

    /// The value parsed from them.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// The value, owned.
    #[must_use]
    pub fn into_value(self) -> Value {
        self.value
    }
}

/// Written as its bytes: the spelling it arrived in, verbatim.
impl Serialize for RawRecord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match RawValue::from_string(self.bytes.clone()) {
            Ok(raw) => raw.serialize(serializer),
            Err(_) => self.value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RawRecord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(|value| Self::from_value(&value))
    }
}

impl JsonSchema for RawRecord {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("RawRecord")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // No `type`: a record is whatever JSON value its queue keeps, and the
        // queue's own schema, where it declares one, is what constrains it.
        schemars::json_schema!({
            "description": "Any JSON value, as one line of JSON: a record carries whatever its queue keeps, so it has no payload schema of its own, and the queue's schema, where it declares one, is what constrains it. Its bytes are written as they arrived."
        })
    }
}

/// Any record, as the bus runs every chain over it.
impl Message for RawRecord {
    const SCHEMA: SchemaId = SchemaId::literal("onemessagebus", "raw-record", 1);
}

/// What a hook is told about the offer it runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HookContext {
    /// The queue the message is offered to.
    pub queue: QueueName,
    /// The correlation of the question the message asks or answers, when it
    /// asks or answers one.
    pub correlation: Option<Correlation>,
}

impl HookContext {
    /// The context of a message offered to `queue`.
    #[must_use]
    pub fn new(queue: QueueName) -> Self {
        Self {
            queue,
            correlation: None,
        }
    }

    /// The same context, for a message asking or answering the question
    /// `correlation` names.
    #[must_use]
    pub fn with_correlation(mut self, correlation: Correlation) -> Self {
        self.correlation = Some(correlation);
        self
    }
}

/// What one hook made of one message.
#[derive(Debug, Clone, PartialEq)]
pub enum HookOutcome<M> {
    /// The message as it was.
    Keep,
    /// This message instead.
    Change(M),
    /// This message, on another queue.
    Reroute {
        /// The queue it goes to.
        queue: QueueName,
        /// The message, kept or changed.
        message: M,
    },
    /// Nothing: the message is recorded in the queue's drop log and appended
    /// nowhere.
    Drop,
    /// The message is refused, and nothing is appended anywhere.
    Refuse {
        /// Why, in the hook's own words.
        reason: String,
    },
    /// No outcome could be decided, so nothing is appended either.
    Unjudged {
        /// Why.
        reason: String,
    },
}

/// One step a queue runs a message through before anything is appended.
pub trait Hook<M: Message>: Send + Sync {
    /// What `message`, offered in `context`, becomes.
    fn run(&self, message: &M, context: &HookContext) -> HookOutcome<M>;
}

/// An ordered chain of hooks, each one's output the next one's input.
pub struct Hooks<M> {
    each: Vec<Arc<dyn Hook<M>>>,
}

impl<M> Clone for Hooks<M> {
    fn clone(&self) -> Self {
        Self {
            each: self.each.clone(),
        }
    }
}

impl<M> Default for Hooks<M> {
    fn default() -> Self {
        Self { each: Vec::new() }
    }
}

impl<M> fmt::Debug for Hooks<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hooks")
            .field("count", &self.each.len())
            .finish()
    }
}

impl<M: Message> Hooks<M> {
    /// No hooks: every message is kept.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same chain with `hook` run after every one before it.
    #[must_use]
    pub fn with(mut self, hook: impl Hook<M> + 'static) -> Self {
        self.each.push(Arc::new(hook));
        self
    }

    /// Run `hook` after every one before it.
    pub fn push(&mut self, hook: Arc<dyn Hook<M>>) {
        self.each.push(hook);
    }

    /// How many hooks the chain holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.each.len()
    }

    /// Whether the chain holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.each.is_empty()
    }

    /// Run `message` through every hook, in order: each one's change is the
    /// next one's input, and a drop, refusal, unjudged answer or reroute ends
    /// the chain. With none of those, the answer is [`HookOutcome::Change`] with
    /// the last change when any hook changed it, and [`HookOutcome::Keep`]
    /// otherwise.
    pub fn run(&self, message: &M, context: &HookContext) -> HookOutcome<M> {
        let mut changed: Option<M> = None;
        for hook in &self.each {
            let current = changed.as_ref().unwrap_or(message);
            match hook.run(current, context) {
                HookOutcome::Keep => {}
                HookOutcome::Change(next) => changed = Some(next),
                ended => return ended,
            }
        }
        changed.map_or(HookOutcome::Keep, HookOutcome::Change)
    }
}

/// Which hook of a queue's chain did something: its position in the chain,
/// and its command when it is one a configuration declares.
///
/// On the wire `{"index": 0, "command": ["<argv>"]}`, `command` absent for a
/// hook registered in code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HookRef {
    /// Its position in the chain of the queue it ran on.
    pub index: usize,
    /// Its argv, for a hook a configuration declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
}

impl fmt::Display for HookRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.command {
            Some(command) => write!(f, "hook {} (`{}`)", self.index, command.join(" ")),
            None => write!(f, "hook {} (registered in code)", self.index),
        }
    }
}

/// One drop, as its queue's drop log records it.
///
/// On the wire one line, `{"schema_version": 1, "at": <epoch ms>, "queue":
/// "<queue>", "hook": {"index": 0, "command": ["<argv>"]}, "message": <the
/// message as the dropping hook received it>, "correlation": "<c>"}`, the
/// message's bytes exactly as that hook received them.
#[derive(Debug, Clone, PartialEq)]
pub struct Dropped {
    /// When, in milliseconds since the Unix epoch.
    pub at: u64,
    /// The queue the dropping hook ran on.
    pub queue: QueueName,
    /// The hook that dropped it.
    pub hook: HookRef,
    /// The message as the dropping hook received it.
    pub message: RawRecord,
    /// The correlation of the question the message asked or answered.
    pub correlation: Option<Correlation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DroppedLine {
    schema_version: u32,
    at: u64,
    queue: QueueName,
    hook: HookRef,
    message: Box<RawValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    correlation: Option<Correlation>,
}

impl Dropped {
    /// The line its drop log holds for it.
    #[must_use]
    pub fn line(&self) -> String {
        let message = RawValue::from_string(self.message.bytes().to_owned()).unwrap_or_else(|_| {
            RawValue::from_string(serde_json::to_string(self.message.value()).unwrap_or_default())
                .unwrap_or_else(|_| RawValue::from_string("null".to_owned()).expect("null is JSON"))
        });
        serde_json::to_string(&DroppedLine {
            schema_version: DROP_LOG_VERSION,
            at: self.at,
            queue: self.queue.clone(),
            hook: self.hook.clone(),
            message,
            correlation: self.correlation.clone(),
        })
        .unwrap_or_default()
    }

    /// The drop one line of a drop log records.
    ///
    /// # Errors
    ///
    /// Why the line is not a drop of this version.
    pub fn parse(line: &[u8]) -> Result<Self, String> {
        let line: DroppedLine =
            serde_json::from_slice(line).map_err(|failure| failure.to_string())?;
        if line.schema_version != DROP_LOG_VERSION {
            return Err(format!(
                "schema_version {} is not the drop log version this build reads ({DROP_LOG_VERSION})",
                line.schema_version
            ));
        }
        Ok(Self {
            at: line.at,
            queue: line.queue,
            hook: line.hook,
            message: RawRecord::parse(line.message.get()).map_err(|failure| failure.to_string())?,
            correlation: line.correlation,
        })
    }
}

/// Now, in milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Why a hook could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookError {
    /// A command hook naming no command.
    #[error("a command hook names no command; give its argv, the program first")]
    NoCommand,
    /// A hook cache naming no command to read the bar's fingerprint with.
    #[error("a hook cache names no bar fingerprint command; give the argv whose output fingerprints the bar, so a moved bar invalidates what was recorded under the old one")]
    NoBar,
}

/// The external hook: a command reading the message on its stdin and
/// answering with what it prints.
///
/// Exit 0 with nothing on stdout is a drop; exit 0 printing the input line
/// back, byte for byte with or without its newline, is a keep; exit 0 printing
/// anything else is a change, which must be exactly one JSON value on one line.
/// The route file [`HOOK_ROUTE_ENV`] names — set only for a hook declaring
/// routes — naming one of them reroutes the message there. Exit 1 is a
/// refusal whose reason is its stderr exactly as written; anything else is
/// unjudged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandHook {
    command: Vec<String>,
    cache: Option<HookCache>,
    routes: Option<Vec<QueueName>>,
}

/// Distinct route files for every run in this process.
static ROUTE_FILES: AtomicU64 = AtomicU64::new(0);

/// The empty file a run's reroute is written to, removed when it goes.
struct RouteFile(PathBuf);

impl RouteFile {
    fn create() -> std::io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let path = std::env::temp_dir().join(format!(
            "onemessagebus-hook-route-{}-{}-{nanos}",
            std::process::id(),
            ROUTE_FILES.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self(path))
    }

    /// The queue name the hook wrote, trimmed: empty when it wrote none.
    fn read(&self) -> std::io::Result<String> {
        std::fs::read_to_string(&self.0).map(|named| named.trim().to_owned())
    }
}

impl Drop for RouteFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl CommandHook {
    /// The hook running `command`, the program first.
    ///
    /// # Errors
    ///
    /// [`HookError::NoCommand`] for an empty argv.
    pub fn new(command: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, HookError> {
        let command: Vec<String> = command.into_iter().map(Into::into).collect();
        if command.first().is_none_or(|program| program.is_empty()) {
            return Err(HookError::NoCommand);
        }
        Ok(Self {
            command,
            cache: None,
            routes: None,
        })
    }

    /// The same hook, recording its keeps, changes and drops in `cache`.
    #[must_use]
    pub fn with_cache(mut self, cache: HookCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// The same hook, allowed to reroute a message to each of `routes`: the
    /// route file is offered to it, and a queue it names outside them is
    /// unjudged.
    #[must_use]
    pub fn with_routes(mut self, routes: impl IntoIterator<Item = QueueName>) -> Self {
        self.routes = Some(routes.into_iter().collect());
        self
    }

    /// The command, the program first.
    #[must_use]
    pub fn command(&self) -> &[String] {
        &self.command
    }

    /// The cache its outcomes are recorded in.
    #[must_use]
    pub fn cache(&self) -> Option<&HookCache> {
        self.cache.as_ref()
    }

    /// The queues it may reroute to, when it declares any.
    #[must_use]
    pub fn routes(&self) -> Option<&[QueueName]> {
        self.routes.as_deref()
    }

    /// What the message `content` becomes: from the cache when a keep, change
    /// or drop of this content under this bar is recorded, and otherwise by
    /// running the command — recording what it answered when it was a keep, a
    /// change or a drop, and never a refusal, an unjudged answer or a reroute.
    #[must_use]
    pub fn run(&self, content: &[u8], context: &HookContext) -> HookOutcome<RawRecord> {
        let key = self
            .cache
            .as_ref()
            .and_then(|cache| cache.key(&self.command, content).map(|key| (cache, key)));
        if let Some((cache, key)) = &key {
            if let Some(outcome) = cache.holds(key) {
                return outcome;
            }
        }
        let outcome = self.execute(content, context);
        if let Some((cache, key)) = &key {
            // A record that cannot be written costs the next offer a run of the
            // command, never a different outcome.
            let _ = cache.record(key, &outcome);
        }
        outcome
    }

    fn rendered(&self) -> String {
        self.command.join(" ")
    }

    fn unjudged(&self, why: impl fmt::Display) -> HookOutcome<RawRecord> {
        HookOutcome::Unjudged {
            reason: format!("the hook `{}` {why}", self.rendered()),
        }
    }

    fn execute(&self, content: &[u8], context: &HookContext) -> HookOutcome<RawRecord> {
        let Some((program, arguments)) = self.command.split_first() else {
            return HookOutcome::Unjudged {
                reason: HookError::NoCommand.to_string(),
            };
        };
        let route = match self.routes {
            Some(_) => match RouteFile::create() {
                Ok(route) => Some(route),
                Err(failure) => {
                    return self
                        .unjudged(format_args!("could not be offered a route file: {failure}"))
                }
            },
            None => None,
        };
        let mut command = Command::new(program);
        command
            .args(arguments)
            .env(HOOK_QUEUE_ENV, context.queue.as_str())
            .env_remove(HOOK_CORRELATION_ENV)
            .env_remove(HOOK_ROUTE_ENV);
        if let Some(correlation) = &context.correlation {
            command.env(HOOK_CORRELATION_ENV, correlation.as_str());
        }
        if let Some(route) = &route {
            command.env(HOOK_ROUTE_ENV, &route.0);
        }
        let spawned = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(failure) => return self.unjudged(format_args!("could not be run: {failure}")),
        };
        let stdin = child.stdin.take();
        let mut offered = content.to_vec();
        offered.push(b'\n');
        // Written from a thread of its own, so a command that writes a long
        // answer before it reads its input cannot deadlock against this one. A
        // command that exits without reading its input is not a failure here:
        // its exit status and output are the answer.
        let writing = match std::thread::Builder::new()
            .name("onemessagebus-hook-stdin".to_owned())
            .spawn(move || {
                if let Some(mut stdin) = stdin {
                    let _ = stdin.write_all(&offered);
                }
            }) {
            Ok(writing) => writing,
            Err(failure) => {
                let _ = child.wait();
                return self.unjudged(format_args!("could not receive its message: {failure}"));
            }
        };
        let output = child.wait_with_output();
        let _ = writing.join();
        let output = match output {
            Ok(output) => output,
            Err(failure) => return self.unjudged(format_args!("did not finish: {failure}")),
        };
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        match output.status.code() {
            Some(0) => {}
            Some(1) if stderr.is_empty() => {
                return HookOutcome::Refuse {
                    reason: format!(
                        "the hook `{}` refused the message and wrote no reason on stderr",
                        self.rendered()
                    ),
                }
            }
            Some(1) => return HookOutcome::Refuse { reason: stderr },
            Some(code) => {
                return self.unjudged(format_args!(
                    "exited {code}, which is neither an answer (0) nor a refusal (1){}",
                    said(&stderr)
                ))
            }
            None => {
                return self.unjudged(format_args!(
                    "was ended by a signal before it answered{}",
                    said(&stderr)
                ))
            }
        }
        let stdout = output.stdout;
        if stdout.is_empty() {
            // A route file beside a drop is ignored.
            return HookOutcome::Drop;
        }
        let kept = stdout == content
            || stdout
                .strip_suffix(b"\n")
                .is_some_and(|line| line == content);
        let message = if kept {
            None
        } else {
            match RawRecord::parse(&stdout) {
                Ok(changed) => Some(changed),
                Err(failure) => {
                    return self.unjudged(format_args!(
                        "printed what is not a change of the message: {failure}"
                    ))
                }
            }
        };
        let named = match route.as_ref().map(RouteFile::read).transpose() {
            Ok(named) => named.unwrap_or_default(),
            Err(failure) => {
                return self.unjudged(format_args!(
                    "left a route file that cannot be read as a queue name: {failure}"
                ))
            }
        };
        if named.is_empty() {
            return message.map_or(HookOutcome::Keep, HookOutcome::Change);
        }
        let routes = self.routes.as_deref().unwrap_or_default();
        let Some(queue) = routes.iter().find(|queue| queue.as_str() == named) else {
            return self.unjudged(format_args!(
                "named `{named}` in its route file, which is not one of its routes ({})",
                routes
                    .iter()
                    .map(QueueName::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let message = match message {
            Some(message) => message,
            None => match RawRecord::parse(content) {
                Ok(kept) => kept,
                Err(failure) => {
                    return self.unjudged(format_args!(
                        "rerouted a message that is not one record: {failure}"
                    ))
                }
            },
        };
        HookOutcome::Reroute {
            queue: queue.clone(),
            message,
        }
    }
}

/// What a command wrote on stderr, as a clause to end a sentence with.
fn said(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("; it said: {trimmed}")
    }
}

impl Hook<RawRecord> for CommandHook {
    fn run(&self, message: &RawRecord, context: &HookContext) -> HookOutcome<RawRecord> {
        CommandHook::run(self, message.bytes().as_bytes(), context)
    }
}

/// Where a command hook records its keeps, changes and drops.
///
/// A record is keyed on the digest of the message's content and on the bar —
/// the hook's command and the fingerprint `bar_fingerprint` prints when it is
/// run. A message offered again under the
/// same bar takes its recorded outcome without running the command; a moved
/// bar prints another fingerprint, so every record made under the old one
/// misses. A fingerprint that cannot be read keys nothing. Only a hook whose
/// outcome depends on nothing but its input and the fingerprinted bar may
/// declare one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCache {
    dir: PathBuf,
    bar_fingerprint: Vec<String>,
}

/// One outcome's key: the content's digest and the bar's.
struct HookKey {
    content: String,
    bar: String,
    fingerprint: String,
    command: Vec<String>,
}

/// What a hook record is keyed by.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordKey {
    schema_version: u32,
    content: String,
    bar: String,
    fingerprint: String,
    command: Vec<String>,
}

/// A recorded change: its key, and the output the hook printed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeRecord {
    schema_version: u32,
    content: String,
    bar: String,
    fingerprint: String,
    command: Vec<String>,
    output: String,
}

/// The document one outcome is recorded as, `<dir>/<content>.<bar>.pass.json`:
/// a change, and only a change, carries its output.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
enum HookRecord {
    Keep(RecordKey),
    Change(ChangeRecord),
    Drop(RecordKey),
}

impl HookRecord {
    /// Whether this is a record of this version under exactly `key`.
    fn keyed(&self, key: &HookKey) -> bool {
        let (version, content, bar, fingerprint, command) = match self {
            Self::Keep(record) | Self::Drop(record) => (
                record.schema_version,
                &record.content,
                &record.bar,
                &record.fingerprint,
                &record.command,
            ),
            Self::Change(record) => (
                record.schema_version,
                &record.content,
                &record.bar,
                &record.fingerprint,
                &record.command,
            ),
        };
        version == HOOK_RECORD_VERSION
            && content == &key.content
            && bar == &key.bar
            && fingerprint == &key.fingerprint
            && command == &key.command
    }
}

/// A pass a validator recorded before validators were removed,
/// `<dir>/<content>.<bar>.pass.json` at [`PASS_RECORD_VERSION`]: read as a keep.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PassRecord {
    schema_version: u32,
    content: String,
    bar: String,
    fingerprint: String,
    command: Vec<String>,
}

impl PassRecord {
    /// Whether this is a pass of this version under exactly `key`.
    fn keyed(&self, key: &HookKey) -> bool {
        self.schema_version == PASS_RECORD_VERSION
            && self.content == key.content
            && self.bar == key.bar
            && self.fingerprint == key.fingerprint
            && self.command == key.command
    }
}

/// The content half of a cache key: the SHA-256 of the message's bytes.
fn content_digest(content: &[u8]) -> String {
    hex(&Sha256::digest(content))
}

/// The bar half of a cache key: the SHA-256 of the command's argv and the
/// fingerprint its bar printed.
fn bar_digest(command: &[String], fingerprint: &str) -> String {
    let mut bar = Sha256::new();
    for word in command {
        bar.update(word.as_bytes());
        bar.update([0]);
    }
    bar.update([0xff]);
    bar.update(fingerprint.as_bytes());
    hex(&bar.finalize())
}

/// What `argv` prints as a bar's fingerprint: its stdout with surrounding
/// whitespace removed, when it exits 0 having printed something.
fn read_fingerprint(argv: &[String]) -> Option<String> {
    let (program, arguments) = argv.split_first()?;
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!printed.is_empty()).then_some(printed)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

impl HookCache {
    /// A cache in `dir`, keyed on the fingerprint `bar_fingerprint` prints.
    ///
    /// # Errors
    ///
    /// [`HookError::NoBar`] for an empty argv.
    pub fn new(
        dir: impl Into<PathBuf>,
        bar_fingerprint: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, HookError> {
        let bar_fingerprint: Vec<String> = bar_fingerprint.into_iter().map(Into::into).collect();
        if bar_fingerprint
            .first()
            .is_none_or(|program| program.is_empty())
        {
            return Err(HookError::NoBar);
        }
        Ok(Self {
            dir: dir.into(),
            bar_fingerprint,
        })
    }

    /// The directory records are kept in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The command whose output fingerprints the bar.
    #[must_use]
    pub fn bar_fingerprint(&self) -> &[String] {
        &self.bar_fingerprint
    }

    /// The bar's fingerprint as its command prints it now: its stdout with
    /// surrounding whitespace removed, when it exits 0 having printed something.
    #[must_use]
    pub fn fingerprint(&self) -> Option<String> {
        read_fingerprint(&self.bar_fingerprint)
    }

    fn key(&self, command: &[String], content: &[u8]) -> Option<HookKey> {
        let fingerprint = self.fingerprint()?;
        Some(HookKey {
            content: content_digest(content),
            bar: bar_digest(command, &fingerprint),
            fingerprint,
            command: command.to_vec(),
        })
    }

    fn path(&self, key: &HookKey) -> PathBuf {
        self.dir
            .join(format!("{}.{}.pass.json", key.content, key.bar))
    }

    /// The outcome recorded under `key`: a hook's record of either version
    /// this build writes, or a pass the removed validators recorded, read as
    /// a keep.
    fn holds(&self, key: &HookKey) -> Option<HookOutcome<RawRecord>> {
        let bytes = std::fs::read(self.path(key)).ok()?;
        let document: Value = serde_json::from_slice(&bytes).ok()?;
        let version = document.get("schema_version").and_then(Value::as_u64)?;
        if version == u64::from(PASS_RECORD_VERSION) {
            let pass: PassRecord = serde_json::from_value(document).ok()?;
            return pass.keyed(key).then_some(HookOutcome::Keep);
        }
        let record: HookRecord = serde_json::from_value(document).ok()?;
        if !record.keyed(key) {
            return None;
        }
        match record {
            HookRecord::Keep(_) => Some(HookOutcome::Keep),
            HookRecord::Drop(_) => Some(HookOutcome::Drop),
            HookRecord::Change(change) => RawRecord::parse(change.output)
                .ok()
                .map(HookOutcome::Change),
        }
    }

    fn record(&self, key: &HookKey, outcome: &HookOutcome<RawRecord>) -> std::io::Result<()> {
        let keyed = || RecordKey {
            schema_version: HOOK_RECORD_VERSION,
            content: key.content.clone(),
            bar: key.bar.clone(),
            fingerprint: key.fingerprint.clone(),
            command: key.command.clone(),
        };
        let record = match outcome {
            HookOutcome::Keep => HookRecord::Keep(keyed()),
            HookOutcome::Drop => HookRecord::Drop(keyed()),
            HookOutcome::Change(changed) => HookRecord::Change(ChangeRecord {
                schema_version: HOOK_RECORD_VERSION,
                content: key.content.clone(),
                bar: key.bar.clone(),
                fingerprint: key.fingerprint.clone(),
                command: key.command.clone(),
                output: changed.bytes().to_owned(),
            }),
            // A refusal, an unjudged answer and a reroute are never recorded:
            // the next offer runs the command again, and a destination is
            // always decided by the routes and the layout as they stand.
            HookOutcome::Reroute { .. }
            | HookOutcome::Refuse { .. }
            | HookOutcome::Unjudged { .. } => return Ok(()),
        };
        std::fs::create_dir_all(&self.dir)?;
        let bytes = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
        let path = self.path(key);
        // Written beside its name and moved into place, so a reader never
        // meets half a record and reads it as none.
        let staged = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&staged, bytes)?;
        std::fs::rename(&staged, &path)
    }
}

/// A hook of `M` run over records: what a Rust hook of a typed message is on
/// a queue a configuration declares. A record that does not read as an `M` is
/// refused, naming why; a hook of [`RawRecord`] is handed the record itself.
struct OnRaw<M, H> {
    hook: H,
    of: PhantomData<fn() -> M>,
}

impl<M: Message + 'static, H: Hook<M>> Hook<RawRecord> for OnRaw<M, H> {
    fn run(&self, message: &RawRecord, context: &HookContext) -> HookOutcome<RawRecord> {
        if let Some(raw) = (message as &dyn Any).downcast_ref::<M>() {
            return raw_outcome(self.hook.run(raw, context));
        }
        let typed = match serde_json::from_value::<M>(message.value().clone()) {
            Ok(typed) => typed,
            Err(failure) => {
                return HookOutcome::Refuse {
                    reason: format!("the message is not a {}: {failure}", M::SCHEMA),
                }
            }
        };
        raw_outcome(self.hook.run(&typed, context))
    }
}

/// A typed outcome as one over records: a change is the compact serialization
/// of what the hook answered, and a [`RawRecord`] is taken as it is.
fn raw_outcome<M: Message + 'static>(outcome: HookOutcome<M>) -> HookOutcome<RawRecord> {
    let record = |message: M| -> Result<RawRecord, HookOutcome<RawRecord>> {
        let boxed: Box<dyn Any> = Box::new(message);
        match boxed.downcast::<RawRecord>() {
            Ok(raw) => Ok(*raw),
            Err(boxed) => {
                let message = boxed.downcast::<M>().map_err(|_| HookOutcome::Unjudged {
                    reason: "the hook's answer is not the message type it was registered for"
                        .to_owned(),
                })?;
                serde_json::to_value(&*message)
                    .map(|value| RawRecord::from_value(&value))
                    .map_err(|failure| HookOutcome::Unjudged {
                        reason: format!(
                            "the hook's change could not be written as JSON: {failure}"
                        ),
                    })
            }
        }
    };
    match outcome {
        HookOutcome::Keep => HookOutcome::Keep,
        HookOutcome::Change(message) => {
            record(message).map_or_else(|ended| ended, HookOutcome::Change)
        }
        HookOutcome::Reroute { queue, message } => record(message).map_or_else(
            |ended| ended,
            |message| HookOutcome::Reroute { queue, message },
        ),
        HookOutcome::Drop => HookOutcome::Drop,
        HookOutcome::Refuse { reason } => HookOutcome::Refuse { reason },
        HookOutcome::Unjudged { reason } => HookOutcome::Unjudged { reason },
    }
}

/// A configured hook: run only on the messages its `when` admits, and keeping
/// every other.
struct Gated {
    when: Option<When>,
    hook: CommandHook,
}

impl Hook<RawRecord> for Gated {
    fn run(&self, message: &RawRecord, context: &HookContext) -> HookOutcome<RawRecord> {
        if self
            .when
            .as_ref()
            .is_none_or(|when| when.admits(message.value()))
        {
            Hook::<RawRecord>::run(&self.hook, message, context)
        } else {
            HookOutcome::Keep
        }
    }
}

/// One hook of a queue's chain, with what names it.
#[derive(Clone)]
struct Link {
    hook: Arc<dyn Hook<RawRecord>>,
    command: Option<Vec<String>>,
}

/// One queue's chain, as the bus runs it: the hooks a configuration declares
/// for it, in declaration order, then the ones a linking consumer registered.
#[derive(Clone, Default)]
pub(crate) struct Chain {
    links: Vec<Link>,
}

impl fmt::Debug for Chain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chain")
            .field("count", &self.links.len())
            .finish()
    }
}

/// What running a chain answered, and which hook ended it.
struct Ran {
    outcome: HookOutcome<RawRecord>,
    /// The hook that answered the outcome ending the chain, or the last that
    /// changed it.
    by: Option<HookRef>,
    /// The message as the hook that ended the chain received it.
    received: RawRecord,
}

impl Chain {
    /// A configured hook, run on the messages `when` admits.
    pub(crate) fn push_configured(&mut self, when: Option<When>, hook: CommandHook) {
        let command = hook.command().to_vec();
        self.links.push(Link {
            hook: Arc::new(Gated { when, hook }),
            command: Some(command),
        });
    }

    /// A hook a linking consumer registered in code.
    pub(crate) fn push_registered<M: Message + 'static>(&mut self, hook: impl Hook<M> + 'static) {
        self.links.push(Link {
            hook: Arc::new(OnRaw {
                hook,
                of: PhantomData,
            }),
            command: None,
        });
    }

    fn named(&self, index: usize) -> HookRef {
        HookRef {
            index,
            command: self.links.get(index).and_then(|link| link.command.clone()),
        }
    }

    fn run(&self, message: &RawRecord, context: &HookContext) -> Ran {
        let mut current = message.clone();
        let mut changed = None;
        for (index, link) in self.links.iter().enumerate() {
            match link.hook.run(&current, context) {
                HookOutcome::Keep => {}
                HookOutcome::Change(next) => {
                    current = next.clone();
                    changed = Some((index, next));
                }
                ended => {
                    return Ran {
                        outcome: ended,
                        by: Some(self.named(index)),
                        received: current,
                    }
                }
            }
        }
        match changed {
            Some((index, next)) => Ran {
                outcome: HookOutcome::Change(next),
                by: Some(self.named(index)),
                received: current,
            },
            None => Ran {
                outcome: HookOutcome::Keep,
                by: None,
                received: current,
            },
        }
    }
}

/// What an offer as a whole became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookedOutcome {
    /// Every record is what the layout made of the offer, as offered.
    Keep,
    /// A record was changed, rerouted, or dropped beside one that was not.
    Change,
    /// The whole offer was dropped: nothing is appended but the drop.
    Drop,
    /// A hook refused it: nothing is appended anywhere.
    Refuse {
        /// Why, in the refusing hook's own words.
        reason: String,
    },
    /// It could not be decided: nothing is appended anywhere.
    Unjudged {
        /// Why.
        reason: String,
    },
}

/// What an offer becomes once every hook has run: the records a send appends,
/// in order, and the drops it records.
// llmlint: ignore[invalid_states_unrepresentable] `Hooked { outcome, records, dropped }` with public fields is the agreed library surface (docs/hooks.md, "The library surface") that onepipeline's envelope reviewer builds against, so its shape is the contract owner's to change, not this type's. `Bus::hooked` is the one producer and never makes a contradictory value, and the one consumer that acts on a hand-made one, `Bus::append_hooked`, appends nothing for a refusal or an unjudged outcome whatever its lists hold, and checks every record and every drop's queue before it appends any.
#[derive(Debug, Clone, PartialEq)]
pub struct Hooked {
    /// The offer's outcome as a whole.
    pub outcome: HookedOutcome,
    /// Each record a send would append, with the queue it lands on; empty for
    /// a refusal and an unjudged offer.
    pub records: Vec<(QueueName, RawRecord)>,
    /// Each drop a send would record in its queue's drop log.
    pub dropped: Vec<Dropped>,
}

/// How a hook ended an offer it did not let through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// It refused the message.
    Refused,
    /// It could not decide.
    Unjudged,
}

/// What ended a decision before anything could be appended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Halt {
    /// A hook refused or could not decide.
    Hook {
        /// The queue it ran on.
        queue: QueueName,
        /// Which hook.
        hook: HookRef,
        /// Whether it refused or could not decide.
        ending: Ending,
        /// Why.
        reason: String,
    },
}

/// An offer decided: what [`Bus::hooked`] answers, and what ended it when
/// something did.
#[derive(Debug, Clone)]
pub(crate) struct Decision {
    pub(crate) hooked: Hooked,
    pub(crate) halt: Option<Halt>,
}

impl Decision {
    /// `Ok` for an offer that may be appended, and the refusal for one that
    /// may not.
    pub(crate) fn appendable(&self) -> Result<(), BusError> {
        match &self.halt {
            None => Ok(()),
            Some(Halt::Hook {
                queue,
                hook,
                ending: Ending::Refused,
                reason,
            }) => Err(BusError::HookRefused {
                queue: queue.clone(),
                hook: hook.clone(),
                reason: reason.clone(),
            }),
            Some(Halt::Hook {
                queue,
                hook,
                ending: Ending::Unjudged,
                reason,
            }) => Err(BusError::HookUnjudged {
                queue: queue.clone(),
                hook: hook.clone(),
                reason: reason.clone(),
            }),
        }
    }

    /// The drop recorded on `queue` itself, when the offer recorded one there.
    pub(crate) fn dropped_on(&self, queue: &QueueName) -> Option<&Dropped> {
        self.hooked
            .dropped
            .iter()
            .find(|dropped| &dropped.queue == queue)
    }

    /// The drop that ended an offer dropped whole: the first it recorded.
    pub(crate) fn whole_drop(&self) -> Option<&Dropped> {
        (self.hooked.outcome == HookedOutcome::Drop)
            .then(|| self.hooked.dropped.first())
            .flatten()
    }
}

/// The working state of one decision.
struct Settling<'a> {
    bus: &'a Bus,
    correlation: Option<&'a Correlation>,
    /// The queue a reply's records are stamped with its correlation on.
    stamped_on: Option<&'a QueueName>,
    records: Vec<(QueueName, RawRecord)>,
    dropped: Vec<Dropped>,
    changed: bool,
}

impl Settling<'_> {
    fn context(&self, queue: &QueueName) -> HookContext {
        let context = HookContext::new(queue.clone());
        match self.correlation {
            Some(correlation) => context.with_correlation(correlation.clone()),
            None => context,
        }
    }

    fn prepared(
        &self,
        queue: &QueueName,
        value: Value,
    ) -> Result<Vec<(QueueName, Value)>, BusError> {
        let routed = self.bus.prepare(queue, value)?;
        Ok(match (self.correlation, self.stamped_on) {
            (Some(correlation), Some(on)) => routed
                .into_iter()
                .map(|(target, record)| {
                    if &target == on {
                        let stamped = crate::ask::stamped(record, correlation);
                        (target, stamped)
                    } else {
                        (target, record)
                    }
                })
                .collect(),
            _ => routed,
        })
    }

    fn drop_on(&mut self, queue: &QueueName, hook: Option<HookRef>, received: RawRecord) {
        self.dropped.push(Dropped {
            at: now_ms(),
            queue: queue.clone(),
            hook: hook.unwrap_or(HookRef {
                index: 0,
                command: None,
            }),
            message: received,
            correlation: self.correlation.cloned(),
        });
    }

    fn halted(queue: &QueueName, ran: Ran, ending: Ending, reason: String) -> Halt {
        Halt::Hook {
            queue: queue.clone(),
            hook: ran.by.unwrap_or(HookRef {
                index: 0,
                command: None,
            }),
            ending,
            reason,
        }
    }

    fn reroute_target(
        &self,
        from: &QueueName,
        ran: &Ran,
        to: &QueueName,
        rerouted: bool,
    ) -> Option<String> {
        let by = ran
            .by
            .as_ref()
            .map_or_else(|| "a hook".to_owned(), ToString::to_string);
        if rerouted {
            return Some(format!(
                "{by} on {from} rerouted a message already rerouted once; a message is rerouted at most once"
            ));
        }
        if self.bus.declared(to).is_err() {
            return Some(format!(
                "{by} on {from} rerouted the message to `{to}`, which is not a queue this configuration declares"
            ));
        }
        None
    }

    /// Run `message`, offered to `queue`, through the layout's preparation and
    /// the queue's chain, and each record the layout routes elsewhere through
    /// that queue's.
    fn settle(
        &mut self,
        queue: &QueueName,
        message: RawRecord,
        rerouted: bool,
        path: &[QueueName],
    ) -> Result<Result<(), Halt>, BusError> {
        let through = [path, std::slice::from_ref(queue)].concat();
        let prepared = self.prepared(queue, message.value().clone())?;
        let ran = match self.bus.chain(queue) {
            Some(chain) => chain.run(&message, &self.context(queue)),
            None => Ran {
                outcome: HookOutcome::Keep,
                by: None,
                received: message.clone(),
            },
        };
        let (last, prepared) = match ran.outcome.clone() {
            HookOutcome::Keep => (message, prepared),
            HookOutcome::Change(changed) => {
                self.changed = true;
                let prepared = self.prepared(queue, changed.value().clone())?;
                (changed, prepared)
            }
            HookOutcome::Reroute {
                queue: to,
                message: moved,
            } => {
                self.changed = true;
                if let Some(why) = self.reroute_target(queue, &ran, &to, rerouted) {
                    return Ok(Err(Self::halted(queue, ran, Ending::Unjudged, why)));
                }
                return self.settle(&to, moved, true, &through);
            }
            HookOutcome::Drop => {
                self.drop_on(queue, ran.by, ran.received);
                return Ok(Ok(()));
            }
            HookOutcome::Refuse { reason } => {
                return Ok(Err(Self::halted(queue, ran, Ending::Refused, reason)))
            }
            HookOutcome::Unjudged { reason } => {
                return Ok(Err(Self::halted(queue, ran, Ending::Unjudged, reason)))
            }
        };
        for (target, value) in prepared {
            // Where no step rewrote the record, the bytes are the ones the
            // message arrived in; where one did, it is written from its value.
            let record = if &value == last.value() {
                last.clone()
            } else {
                RawRecord::from_value(&value)
            };
            if &target == queue {
                self.records.push((target, record));
                continue;
            }
            if let Err(halt) = self.routed(&target, record, rerouted, &through)? {
                return Ok(Err(halt));
            }
        }
        Ok(Ok(()))
    }

    /// Run a record the layout routed to `queue` through that queue's chain,
    /// and what a change there routes on through each destination's. `path` is
    /// the queues the record has already passed through.
    fn routed(
        &mut self,
        queue: &QueueName,
        record: RawRecord,
        rerouted: bool,
        path: &[QueueName],
    ) -> Result<Result<(), Halt>, BusError> {
        let through = [path, std::slice::from_ref(queue)].concat();
        let Some(chain) = self.bus.chain(queue) else {
            self.records.push((queue.clone(), record));
            return Ok(Ok(()));
        };
        let ran = chain.run(&record, &self.context(queue));
        match ran.outcome.clone() {
            HookOutcome::Keep => self.records.push((queue.clone(), record)),
            HookOutcome::Change(changed) => {
                self.changed = true;
                // What the hook made is prepared again for the queue it is on,
                // so that queue's grants, checks and schema apply to it, and
                // what that routes elsewhere runs that queue's hooks. Each step
                // adds a queue to the path, so a layout cannot loop for ever.
                for (target, value) in self.prepared(queue, changed.value().clone())? {
                    let record = if &value == changed.value() {
                        changed.clone()
                    } else {
                        RawRecord::from_value(&value)
                    };
                    if &target == queue {
                        self.records.push((target, record));
                        continue;
                    }
                    if through.contains(&target) {
                        let why = format!(
                            "its change is routed by the layout back to `{target}`, which this record already passed through"
                        );
                        return Ok(Err(Self::halted(queue, ran, Ending::Unjudged, why)));
                    }
                    if let Err(halt) = self.routed(&target, record, rerouted, &through)? {
                        return Ok(Err(halt));
                    }
                }
            }
            HookOutcome::Reroute { queue: to, message } => {
                self.changed = true;
                if let Some(why) = self.reroute_target(queue, &ran, &to, rerouted) {
                    return Ok(Err(Self::halted(queue, ran, Ending::Unjudged, why)));
                }
                return self.settle(&to, message, true, &through);
            }
            HookOutcome::Drop => {
                self.changed = true;
                self.drop_on(queue, ran.by, ran.received);
            }
            HookOutcome::Refuse { reason } => {
                return Ok(Err(Self::halted(queue, ran, Ending::Refused, reason)))
            }
            HookOutcome::Unjudged { reason } => {
                return Ok(Err(Self::halted(queue, ran, Ending::Unjudged, reason)))
            }
        }
        Ok(Ok(()))
    }
}

impl Bus {
    /// The same bus, running every message offered to `queue` through `hook`
    /// as well, after every hook the configuration declares for it and every
    /// one registered before it. A Rust hook is code, so it is registered here
    /// rather than in the file; a record that does not read as an `M` is
    /// refused naming why.
    ///
    /// # Errors
    ///
    /// [`BusError::UnknownQueue`].
    pub fn with_hook<M: Message + 'static>(
        mut self,
        queue: &QueueName,
        hook: impl Hook<M> + 'static,
    ) -> Result<Self, BusError> {
        self.declared(queue)?;
        self.chain_mut(queue).push_registered(hook);
        Ok(self)
    }

    /// What offering `record` to `queue` would become, appending nothing but
    /// the records a hook cache writes: the layout's preparation, then the
    /// queue's hooks, then each routed record's queue's hooks.
    ///
    /// # Errors
    ///
    /// As [`prepare`](Self::prepare): an unknown queue, or the layout's refusal
    /// of the offer or of what a hook made of it; and a record its queue's
    /// schema refuses, as a send would refuse it.
    pub fn hooked(&self, queue: &QueueName, record: RawRecord) -> Result<Hooked, BusError> {
        Ok(self.decide(queue, record, None, None)?.hooked)
    }

    /// Decide an offer: every hook run, nothing appended.
    pub(crate) fn decide(
        &self,
        queue: &QueueName,
        record: RawRecord,
        correlation: Option<&Correlation>,
        stamped_on: Option<&QueueName>,
    ) -> Result<Decision, BusError> {
        let mut settling = Settling {
            bus: self,
            correlation,
            stamped_on,
            records: Vec::new(),
            dropped: Vec::new(),
            changed: false,
        };
        let refused = |halt: Halt| {
            let outcome = match &halt {
                Halt::Hook {
                    ending: Ending::Refused,
                    reason,
                    ..
                } => HookedOutcome::Refuse {
                    reason: reason.clone(),
                },
                Halt::Hook { reason, .. } => HookedOutcome::Unjudged {
                    reason: reason.clone(),
                },
            };
            Decision {
                hooked: Hooked {
                    outcome,
                    records: Vec::new(),
                    dropped: Vec::new(),
                },
                halt: Some(halt),
            }
        };
        if let Err(halt) = settling.settle(queue, record, false, &[])? {
            return Ok(refused(halt));
        }
        // Every record is checked against its queue's schema as a send checks
        // it, so a dry run never answers a record its send would refuse.
        self.admit_all(&settling.records)?;
        let outcome = if settling.records.is_empty() && !settling.dropped.is_empty() {
            HookedOutcome::Drop
        } else if settling.changed {
            HookedOutcome::Change
        } else {
            HookedOutcome::Keep
        };
        Ok(Decision {
            hooked: Hooked {
                outcome,
                records: settling.records,
                dropped: settling.dropped,
            },
            halt: None,
        })
    }

    /// Append what an offer became: each record's bytes verbatim — an event
    /// queue's line framing and an id the queue numbers being the queue's own
    /// fields, added as a send adds them — then each drop to its queue's drop
    /// log. Every record is checked against its queue's schema before any is
    /// appended. An offer refused or unjudged appends nothing.
    ///
    /// # Errors
    ///
    /// [`BusError::UnknownQueue`] for a record or a drop naming a queue this bus
    /// does not declare, or a record its queue refuses — nothing is appended
    /// then — or the first append that failed, when what was appended before it
    /// stays appended.
    pub fn append_hooked(
        &self,
        hooked: &Hooked,
    ) -> Result<Vec<(QueueName, Pushed<Value>)>, BusError> {
        if matches!(
            hooked.outcome,
            HookedOutcome::Refuse { .. } | HookedOutcome::Unjudged { .. }
        ) {
            return Ok(Vec::new());
        }
        for (target, _) in &hooked.records {
            self.declared(target)?;
        }
        self.admit_all(&hooked.records)?;
        for dropped in &hooked.dropped {
            self.declared(&dropped.queue)?;
        }
        let mut pushed = Vec::new();
        for (target, record) in &hooked.records {
            let landed = self.queue(target)?.push_raw(record)?;
            pushed.push((target.clone(), landed));
        }
        for dropped in &hooked.dropped {
            self.transport()
                .append(&drop_log(&dropped.queue), dropped.line().as_bytes())
                .map_err(QueueError::from)?;
        }
        Ok(pushed)
    }

    /// Check each of `records` against its queue's schema as its push will,
    /// with the id its queue will give it: the next id the queue gives, moved on
    /// past each earlier record of the same offer on the same queue. A writer
    /// appending between this check and the push is caught by the push's own.
    fn admit_all(&self, records: &[(QueueName, RawRecord)]) -> Result<(), BusError> {
        let mut next: BTreeMap<&QueueName, Option<u64>> = BTreeMap::new();
        for (target, record) in records {
            let queue = self.queue(target)?;
            if queue.spec().schema.is_none() {
                // Nothing to check a record against, so no id to look up.
                continue;
            }
            let id = match next.get(target) {
                Some(id) => *id,
                None => queue.next_id()?,
            };
            queue.admits(record.value(), id)?;
            next.insert(target, id.map(|id| id.saturating_add(1)));
        }
        Ok(())
    }

    /// Every drop `queue`'s drop log records, oldest first; a line this build
    /// cannot read is passed over.
    ///
    /// # Errors
    ///
    /// [`BusError::UnknownQueue`], or a transport failure.
    pub fn dropped(&self, queue: &QueueName) -> Result<Vec<Dropped>, BusError> {
        self.declared(queue)?;
        let batch = self
            .transport()
            .read(&drop_log(queue), None, usize::MAX)
            .map_err(QueueError::from)?;
        Ok(batch
            .records
            .iter()
            .filter_map(|stored| Dropped::parse(&stored.bytes).ok())
            .collect())
    }
}
