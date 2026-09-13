//! Queues over a transport, with declared policies.
//!
//! A queue is a log kept on a [`Transport`] and a [`Policy`] saying what
//! reading it means. A policy that asks for nothing is a **plain** queue: its
//! records are read in order through each consumer's cursor, and a claim is the
//! cursor moving past a record. A policy that holds a claimed record pending,
//! claims blocking records first, supersedes waiting records or keeps a
//! projection is an **event** queue: every state a record reaches — queued,
//! claimed, answered, abandoned, attended — is one more line of the log, and
//! what is waiting, what is pending and what nobody is listening for any more is
//! the log folded. `docs/queues.md` states both, and the projection's account.
//!
//! The queue owns four fields of an event queue's records, and names nothing
//! else in them: `id` (allocated one past the highest the log has queued),
//! `blocking`, `abandoned` (omitted while false) and `asker` (who raised it).
//! Every other field is the consumer's, and a typed queue ([`Queue`]) writes each
//! record in its own type's field order.
//!
//! Nothing accepted is lost and nothing claimed is handed out twice: a claim is
//! recorded — an event or a cursor — under the queue's exclusive section before
//! the record is handed over, so a claimant that crashes afterwards leaves a
//! record claimed rather than a record the next reader takes again.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::schema::{CheckError, Message, Registry, RegistryError, SchemaId, SchemaViolation};
use crate::transport::{
    Changed, ConsumerName, DocumentName, Fingerprint, Position, QueueName, Transport,
    TransportError,
};
use crate::validate::{ValidationContext, Validators, Verdict};

/// What a queue promises about delivery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Delivery {
    /// Nothing accepted is lost; a claim is a record, so a crashed claimant's
    /// record is not handed out again.
    #[default]
    // llmlint: ignore[names_match_behavior] Contract Q names this variant `Delivery::AtLeastOnce` and defines it as "nothing accepted is lost; a claim is a record", and the same contract requires that a crashed claimant's record is not handed out twice; the record stays kept, pending and readable rather than lost, and renaming the variant is a change for the contract's owner rather than this node.
    AtLeastOnce,
}

/// What a queue promises about order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Ordering {
    /// Records are totally ordered within one queue, and across queues nothing
    /// is promised.
    #[default]
    PerQueue,
}

/// What a queue keeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Retention {
    /// Append-only: nothing is deleted, and the log is the record.
    #[default]
    Keep,
}

/// A dotted path to a field of a JSON record: `source`, `reply.completion`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    /// The value at this path in `record`, when every step is an object key
    /// that is there.
    #[must_use]
    pub fn get<'a>(&self, record: &'a Value) -> Option<&'a Value> {
        self.0
            .iter()
            .try_fold(record, |value, step| value.as_object()?.get(step))
    }
}

/// Why text is not a [`FieldPath`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not a field path: {why}; a field path is object keys joined by `.`, e.g. reply.completion")]
pub struct FieldPathError {
    /// What was offered.
    pub text: String,
    /// What is wrong with it.
    pub why: &'static str,
}

impl FromStr for FieldPath {
    type Err = FieldPathError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let steps: Vec<String> = text.split('.').map(str::to_owned).collect();
        if steps.iter().any(|step| step.trim().is_empty()) {
            return Err(FieldPathError {
                text: text.to_owned(),
                why: if text.is_empty() {
                    "it is empty"
                } else {
                    "one of its keys is empty"
                },
            });
        }
        Ok(Self(steps))
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("."))
    }
}

impl Serialize for FieldPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FieldPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for FieldPath {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("FieldPath")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Object keys joined by `.`: the path to one field of a record.",
            "pattern": "^[^.]+(\\.[^.]+)*$"
        })
    }
}

/// A test of one JSON record.
///
/// On the wire one object with exactly one form: `{"field": P, "equals": V}`,
/// `{"field": P, "present": true|false}` (there and not `null`),
/// `{"field": P, "non_empty": true|false}` (there, and not `null`, `""`, `[]` or
/// `{}`), `{"all": [...]}`, `{"any": [...]}` or `{"not": {...}}`.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// The field is there and equals the value.
    Equals {
        /// The field.
        field: FieldPath,
        /// The value it must equal.
        value: Value,
    },
    /// The field is there and not `null` — or, with `present: false`, it is not.
    Present {
        /// The field.
        field: FieldPath,
        /// Whether it must be present.
        present: bool,
    },
    /// The field holds something — or, with `non_empty: false`, it does not.
    NonEmpty {
        /// The field.
        field: FieldPath,
        /// Whether it must be non-empty.
        non_empty: bool,
    },
    /// Every one of these holds (and so an empty list always does).
    All(Vec<Predicate>),
    /// At least one of these holds (and so an empty list never does).
    Any(Vec<Predicate>),
    /// This does not hold.
    Not(Box<Predicate>),
}

impl Predicate {
    /// `field == value`.
    #[must_use]
    pub fn equals(field: FieldPath, value: impl Into<Value>) -> Self {
        Self::Equals {
            field,
            value: value.into(),
        }
    }

    /// Whether `record` satisfies this.
    #[must_use]
    pub fn matches(&self, record: &Value) -> bool {
        match self {
            Self::Equals { field, value } => field.get(record) == Some(value),
            Self::Present { field, present } => {
                field.get(record).is_some_and(|value| !value.is_null()) == *present
            }
            Self::NonEmpty { field, non_empty } => {
                let holds = field.get(record).is_some_and(|value| match value {
                    Value::Null => false,
                    Value::String(text) => !text.is_empty(),
                    Value::Array(items) => !items.is_empty(),
                    Value::Object(fields) => !fields.is_empty(),
                    Value::Bool(_) | Value::Number(_) => true,
                });
                holds == *non_empty
            }
            Self::All(each) => each.iter().all(|predicate| predicate.matches(record)),
            Self::Any(each) => each.iter().any(|predicate| predicate.matches(record)),
            Self::Not(inner) => !inner.matches(record),
        }
    }

    /// A predicate from a spec: inline JSON, or a path to a YAML document.
    ///
    /// # Errors
    ///
    /// A spec that is neither, or a document that is not one predicate, named.
    pub fn read(spec: &str) -> Result<Self, String> {
        let trimmed = spec.trim_start();
        let text = if trimmed.starts_with('{') {
            spec.to_owned()
        } else {
            std::fs::read_to_string(spec).map_err(|failure| {
                format!("{spec:?} is neither inline JSON nor a readable predicate file: {failure}")
            })?
        };
        serde_norway::from_str(&text).map_err(|failure| format!("not a predicate: {failure}"))
    }
}

/// The wire shape of a [`Predicate`], before exactly one form is checked.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Predicate")]
struct PredicateWire {
    /// The field `equals`, `present` and `non_empty` test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    field: Option<FieldPath>,
    /// The field is there and equals this.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "explicit"
    )]
    equals: Option<Value>,
    /// The field is there and not null (true), or not (false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    present: Option<bool>,
    /// The field holds something (true), or not (false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    non_empty: Option<bool>,
    /// Every one of these holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    all: Option<Vec<PredicateWire>>,
    /// At least one of these holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    any: Option<Vec<PredicateWire>>,
    /// This does not hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not: Option<Box<PredicateWire>>,
}

/// An `equals` that is present reads as `Some`, even when it is `null`.
fn explicit<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

impl TryFrom<PredicateWire> for Predicate {
    type Error = String;

