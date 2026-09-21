//! A layout declared as data: the document a program that owns a protocol
//! publishes, in a schema bundle's `layouts` member, so a bus configuration
//! links its queues, operations, authors and record preparation rather than a
//! build linking them as code (`docs/queues.md`, `docs/schema-links.md`).
//!
//! A [`LayoutDocument`] is the one declaration of that document's shape: a
//! publisher builds its document through it and a reader reads one with it,
//! each held to the rules [`LayoutDocument::new`] states, so a document a
//! publisher could build is one a consumer reads. A [`LinkedLayout`] is a
//! document bound to the schemas its bundles carry: the [`Layout`] a
//! configuration's `profile` resolves against, exactly as it resolves against
//! one a program compiled in — and [`Layouts::with_linked`](crate::Layouts::with_linked)
//! lets a compiled-in layout of the same name keep its own.
//!
//! Preparation is a list of steps per queue, run in order over what was
//! offered to that queue. Each step may carry a `when` — the core's
//! [`Predicate`] over the record as the steps before it left it — and is
//! passed over when that does not hold:
//!
//! | step | what it does |
//! | --- | --- |
//! | [`stamp`](StampStep) | sets a member to the current time in epoch milliseconds, when it is absent |
//! | [`rename`](RenameStep) | moves a member to another name, dropped when that name is already there |
//! | [`check`](CheckStep) | refuses a record, or the member at a path, that a registered schema refuses |
//! | [`version`](VersionStep) | reads a version in its read-set as the version it names, and requires that version where a predicate holds |
//! | [`grant`](GrantStep) | refuses an author the layout does not declare, and each op word — found at a path in each item of a list, or named — the author is not granted |
//! | [`route`](RouteStep) | splits the record onto several queues by the members it carries, each a projection of those members |
//!
//! Every refusal is in the layout's own words: each step that refuses carries
//! its text, with `{placeholders}` the step fills in.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::author::{Allowlist, Author, OpWord, Operation as _};
use crate::config::{Layout, RefusalReason};
use crate::queue::{FieldPath, Predicate, QueueSpec};
use crate::schema::{CheckError, Registry, SchemaId};
use crate::transport::QueueName;

/// A layout's name, as a configuration's `profile` gives it.
const NAME_PATTERN: &str = "^[a-z][a-z0-9-]{0,63}$";

/// A layout's name: a lowercase word of up to 64 letters, digits and hyphens,
/// starting with a letter. Only [`new`](Self::new) and a read make one, so a
/// name outside the grammar is not representable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LayoutName(String);

impl LayoutName {
    /// `text` as a layout name.
    ///
    /// # Errors
    ///
    /// The refusal, for text outside `^[a-z][a-z0-9-]{0,63}$`.
    pub fn new(text: &str) -> Result<Self, String> {
        let is_a_word = text.len() <= 64
            && text.starts_with(|c: char| c.is_ascii_lowercase())
            && text
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if is_a_word {
            Ok(Self(text.to_owned()))
        } else {
            Err(format!("{text:?} is not a layout name ({NAME_PATTERN})"))
        }
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LayoutName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<&str> for LayoutName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl<'de> Deserialize<'de> for LayoutName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for LayoutName {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("LayoutName")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A layout's name: a lowercase word of up to 64 letters, digits and hyphens.",
            "pattern": NAME_PATTERN
        })
    }
}

/// A top-level member of a record, by name: text with something other than
/// whitespace in it. Only [`new`](Self::new) and a read make one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MemberName(String);

impl MemberName {
    /// `text` as a member name.
    ///
    /// # Errors
    ///
    /// The refusal, for blank text.
    pub fn new(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            Err(format!("{text:?} is not a member name: it is blank"))
        } else {
            Ok(Self(text.to_owned()))
        }
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MemberName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for MemberName {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("MemberName")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A top-level member of a record, by name.",
            "pattern": ".*\\S.*"
        })
    }
}

/// A list holding at least one item. Only [`new`](Self::new) and a read make
/// one, so an empty list is not representable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct NonEmpty<T>(Vec<T>);

impl<T> NonEmpty<T> {
    /// `items`, when there is at least one.
    ///
    /// # Errors
    ///
    /// The refusal, for an empty list.
    pub fn new(items: Vec<T>) -> Result<Self, String> {
        if items.is_empty() {
            Err("is empty; it holds at least one".to_owned())
        } else {
            Ok(Self(items))
        }
    }
}

