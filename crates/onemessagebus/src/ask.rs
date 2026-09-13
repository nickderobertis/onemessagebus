//! Asking a question on a queue, and the answers it can have.
//!
//! [`Bus::ask`] raises a question on an event queue — stamped with a
//! [`Correlation`] the bus mints, and with its blocking flag, its asker and what
//! it is about — and hands back a [`Pending`]: the handle its answer arrives on.
//! Only a reply record echoing that correlation answers it, and
//! [`Pending::wait`] answers an [`Answer`]: the reply, or a timeout, an abandoned
//! listener, or a refusal. **Nothing turns a timeout, a session bound or an
//! abandoned listener into a reply**: the type has no path from one to the other.
//!
//! [`Bus::reply`] is the other side: it binds a reply to the pending ask whose
//! correlation it echoes, refuses one echoing a correlation nothing pending
//! holds, and binds one echoing none to the queue's one pending ask, refusing it
//! when there is not exactly one. `docs/ask.md` states the contract.

use std::collections::BTreeSet;
use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering as Sequence};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::config::{Bus, BusError};
use crate::queue::{Asker, Claimed, Lifetime, Pushed, QueueError, RawQueue};
use crate::schema::{CheckError, Message, Registry};
use crate::transport::{Position, QueueName};
use crate::validate::Verdict;

/// The field a question and a reply carry their correlation in.
pub const CORRELATION: &str = "correlation";

/// The field a question carries what it is about in.
pub const ABOUT: &str = "about";

/// The longest correlation accepted, in bytes.
pub const CORRELATION_LIMIT: usize = 128;

/// The longest address accepted, in bytes.
pub const ADDRESS_LIMIT: usize = 512;

/// How long one slice of a wait lasts before the question's own state is read
/// again: an abandonment moves the question's queue, not the answer queue.
const WAIT_SLICE: Duration = Duration::from_millis(200);

/// Which question a reply answers: minted by the bus when a question is asked,
/// stamped on the question as its `correlation` field, and required on the
/// reply that answers it.
///
/// Opaque: compared for equality and never parsed for meaning. Text from
/// outside is refused unless it is 1 to [`CORRELATION_LIMIT`] ASCII letters,
/// digits, `.`, `_`, `:` and `-`, starting with a letter or a digit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Correlation(String);

/// Why text is not a [`Correlation`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not a correlation: {why}; a correlation is the token `ask` printed: 1 to {CORRELATION_LIMIT} ASCII letters, digits, `.`, `_`, `:` and `-`, starting with a letter or a digit")]
pub struct CorrelationError {
    /// What was offered.
    pub text: String,
    /// What is wrong with it.
    pub why: &'static str,
}

/// Mints counted from process start, so two mints in one process never share
/// the fallback's input.
static MINTED: AtomicU64 = AtomicU64::new(0);

impl Correlation {
    /// A correlation nothing else has: `c-` and 32 hex digits of the operating
    /// system's randomness — or, where the system refuses to give any, of a
    /// digest of the time, this process and a count of mints.
    #[must_use]
    pub fn mint() -> Self {
        let mut bytes = [0u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            let mut fallback = Sha256::new();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            fallback.update(nanos.to_le_bytes());
            fallback.update(std::process::id().to_le_bytes());
            fallback.update(MINTED.fetch_add(1, Sequence::SeqCst).to_le_bytes());
            fallback.update(format!("{:?}", std::thread::current().id()).as_bytes());
            bytes.copy_from_slice(&fallback.finalize()[..16]);
        }
        Self(format!("c-{}", hex(&bytes)))
    }

    /// The token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Correlation {
    type Err = CorrelationError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refuse = |why| CorrelationError {
            text: text.to_owned(),
            why,
        };
        let Some(first) = text.bytes().next() else {
            return Err(refuse("it is empty"));
        };
        if text.len() > CORRELATION_LIMIT {
            return Err(refuse("it is longer than a correlation can be"));
        }
        if !first.is_ascii_alphanumeric() {
            return Err(refuse("it does not start with a letter or a digit"));
        }
        if !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
        {
            return Err(refuse("it carries a character a correlation does not"));
        }
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for Correlation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Correlation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Correlation {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Correlation")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Which question a reply answers: the token the bus minted when it was asked.",
            "pattern": "^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$"
        })
    }
}