    fn try_from(wire: PredicateWire) -> Result<Self, Self::Error> {
        let forms: Vec<&str> = [
            wire.equals.as_ref().map(|_| "equals"),
            wire.present.map(|_| "present"),
            wire.non_empty.map(|_| "non_empty"),
            wire.all.as_ref().map(|_| "all"),
            wire.any.as_ref().map(|_| "any"),
            wire.not.as_ref().map(|_| "not"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let form = match forms.as_slice() {
            [form] => *form,
            [] => {
                return Err(
                    "a predicate names one of equals, present, non_empty, all, any and not, and this names none"
                        .to_owned(),
                )
            }
            many => {
                return Err(format!(
                    "a predicate names exactly one of equals, present, non_empty, all, any and not, and this names {}",
                    many.join(" and ")
                ))
            }
        };
        let needs_field = matches!(form, "equals" | "present" | "non_empty");
        let field = match (needs_field, wire.field) {
            (true, Some(field)) => Some(field),
            (true, None) => {
                return Err(format!("`{form}` tests a field, and this names no `field`"))
            }
            (false, Some(field)) => {
                return Err(format!(
                    "`{form}` takes no `field`, and this names `{field}`"
                ))
            }
            (false, None) => None,
        };
        let each = |list: Vec<PredicateWire>| -> Result<Vec<Predicate>, String> {
            list.into_iter().map(Predicate::try_from).collect()
        };
        Ok(match (form, field) {
            ("equals", Some(field)) => Self::Equals {
                field,
                value: wire.equals.unwrap_or(Value::Null),
            },
            ("present", Some(field)) => Self::Present {
                field,
                present: wire.present.unwrap_or(true),
            },
            ("non_empty", Some(field)) => Self::NonEmpty {
                field,
                non_empty: wire.non_empty.unwrap_or(true),
            },
            ("all", _) => Self::All(each(wire.all.unwrap_or_default())?),
            ("any", _) => Self::Any(each(wire.any.unwrap_or_default())?),
            (_, _) => Self::Not(Box::new(Predicate::try_from(
                *wire
                    .not
                    .ok_or_else(|| "`not` holds no predicate".to_owned())?,
            )?)),
        })
    }
}

impl From<&Predicate> for PredicateWire {
    fn from(predicate: &Predicate) -> Self {
        let mut wire = Self {
            field: None,
            equals: None,
            present: None,
            non_empty: None,
            all: None,
            any: None,
            not: None,
        };
        match predicate {
            Predicate::Equals { field, value } => {
                wire.field = Some(field.clone());
                wire.equals = Some(value.clone());
            }
            Predicate::Present { field, present } => {
                wire.field = Some(field.clone());
                wire.present = Some(*present);
            }
            Predicate::NonEmpty { field, non_empty } => {
                wire.field = Some(field.clone());
                wire.non_empty = Some(*non_empty);
            }
            Predicate::All(each) => wire.all = Some(each.iter().map(Self::from).collect()),
            Predicate::Any(each) => wire.any = Some(each.iter().map(Self::from).collect()),
            Predicate::Not(inner) => wire.not = Some(Box::new(Self::from(inner.as_ref()))),
        }
        wire
    }
}

impl Serialize for Predicate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        PredicateWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Predicate {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Predicate::try_from(PredicateWire::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Predicate {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        PredicateWire::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        PredicateWire::json_schema(generator)
    }
}

/// A newer record replacing a waiting older one.
///
/// When a record that `when` admits is queued (any record, where `when` is
/// absent), every waiting record whose `key` field equals the new record's is
/// removed from waiting. Only waiting records are replaced: a record already
/// claimed is not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Supersede {
    /// The field whose equal values replace one another.
    pub key: FieldPath,
    /// Which records supersede at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Predicate>,
}

/// What reading a queue means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Nothing accepted is lost; a claim is a record.
    #[serde(default)]
    pub delivery: Delivery,
    /// Records are ordered within the queue.
    #[serde(default)]
    pub ordering: Ordering,
    /// A newer record with the same key replaces a waiting older one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersede_on: Option<Supersede>,
    /// A claimed blocking record is pending until answered; one pending at a
    /// time.
    #[serde(default)]
    pub hold_pending: bool,
    /// A claim hands out a blocking record before any non-blocking one.
    #[serde(default)]
    pub blocking_first: bool,
    /// Append-only; nothing deleted.
    #[serde(default)]
    pub retention: Retention,
    /// Keep a folded projection document under this name, stamped with the log
    /// bytes it accounts for and sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<DocumentName>,
}

impl Default for Policy {
    /// A plain queue: at-least-once, ordered, kept, and nothing else.
    fn default() -> Self {
        Self {
            delivery: Delivery::AtLeastOnce,
            ordering: Ordering::PerQueue,
            supersede_on: None,
            hold_pending: false,
            blocking_first: false,
            retention: Retention::Keep,
            projection: None,
        }
    }
}

impl Policy {
    /// Whether a queue under this policy keeps an event log, which every
    /// policy asking for a pending slot, blocking-first claims, superseding or a
    /// projection does. A queue asking for none of them is a plain log.
    #[must_use]
    pub fn keeps_events(&self) -> bool {
        self.hold_pending
            || self.blocking_first
            || self.supersede_on.is_some()
            || self.projection.is_some()
    }
}

/// One queue as a layout or a configuration declares it: its name, its policy,
/// and the keys that sit beside the policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueSpec {
    /// The queue.
    pub name: QueueName,
    /// What reading it means.
    #[serde(default)]
    pub policy: Policy,
    /// The schema every record pushed onto it is validated against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaId>,
    /// The queue a reply to one of its pending records is appended to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<QueueName>,
    /// Which records a claim hands out: a claim passes over a record this does
    /// not admit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<Predicate>,
    /// The consumers whose cursors `status` reports.
    #[serde(default = "default_consumers")]
    pub consumers: Vec<ConsumerName>,
    /// Whether a push stamps each record's `id` with the number of records before
    /// it. An event queue always allocates `id`, so this is a plain queue's.
    #[serde(default)]
    pub numbered: bool,
}

fn default_consumers() -> Vec<ConsumerName> {
    vec![ConsumerName::default_consumer()]
}

impl QueueSpec {
    /// A queue under `policy`, with nothing beside it and the default consumer.
    #[must_use]
    pub fn new(name: QueueName, policy: Policy) -> Self {
        Self {
            name,
            policy,
            schema: None,
            answers: None,
            claims: None,
            consumers: default_consumers(),
            numbered: false,
        }
    }
}

/// One asker's name: the word by which two listeners are one side.
///
/// A listener is something an asker rents, never the asker itself: an asker may
/// raise a question through one listener and wait for its answer through a
/// succession of them. Two listeners carrying the same asker are one asker, and
/// the later takes back over what the earlier left ([`RawQueue::attend`]).
///
/// Compared for equality and never parsed. The two values that are not
/// identities are refused where a name enters: a **blank** one, which every
/// listener carrying it would match, and one that is **not Unicode**, which
/// collapses onto every other such value when read as text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Asker(String);

/// Why a value names no asker.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AskerRefused {
    /// The value is not Unicode.
    #[error("{from} is set to a value this host cannot read as text; an asker is compared to other askers as one word, and two values that are not text read as the same word — set it to a name in Unicode, or leave it unset for a session that listens on its own")]
    NotUnicode {
        /// Where the value came from: an environment variable, a flag.
        from: String,
    },
    /// The value is blank.
    #[error("{from} is set to a blank value, which names no asker; leave it unset for a session that listens on its own, or set it to the one value every session of this asker carries")]
    Blank {
        /// Where the value came from.
        from: String,
    },
}

impl Asker {
    /// The asker `value` names, where `from` says where it came from — an
    /// environment variable's name, a flag — for the refusal.
    ///
    /// # Errors
    ///
    /// [`AskerRefused`] for a value that is not Unicode, or is blank.
    pub fn named(value: &OsStr, from: &str) -> Result<Self, AskerRefused> {
        let text = value.to_str().ok_or_else(|| AskerRefused::NotUnicode {
            from: from.to_owned(),
        })?;
        Self::new(text, from)
    }