impl<T> std::ops::Deref for NonEmpty<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for NonEmpty<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Vec::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl<T: JsonSchema> JsonSchema for NonEmpty<T> {
    fn schema_name() -> Cow<'static, str> {
        Cow::Owned(format!("NonEmpty_{}", T::schema_name()))
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = Vec::<T>::json_schema(generator);
        schema.insert("minItems".to_owned(), Value::from(1));
        schema
    }

    fn inline_schema() -> bool {
        true
    }
}

/// A layout declared as data: everything a compiled-in [`Layout`] declares as
/// code — its name, queues, operation vocabulary, authors and how an offer to
/// each queue is prepared — as one JSON document.
///
/// Its fields are held to the rules [`new`](Self::new) states wherever one is
/// made — read from JSON or built — so a document that names a queue twice, an
/// op that is not one, or a step routing to a queue it does not declare is not
/// representable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "LayoutDocumentWire", into = "LayoutDocumentWire")]
pub struct LayoutDocument {
    name: LayoutName,
    description: Option<String>,
    queues: NonEmpty<QueueSpec>,
    operations: Vec<OpWord>,
    authors: BTreeMap<Author, LayoutAuthor>,
    prepare: BTreeMap<QueueName, Vec<PrepareStep>>,
}

/// The wire shape of a [`LayoutDocument`], before what its keys say of one
/// another is checked.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "LayoutDocument")]
struct LayoutDocumentWire {
    /// The name a configuration's `profile` gives.
    name: LayoutName,
    /// What the layout is, for a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// The queues it declares, each with its policy and the keys beside it,
    /// as a configuration's queue keys name them: at least one.
    queues: NonEmpty<QueueSpec>,
    /// Its operation vocabulary: every op word an author may be granted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    operations: Vec<OpWord>,
    /// Its authors, and what each may issue. A configuration may narrow what a
    /// layout grants one of these and never widen it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    authors: BTreeMap<Author, LayoutAuthor>,
    /// How an offer to each queue is prepared, by the queue it is offered to:
    /// steps run in order. A queue named nowhere here keeps what is offered.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    prepare: BTreeMap<QueueName, Vec<PrepareStep>>,
}

impl TryFrom<LayoutDocumentWire> for LayoutDocument {
    type Error = String;

    fn try_from(wire: LayoutDocumentWire) -> Result<Self, String> {
        Self::new(
            wire.name,
            wire.description,
            wire.queues,
            wire.operations,
            wire.authors,
            wire.prepare,
        )
    }
}

impl From<LayoutDocument> for LayoutDocumentWire {
    fn from(document: LayoutDocument) -> Self {
        Self {
            name: document.name,
            description: document.description,
            queues: document.queues,
            operations: document.operations,
            authors: document.authors,
            prepare: document.prepare,
        }
    }
}

impl JsonSchema for LayoutDocument {
    fn schema_name() -> Cow<'static, str> {
        LayoutDocumentWire::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        LayoutDocumentWire::json_schema(generator)
    }
}

/// One author a layout declares. On the wire `{"every_op": true}` or
/// `{"capabilities": [...]}`, each with optional `refusals`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LayoutAuthorWire", into = "LayoutAuthorWire")]
pub struct LayoutAuthor {
    /// What it may issue.
    pub grants: Grants,
    /// Why each operation it is not granted is refused, by op word.
    pub refusals: BTreeMap<OpWord, RefusalReason>,
}

/// What a layout's author may issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grants {
    /// Every operation of the vocabulary: the author a layout trusts with
    /// everything.
    EveryOp,
    /// These operations.
    Only(Vec<OpWord>),
}

/// The wire shape of a [`LayoutAuthor`], before its grant form is checked.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "LayoutAuthor")]
struct LayoutAuthorWire {
    /// Granted every operation of the vocabulary — the one a layout trusts
    /// with everything. Exclusive of `capabilities`.
    #[serde(default, skip_serializing_if = "is_false")]
    every_op: bool,
    /// The operations it may issue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    capabilities: Vec<OpWord>,
    /// Why each operation it is not granted is refused, by op word.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    refusals: BTreeMap<OpWord, RefusalReason>,
}

impl TryFrom<LayoutAuthorWire> for LayoutAuthor {
    type Error = String;