/// What a question is about — a node, a workstream, a file — as the consumer
/// names it. Compared for equality and never parsed; refused where it enters
/// when it is blank, carries a control character, or is longer than
/// [`ADDRESS_LIMIT`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Address(String);

/// Why text is not an [`Address`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not an address: {why}; an address names what a question is about, in one line of up to {ADDRESS_LIMIT} bytes")]
pub struct AddressError {
    /// What was offered.
    pub text: String,
    /// What is wrong with it.
    pub why: &'static str,
}

impl Address {
    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Address {
    type Err = AddressError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refuse = |why| AddressError {
            text: text.to_owned(),
            why,
        };
        if text.trim().is_empty() {
            return Err(refuse("it is blank"));
        }
        if text.len() > ADDRESS_LIMIT {
            return Err(refuse("it is longer than an address can be"));
        }
        if text.chars().any(char::is_control) {
            return Err(refuse("it carries a control character"));
        }
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Address {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Address")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "What a question is about, as the consumer names it: one non-blank line.",
            "minLength": 1,
            "maxLength": ADDRESS_LIMIT
        })
    }
}

/// How a question is asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AskOptions {
    /// Whether the asker waits on the answer: a blocking question is claimed
    /// before others and held pending until answered, where the queue's policy
    /// says so.
    pub blocking: bool,
    /// Who asks: a later listener of the same asker takes the question back
    /// when an earlier one abandoned it.
    pub asker: Option<Asker>,
    /// What the question is about.
    pub about: Option<Address>,
}

/// What refused a question or a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RefusalKind {
    /// A validator refused it, or could not judge it.
    Validator,
    /// The layout or its allowlist refused it.
    Capability,
    /// It does not satisfy the schema it is read as.
    Schema,
    /// Its queue could not be read or written.
    Queue,
}

/// Why the bus refused a question or the reply to one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[error("{reason}")]
pub struct Refusal {
    /// What refused.
    pub kind: RefusalKind,
    /// Why, in words naming what was refused.
    pub reason: String,
}

impl Refusal {
    fn of(kind: RefusalKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
        }
    }
}

/// What waiting for an answer answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer<R> {
    /// A reply record echoing this ask's correlation, read as an `R`.
    Reply(R),
    /// The wait elapsed; the question stands.
    Timeout,
    /// The listener was abandoned and nobody re-attended.
    Abandoned,
    /// The bus refused the question or the reply.
    Refused(Refusal),
}

impl<R> Answer<R> {
    /// The word the command line prints for this answer: `reply`, `timeout`,
    /// `abandoned` or `refused`.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Reply(_) => "reply",
            Self::Timeout => "timeout",
            Self::Abandoned => "abandoned",
            Self::Refused(_) => "refused",
        }
    }
}

/// Every record one offer appended, in order.
type Appended = Vec<(QueueName, Pushed<Value>)>;

/// The handle an asked question's answer arrives on.
pub struct Pending<R> {
    correlation: Correlation,
    questions: RawQueue,
    answers: RawQueue,
    position: Position,
    id: u64,
    asker: Option<Asker>,
    reply: PhantomData<fn() -> R>,
}