    /// The same check, over a value that is already text.
    ///
    /// # Errors
    ///
    /// [`AskerRefused::Blank`] for a blank value.
    pub fn new(text: &str, from: &str) -> Result<Self, AskerRefused> {
        if text.trim().is_empty() {
            return Err(AskerRefused::Blank {
                from: from.to_owned(),
            });
        }
        Ok(Self(text.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Asker {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::new(&text, "the recorded asker").map_err(serde::de::Error::custom)
    }
}

/// Why a queue did not do what it was asked.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    /// The transport failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// A record that does not conform to the queue's schema.
    #[error("{queue}: the record is not a {violation}")]
    Violation {
        /// The queue.
        queue: QueueName,
        /// What the schema said.
        violation: Box<SchemaViolation>,
    },
    /// The queue's schema is not registered.
    #[error("{queue}: {refusal}")]
    Unregistered {
        /// The queue.
        queue: QueueName,
        /// What the registry said.
        refusal: Box<RegistryError>,
    },
    /// A record an event queue or a numbered queue cannot keep: not an object.
    #[error("{queue}: a record on this queue is a JSON object, and this is {shape}")]
    NotAnObject {
        /// The queue.
        queue: QueueName,
        /// What it is instead.
        shape: &'static str,
    },
    /// A record that does not read as the queue's own type.
    #[error("{queue}: the record does not read as this queue's record type: {why}")]
    Shape {
        /// The queue.
        queue: QueueName,
        /// What reading it said.
        why: String,
    },
    /// The last id there is has been allocated.
    #[error("{queue}: the queue has no id left to allocate; the last one, {last}, has already been queued")]
    NoIdLeft {
        /// The queue.
        queue: QueueName,
        /// The last id.
        last: u64,
    },
    /// An operation only an event queue has, asked of a plain one.
    #[error("{queue} is a plain queue, so it has no {what}; only a queue whose policy holds pending records, claims blocking records first, supersedes or keeps a projection does")]
    NotAnEventQueue {
        /// The queue.
        queue: QueueName,
        /// What was asked for.
        what: &'static str,
    },
    /// An answer to a position that is not where the pending record was
    /// claimed.
    #[error("{queue}: {why}")]
    NotPending {
        /// The queue.
        queue: QueueName,
        /// What was found instead.
        why: String,
    },
    /// An answer naming a reply position no record on the queue's `answers`
    /// queue ends at, or asked of a queue that declares no `answers` queue.
    #[error("{queue}: {why}")]
    NoReply {
        /// The queue whose pending slot was to be answered.
        queue: QueueName,
        /// Why the position names no reply.
        why: String,
    },
    /// A validator refused the record, so nothing was appended.
    #[error("{queue}: refused before anything was appended: {reason}")]
    Refused {
        /// The queue it was offered to.
        queue: QueueName,
        /// The reason the validator gave, unaltered.
        reason: String,
    },
    /// A validator could not judge the record, so nothing was appended.
    #[error("{queue}: not appended, because it could not be judged: {reason}")]
    Unjudged {
        /// The queue it was offered to.
        queue: QueueName,
        /// Why it could not be judged.
        reason: String,
    },
}

impl QueueError {
    /// `Ok` for a pass, and the refusal a verdict that is not one makes on
    /// `queue`.
    ///
    /// # Errors
    ///
    /// [`QueueError::Refused`] or [`QueueError::Unjudged`], carrying the
    /// verdict's reason unaltered.
    pub fn of_verdict(queue: &QueueName, verdict: Verdict) -> Result<(), Self> {
        match verdict {
            Verdict::Pass => Ok(()),
            Verdict::Refuse { reason } => Err(Self::Refused {
                queue: queue.clone(),
                reason,
            }),
            Verdict::Unjudged { reason } => Err(Self::Unjudged {
                queue: queue.clone(),
                reason,
            }),
        }
    }
}

/// A record a push appended.
#[derive(Debug, Clone, PartialEq)]
pub struct Pushed<M> {
    /// The record as it was appended.
    pub record: M,
    /// The position after it.
    pub position: Position,
    /// The id it was given, on a queue that gives one.
    pub id: Option<u64>,
}

/// A record a claim handed out.
#[derive(Debug, Clone, PartialEq)]
pub struct Claimed<M> {
    /// The record.
    pub record: M,
    /// Where the claim was recorded: the position after its claim event on an
    /// event queue, after the record itself on a plain one.
    pub position: Position,
    /// The record's id, on a queue that gives one.
    pub id: Option<u64>,
}

/// Everything a `status` read of one queue reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueStatus {
    /// The queue.
    pub queue: QueueName,
    /// Whether it keeps an event log.
    pub events: bool,
    /// How many records its log holds.
    pub records: u64,
    /// The records waiting to be claimed, oldest first: on an event queue every
    /// waiting record, abandoned ones included; on a plain one the records after
    /// the default consumer's cursor that a claim hands out.
    pub waiting: Vec<Value>,
    /// The record in the pending slot, abandoned or not.
    pub pending: Option<Value>,
    /// Where the pending record was claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_position: Option<Position>,
    /// The records nobody is listening for any more, waiting or pending.
    pub abandoned: Vec<Value>,
    /// How many waiting records somebody is still owed a reading of.
    pub unread: u64,
    /// Each declared consumer's cursor, `null` for one that has read nothing.
    pub cursors: BTreeMap<ConsumerName, Option<Position>>,
}

/// What one line of an event queue's log says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Queued,
    Claimed,
    Answered,
    Abandoned,
    Attended,
}

impl Event {
    const fn word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Answered => "answered",
            Self::Abandoned => "abandoned",
            Self::Attended => "attended",
        }
    }

    fn of_word(word: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Claimed,
            Self::Answered,
            Self::Abandoned,
            Self::Attended,
        ]
        .into_iter()
        .find(|event| event.word() == word)
    }
}

/// An event queue folded: the projection document's claims, and its stamp and
/// seal.
#[derive(Debug, Clone, Default, PartialEq)]
struct Folded {
    waiting: Vec<Value>,
    pending: Option<Value>,
    next_id: u64,
    accounted: Option<u64>,
    seal: Option<u128>,
}

/// FNV-1a's 128-bit offset basis: the digest of no bytes.
const NOTHING_DIGESTED: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;

/// FNV-1a's 128-bit prime, `2^88 + 0x13b`.
const FNV_PRIME: u128 = (1 << 88) | 0x13b;