    fn try_from(wire: LayoutAuthorWire) -> Result<Self, String> {
        let grants = match (wire.every_op, wire.capabilities) {
            (true, capabilities) if !capabilities.is_empty() => {
                return Err("`every_op` grants every op, so it takes no `capabilities`".to_owned())
            }
            (true, _) => Grants::EveryOp,
            (false, capabilities) => Grants::Only(capabilities),
        };
        Ok(Self {
            grants,
            refusals: wire.refusals,
        })
    }
}

impl From<LayoutAuthor> for LayoutAuthorWire {
    fn from(author: LayoutAuthor) -> Self {
        let (every_op, capabilities) = match author.grants {
            Grants::EveryOp => (true, Vec::new()),
            Grants::Only(capabilities) => (false, capabilities),
        };
        Self {
            every_op,
            capabilities,
            refusals: author.refusals,
        }
    }
}

impl JsonSchema for LayoutAuthor {
    fn schema_name() -> Cow<'static, str> {
        LayoutAuthorWire::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        LayoutAuthorWire::json_schema(generator)
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// One step of preparing an offer: on the wire, an object with exactly one
/// key, the step's kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PrepareStep {
    /// Set a member to the current time in epoch milliseconds, when absent.
    Stamp(StampStep),
    /// Move a member to another name.
    Rename(RenameStep),
    /// Refuse what a registered schema refuses.
    Check(CheckStep),
    /// Read a version in a read-set as one version, and require it.
    Version(VersionStep),
    /// Refuse an undeclared author, and each op word it is not granted.
    Grant(GrantStep),
    /// Split the record onto several queues by the members it carries.
    Route(RouteStep),
}

/// `stamp`: set `member` to the current time, in milliseconds since the Unix
/// epoch, when the record has no such member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StampStep {
    /// The top-level member stamped.
    pub member: MemberName,
    /// When to stamp; always when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Predicate>,
}

/// `rename`: move the member `from` to `to`, where it keeps its value. When the
/// record already has `to`, `from` is dropped and `to` kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameStep {
    /// The top-level member moved.
    pub from: MemberName,
    /// The name it moves to.
    pub to: MemberName,
    /// When to rename; always when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Predicate>,
}

/// `check`: refuse the record — or the value at `at` — when it does not conform
/// to the registered schema `schema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckStep {
    /// The schema, registered by a bundle a configuration links.
    pub schema: SchemaId,
    /// The path of the value checked; the whole record when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<FieldPath>,
    /// The refusal: `{why}` is what the schema refused.
    pub refusal: RefusalReason,
    /// When to check; always when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Predicate>,
}

/// `version`: a version at `at` that `reads` names is read — and written — as
/// `value`; and where `required_when` holds, a version other than `value` is
/// refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionStep {
    /// The path of the version.
    pub at: FieldPath,
    /// The version required, and the one a version in `reads` is read as.
    pub value: u64,
    /// Older versions read as `value`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<u64>,
    /// Where `value` is required; nowhere when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_when: Option<Predicate>,
    /// The refusal: `{value}` is the version required, `{found}` the one
    /// there (`none` when absent).
    pub refusal: RefusalReason,
    /// When the step runs at all; always when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Predicate>,
}

/// `grant`: the author named at `author` — or `default_author` where the path
/// holds none — must be one the allowlist declares, and each op word the step
/// finds ([`GrantOps`]) must be one the author is granted.
///
/// On the wire the op words are named by `each` and `op` together, or by
/// `word`, or by neither.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "GrantStepWire", into = "GrantStepWire")]
pub struct GrantStep {
    /// The path of the author.
    pub author: FieldPath,
    /// The author where the path holds none.
    pub default_author: Option<Author>,
    /// The op words checked.
    pub ops: GrantOps,
    /// The refusal of an op the author is not granted: `{op}`, `{author}`, and
    /// `{reason}` — the reason the allowlist records.
    pub refusal: Option<RefusalReason>,
    /// The refusal of a word that is no op of the vocabulary: `{op}`, `{ops}`.
    pub unknown: Option<RefusalReason>,
    /// The refusal of an author the allowlist does not declare: `{author}`,
    /// `{authors}`.
    pub undeclared: Option<RefusalReason>,
    /// The refusal of a record whose author, list or op word is not text where
    /// the step looks: `{why}`.
    pub malformed: Option<RefusalReason>,
    /// When to check; always when absent.
    pub when: Option<Predicate>,
}