impl<R> fmt::Debug for Pending<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending")
            .field("correlation", &self.correlation)
            .field("queue", self.questions.name())
            .field("position", &self.position)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<R: Message> Pending<R> {
    /// The correlation a reply echoes to answer this ask.
    #[must_use]
    pub fn correlation(&self) -> &Correlation {
        &self.correlation
    }

    /// The queue the question was asked on.
    #[must_use]
    pub fn queue(&self) -> &QueueName {
        self.questions.name()
    }

    /// The position after the question on its queue.
    #[must_use]
    pub fn position(&self) -> Position {
        self.position
    }

    /// The question's id on its queue.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The asker the question names, when it names one.
    #[must_use]
    pub fn asker(&self) -> Option<&Asker> {
        self.asker.as_ref()
    }

    /// Wait up to `timeout` for the answer.
    ///
    /// A reply record on the answer queue echoing this correlation is the
    /// answer, read as an `R` — or refused, naming the schema and the pointer,
    /// when it is not one. With none, a question abandoned and not re-attended
    /// answers [`Answer::Abandoned`], and a wait that elapses answers
    /// [`Answer::Timeout`]. Nothing is appended anywhere by waiting.
    pub fn wait(&self, timeout: Duration) -> Answer<R> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let since = match self.answers.fingerprint() {
                Ok(since) => since,
                Err(failure) => return self.unreadable(&failure),
            };
            match self.settled() {
                Ok(Some(answer)) => return answer,
                Ok(None) => {}
                Err(failure) => return self.unreadable(&failure),
            }
            let slice = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Answer::Timeout;
                    }
                    left.min(WAIT_SLICE)
                }
                // A deadline past what the clock can name is no deadline.
                None => WAIT_SLICE,
            };
            if let Err(failure) = self.answers.wait_for_change(&since, slice) {
                return self.unreadable(&failure);
            }
        }
    }

    /// Take the question back after a lost wait: a question abandoned is
    /// attended again, so the next [`wait`](Self::wait) waits for its reply.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn rearm(&self) -> Result<(), QueueError> {
        self.questions.mark(self.id, false).map(|_| ())
    }

    /// Say nobody is listening for this answer now: the question is marked
    /// abandoned — kept, readable, and still answerable — until a listener
    /// re-attends it.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn abandon(&self) -> Result<(), QueueError> {
        self.questions.mark(self.id, true).map(|_| ())
    }

    /// Whether the question is marked abandoned now.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn is_abandoned(&self) -> Result<bool, QueueError> {
        self.questions.is_marked_abandoned(self.id)
    }

    /// The first reply record on the answer queue echoing this correlation.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn reply_record(&self) -> Result<Option<Value>, QueueError> {
        Ok(replies_on(&self.answers)?
            .into_iter()
            .find(|(correlation, _)| *correlation == self.correlation)
            .map(|(_, record)| record))
    }

    fn settled(&self) -> Result<Option<Answer<R>>, QueueError> {
        if let Some(record) = self.reply_record()? {
            return Ok(Some(self.read(record)));
        }
        if self.is_abandoned()? {
            return Ok(Some(Answer::Abandoned));
        }
        Ok(None)
    }

    fn unreadable(&self, failure: &QueueError) -> Answer<R> {
        Answer::Refused(Refusal::of(
            RefusalKind::Queue,
            format!(
                "the answer to {} on {} could not be read: {failure}",
                self.correlation,
                self.questions.name()
            ),
        ))
    }

    /// A reply record read as an `R`, or the refusal naming the schema and the
    /// pointer it fails at.
    fn read(&self, record: Value) -> Answer<R> {
        let refused = |why: String| {
            Answer::Refused(Refusal::of(
                RefusalKind::Schema,
                format!(
                    "the reply echoing {} on {} is refused: {why}",
                    self.correlation,
                    self.answers.name()
                ),
            ))
        };
        if let Some(schema) = &self.answers.spec().schema {
            if let Err(CheckError::Violation(violation)) =
                self.answers.registry().check(schema, &record)
            {
                return refused(violation.to_string());
            }
        }
        let mut registry = Registry::new();
        if let Err(failure) = registry.register::<R>() {
            return refused(failure.to_string());
        }
        if let Err(CheckError::Violation(violation)) = registry.check(&R::SCHEMA, &record) {
            return refused(violation.to_string());
        }
        match serde_json::from_value::<R>(record) {
            Ok(reply) => Answer::Reply(reply),
            Err(failure) => refused(format!("it does not read as a {}: {failure}", R::SCHEMA)),
        }
    }
}

/// A question on a queue: its correlation, its record and where it was queued.
#[derive(Debug, Clone)]
struct Question {
    correlation: Correlation,
    claimed: Claimed<Value>,
    id: u64,
}

/// Every question a queue's log has queued, in order, each once.
fn questions_on(questions: &RawQueue) -> Result<Vec<Question>, QueueError> {
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    for (line, after) in questions.log(None)? {
        if line.get("event").and_then(Value::as_str) != Some("queued") {
            continue;
        }
        let record = without_event(line);
        let (Some(correlation), Some(id)) = (correlation_of(&record), record_id(&record)) else {
            continue;
        };
        if seen.insert(correlation.clone()) {
            found.push(Question {
                correlation,
                claimed: Claimed {
                    record,
                    position: after,
                    id: Some(id),
                },
                id,
            });
        }
    }
    Ok(found)
}