/// FNV-1a over `bytes`, continuing from `from`: the seal `onepipeline` stamps
/// its projection with. An integrity check against accidents, not a security
/// boundary: a rewrite crafted to match is not a failure anybody here meets.
fn digested(from: u128, bytes: &[u8]) -> u128 {
    bytes.iter().fold(from, |digest, byte| {
        (digest ^ u128::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

impl Folded {
    /// The seal over the claims: the waiting records, the pending one, the id
    /// the next record takes, and the bytes of the log accounted for. `None` for
    /// an unstamped projection, which has no claim to vouch for.
    fn sealed(&self) -> Option<u128> {
        let accounted = self.accounted?;
        let claims =
            serde_json::to_vec(&(&self.waiting, &self.pending, self.next_id)).unwrap_or_default();
        Some(digested(
            digested(NOTHING_DIGESTED, &claims),
            &accounted.to_le_bytes(),
        ))
    }

    fn seal(&mut self) {
        self.seal = self.sealed();
    }

    /// Whether the claims are as a writer left them: unstamped (an older
    /// writer's, checked against the log instead), or stamped with a seal that
    /// still matches.
    fn is_intact(&self) -> bool {
        self.accounted.is_none() || self.seal.is_some() && self.seal == self.sealed()
    }

    fn render(&self) -> String {
        let mut document = Map::new();
        document.insert("waiting".to_owned(), Value::Array(self.waiting.clone()));
        document.insert(
            "pending".to_owned(),
            self.pending.clone().unwrap_or(Value::Null),
        );
        document.insert("next_id".to_owned(), Value::from(self.next_id));
        if let Some(accounted) = self.accounted {
            document.insert("accounted".to_owned(), Value::from(accounted));
        }
        if let Some(seal) = self.seal {
            document.insert("seal".to_owned(), Value::String(format!("{seal:032x}")));
        }
        serde_json::to_string_pretty(&Value::Object(document)).unwrap_or_default()
    }
}

/// `fields` without `key`, and what `key` held, every other key in its place.
///
/// Not `Map::remove`: under `preserve_order` that swaps the last key into the
/// hole, and a record read back through it comes out in another field order —
/// under another seal.
fn without_key(fields: Map<String, Value>, key: &str) -> (Option<Value>, Map<String, Value>) {
    let mut taken = None;
    let kept = fields
        .into_iter()
        .filter_map(|(name, value)| {
            if name == key {
                taken = Some(value);
                None
            } else {
                Some((name, value))
            }
        })
        .collect();
    (taken, kept)
}

fn record_id(record: &Value) -> Option<u64> {
    record.get("id").and_then(Value::as_u64)
}

fn is_blocking(record: &Value) -> bool {
    record
        .get("blocking")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn is_abandoned(record: &Value) -> bool {
    record
        .get("abandoned")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn asker_of(record: &Value) -> Option<&str> {
    record
        .get("asker")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
}

fn shape_word(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Reshapes a record into the queue's own record type and back, so what the
/// queue writes is in that type's field order and carries only its fields.
type Shape = Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

/// The shape of an untyped queue: the record as it is.
fn as_it_is() -> Shape {
    Arc::new(Ok)
}

/// The shape of a queue of `M`.
fn shape_of<M: Message>() -> Shape {
    Arc::new(|value: Value| {
        let typed: M = serde_json::from_value(value).map_err(|failure| failure.to_string())?;
        serde_json::to_value(&typed).map_err(|failure| failure.to_string())
    })
}

/// A queue over JSON records: what a configuration-declared queue is, and what
/// [`Queue`] is over its record type.
#[derive(Clone)]
pub struct RawQueue {
    transport: Arc<dyn Transport>,
    spec: QueueSpec,
    registry: Arc<Registry>,
    shape: Shape,
    validators: Validators<Value>,
}

impl fmt::Debug for RawQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawQueue")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

impl RawQueue {
    /// The queue `spec` declares, kept on `transport`, its records checked
    /// against `registry` where the spec names a schema.
    #[must_use]
    pub fn open(transport: Arc<dyn Transport>, spec: QueueSpec, registry: Arc<Registry>) -> Self {
        Self {
            transport,
            spec,
            registry,
            shape: as_it_is(),
            validators: Validators::new(),
        }
    }

    /// The same queue, judging every record pushed onto it by `validators`
    /// before anything is appended.
    #[must_use]
    pub fn with_validators(mut self, validators: Validators<Value>) -> Self {
        self.validators = validators;
        self
    }

    /// The validators a record pushed onto this queue is judged by.
    #[must_use]
    pub fn validators(&self) -> &Validators<Value> {
        &self.validators
    }

    /// The declaration this queue was opened with.
    #[must_use]
    pub fn spec(&self) -> &QueueSpec {
        &self.spec
    }

    /// The queue's name.
    #[must_use]
    pub fn name(&self) -> &QueueName {
        &self.spec.name
    }

    /// The transport it is kept on.
    #[must_use]
    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    fn event_queue(&self, what: &'static str) -> Result<(), QueueError> {
        if self.spec.policy.keeps_events() {
            Ok(())
        } else {
            Err(QueueError::NotAnEventQueue {
                queue: self.spec.name.clone(),
                what,
            })
        }
    }

    fn check(&self, record: &Value) -> Result<(), QueueError> {
        let Some(schema) = &self.spec.schema else {
            return Ok(());
        };
        match self.registry.check(schema, record) {
            Ok(()) => Ok(()),
            Err(CheckError::Violation(violation)) => Err(QueueError::Violation {
                queue: self.spec.name.clone(),
                violation: Box::new(violation),
            }),
            Err(CheckError::Registry(refusal)) => Err(QueueError::Unregistered {
                queue: self.spec.name.clone(),
                refusal: Box::new(refusal),
            }),
        }
    }

    fn reshape(&self, record: Value) -> Result<Value, QueueError> {
        (self.shape)(record).map_err(|why| QueueError::Shape {
            queue: self.spec.name.clone(),
            why,
        })
    }

    fn object(&self, record: Value) -> Result<Map<String, Value>, QueueError> {
        match record {
            Value::Object(fields) => Ok(fields),
            other => Err(QueueError::NotAnObject {
                queue: self.spec.name.clone(),
                shape: shape_word(&other),
            }),
        }
    }

    /// `record` with its `id` set, in place where it has one and first where it
    /// has none.
    fn with_id(&self, record: Value, id: u64) -> Result<Value, QueueError> {
        let mut fields = self.object(record)?;
        if let Some(slot) = fields.get_mut("id") {
            *slot = Value::from(id);
            return Ok(Value::Object(fields));
        }
        let mut numbered = Map::new();
        numbered.insert("id".to_owned(), Value::from(id));
        numbered.extend(fields);
        Ok(Value::Object(numbered))
    }

    fn with_abandoned(&self, record: &Value, abandoned: bool) -> Value {
        let Some(fields) = record.as_object() else {
            return record.clone();
        };
        let (_, mut fields) = without_key(fields.clone(), "abandoned");
        if abandoned {
            fields.insert("abandoned".to_owned(), Value::Bool(true));
        }
        (self.shape)(Value::Object(fields)).unwrap_or_else(|_| record.clone())
    }

    /// One line of the log read back: what it says happened, and the record.
    /// `None` for a line this queue cannot read, which is passed over.
    fn parse_line(&self, bytes: &[u8]) -> Option<(Option<Event>, Value)> {
        let Ok(Value::Object(line)) = serde_json::from_slice::<Value>(bytes) else {
            return None;
        };
        let (event, fields) = without_key(line, "event");
        let event = match event {
            None | Some(Value::Null) => None,
            Some(Value::String(word)) => Some(Event::of_word(&word)?),
            Some(_) => return None,
        };
        let record = (self.shape)(Value::Object(fields)).ok()?;
        record_id(&record)?;
        Some((event, record))
    }

    fn frame(event: Event, record: &Value) -> Vec<u8> {
        let mut line = Map::new();
        line.insert("event".to_owned(), Value::String(event.word().to_owned()));
        if let Some(fields) = record.as_object() {
            line.extend(fields.clone());
        }
        serde_json::to_vec(&Value::Object(line)).unwrap_or_default()
    }

    fn parse_projection(&self, bytes: &[u8]) -> Option<Folded> {
        let Ok(Value::Object(document)) = serde_json::from_slice::<Value>(bytes) else {
            return None;
        };
        let waiting = match document.get("waiting") {
            None => Vec::new(),
            Some(Value::Array(records)) => records
                .iter()
                .map(|record| (self.shape)(record.clone()).ok())
                .collect::<Option<Vec<Value>>>()?,
            Some(_) => return None,
        };
        let pending = match document.get("pending") {
            None | Some(Value::Null) => None,
            Some(record) => Some((self.shape)(record.clone()).ok()?),
        };
        let next_id = match document.get("next_id") {
            None => 0,
            Some(value) => value.as_u64()?,
        };
        let accounted = match document.get("accounted") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64()?),
        };
        let seal = match document.get("seal") {
            None | Some(Value::Null) => None,
            Some(Value::String(hex))
                if hex.len() == 32
                    && hex
                        .bytes()
                        .all(|digit| matches!(digit, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                Some(u128::from_str_radix(hex, 16).ok()?)
            }
            Some(_) => return None,
        };
        Some(Folded {
            waiting,
            pending,
            next_id,
            accounted,
            seal,
        })
    }

    /// Fold one line of the log into `folded`: the one place an event queue's
    /// transitions are defined, applied alike by a writer to what it just
    /// appended and by a reader to what the log grew by. A line about a record
    /// the fold no longer holds folds to nothing, which is what makes a replay
    /// from any checkpoint land on the same state.
    fn apply(&self, folded: &mut Folded, event: Option<Event>, record: &Value) {
        let Some(id) = record_id(record) else {
            return;
        };
        // A line with no event is an older writer's, which logged a record when
        // it was queued and again when it was abandoned or taken back, and never
        // a claim or an answer.
        let event = event.unwrap_or(if id >= folded.next_id {
            Event::Queued
        } else if is_abandoned(record) {
            Event::Abandoned
        } else {
            Event::Attended
        });
        let policy = &self.spec.policy;
        match event {
            Event::Queued => {
                // An id with no successor is one no writer here allocated.
                let Some(after) = id.checked_add(1) else {
                    return;
                };
                if let Some(supersede) = &policy.supersede_on {
                    let admits = supersede
                        .when
                        .as_ref()
                        .is_none_or(|when| when.matches(record));
                    if admits {
                        let key = supersede.key.get(record);
                        folded
                            .waiting
                            .retain(|waiting| supersede.key.get(waiting) != key);
                    }
                }
                folded.waiting.push(record.clone());
                folded.next_id = folded.next_id.max(after);
            }
            Event::Claimed => {
                let Some(at) = folded
                    .waiting
                    .iter()
                    .position(|waiting| record_id(waiting) == Some(id))
                else {
                    return;
                };
                let taken = folded.waiting.remove(at);
                // A blocking record outlives its delivery while it waits for an
                // answer. An abandoned one takes the slot only when nothing holds
                // it, and the abandoned one it displaces goes back among the
                // readable ones rather than being written over.
                if policy.hold_pending
                    && is_blocking(&taken)
                    && (!is_abandoned(&taken) || folded.pending.is_none())
                {
                    if let Some(displaced) = folded.pending.replace(taken) {
                        if is_abandoned(&displaced) {
                            folded.waiting.push(displaced);
                        }
                    }
                }
            }
            Event::Answered => {
                if folded
                    .pending
                    .as_ref()
                    .is_some_and(|held| record_id(held) == Some(id))
                {
                    folded.pending = None;
                }
            }
            Event::Abandoned | Event::Attended => {
                let abandoned = event == Event::Abandoned;
                let shape = &self.shape;
                for held in folded.waiting.iter_mut().chain(folded.pending.iter_mut()) {
                    if record_id(held) == Some(id) {
                        let (_, mut fields) =
                            without_key(held.as_object().cloned().unwrap_or_default(), "abandoned");
                        if abandoned {
                            fields.insert("abandoned".to_owned(), Value::Bool(true));
                        }
                        if let Ok(reshaped) = shape(Value::Object(fields)) {
                            *held = reshaped;
                        }
                    }
                }
            }
        }
    }

    /// The projection brought up to date with every line the log has grown by
    /// since it was stamped, and whether anything was folded.
    ///
    /// A stamped projection that does not seal is read as no projection, so the
    /// whole log is folded rather than its claims trusted. An unstamped one is an
    /// older writer's: every id below its `next_id` is taken at its word, and a
    /// logged id at or past it — a record whose write-back an older writer lost —
    /// is folded in from the log. A log shorter than the stamp was replaced, and
    /// is folded whole. A torn trailing record ends the fold, and the stamp stays
    /// before it until its writer finishes it.
    fn current(&self, transport: &dyn Transport) -> Result<(Folded, bool), QueueError> {
        let queue = &self.spec.name;
        let checkpoint = match &self.spec.policy.projection {
            Some(name) => transport
                .document(queue, name)
                .ok()
                .flatten()
                .and_then(|bytes| self.parse_projection(&bytes))
                .filter(Folded::is_intact),
            None => None,
        };
        let mut floor: Option<u64> = None;
        let (mut folded, mut from) = match checkpoint {
            Some(projection) => match projection.accounted {
                Some(accounted) => (projection, accounted),
                None => {
                    floor = Some(projection.next_id);
                    (projection, 0)
                }
            },
            None => (Folded::default(), 0),
        };
        let mut folded_any = floor.is_some();
        let start = (from > 0).then(|| Position::from_token(from));
        let batch = match transport.read(queue, start.as_ref(), usize::MAX) {
            Ok(batch) => batch,
            Err(TransportError::PastEnd { .. } | TransportError::NotABoundary { .. }) => {
                folded = Folded::default();
                from = 0;
                folded_any = true;
                transport.read(queue, None, usize::MAX)?
            }
            Err(failure) => return Err(failure.into()),
        };
        let mut accounted = from;
        for stored in batch.records {
            accounted = stored.after.token();
            folded_any = true;
            if let Some((event, record)) = self.parse_line(&stored.bytes) {
                if floor.is_some_and(|floor| record_id(&record).is_some_and(|id| id < floor)) {
                    continue;
                }
                self.apply(&mut folded, event, &record);
            }
        }
        folded.accounted = Some(accounted);
        folded.seal();
        Ok((folded, folded_any))
    }

    /// The event queue as it stands, repairing its projection when the log had
    /// grown past it. The repair is a cache write: one that fails costs the next
    /// reader a fold, never an answer, so it is not what this read reports.
    fn snapshot(&self) -> Result<Folded, QueueError> {
        let (folded, folded_any) = self.current(self.transport.as_ref())?;
        if folded_any {
            if let Some(name) = &self.spec.policy.projection {
                let _ = self.transport.replace_document(
                    &self.spec.name,
                    name,
                    folded.render().as_bytes(),
                );
            }
        }
        Ok(folded)
    }

    /// Append what `derive` decides to the log under the queue's exclusive
    /// section, fold each line in, and write the projection as it then stands.
    /// Every mutation of an event queue is this: what `derive` sees is the log as
    /// it is, and nothing lands between the decision and its record. A refusal
    /// `derive` hands back records nothing.
    fn record<T>(
        &self,
        derive: impl FnOnce(&Folded) -> Result<(Vec<(Event, Value)>, T), QueueError>,
    ) -> Result<(T, Vec<(Value, Position)>), QueueError> {
        let queue = &self.spec.name;
        self.within_section(|inner| {
            let (mut folded, _) = self.current(inner)?;
            let (events, extra) = derive(&folded)?;
            let mut recorded = Vec::new();
            for (event, record) in events {
                let position = inner.append(queue, &Self::frame(event, &record))?;
                self.apply(&mut folded, Some(event), &record);
                folded.accounted = Some(position.token());
                recorded.push((record, position));
            }
            folded.seal();
            if let Some(name) = &self.spec.policy.projection {
                inner.replace_document(queue, name, folded.render().as_bytes())?;
            }
            Ok((extra, recorded))
        })
    }

    /// Run `body` inside the queue's exclusive section and hand back what it
    /// answered: the one place a section's answer is carried out of it.
    fn within_section<T>(
        &self,
        body: impl FnOnce(&dyn Transport) -> Result<T, QueueError>,
    ) -> Result<T, QueueError> {
        let mut body = Some(body);
        let mut outcome = None;
        self.transport.exclusive(&self.spec.name, &mut |inner| {
            if let Some(body) = body.take() {
                outcome = Some(body(inner));
            }
            Ok(())
        })?;
        outcome.unwrap_or_else(|| {
            Err(QueueError::Transport(TransportError::Backend {
                transport: "queue".to_owned(),
                detail: "the transport's exclusive section never ran its body".to_owned(),
            }))
        })
    }

    /// Where each record's latest claim was recorded, read off the whole log.
    fn claim_positions(&self) -> Result<BTreeMap<u64, Position>, QueueError> {
        let batch = self.transport.read(&self.spec.name, None, usize::MAX)?;
        let mut positions = BTreeMap::new();
        let mut next_id = 0;
        for stored in batch.records {
            if let Some((event, record)) = self.parse_line(&stored.bytes) {
                let Some(id) = record_id(&record) else {
                    continue;
                };
                if event.is_none() && id >= next_id || event == Some(Event::Queued) {
                    next_id = next_id.max(id.saturating_add(1));
                }
                if event == Some(Event::Claimed) {
                    positions.insert(id, stored.after);
                }
            }
        }
        Ok(positions)
    }

    /// Every record of a plain queue after `from`, parsed, with the position
    /// after each; a line that is not JSON is passed over.
    fn plain_after(
        &self,
        transport: &dyn Transport,
        from: Option<&Position>,
    ) -> Result<Vec<(Value, Position)>, QueueError> {
        let batch = transport.read(&self.spec.name, from, usize::MAX)?;
        Ok(batch
            .records
            .into_iter()
            .filter_map(|stored| {
                serde_json::from_slice::<Value>(&stored.bytes)
                    .ok()
                    .map(|record| (record, stored.after))
            })
            .collect())
    }

    fn claimable(&self, record: &Value) -> bool {
        self.spec
            .claims
            .as_ref()
            .is_none_or(|claims| claims.matches(record))
    }

    /// Judge `record` by the queue's validators, validate it against the queue's
    /// schema, and append it.
    ///
    /// The validators judge the record as it was offered, before anything else
    /// happens to it. On an event queue the record is given the next id — one
    /// past the highest the log has queued — superseding what its policy says it
    /// supersedes; on a numbered plain queue it is given the number of records
    /// before it. Both are allocated under the queue's exclusive section, so two
    /// writers never take one id. The record is validated against the schema as
    /// it will be written, id included.
    ///
    /// # Errors
    ///
    /// [`QueueError::Refused`] or [`QueueError::Unjudged`] for a record the
    /// validators did not pass, a record the schema refuses, a record an id
    /// cannot be set on, the last id already taken, or a transport failure.
    /// Nothing is appended.
    pub fn push(&self, record: Value) -> Result<Pushed<Value>, QueueError> {
        let context = ValidationContext::new(self.spec.name.clone());
        QueueError::of_verdict(&self.spec.name, self.validators.judge(&record, &context))?;
        self.push_judged(record)
    }

    /// [`push`](Self::push), for a record its validators have already judged.
    pub(crate) fn push_judged(&self, record: Value) -> Result<Pushed<Value>, QueueError> {
        if self.spec.policy.keeps_events() {
            let ((), mut recorded) = self.record(|folded| {
                if folded.next_id == u64::MAX {
                    return Err(QueueError::NoIdLeft {
                        queue: self.spec.name.clone(),
                        last: u64::MAX - 1,
                    });
                }
                let numbered = self.reshape(self.with_id(record, folded.next_id)?)?;
                self.check(&numbered)?;
                Ok((vec![(Event::Queued, numbered)], ()))
            })?;
            let (record, position) = recorded.pop().ok_or_else(|| QueueError::Shape {
                queue: self.spec.name.clone(),
                why: "the push recorded nothing".to_owned(),
            })?;
            let id = record_id(&record);
            return Ok(Pushed {
                record,
                position,
                id,
            });
        }
        if !self.spec.numbered {
            let record = self.reshape(record)?;
            self.check(&record)?;
            let line = serde_json::to_vec(&record).unwrap_or_default();
            let position = self.transport.append(&self.spec.name, &line)?;
            let id = record_id(&record);
            return Ok(Pushed {
                record,
                position,
                id,
            });
        }
        self.within_section(|inner| {
            let count = inner.read(&self.spec.name, None, usize::MAX)?.records.len() as u64;
            let numbered = self.reshape(self.with_id(record, count)?)?;
            self.check(&numbered)?;
            let line = serde_json::to_vec(&numbered).unwrap_or_default();
            let position = inner.append(&self.spec.name, &line)?;
            Ok(Pushed {
                record: numbered,
                position,
                id: Some(count),
            })
        })
    }

    /// Claim the next record for `consumer`, recording the claim before handing
    /// the record over.
    ///
    /// On an event queue the claim is queue-wide: a blocking record first where
    /// the policy says so, then the oldest nobody abandoned, then an abandoned one
    /// — its text is still what a reader reads it for. A blocking record claimed
    /// under `hold_pending` takes the pending slot. On a plain queue the claim is
    /// `consumer`'s: the first record after its cursor that the declaration's
    /// `claims` admits, and the cursor moves just past it.
    ///
    /// # Errors
    ///
    /// A transport failure. Nothing is recorded.
    pub fn claim(&self, consumer: &ConsumerName) -> Result<Option<Claimed<Value>>, QueueError> {
        if self.spec.policy.keeps_events() {
            let blocking_first = self.spec.policy.blocking_first;
            let ((), mut recorded) = self.record(|folded| {
                let unabandoned = folded
                    .waiting
                    .iter()
                    .position(|record| !is_abandoned(record));
                let next = if blocking_first {
                    folded
                        .waiting
                        .iter()
                        .position(|record| is_blocking(record) && !is_abandoned(record))
                        .or(unabandoned)
                } else {
                    unabandoned
                }
                .unwrap_or(0);
                Ok((
                    folded
                        .waiting
                        .get(next)
                        .map(|record| vec![(Event::Claimed, record.clone())])
                        .unwrap_or_default(),
                    (),
                ))
            })?;
            return Ok(recorded.pop().map(|(record, position)| Claimed {
                id: record_id(&record),
                record,
                position,
            }));
        }
        self.within_section(|inner| {
            let cursor = inner.cursor(&self.spec.name, consumer)?;
            let found = self
                .plain_after(inner, cursor.as_ref())?
                .into_iter()
                .find(|(record, _)| self.claimable(record));
            if let Some((_, after)) = &found {
                inner.commit(&self.spec.name, consumer, after)?;
            }
            Ok(found.map(|(record, position)| Claimed {
                id: record_id(&record),
                record,
                position,
            }))
        })
    }

    /// Release the pending slot `claimed` holds, recording that the answer at
    /// `reply_position` — on the declaration's `answers` queue — answered it.
    /// Answers `false`, recording nothing, when the slot no longer holds it.
    ///
    /// # Errors
    ///
    /// [`QueueError::NoReply`] when no record on the `answers` queue ends at
    /// `reply_position`, or the queue declares no `answers` queue, with nothing
    /// recorded; [`QueueError::NotAnEventQueue`] on a plain queue, or a
    /// transport failure.
    pub fn answer(
        &self,
        claimed: &Claimed<Value>,
        reply_position: &Position,
    ) -> Result<bool, QueueError> {
        self.event_queue("pending slot to answer")?;
        self.reply_at(reply_position)?;
        let id = claimed.id;
        let (answered, _) = self.record(|folded| {
            Ok(match &folded.pending {
                Some(held) if id.is_some() && record_id(held) == id => {
                    (vec![(Event::Answered, held.clone())], true)
                }
                _ => (Vec::new(), false),
            })
        })?;
        Ok(answered)
    }

    /// Refuse `reply_position` unless a record on the declaration's `answers`
    /// queue ends there.
    fn reply_at(&self, reply_position: &Position) -> Result<(), QueueError> {
        let Some(answers) = &self.spec.answers else {
            return Err(QueueError::NoReply {
                queue: self.spec.name.clone(),
                why: "it declares no answers queue for a reply to be on; answer_pending releases its slot".to_owned(),
            });
        };
        let batch = self.transport.read(answers, None, usize::MAX)?;
        if batch
            .records
            .iter()
            .any(|stored| stored.after == *reply_position)
        {
            return Ok(());
        }
        Err(QueueError::NoReply {
            queue: self.spec.name.clone(),
            why: format!(
                "no reply on {answers} ends at position {reply_position}; append the reply before answering with it"
            ),
        })
    }

    /// Release whatever the pending slot holds, abandoned or not, and hand back
    /// what was released.
    ///
    /// # Errors
    ///
    /// [`QueueError::NotAnEventQueue`] on a plain queue, or a transport failure.
    pub fn answer_pending(&self) -> Result<Option<Value>, QueueError> {
        self.event_queue("pending slot to answer")?;
        let (_, mut recorded) = self.record(|folded| {
            Ok((
                folded
                    .pending
                    .iter()
                    .map(|held| (Event::Answered, held.clone()))
                    .collect(),
                (),
            ))
        })?;
        Ok(recorded.pop().map(|(record, _)| record))
    }

    /// The record the pending slot holds, where somebody is still listening for
    /// its answer, with where it was claimed. `consumer` is who asks; the slot is
    /// the queue's.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn pending(&self, consumer: &ConsumerName) -> Result<Option<Claimed<Value>>, QueueError> {
        let _ = consumer;
        Ok(self
            .held()?
            .filter(|claimed| !is_abandoned(&claimed.record)))
    }

    /// Whatever the pending slot holds, abandoned or not, with where it was
    /// claimed. `None` on a plain queue, which has no slot.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn held(&self) -> Result<Option<Claimed<Value>>, QueueError> {
        if !self.spec.policy.keeps_events() {
            return Ok(None);
        }
        let Some(record) = self.snapshot()?.pending else {
            return Ok(None);
        };
        let id = record_id(&record);
        let position = id
            .and_then(|id| self.claim_positions().ok()?.get(&id).copied())
            .unwrap_or(Position::from_token(0));
        Ok(Some(Claimed {
            record,
            position,
            id,
        }))
    }

    /// Answer the pending record claimed at `position` with the reply at
    /// `reply_position`.
    ///
    /// # Errors
    ///
    /// [`QueueError::NotPending`] when nothing is pending, or the pending record
    /// was not claimed at `position`; [`QueueError::NoReply`] as
    /// [`answer`](Self::answer); a plain queue, or a transport failure.
    pub fn answer_at(
        &self,
        position: &Position,
        reply_position: &Position,
    ) -> Result<Claimed<Value>, QueueError> {
        let claimed = self.pending_at(position)?;
        self.answer(&claimed, reply_position)?;
        Ok(claimed)
    }

    /// The pending record, refused unless it was claimed at `position`.
    ///
    /// # Errors
    ///
    /// As [`answer_at`](Self::answer_at).
    pub fn pending_at(&self, position: &Position) -> Result<Claimed<Value>, QueueError> {
        self.event_queue("pending slot to answer")?;
        match self.held()? {
            None => Err(QueueError::NotPending {
                queue: self.spec.name.clone(),
                why: "nothing is pending, so there is nothing to answer".to_owned(),
            }),
            Some(held) if held.position != *position => Err(QueueError::NotPending {
                queue: self.spec.name.clone(),
                why: format!(
                    "the pending record was claimed at position {}, not {position}",
                    held.position
                ),
            }),
            Some(held) => Ok(held),
        }
    }

    /// The records waiting to be claimed, oldest first: on an event queue every
    /// waiting record, abandoned ones included; on a plain queue the records
    /// after the default consumer's cursor that a claim would hand out.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn waiting(&self) -> Result<Vec<Value>, QueueError> {
        if self.spec.policy.keeps_events() {
            return Ok(self.snapshot()?.waiting);
        }
        let cursor = self
            .transport
            .cursor(&self.spec.name, &ConsumerName::default_consumer())?;
        Ok(self
            .plain_after(self.transport.as_ref(), cursor.as_ref())?
            .into_iter()
            .filter(|(record, _)| self.claimable(record))
            .map(|(record, _)| record)
            .collect())
    }

    /// How many waiting records somebody is still owed a reading of: the
    /// unabandoned ones.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn unread_count(&self) -> Result<usize, QueueError> {
        Ok(self
            .waiting()?
            .iter()
            .filter(|record| !is_abandoned(record))
            .count())
    }

    /// Mark every waiting or pending record whose id is in `ids` as abandoned —
    /// nobody is listening for its answer now — and hand back what was marked.
    ///
    /// Marked, never removed: the text stays readable and claimable, the pending
    /// slot keeps what it holds, and what gives way is the record's claim on the
    /// unread count and on being reported pending. A record already marked is not
    /// marked twice.
    ///
    /// # Errors
    ///
    /// [`QueueError::NotAnEventQueue`] on a plain queue, or a transport failure.
    pub fn abandon(&self, ids: &[u64]) -> Result<Vec<Value>, QueueError> {
        self.event_queue("abandoned records")?;
        let ((), recorded) = self.record(|folded| {
            Ok((
                folded
                    .waiting
                    .iter()
                    .chain(folded.pending.iter())
                    .filter(|record| {
                        record_id(record).is_some_and(|id| ids.contains(&id))
                            && !is_abandoned(record)
                    })
                    .map(|record| (Event::Abandoned, self.with_abandoned(record, true)))
                    .collect(),
                (),
            ))
        })?;
        Ok(recorded.into_iter().map(|(record, _)| record).collect())
    }

    /// Take back over every abandoned record `asker` raised, and hand back what
    /// was taken. Scoped to the asker: a record another asker raised, or one
    /// naming no asker, is taken by nobody.
    ///
    /// # Errors
    ///
    /// [`QueueError::NotAnEventQueue`] on a plain queue, or a transport failure.
    pub fn attend(&self, asker: &Asker) -> Result<Vec<Value>, QueueError> {
        self.event_queue("abandoned records")?;
        let ((), recorded) = self.record(|folded| {
            Ok((
                folded
                    .waiting
                    .iter()
                    .chain(folded.pending.iter())
                    .filter(|record| {
                        is_abandoned(record) && asker_of(record) == Some(asker.as_str())
                    })
                    .map(|record| (Event::Attended, self.with_abandoned(record, false)))
                    .collect(),
                (),
            ))
        })?;
        Ok(recorded.into_iter().map(|(record, _)| record).collect())
    }

    /// Whether the record `id` is marked abandoned now, read off the whole log
    /// rather than the fold, so a record a claim took out of the fold still
    /// answers: its latest `abandoned` or `attended` line decides, and a record
    /// never marked is not abandoned.
    ///
    /// # Errors
    ///
    /// [`QueueError::NotAnEventQueue`] on a plain queue, or a transport failure.
    pub(crate) fn is_marked_abandoned(&self, id: u64) -> Result<bool, QueueError> {
        self.event_queue("abandoned records")?;
        let batch = self.transport.read(&self.spec.name, None, usize::MAX)?;
        let mut abandoned = false;
        for stored in batch.records {
            let Some((event, record)) = self.parse_line(&stored.bytes) else {
                continue;
            };
            if record_id(&record) != Some(id) {
                continue;
            }
            abandoned = match event {
                Some(Event::Abandoned) => true,
                Some(Event::Attended) => false,
                Some(Event::Queued) | None => is_abandoned(&record),
                Some(Event::Claimed | Event::Answered) => abandoned,
            };
        }
        Ok(abandoned)
    }

    /// Mark the record `id` abandoned — or attended again — whether or not the
    /// fold still holds it, and answer whether a mark was recorded: none is
    /// when the record already stands so, or the log never queued it.
    ///
    /// # Errors
    ///
    /// [`QueueError::NotAnEventQueue`] on a plain queue, or a transport failure.
    pub(crate) fn mark(&self, id: u64, abandoned: bool) -> Result<bool, QueueError> {
        if self.is_marked_abandoned(id)? == abandoned {
            return Ok(false);
        }
        let batch = self.transport.read(&self.spec.name, None, usize::MAX)?;
        let Some(logged) = batch
            .records
            .iter()
            .filter_map(|stored| self.parse_line(&stored.bytes))
            .find(|(event, record)| {
                record_id(record) == Some(id) && matches!(event, Some(Event::Queued) | None)
            })
            .map(|(_, record)| record)
        else {
            return Ok(false);
        };
        let event = if abandoned {
            Event::Abandoned
        } else {
            Event::Attended
        };
        let ((), recorded) = self.record(|folded| {
            let current = folded
                .waiting
                .iter()
                .chain(folded.pending.iter())
                .find(|held| record_id(held) == Some(id))
                .cloned()
                .unwrap_or(logged);
            Ok((vec![(event, self.with_abandoned(&current, abandoned))], ()))
        })?;
        Ok(!recorded.is_empty())
    }

    /// The registry the queue's schema is checked against.
    pub(crate) fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// Every line of the log after `from`, parsed, with the position after each:
    /// on an event queue each line is an event (`{"event": ..., ...record}`).
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn log(&self, from: Option<&Position>) -> Result<Vec<(Value, Position)>, QueueError> {
        self.plain_after(self.transport.as_ref(), from)
    }

    /// The queue's change token.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn fingerprint(&self) -> Result<Fingerprint, QueueError> {
        Ok(self.transport.fingerprint(&self.spec.name)?)
    }

    /// Wait up to `timeout` for the queue to move from `since`.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn wait_for_change(
        &self,
        since: &Fingerprint,
        timeout: Duration,
    ) -> Result<Changed, QueueError> {
        Ok(self
            .transport
            .wait_for_change(&self.spec.name, since, timeout)?)
    }

    /// Everything `status` reports of this queue.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn status(&self) -> Result<QueueStatus, QueueError> {
        let records = self
            .transport
            .read(&self.spec.name, None, usize::MAX)?
            .records
            .len() as u64;
        let mut cursors = BTreeMap::new();
        for consumer in &self.spec.consumers {
            cursors.insert(
                consumer.clone(),
                self.transport.cursor(&self.spec.name, consumer)?,
            );
        }
        let events = self.spec.policy.keeps_events();
        let (waiting, pending, pending_position) = if events {
            let held = self.held()?;
            let waiting = self.snapshot()?.waiting;
            let position = held.as_ref().map(|claimed| claimed.position);
            (waiting, held.map(|claimed| claimed.record), position)
        } else {
            (self.waiting()?, None, None)
        };
        let abandoned: Vec<Value> = waiting
            .iter()
            .chain(pending.iter())
            .filter(|record| is_abandoned(record))
            .cloned()
            .collect();
        let unread = waiting
            .iter()
            .filter(|record| !is_abandoned(record))
            .count() as u64;
        Ok(QueueStatus {
            queue: self.spec.name.clone(),
            events,
            records,
            waiting,
            pending,
            pending_position,
            abandoned,
            unread,
            cursors,
        })
    }
}