/// Which op words a [`GrantStep`] checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantOps {
    /// None: the author alone is checked.
    AuthorOnly,
    /// One op word, outright.
    Word(OpWord),
    /// The op word at `op` in each item of the list at `each`; absent or
    /// `null`, no op words.
    Each {
        /// The path of the list.
        each: FieldPath,
        /// The path of the op word within each item.
        op: FieldPath,
    },
}

/// The wire shape of a [`GrantStep`], before its op form is checked.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "GrantStep")]
struct GrantStepWire {
    /// The path of the author.
    author: FieldPath,
    /// The author where the path holds none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_author: Option<Author>,
    /// The path of a list whose every item names an op word; absent or `null`,
    /// no op words. Named with `op`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    each: Option<FieldPath>,
    /// The path of the op word within each item of `each`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    op: Option<FieldPath>,
    /// One op word checked outright. Exclusive of `each` and `op`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    word: Option<OpWord>,
    /// The refusal of an op the author is not granted: `{op}`, `{author}`, and
    /// `{reason}` — the reason the allowlist records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refusal: Option<RefusalReason>,
    /// The refusal of a word that is no op of the vocabulary: `{op}`, `{ops}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unknown: Option<RefusalReason>,
    /// The refusal of an author the allowlist does not declare: `{author}`,
    /// `{authors}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    undeclared: Option<RefusalReason>,
    /// The refusal of a record whose author, list or op word is not text where
    /// the step looks: `{why}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    malformed: Option<RefusalReason>,
    /// When to check; always when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    when: Option<Predicate>,
}

impl TryFrom<GrantStepWire> for GrantStep {
    type Error = String;

    fn try_from(wire: GrantStepWire) -> Result<Self, String> {
        let ops = match (wire.word, wire.each, wire.op) {
            (None, None, None) => GrantOps::AuthorOnly,
            (Some(word), None, None) => GrantOps::Word(word),
            (None, Some(each), Some(op)) => GrantOps::Each { each, op },
            (Some(_), _, _) => {
                return Err(
                    "`word` names one op outright, so it takes no `each` or `op`".to_owned(),
                )
            }
            (None, Some(_), None) => return Err(
                "`each` names a list, and `op` the op word within each item; this names no `op`"
                    .to_owned(),
            ),
            (None, None, Some(_)) => {
                return Err(
                    "`op` is the op word within each item of `each`; this names no `each`"
                        .to_owned(),
                )
            }
        };
        Ok(Self {
            author: wire.author,
            default_author: wire.default_author,
            ops,
            refusal: wire.refusal,
            unknown: wire.unknown,
            undeclared: wire.undeclared,
            malformed: wire.malformed,
            when: wire.when,
        })
    }
}

impl From<GrantStep> for GrantStepWire {
    fn from(step: GrantStep) -> Self {
        let (word, each, op) = match step.ops {
            GrantOps::AuthorOnly => (None, None, None),
            GrantOps::Word(word) => (Some(word), None, None),
            GrantOps::Each { each, op } => (None, Some(each), Some(op)),
        };
        Self {
            author: step.author,
            default_author: step.default_author,
            each,
            op,
            word,
            refusal: step.refusal,
            unknown: step.unknown,
            undeclared: step.undeclared,
            malformed: step.malformed,
            when: step.when,
        }
    }
}

impl JsonSchema for GrantStep {
    fn schema_name() -> Cow<'static, str> {
        GrantStepWire::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        GrantStepWire::json_schema(generator)
    }
}

/// `route`: the records the offer becomes — the last step of its queue. Each route whose `on` members the
/// record carries — any one of them there, not `null`, and not an empty list —
/// is taken, in the order declared; where none is, the `fallback` route is.
/// A record no route takes, and with no fallback, stays on the queue it was
/// offered to as it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteStep {
    /// The routes, in the order their records are pushed: at least one.
    pub routes: NonEmpty<Route>,
    /// The queue of the route taken when no route's members are there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<QueueName>,
    /// When to route; always when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Predicate>,
}

/// One route of a [`RouteStep`]: the queue a projection of the offer goes to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// The queue the projection is pushed onto.
    pub queue: QueueName,
    /// The members whose presence takes this route: at least one.
    pub on: NonEmpty<MemberName>,
    /// The members the projection carries, in this order, each one the record
    /// has: at least one.
    pub take: NonEmpty<MemberName>,
    /// The member the projection is carried under; at the top level when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<MemberName>,
    /// Members of the routed record stamped with the current time in epoch
    /// milliseconds, beside `under`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stamp: Vec<MemberName>,
}

