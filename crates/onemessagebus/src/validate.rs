//! Validators: what a queue judges a message by before anything is appended.
//!
//! A [`Validator`] answers one [`Verdict`] for one message: [`Verdict::Pass`],
//! [`Verdict::Refuse`] with the reason it is refused, or [`Verdict::Unjudged`]
//! when it could not be judged at all — and an unjudged message is never sent.
//! [`Validators`] is an ordered list of them: every one runs, the first refusal
//! is the verdict, and otherwise any unjudged one makes the whole verdict
//! unjudged.
//!
//! Deterministic validators are Rust types. [`CommandValidator`] is the external
//! one, the door a host's judged bar comes through: the message as JSON on its
//! stdin, exit 0 a pass, exit 1 a refusal with its stderr as the reason, and
//! anything else unjudged. A [`PassCache`] beside it records each pass under the
//! message's content digest and the bar's fingerprint, so an unchanged message
//! under an unchanged bar is not judged twice, and moving the bar invalidates
//! every record made under the one before. Only a pass is ever recorded.
//! `docs/validators.md` states the contract.

use std::fmt;
use std::io::Write as _;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::ask::Correlation;
use crate::queue::{FieldPath, Predicate};
use crate::schema::Message;
use crate::transport::QueueName;

/// The environment variable a command validator is told the queue in.
pub const VALIDATE_QUEUE_ENV: &str = "ONEMESSAGEBUS_VALIDATE_QUEUE";

/// The environment variable a command validator is told the correlation of the
/// question the message asks or answers in, when it asks or answers one.
pub const VALIDATE_CORRELATION_ENV: &str = "ONEMESSAGEBUS_VALIDATE_CORRELATION";

/// The version of the pass record a [`PassCache`] writes.
pub const PASS_RECORD_VERSION: u32 = 1;

/// What judging one message answered.
///
/// On the wire `{"verdict": "pass"}`, `{"verdict": "refuse", "reason": ...}` or
/// `{"verdict": "unjudged", "reason": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "verdict", rename_all = "lowercase")]
pub enum Verdict {
    /// The message may be sent.
    Pass,
    /// The message is refused.
    Refuse {
        /// Why, in the validator's own words.
        reason: String,
    },
    /// The message could not be judged, so it is not sent either.
    Unjudged {
        /// Why no judgement could be made.
        reason: String,
    },
}

impl Verdict {
    /// Whether this verdict lets the message be sent: a pass, and nothing else.
    #[must_use]
    pub fn passes(&self) -> bool {
        matches!(self, Self::Pass)
    }

    /// The reason a refusal or an unjudged verdict gives.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Pass => None,
            Self::Refuse { reason } | Self::Unjudged { reason } => Some(reason),
        }
    }
}

/// What a validator is told about the send it judges.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ValidationContext {
    /// The queue the message is offered to.
    pub queue: QueueName,
    /// The correlation of the question the message asks or answers, when it
    /// asks or answers one.
    pub correlation: Option<Correlation>,
}

impl ValidationContext {
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

/// One judgement a queue makes of a message before it is appended.
pub trait Validator<M: Message>: Send + Sync {
    /// Judge `message`, offered in `context`.
    fn validate(&self, message: &M, context: &ValidationContext) -> Verdict;
}

/// An ordered list of validators, judged together.
pub struct Validators<M> {
    each: Vec<Arc<dyn Validator<M>>>,
}

impl<M> Clone for Validators<M> {
    fn clone(&self) -> Self {
        Self {
            each: self.each.clone(),
        }
    }
}

impl<M> Default for Validators<M> {
    fn default() -> Self {
        Self { each: Vec::new() }
    }
}

impl<M> fmt::Debug for Validators<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Validators")
            .field("count", &self.each.len())
            .finish()
    }
}

impl<M: Message> Validators<M> {
    /// No validators: every message passes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same list with `validator` judged after every one before it.
    #[must_use]
    pub fn with(mut self, validator: impl Validator<M> + 'static) -> Self {
        self.each.push(Arc::new(validator));
        self
    }

    /// Add `validator` after every one before it.
    pub fn push(&mut self, validator: Arc<dyn Validator<M>>) {
        self.each.push(validator);
    }