/// A queue of `M`: every record is validated against `M::SCHEMA` before it is
/// appended, and written in `M`'s own field order.
pub struct Queue<M: Message> {
    raw: RawQueue,
    validators: Validators<M>,
    record: PhantomData<fn() -> M>,
}

impl<M: Message> Clone for Queue<M> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            validators: self.validators.clone(),
            record: PhantomData,
        }
    }
}

impl<M: Message> fmt::Debug for Queue<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Queue")
            .field("schema", &M::SCHEMA)
            .field("raw", &self.raw)
            .finish()
    }
}

fn typed<M: Message>(queue: &QueueName, value: Value) -> Result<M, QueueError> {
    serde_json::from_value(value).map_err(|failure| QueueError::Shape {
        queue: queue.clone(),
        why: failure.to_string(),
    })
}

impl<M: Message + 'static> Queue<M> {
    /// The queue `spec` declares, of `M`, kept on `transport`. The spec's schema
    /// becomes `M::SCHEMA`, registered from `M`'s own JSON Schema.
    ///
    /// # Errors
    ///
    /// [`QueueError::Unregistered`] when `M`'s schema cannot be registered.
    pub fn open(transport: Arc<dyn Transport>, mut spec: QueueSpec) -> Result<Self, QueueError> {
        let mut registry = Registry::new();
        registry
            .register::<M>()
            .map_err(|refusal| QueueError::Unregistered {
                queue: spec.name.clone(),
                refusal: Box::new(refusal),
            })?;
        spec.schema = Some(M::SCHEMA);
        let mut raw = RawQueue::open(transport, spec, Arc::new(registry));
        raw.shape = shape_of::<M>();
        Ok(Self {
            raw,
            validators: Validators::new(),
            record: PhantomData,
        })
    }

    /// The same queue, judging every record pushed onto it by `validators`
    /// before anything is appended.
    #[must_use]
    pub fn with_validators(mut self, validators: Validators<M>) -> Self {
        self.validators = validators;
        self
    }

    /// The untyped queue underneath.
    #[must_use]
    pub fn raw(&self) -> &RawQueue {
        &self.raw
    }

    fn value(&self, record: &M) -> Result<Value, QueueError> {
        serde_json::to_value(record).map_err(|failure| QueueError::Shape {
            queue: self.raw.spec.name.clone(),
            why: failure.to_string(),
        })
    }

    fn claimed(&self, claimed: Claimed<Value>) -> Result<Claimed<M>, QueueError> {
        Ok(Claimed {
            record: typed(&self.raw.spec.name, claimed.record)?,
            position: claimed.position,
            id: claimed.id,
        })
    }

    /// Judge `record` by the queue's validators, validate it against
    /// `M::SCHEMA` and append it; see [`RawQueue::push`].
    ///
    /// # Errors
    ///
    /// As [`RawQueue::push`].
    pub fn push(&self, record: &M) -> Result<Pushed<M>, QueueError> {
        let queue = &self.raw.spec.name;
        let context = ValidationContext::new(queue.clone());
        QueueError::of_verdict(queue, self.validators.judge(record, &context))?;
        let pushed = self.raw.push(self.value(record)?)?;
        Ok(Pushed {
            record: typed(&self.raw.spec.name, pushed.record)?,
            position: pushed.position,
            id: pushed.id,
        })
    }

    /// Claim the next record for `consumer`; see [`RawQueue::claim`].
    ///
    /// # Errors
    ///
    /// As [`RawQueue::claim`].
    pub fn claim(&self, consumer: &ConsumerName) -> Result<Option<Claimed<M>>, QueueError> {
        self.raw
            .claim(consumer)?
            .map(|claimed| self.claimed(claimed))
            .transpose()
    }

    /// Release the pending slot `claimed` holds; see [`RawQueue::answer`].
    ///
    /// # Errors
    ///
    /// As [`RawQueue::answer`].
    pub fn answer(
        &self,
        claimed: &Claimed<M>,
        reply_position: &Position,
    ) -> Result<bool, QueueError> {
        let untyped = Claimed {
            record: self.value(&claimed.record)?,
            position: claimed.position,
            id: claimed.id,
        };
        self.raw.answer(&untyped, reply_position)
    }

    /// The record pending an answer; see [`RawQueue::pending`].
    ///
    /// # Errors
    ///
    /// As [`RawQueue::pending`].
    pub fn pending(&self, consumer: &ConsumerName) -> Result<Option<Claimed<M>>, QueueError> {
        self.raw
            .pending(consumer)?
            .map(|claimed| self.claimed(claimed))
            .transpose()
    }

    /// The records waiting; see [`RawQueue::waiting`].
    ///
    /// # Errors
    ///
    /// As [`RawQueue::waiting`].
    pub fn waiting(&self) -> Result<Vec<M>, QueueError> {
        self.raw
            .waiting()?
            .into_iter()
            .map(|record| typed(&self.raw.spec.name, record))
            .collect()
    }

    /// How many waiting records are still owed a reading; see
    /// [`RawQueue::unread_count`].
    ///
    /// # Errors
    ///
    /// As [`RawQueue::unread_count`].
    pub fn unread_count(&self) -> Result<usize, QueueError> {
        self.raw.unread_count()
    }
}