impl LayoutDocument {
    /// A layout of `queues` under `name`, with its operations, authors and
    /// preparation.
    ///
    /// # Errors
    ///
    /// The key and what is wrong — what the types alone cannot rule out: a
    /// queue declared twice; an `answers` naming no declared queue; an op word
    /// declared twice or blank; an author granted — or given a refusal for — a
    /// word that is no op, or a refusal for an op it is granted; a `prepare`
    /// key naming no declared queue; and a `route` not last among its queue's
    /// steps, routing to a queue not declared, or with a fallback naming no
    /// route.
    pub fn new(
        name: LayoutName,
        description: Option<String>,
        queues: NonEmpty<QueueSpec>,
        operations: Vec<OpWord>,
        authors: BTreeMap<Author, LayoutAuthor>,
        prepare: BTreeMap<QueueName, Vec<PrepareStep>>,
    ) -> Result<Self, String> {
        let document = Self {
            name,
            description,
            queues,
            operations,
            authors,
            prepare,
        };
        document.check()?;
        Ok(document)
    }

    /// The name a configuration's `profile` gives.
    #[must_use]
    pub fn name(&self) -> &LayoutName {
        &self.name
    }

    /// What the layout is, for a person.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The queues it declares.
    #[must_use]
    pub fn queues(&self) -> &[QueueSpec] {
        &self.queues
    }

    /// Its operation vocabulary.
    #[must_use]
    pub fn operations(&self) -> &[OpWord] {
        &self.operations
    }

    /// Its authors, and what each may issue.
    #[must_use]
    pub fn authors(&self) -> &BTreeMap<Author, LayoutAuthor> {
        &self.authors
    }

    /// How an offer to each queue is prepared, by the queue it is offered to.
    #[must_use]
    pub fn prepare(&self) -> &BTreeMap<QueueName, Vec<PrepareStep>> {
        &self.prepare
    }

    /// Hold the document to what its keys say of one another, naming the key
    /// that fails relative to the document.
    fn check(&self) -> Result<(), String> {
        let mut declared: BTreeMap<&QueueName, usize> = BTreeMap::new();
        for (index, queue) in self.queues.iter().enumerate() {
            if let Some(first) = declared.insert(&queue.name, index) {
                return Err(format!(
                    "queues[{index}].name: `{}` is already declared by queues[{first}]",
                    queue.name
                ));
            }
        }
        for (index, queue) in self.queues.iter().enumerate() {
            if let Some(answers) = &queue.answers {
                if !declared.contains_key(answers) {
                    return Err(format!(
                        "queues[{index}].answers: `{answers}` is not a queue this layout declares"
                    ));
                }
            }
        }
        let mut ops = BTreeSet::new();
        for (index, op) in self.operations.iter().enumerate() {
            if op.0.trim().is_empty() {
                return Err(format!("operations[{index}]: is empty"));
            }
            if !ops.insert(op) {
                return Err(format!("operations[{index}]: `{}` is declared twice", op.0));
            }
        }
        let is_op = |key: &str, word: &OpWord| {
            if ops.contains(word) {
                Ok(())
            } else {
                Err(format!(
                    "{key}: `{}` is not an op; the ops are: {}",
                    word.0,
                    self.op_list()
                ))
            }
        };
        for (author, declared_author) in &self.authors {
            if let Grants::Only(capabilities) = &declared_author.grants {
                for word in capabilities {
                    is_op(&format!("authors.{author}.capabilities"), word)?;
                }
            }
            for word in declared_author.refusals.keys() {
                let key = format!("authors.{author}.refusals.{}", word.0);
                is_op(&key, word)?;
                let granted = match &declared_author.grants {
                    Grants::EveryOp => true,
                    Grants::Only(capabilities) => capabilities.contains(word),
                };
                if granted {
                    return Err(format!("{key}: a granted op may not have a refusal"));
                }
            }
        }
        for (queue, steps) in &self.prepare {
            if !declared.contains_key(queue) {
                return Err(format!(
                    "prepare.{queue}: `{queue}` is not a queue this layout declares"
                ));
            }
            for (index, step) in steps.iter().enumerate() {
                step.check(&declared)
                    .map_err(|why| format!("prepare.{queue}[{index}].{why}"))?;
                if matches!(step, PrepareStep::Route(_)) && index + 1 != steps.len() {
                    return Err(format!(
                        "prepare.{queue}[{index}].route: a route is the last step of its queue"
                    ));
                }
            }
        }
        Ok(())
    }