    /// How many validators the list holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.each.len()
    }

    /// Whether the list holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.each.is_empty()
    }

    /// Judge `message` by every validator, in order.
    ///
    /// Every one runs, whatever an earlier one answered. The first refusal is
    /// the verdict; with none, the first unjudged one is; with neither, the
    /// message passes.
    pub fn judge(&self, message: &M, context: &ValidationContext) -> Verdict {
        combined(
            self.each
                .iter()
                .map(|validator| validator.validate(message, context))
                // Collected first, so every validator runs whatever an earlier
                // one answered.
                .collect::<Vec<_>>(),
        )
    }
}

/// Several verdicts as one: the first refusal, else the first unjudged, else a
/// pass.
pub(crate) fn combined(verdicts: impl IntoIterator<Item = Verdict>) -> Verdict {
    let mut refused = None;
    let mut unjudged = None;
    for verdict in verdicts {
        match verdict {
            Verdict::Pass => {}
            Verdict::Refuse { reason } => {
                refused.get_or_insert(reason);
            }
            Verdict::Unjudged { reason } => {
                unjudged.get_or_insert(reason);
            }
        }
    }
    match (refused, unjudged) {
        (Some(reason), _) => Verdict::Refuse { reason },
        (None, Some(reason)) => Verdict::Unjudged { reason },
        (None, None) => Verdict::Pass,
    }
}

/// A validator of `M`, judging JSON records: what a Rust validator of a typed
/// message is on a queue a configuration declares. A record that does not read
/// as an `M` is refused, naming why.
pub struct OnRecords<M, V> {
    validator: V,
    of: PhantomData<fn() -> M>,
}

impl<M: Message, V: Validator<M>> OnRecords<M, V> {
    /// `validator`, over JSON records.
    #[must_use]
    pub fn new(validator: V) -> Self {
        Self {
            validator,
            of: PhantomData,
        }
    }
}

impl<M: Message, V: Validator<M>> Validator<Value> for OnRecords<M, V> {
    fn validate(&self, message: &Value, context: &ValidationContext) -> Verdict {
        match serde_json::from_value::<M>(message.clone()) {
            Ok(typed) => self.validator.validate(&typed, context),
            Err(failure) => Verdict::Refuse {
                reason: format!("the message is not a {}: {failure}", M::SCHEMA),
            },
        }
    }
}

/// When a configured validator judges a message at all.
///
/// On the wire `{"carries": P}` — the field at `P` is there and holds something
/// (not `null`, `""`, `[]` or `{}`) — or any queue predicate (`docs/queues.md`).
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    /// The message carries something at this field.
    Carries(FieldPath),
    /// The message satisfies this predicate.
    Matches(Predicate),
}

impl When {
    /// Whether a message is judged.
    #[must_use]
    pub fn admits(&self, message: &Value) -> bool {
        match self {
            Self::Carries(field) => Predicate::NonEmpty {
                field: field.clone(),
                non_empty: true,
            }
            .matches(message),
            Self::Matches(predicate) => predicate.matches(message),
        }
    }
}

/// The key naming the `carries` form.
const CARRIES: &str = "carries";

impl Serialize for When {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Carries(field) => {
                let mut form = Map::new();
                form.insert(CARRIES.to_owned(), Value::String(field.to_string()));
                form.serialize(serializer)
            }
            Self::Matches(predicate) => predicate.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for When {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if let Some(form) = value.as_object().filter(|form| form.contains_key(CARRIES)) {
            if form.len() != 1 {
                let others: Vec<&str> = form
                    .keys()
                    .map(String::as_str)
                    .filter(|key| *key != CARRIES)
                    .collect();
                return Err(serde::de::Error::custom(format!(
                    "`carries` stands alone in a `when`, and this also names {}",
                    others.join(" and ")
                )));
            }
            let field = form
                .get(CARRIES)
                .and_then(Value::as_str)
                .ok_or_else(|| serde::de::Error::custom("`carries` names a field path"))?
                .parse::<FieldPath>()
                .map_err(serde::de::Error::custom)?;
            return Ok(Self::Carries(field));
        }
        Predicate::deserialize(value)
            .map(Self::Matches)
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for When {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("When")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let field = generator.subschema_for::<FieldPath>();
        let predicate = generator.subschema_for::<Predicate>();
        schemars::json_schema!({
            "description": "When a validator judges a message: `{carries: <field path>}`, or any queue predicate.",
            "anyOf": [
                {
                    "type": "object",
                    "properties": { "carries": field },
                    "required": ["carries"],
                    "additionalProperties": false
                },
                predicate
            ]
        })
    }
}

/// Why a validator could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidatorError {
    /// A command validator naming no command.
    #[error("a command validator names no command; give its argv, the program first")]
    NoCommand,
    /// A pass cache naming no command to read the bar's fingerprint with.
    #[error("a pass cache names no bar fingerprint command; give the argv whose output fingerprints the bar, so a moved bar invalidates what was passed under the old one")]
    NoBar,
}