/// How long a subscription's claims stay its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lifetime {
    /// This listener alone: it adopts nothing, and nothing adopts what it
    /// raised.
    Session,
    /// A listener of an asker: it takes back what an earlier listener of the
    /// same asker abandoned, and a later one takes back what it abandons.
    Durable(Asker),
}

/// A listener on one event queue: what it raises and claims is its own until it
/// [`abandon`](Self::abandon)s them.
#[derive(Debug)]
pub struct Subscription {
    queue: RawQueue,
    consumer: ConsumerName,
    lifetime: Lifetime,
    /// The ids this listener raised, claimed or took back.
    touched: Mutex<Vec<u64>>,
}

impl Subscription {
    /// Start listening. A durable listener first takes back everything its asker
    /// left abandoned, and those become its own.
    ///
    /// # Errors
    ///
    /// [`QueueError::NotAnEventQueue`] on a plain queue, or a transport failure.
    pub fn open(
        queue: RawQueue,
        consumer: ConsumerName,
        lifetime: Lifetime,
    ) -> Result<Self, QueueError> {
        queue.event_queue("listeners that abandon and attend")?;
        let mut touched = Vec::new();
        if let Lifetime::Durable(asker) = &lifetime {
            touched.extend(queue.attend(asker)?.iter().filter_map(record_id));
        }
        Ok(Self {
            queue,
            consumer,
            lifetime,
            touched: Mutex::new(touched),
        })
    }