    fn op_list(&self) -> String {
        self.operations
            .iter()
            .map(|op| op.0.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The layout's authors and their grants over its vocabulary.
    #[must_use]
    pub fn allowlist(&self) -> Allowlist<OpWord> {
        let mut allowlist = Allowlist::new(self.operations.iter().cloned());
        for (author, declared) in &self.authors {
            allowlist.declare(author.clone());
            let granted: Vec<OpWord> = match &declared.grants {
                Grants::EveryOp => self.operations.clone(),
                Grants::Only(capabilities) => capabilities.clone(),
            };
            for op in granted {
                allowlist.grant(author.clone(), op);
            }
            for (op, reason) in &declared.refusals {
                allowlist.refuse(author.clone(), op, reason.as_str());
            }
        }
        allowlist
    }

    /// Every schema a `check` step names, where it names it.
    fn checked_schemas(&self) -> impl Iterator<Item = (String, &SchemaId)> {
        self.prepare.iter().flat_map(|(queue, steps)| {
            steps
                .iter()
                .enumerate()
                .filter_map(move |(index, step)| match step {
                    PrepareStep::Check(check) => Some((
                        format!("prepare.{queue}[{index}].check.schema"),
                        &check.schema,
                    )),
                    _ => None,
                })
        })
    }
}

impl PrepareStep {
    fn check(&self, declared: &BTreeMap<&QueueName, usize>) -> Result<(), String> {
        let Self::Route(route) = self else {
            return Ok(());
        };
        for (index, each) in route.routes.iter().enumerate() {
            if !declared.contains_key(&each.queue) {
                return Err(format!(
                    "route.routes[{index}].queue: `{}` is not a queue this layout declares",
                    each.queue
                ));
            }
        }
        if let Some(fallback) = &route.fallback {
            if !route.routes.iter().any(|each| &each.queue == fallback) {
                return Err(format!(
                    "route.fallback: `{fallback}` is the queue of no route"
                ));
            }
        }
        Ok(())
    }

    fn when(&self) -> Option<&Predicate> {
        match self {
            Self::Stamp(step) => step.when.as_ref(),
            Self::Rename(step) => step.when.as_ref(),
            Self::Check(step) => step.when.as_ref(),
            Self::Version(step) => step.when.as_ref(),
            Self::Grant(step) => step.when.as_ref(),
            Self::Route(step) => step.when.as_ref(),
        }
    }
}

/// A [`LayoutDocument`] bound to the schemas the bundles that link it carry:
/// the [`Layout`] a configuration's `profile` resolves against.
#[derive(Debug, Clone)]
pub struct LinkedLayout {
    document: LayoutDocument,
    /// The schemas the linked bundles carry, as documents: what
    /// [`registry`](Layout::registry) answers a registry of.
    schemas: Vec<(SchemaId, Value)>,
    /// The same schemas, compiled once: what a `check` step checks against.
    registry: Arc<Registry>,
}

impl LinkedLayout {
    /// `document`, bound to the schemas `registry` holds: the ones its queues
    /// and its `check` steps name.
    ///
    /// # Errors
    ///
    /// A `check` step naming a schema `registry` does not hold, naming its key.
    pub fn new(document: LayoutDocument, registry: &Registry) -> Result<Self, String> {
        for (key, schema) in document.checked_schemas() {
            if registry.schema(schema).is_none() {
                return Err(format!(
                    "{key}: {schema} is not a schema a linked bundle carries"
                ));
            }
        }
        let schemas: Vec<(SchemaId, Value)> = registry
            .ids()
            .into_iter()
            .filter_map(|id| registry.schema(&id).cloned().map(|schema| (id, schema)))
            .collect();
        let registry = Arc::new(registry_of(&schemas));
        Ok(Self {
            document,
            schemas,
            registry,
        })
    }

    /// The document.
    #[must_use]
    pub fn document(&self) -> &LayoutDocument {
        &self.document
    }
}

/// A registry of `schemas`, each already registered once by the registry they
/// were read from, and so registrable again.
fn registry_of(schemas: &[(SchemaId, Value)]) -> Registry {
    let mut registry = Registry::new();
    for (id, schema) in schemas {
        registry
            .register_schema(id.clone(), schema.clone())
            .unwrap_or_else(|failure| unreachable!("{id} registered once already: {failure}"));
    }
    registry
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// `template` with each `{key}` of `values` replaced by its value.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    values
        .iter()
        .fold(template.to_owned(), |text, (key, value)| {
            text.replace(&format!("{{{key}}}"), value)
        })
}

/// Whether a routed member is there: present, not `null`, not an empty list.
fn carries(fields: &Map<String, Value>, member: &str) -> bool {
    match fields.get(member) {
        None | Some(Value::Null) => false,
        Some(Value::Array(items)) => !items.is_empty(),
        Some(_) => true,
    }
}

fn stamp(fields: &mut Map<String, Value>, member: &str) {
    if !fields.contains_key(member) {
        fields.insert(member.to_owned(), Value::from(now_millis()));
    }
}

impl StampStep {
    fn apply(&self, record: &mut Value) {
        if let Value::Object(fields) = record {
            stamp(fields, self.member.as_str());
        }
    }
}

impl RenameStep {
    fn apply(&self, record: &mut Value) {
        let Value::Object(fields) = record else {
            return;
        };
        let Some(value) = fields.get(self.from.as_str()).cloned() else {
            return;
        };
        // Rebuilt rather than taken out with `remove`, which reorders.
        let mut renamed: Map<String, Value> = std::mem::take(fields)
            .into_iter()
            .filter(|(key, _)| key.as_str() != self.from.as_str())
            .collect();
        renamed.entry(self.to.as_str().to_owned()).or_insert(value);
        *fields = renamed;
    }
}

impl CheckStep {
    fn apply(&self, record: &Value, registry: &Registry) -> Result<(), String> {
        let checked = match &self.at {
            Some(path) => path.get(record).unwrap_or(&Value::Null),
            None => record,
        };
        match registry.check(&self.schema, checked) {
            Ok(()) => Ok(()),
            Err(CheckError::Violation(violation)) => {
                let why = match violation.pointer.as_str() {
                    "" => violation.message,
                    pointer => format!("at {pointer}: {}", violation.message),
                };
                Err(fill(self.refusal.as_str(), &[("why", &why)]))
            }
            // Not measured by coverage, and cannot run: `LinkedLayout::new` refuses a
            // `check` step naming a schema its registry does not hold, and holding
            // the id is the only thing a registry check can fail on.
            Err(CheckError::Registry(failure)) => unreachable!("bound when linked: {failure}"),
        }
    }
}

impl VersionStep {
    fn apply(&self, record: &mut Value) -> Result<(), String> {
        if let Some(slot) = self.at.get_mut(record) {
            if slot
                .as_u64()
                .is_some_and(|found| self.reads.contains(&found))
            {
                *slot = Value::from(self.value);
            }
        }
        if self
            .required_when
            .as_ref()
            .is_some_and(|required| required.matches(record))
        {
            let found = self.at.get(record);
            if found.and_then(Value::as_u64) != Some(self.value) {
                let found = found.map_or_else(|| "none".to_owned(), ToString::to_string);
                return Err(fill(
                    self.refusal.as_str(),
                    &[("value", &self.value.to_string()), ("found", &found)],
                ));
            }
        }
        Ok(())
    }
}

impl GrantStep {
    fn malformed(&self, why: &str) -> String {
        fill(
            self.malformed
                .as_ref()
                .map_or("{why}", RefusalReason::as_str),
            &[("why", why)],
        )
    }

    fn apply(&self, record: &Value, allowlist: &Allowlist<OpWord>) -> Result<(), String> {
        let author = match self.author.get(record) {
            None | Some(Value::Null) => match &self.default_author {
                Some(author) => author.clone(),
                None => return Err(self.malformed(&format!("`{}` names no author", self.author))),
            },
            Some(Value::String(word)) => Author(word.clone()),
            Some(_) => return Err(self.malformed(&format!("`{}` is not text", self.author))),
        };
        if !allowlist.declares(&author) {
            let authors = allowlist
                .authors()
                .iter()
                .map(Author::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(fill(
                self.undeclared.as_ref().map_or(
                    "the author `{author}` is not declared; the declared authors are: {authors}",
                    RefusalReason::as_str,
                ),
                &[("author", author.as_str()), ("authors", &authors)],
            ));
        }
        let mut words = Vec::new();
        if let GrantOps::Word(word) = &self.ops {
            words.push(word.0.clone());
        }
        if let GrantOps::Each { each, op } = &self.ops {
            match each.get(record) {
                None | Some(Value::Null) => {}
                Some(Value::Array(items)) => {
                    for (index, item) in items.iter().enumerate() {
                        let Some(word) = op.get(item).and_then(Value::as_str) else {
                            return Err(
                                self.malformed(&format!("`{each}[{index}]` names no `{op}`"))
                            );
                        };
                        words.push(word.to_owned());
                    }
                }
                Some(_) => return Err(self.malformed(&format!("`{each}` is not a list"))),
            }
        }
        for word in words {
            let Some(op) = allowlist
                .vocabulary()
                .iter()
                .find(|op| op.name() == word)
                .cloned()
            else {
                let ops = allowlist
                    .vocabulary()
                    .iter()
                    .map(|op| op.name().to_owned())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(fill(
                    self.unknown.as_ref().map_or(
                        "'{op}' is not an op; the ops are: {ops}",
                        RefusalReason::as_str,
                    ),
                    &[("op", &word), ("ops", &ops)],
                ));
            };
            allowlist.allows(&author, &op).map_err(|refusal| {
                fill(
                    self.refusal.as_ref().map_or(
                        "'{op}' is not an op {author} may issue: {reason}",
                        RefusalReason::as_str,
                    ),
                    &[
                        ("op", &refusal.op),
                        ("author", refusal.author.as_str()),
                        ("reason", &refusal.reason),
                    ],
                )
            })?;
        }
        Ok(())
    }
}

impl RouteStep {
    fn apply(&self, record: &Value) -> Option<Vec<(QueueName, Value)>> {
        let fields = record.as_object()?;
        let mut taken: Vec<&Route> = self
            .routes
            .iter()
            .filter(|route| {
                route
                    .on
                    .iter()
                    .any(|member| carries(fields, member.as_str()))
            })
            .collect();
        if taken.is_empty() {
            taken.extend(
                self.fallback
                    .as_ref()
                    .and_then(|queue| self.routes.iter().find(|route| &route.queue == queue)),
            );
        }
        if taken.is_empty() {
            return None;
        }
        Some(
            taken
                .into_iter()
                .map(|route| (route.queue.clone(), route.project(fields)))
                .collect(),
        )
    }
}

impl Route {
    fn project(&self, fields: &Map<String, Value>) -> Value {
        let projected: Map<String, Value> = self
            .take
            .iter()
            .filter_map(|member| {
                fields
                    .get(member.as_str())
                    .map(|value| (member.as_str().to_owned(), value.clone()))
            })
            .collect();
        let mut routed = match &self.under {
            Some(under) => {
                let mut wrapped = Map::new();
                wrapped.insert(under.as_str().to_owned(), Value::Object(projected));
                wrapped
            }
            None => projected,
        };
        for member in &self.stamp {
            stamp(&mut routed, member.as_str());
        }
        Value::Object(routed)
    }
}

impl Layout for LinkedLayout {
    fn name(&self) -> &str {
        self.document.name.as_str()
    }

    fn queues(&self) -> Vec<QueueSpec> {
        self.document.queues.to_vec()
    }

    fn allowlist(&self) -> Allowlist<OpWord> {
        self.document.allowlist()
    }

    fn registry(&self) -> Registry {
        registry_of(&self.schemas)
    }

    /// The document's steps for `queue`, in order: each passed over where its
    /// `when` does not hold, and the first refusal refusing the offer.
    fn prepare(
        &self,
        queue: &QueueName,
        record: Value,
        allowlist: &Allowlist<OpWord>,
    ) -> Result<Vec<(QueueName, Value)>, String> {
        let mut record = record;
        let mut routed = None;
        for step in self.document.prepare.get(queue).into_iter().flatten() {
            if step.when().is_some_and(|when| !when.matches(&record)) {
                continue;
            }
            match step {
                PrepareStep::Stamp(stamp) => stamp.apply(&mut record),
                PrepareStep::Rename(rename) => rename.apply(&mut record),
                PrepareStep::Check(check) => check.apply(&record, &self.registry)?,
                PrepareStep::Version(version) => version.apply(&mut record)?,
                PrepareStep::Grant(grant) => grant.apply(&record, allowlist)?,
                PrepareStep::Route(route) => routed = route.apply(&record),
            }
        }
        Ok(routed.unwrap_or_else(|| vec![(queue.clone(), record)]))
    }
}