/// Every reply record an answer queue holds that echoes a correlation, oldest
/// first.
fn replies_on(answers: &RawQueue) -> Result<Vec<(Correlation, Value)>, QueueError> {
    Ok(answers
        .log(None)?
        .into_iter()
        .filter(|(line, _)| {
            line.get("event")
                .and_then(Value::as_str)
                .is_none_or(|event| event == "queued")
        })
        .filter_map(|(line, _)| {
            let record = without_event(line);
            correlation_of(&record).map(|correlation| (correlation, record))
        })
        .collect())
}

fn without_event(line: Value) -> Value {
    match line {
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .filter(|(key, _)| key != "event")
                .collect::<Map<String, Value>>(),
        ),
        other => other,
    }
}

fn correlation_of(record: &Value) -> Option<Correlation> {
    record
        .get(CORRELATION)
        .and_then(Value::as_str)
        .and_then(|text| text.parse().ok())
}

fn record_id(record: &Value) -> Option<u64> {
    record.get("id").and_then(Value::as_u64)
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

/// `record` with `correlation` set: in place where it has one, last where not.
fn stamped(record: Value, correlation: &Correlation) -> Value {
    match record {
        Value::Object(mut fields) => {
            fields.insert(
                CORRELATION.to_owned(),
                Value::String(correlation.as_str().to_owned()),
            );
            Value::Object(fields)
        }
        other => other,
    }
}

/// What a reply bound to, and what it appended.
#[derive(Debug, Clone, PartialEq)]
pub struct Bound {
    /// The correlation of the question it answers, where that question carries
    /// one.
    pub correlation: Option<Correlation>,
    /// The question: its record, where it was queued (or, bound by a claim
    /// position, where it was claimed), and its id.
    pub question: Claimed<Value>,
    /// Whether the reply reached the answer queue. A reply routed to another
    /// queue alone — a planner channel envelope carrying only commands — answers
    /// nothing, and the question stands.
    pub answered: bool,
    /// Every record appended, in order.
    pub sent: Vec<(QueueName, Pushed<Value>)>,
}

fn listing(outstanding: &[Question]) -> String {
    const SHOWN: usize = 8;
    match outstanding.len() {
        0 => "nothing is pending there".to_owned(),
        count => {
            let named: Vec<&str> = outstanding
                .iter()
                .take(SHOWN)
                .map(|question| question.correlation.as_str())
                .collect();
            let more = count.saturating_sub(SHOWN);
            format!(
                "pending there: {}{}",
                named.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            )
        }
    }
}

impl Bus {
    /// The declared queue `queue`, as a queue questions are asked on, and the
    /// queue its answers are appended to.
    fn askable(&self, queue: &QueueName) -> Result<(RawQueue, RawQueue), BusError> {
        let questions = self.queue(queue)?;
        if !questions.spec().policy.keeps_events() {
            return Err(QueueError::NotAnEventQueue {
                queue: queue.clone(),
                what: "questions to ask or answer",
            }
            .into());
        }
        let Some(answers) = questions.spec().answers.clone() else {
            return Err(BusError::NotAskable {
                queue: queue.clone(),
                why: "it declares no queue its answers are appended to (`answers`)".to_owned(),
            });
        };
        let answers = self.queue(&answers)?;
        Ok((questions, answers))
    }

    /// Raise `question` on `queue` and hand back the handle its answer arrives
    /// on.
    ///
    /// The question is checked against `Q`'s schema, stamped with a minted
    /// [`Correlation`], its `blocking` flag, its `asker` and what it is `about`,
    /// shaped by the layout, judged by the validators, validated against the
    /// queue's schema and appended. Only a reply echoing the correlation answers
    /// it.
    ///
    /// # Errors
    ///
    /// An undeclared queue; one that keeps no events or declares no answer
    /// queue ([`BusError::NotAskable`]); a question that is not a JSON object
    /// or does not satisfy `Q`'s schema or the queue's; the layout's refusal; a
    /// validator's refusal or an unjudged verdict. Nothing is appended.
    pub fn ask<Q: Message, R: Message>(
        &self,
        queue: &QueueName,
        question: Q,
        options: AskOptions,
    ) -> Result<Pending<R>, BusError> {
        let (questions, answers) = self.askable(queue)?;
        let value = serde_json::to_value(&question).map_err(|failure| QueueError::Shape {
            queue: queue.clone(),
            why: failure.to_string(),
        })?;
        let mut registry = Registry::new();
        registry
            .register::<Q>()
            .map_err(|refusal| QueueError::Unregistered {
                queue: queue.clone(),
                refusal: Box::new(refusal),
            })?;
        if let Err(CheckError::Violation(violation)) = registry.check(&Q::SCHEMA, &value) {
            return Err(QueueError::Violation {
                queue: queue.clone(),
                violation: Box::new(violation),
            }
            .into());
        }
        let Value::Object(mut fields) = value else {
            return Err(QueueError::NotAnObject {
                queue: queue.clone(),
                shape: "not an object",
            }
            .into());
        };
        let asked: BTreeSet<Correlation> = questions_on(&questions)?
            .into_iter()
            .map(|question| question.correlation)
            .collect();
        let correlation = std::iter::repeat_with(Correlation::mint)
            .find(|minted| !asked.contains(minted))
            .unwrap_or_else(Correlation::mint);
        fields.insert("blocking".to_owned(), Value::Bool(options.blocking));
        if let Some(asker) = &options.asker {
            fields.insert("asker".to_owned(), Value::String(asker.as_str().to_owned()));
        }
        if let Some(about) = &options.about {
            fields.insert(ABOUT.to_owned(), Value::String(about.as_str().to_owned()));
        }
        fields.insert(
            CORRELATION.to_owned(),
            Value::String(correlation.as_str().to_owned()),
        );
        let offered = Value::Object(fields);
        let routed = self.prepare(queue, offered.clone())?;
        QueueError::of_verdict(
            queue,
            self.judge(queue, &offered, &routed, Some(&correlation)),
        )?;
        let mut raised = None;
        for (target, record) in routed {
            let pushed = self.queue(&target)?.push_judged(record)?;
            if &target == queue {
                raised = Some(pushed);
            }
        }
        let Some((position, Some(id))) = raised.map(|pushed| (pushed.position, pushed.id)) else {
            return Err(BusError::NotAskable {
                queue: queue.clone(),
                why: "the layout routed the question to no record with an id on the queue it was asked on".to_owned(),
            });
        };
        Ok(Pending {
            correlation,
            questions,
            answers,
            position,
            id,
            asker: options.asker,
            reply: PhantomData,
        })
    }

    /// Listen again for the question `correlation` minted on `queue`, raising
    /// nothing: the re-arm after a lost wait.
    ///
    /// A durable listener of the asker the question names takes it back — it is
    /// attended, and a later wait waits for its reply. Any other listener
    /// attends nothing, so a question abandoned stays abandoned for it and its
    /// wait answers [`Answer::Abandoned`].
    ///
    /// # Errors
    ///
    /// An undeclared or unaskable queue, [`BusError::Unbound`] for a
    /// correlation no question on it carries, or a queue failure.
    pub fn listen<R: Message>(
        &self,
        queue: &QueueName,
        correlation: &Correlation,
        lifetime: &Lifetime,
    ) -> Result<Pending<R>, BusError> {
        let (questions, answers) = self.askable(queue)?;
        let question = questions_on(&questions)?
            .into_iter()
            .find(|question| question.correlation == *correlation)
            .ok_or_else(|| BusError::Unbound {
                queue: queue.clone(),
                why: format!("no question on {queue} carries the correlation {correlation}"),
            })?;
        let named = question
            .claimed
            .record
            .get("asker")
            .and_then(Value::as_str)
            .and_then(|name| Asker::new(name, "the recorded asker").ok());
        let pending = Pending {
            correlation: correlation.clone(),
            position: question.claimed.position,
            id: question.id,
            questions,
            answers,
            asker: named,
            reply: PhantomData,
        };
        if let Lifetime::Durable(asker) = lifetime {
            if pending.asker() == Some(asker) {
                pending.rearm()?;
            }
        }
        Ok(pending)
    }

    /// Answer the pending ask on `queue` whose correlation `correlation` names
    /// — or, naming none, the one ask pending there — with `reply`.
    ///
    /// An ask is pending from when it is queued until a reply echoing its
    /// correlation is on the answer queue, abandoned or not. The reply is
    /// shaped by the layout as an offer to the answer queue, the record that
    /// lands there stamped with the correlation, judged by the validators,
    /// and appended; a question holding the queue's pending slot is released.
    ///
    /// # Errors
    ///
    /// [`BusError::Unbound`] for a correlation nothing pending holds, naming
    /// it, or — naming none — for a queue with no pending ask or more than one;
    /// an unaskable queue; the layout's refusal; a validator's refusal or an
    /// unjudged verdict. Nothing is appended.
    pub fn reply(
        &self,
        queue: &QueueName,
        correlation: Option<&Correlation>,
        reply: Value,
    ) -> Result<Bound, BusError> {
        let (questions, answers) = self.askable(queue)?;
        let replied: BTreeSet<Correlation> = replies_on(&answers)?
            .into_iter()
            .map(|(correlation, _)| correlation)
            .collect();
        let outstanding: Vec<Question> = questions_on(&questions)?
            .into_iter()
            .filter(|question| !replied.contains(&question.correlation))
            .collect();
        let question = match correlation {
            Some(correlation) => outstanding
                .iter()
                .find(|question| question.correlation == *correlation)
                .cloned()
                .ok_or_else(|| BusError::Unbound {
                    queue: queue.clone(),
                    why: format!(
                        "no pending ask carries the correlation {correlation}, so the reply echoing it answers nothing; {}",
                        listing(&outstanding)
                    ),
                })?,
            None => match outstanding.as_slice() {
                [only] => only.clone(),
                [] => {
                    return Err(BusError::Unbound {
                        queue: queue.clone(),
                        why: "the reply echoes no correlation, and no ask is pending to bind it to"
                            .to_owned(),
                    })
                }
                many => {
                    return Err(BusError::Unbound {
                        queue: queue.clone(),
                        why: format!(
                            "the reply echoes no correlation, and {} asks are pending, so it binds to none of them; echo one — {}",
                            many.len(),
                            listing(many)
                        ),
                    })
                }
            },
        };
        let replied = self.append_reply(&answers, Some(&question.correlation), reply)?;
        if let Some(at) = &replied.1 {
            if let Some(held) = questions.held()? {
                if held.id == Some(question.id) {
                    questions.answer(&held, at)?;
                }
            }
        }
        Ok(Bound {
            correlation: Some(question.correlation),
            question: question.claimed,
            answered: replied.1.is_some(),
            sent: replied.0,
        })
    }

    /// Answer the record `queue` holds pending, claimed at `position`, with
    /// `reply` — the reply stamped with the pending question's correlation
    /// where it carries one.
    ///
    /// # Errors
    ///
    /// As [`reply`](Self::reply), with [`QueueError::NotPending`] for a
    /// position the pending record was not claimed at, and
    /// [`BusError::Unbound`] when another reply released the record between
    /// the check and the release — this reply is then appended and answers
    /// nothing.
    pub fn reply_at(
        &self,
        queue: &QueueName,
        position: &Position,
        reply: Value,
    ) -> Result<Bound, BusError> {
        let (questions, answers) = self.askable(queue)?;
        let held = questions.pending_at(position)?;
        let correlation = correlation_of(&held.record);
        let (sent, at) = self.append_reply(&answers, correlation.as_ref(), reply)?;
        if let Some(at) = &at {
            if !questions.answer(&held, at)? {
                return Err(BusError::Unbound {
                    queue: queue.clone(),
                    why: format!(
                        "the record pending at position {} was answered by another reply first; this reply was appended to {} at position {at} and answers nothing",
                        held.position,
                        answers.name()
                    ),
                });
            }
        }
        Ok(Bound {
            correlation,
            question: held,
            answered: at.is_some(),
            sent,
        })
    }

    /// Shape, stamp, judge and append one reply to the answer queue `answers`:
    /// everything appended, and the position after the record on the answer
    /// queue when one landed there.
    fn append_reply(
        &self,
        answers: &RawQueue,
        correlation: Option<&Correlation>,
        reply: Value,
    ) -> Result<(Appended, Option<Position>), BusError> {
        let answer_queue = answers.name().clone();
        let routed: Vec<(QueueName, Value)> = self
            .prepare(&answer_queue, reply.clone())?
            .into_iter()
            .map(|(target, record)| match correlation {
                Some(correlation) if target == answer_queue => {
                    let stamped = stamped(record, correlation);
                    (target, stamped)
                }
                _ => (target, record),
            })
            .collect();
        let verdict: Verdict = self.judge(&answer_queue, &reply, &routed, correlation);
        QueueError::of_verdict(&answer_queue, verdict)?;
        let mut sent = Vec::new();
        let mut at = None;
        for (target, record) in routed {
            let pushed = self.queue(&target)?.push_judged(record)?;
            if target == answer_queue {
                at = Some(pushed.position);
            }
            sent.push((target, pushed));
        }
        Ok((sent, at))
    }
}