    /// The queue listened on.
    #[must_use]
    pub fn queue(&self) -> &RawQueue {
        &self.queue
    }

    /// Who listens.
    #[must_use]
    pub fn consumer(&self) -> &ConsumerName {
        &self.consumer
    }

    /// How long its claims stay its own.
    #[must_use]
    pub fn lifetime(&self) -> &Lifetime {
        &self.lifetime
    }

    fn touch(&self, id: Option<u64>) {
        if let Some(id) = id {
            self.touched
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(id);
        }
    }

    /// Raise `record` through this listener: stamped with its asker when it is
    /// durable, and its own to abandon.
    ///
    /// # Errors
    ///
    /// As [`RawQueue::push`].
    pub fn push(&self, record: Value) -> Result<Pushed<Value>, QueueError> {
        let record = match (&self.lifetime, record) {
            (Lifetime::Durable(asker), Value::Object(mut fields)) => {
                fields.insert("asker".to_owned(), Value::String(asker.as_str().to_owned()));
                Value::Object(fields)
            }
            (_, record) => record,
        };
        let pushed = self.queue.push(record)?;
        self.touch(pushed.id);
        Ok(pushed)
    }

    /// Claim the next record through this listener, making it its own.
    ///
    /// # Errors
    ///
    /// As [`RawQueue::claim`].
    pub fn claim(&self) -> Result<Option<Claimed<Value>>, QueueError> {
        let claimed = self.queue.claim(&self.consumer)?;
        if let Some(claimed) = &claimed {
            self.touch(claimed.id);
        }
        Ok(claimed)
    }

    /// Mark everything this listener raised, claimed or took back, and has not
    /// seen answered, as abandoned — kept, uncounted, still readable — and hand
    /// back what was marked.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn abandon(&self) -> Result<Vec<Value>, QueueError> {
        let touched = self
            .touched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.queue.abandon(&touched)
    }
}