/// The external validator: a command judging the message on its stdin.
///
/// Exit 0 is a pass; exit 1 is a refusal, whose reason is the command's stderr
/// exactly as it wrote it; any other exit, a command that cannot be run, and
/// one a signal ended are unjudged. Its stdout is discarded. The queue the
/// message is offered to is in [`VALIDATE_QUEUE_ENV`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandValidator {
    command: Vec<String>,
    cache: Option<PassCache>,
}

impl CommandValidator {
    /// The validator running `command`, the program first.
    ///
    /// # Errors
    ///
    /// [`ValidatorError::NoCommand`] for an empty argv.
    pub fn new(
        command: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, ValidatorError> {
        let command: Vec<String> = command.into_iter().map(Into::into).collect();
        if command.first().is_none_or(|program| program.is_empty()) {
            return Err(ValidatorError::NoCommand);
        }
        Ok(Self {
            command,
            cache: None,
        })
    }

    /// The same validator, recording its passes in `cache`.
    #[must_use]
    pub fn with_cache(mut self, cache: PassCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// The command, the program first.
    #[must_use]
    pub fn command(&self) -> &[String] {
        &self.command
    }

    /// The cache its passes are recorded in.
    #[must_use]
    pub fn cache(&self) -> Option<&PassCache> {
        self.cache.as_ref()
    }

    /// Judge the message `content`: from the cache when a pass of this content
    /// under this bar is recorded, and otherwise by running the command —
    /// recording what it answered only when it was a pass.
    #[must_use]
    pub fn judge(&self, content: &[u8], context: &ValidationContext) -> Verdict {
        let key = self
            .cache
            .as_ref()
            .and_then(|cache| cache.key(&self.command, content).map(|key| (cache, key)));
        if let Some((cache, key)) = &key {
            if cache.holds(key) {
                return Verdict::Pass;
            }
        }
        let verdict = self.run(content, context);
        if verdict.passes() {
            if let Some((cache, key)) = &key {
                // A record that cannot be written costs the next send a run of
                // the command, never a different verdict.
                let _ = cache.record(key);
            }
        }
        verdict
    }

    fn rendered(&self) -> String {
        self.command.join(" ")
    }

    fn run(&self, content: &[u8], context: &ValidationContext) -> Verdict {
        let Some((program, arguments)) = self.command.split_first() else {
            return Verdict::Unjudged {
                reason: ValidatorError::NoCommand.to_string(),
            };
        };
        let mut command = Command::new(program);
        command
            .args(arguments)
            .env(VALIDATE_QUEUE_ENV, context.queue.as_str())
            .env_remove(VALIDATE_CORRELATION_ENV);
        if let Some(correlation) = &context.correlation {
            command.env(VALIDATE_CORRELATION_ENV, correlation.as_str());
        }
        let spawned = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(failure) => {
                return Verdict::Unjudged {
                    reason: format!(
                        "the validator `{}` could not be run: {failure}",
                        self.rendered()
                    ),
                }
            }
        };
        let stdin = child.stdin.take();
        let mut offered = content.to_vec();
        offered.push(b'\n');
        // Written from a thread of its own, so a command that writes a long
        // refusal before it reads its input cannot deadlock against this one.
        // A command that exits without reading its input is not a failure here:
        // its exit status is the verdict.
        let writing = match std::thread::Builder::new()
            .name("onemessagebus-validator-stdin".to_owned())
            .spawn(move || {
                if let Some(mut stdin) = stdin {
                    let _ = stdin.write_all(&offered);
                }
            }) {
            Ok(writing) => writing,
            Err(failure) => {
                let _ = child.wait();
                return Verdict::Unjudged {
                    reason: format!(
                        "the validator `{}` could not receive its message: {failure}",
                        self.rendered()
                    ),
                };
            }
        };
        let output = child.wait_with_output();
        let _ = writing.join();
        let output = match output {
            Ok(output) => output,
            Err(failure) => {
                return Verdict::Unjudged {
                    reason: format!(
                        "the validator `{}` did not finish: {failure}",
                        self.rendered()
                    ),
                }
            }
        };
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        match output.status.code() {
            Some(0) => Verdict::Pass,
            Some(1) if stderr.trim().is_empty() => Verdict::Refuse {
                reason: format!(
                    "the validator `{}` refused the message and wrote no reason on stderr",
                    self.rendered()
                ),
            },
            Some(1) => Verdict::Refuse { reason: stderr },
            Some(code) => Verdict::Unjudged {
                reason: format!(
                    "the validator `{}` exited {code}, which is neither a pass (0) nor a refusal (1){}",
                    self.rendered(),
                    said(&stderr)
                ),
            },
            None => Verdict::Unjudged {
                reason: format!(
                    "the validator `{}` was ended by a signal before it answered{}",
                    self.rendered(),
                    said(&stderr)
                ),
            },
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

impl<M: Message> Validator<M> for CommandValidator {
    fn validate(&self, message: &M, context: &ValidationContext) -> Verdict {
        match serde_json::to_vec(message) {
            Ok(content) => self.judge(&content, context),
            Err(failure) => Verdict::Unjudged {
                reason: format!("the message could not be written as JSON: {failure}"),
            },
        }
    }
}

/// Where a command validator records its passes.
///
/// A record is keyed on the digest of the message's content and on the bar:
/// the validator's command and the fingerprint `bar_fingerprint` prints when
/// it is run. A message judged again under the same bar is passed from the
/// record without running the command; a moved bar prints another fingerprint,
/// so every record made under the old one misses. A fingerprint that cannot be
/// read — the command fails, or prints nothing — keys nothing: the message is
/// judged by the command and nothing is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassCache {
    dir: PathBuf,
    bar_fingerprint: Vec<String>,
}

/// One pass's key: the content's digest and the bar's.
struct PassKey {
    content: String,
    bar: String,
    fingerprint: String,
    command: Vec<String>,
}

/// The document one pass is recorded as, `<dir>/<content>.<bar>.pass.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PassRecord {
    schema_version: u32,
    content: String,
    bar: String,
    fingerprint: String,
    command: Vec<String>,
}

impl PassCache {
    /// A cache in `dir`, keyed on the fingerprint `bar_fingerprint` prints.
    ///
    /// # Errors
    ///
    /// [`ValidatorError::NoBar`] for an empty argv.
    pub fn new(
        dir: impl Into<PathBuf>,
        bar_fingerprint: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, ValidatorError> {
        let bar_fingerprint: Vec<String> = bar_fingerprint.into_iter().map(Into::into).collect();
        if bar_fingerprint
            .first()
            .is_none_or(|program| program.is_empty())
        {
            return Err(ValidatorError::NoBar);
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
        let (program, arguments) = self.bar_fingerprint.split_first()?;
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

    /// Whether a pass of `content` by `command` under the bar as it stands now
    /// is recorded.
    #[must_use]
    pub fn holds_pass(&self, command: &[String], content: &[u8]) -> bool {
        self.key(command, content)
            .is_some_and(|key| self.holds(&key))
    }

    fn key(&self, command: &[String], content: &[u8]) -> Option<PassKey> {
        let fingerprint = self.fingerprint()?;
        let mut bar = Sha256::new();
        for word in command {
            bar.update(word.as_bytes());
            bar.update([0]);
        }
        bar.update([0xff]);
        bar.update(fingerprint.as_bytes());
        Some(PassKey {
            content: hex(&Sha256::digest(content)),
            bar: hex(&bar.finalize()),
            fingerprint,
            command: command.to_vec(),
        })
    }

    fn path(&self, key: &PassKey) -> PathBuf {
        self.dir
            .join(format!("{}.{}.pass.json", key.content, key.bar))
    }

    fn holds(&self, key: &PassKey) -> bool {
        let Ok(bytes) = std::fs::read(self.path(key)) else {
            return false;
        };
        serde_json::from_slice::<PassRecord>(&bytes).is_ok_and(|record| {
            record.schema_version == PASS_RECORD_VERSION
                && record.content == key.content
                && record.bar == key.bar
                && record.fingerprint == key.fingerprint
                && record.command == key.command
        })
    }

    fn record(&self, key: &PassKey) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let record = PassRecord {
            schema_version: PASS_RECORD_VERSION,
            content: key.content.clone(),
            bar: key.bar.clone(),
            fingerprint: key.fingerprint.clone(),
            command: key.command.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
        let path = self.path(key);
        // Written beside its name and moved into place, so a reader never
        // meets half a record and reads it as none.
        let staged = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&staged, bytes)?;
        std::fs::rename(&staged, &path)
    }
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
