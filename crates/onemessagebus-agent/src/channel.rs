//! The planner channel: `onepipeline`'s channel directory, declared as a layout
//! over the core's queues.
//!
//! `onepipeline` keeps a run's planner channel in `runs/<run>/channel/`: the
//! surfaces a planner is asked about, the replies it writes, the command
//! envelopes a reconciler drains and what each was answered with. This module
//! declares that directory as the `planner-channel` [`Layout`] — four queues,
//! their policies, the channel's authors and operations and the refusals each
//! makes — so a directory `onepipeline` 0.28.2 wrote is read by this crate, and a
//! directory this crate writes is read by `onepipeline` 0.28.2.
//!
//! | queue | policy | files |
//! | --- | --- | --- |
//! | [`SURFACES`] | held pending, blocking first, a waiting check-in superseded, projected | `surfaces.jsonl`, `queue.json` |
//! | [`REPLIES`] | plain, numbered, a claim passing over a commands-only reply | `replies.jsonl`, `replies-cursor.json` |
//! | [`COMMANDS`] | plain, numbered | `commands.jsonl`, `commands-cursor.json` |
//! | [`COMMAND_OUTCOMES`] | plain | `command-outcomes.jsonl` |
//!
//! **A departure from Contract Q's wording, ruled by the manager:** the contract
//! states the surfaces queue supersedes on `kind == check-in`, and this layout
//! supersedes on **`source == check-in`**, because that is what 0.28.2's
//! `channel.rs` does: an observer's frame of kind `check-in` carries source
//! `proposal` and is not superseded there. Byte compatibility wins over the
//! wording; the `Supersede { key, when }` shape is unchanged.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use onemessagebus::{
    Allowlist, Asker, Author, ConsumerName, Correlation, Layout, Message, OpWord, Operation,
    Policy, Position, Predicate, Pushed, Queue, QueueError, QueueName, QueueSpec, Registry, Router,
    SchemaId, Supersede, Transport,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The layout's name, as a configuration's `profile` gives it.
pub const PLANNER_CHANNEL: &str = "planner-channel";

/// The queue of planner surfaces: `surfaces.jsonl`.
pub const SURFACES: &str = "surfaces";

/// The queue of replies: `replies.jsonl`.
pub const REPLIES: &str = "replies";

/// The queue of command envelopes: `commands.jsonl`.
pub const COMMANDS: &str = "commands";

/// The queue of command outcomes: `command-outcomes.jsonl`.
pub const COMMAND_OUTCOMES: &str = "command-outcomes";

/// The surfaces queue's projection document: `queue.json`.
pub const PROJECTION: &str = "queue.json";

/// The environment variable `onepipeline` reads a serving session's asker from.
pub const ASKER_ENV: &str = "ONEPIPELINE_CHANNEL_ASKER";

/// The reply envelope version an edit envelope must be read at.
pub const REPLY_ENVELOPE_VERSION: u32 = 3;

/// Every reply envelope version read, newest first; each is read at
/// [`REPLY_ENVELOPE_VERSION`].
pub const REPLY_ENVELOPE_VERSIONS_READ: &[u32] = &[REPLY_ENVELOPE_VERSION, 2];

/// `agent.planner-surface@1`: [`Surface`].
pub const SURFACE_SCHEMA: SchemaId = SchemaId::literal("agent", "planner-surface", 1);

/// `agent.queued-reply@1`: [`QueuedReply`].
pub const QUEUED_REPLY_SCHEMA: SchemaId = SchemaId::literal("agent", "queued-reply", 1);

/// `agent.queued-commands@1`: [`QueuedCommands`].
pub const QUEUED_COMMANDS_SCHEMA: SchemaId = SchemaId::literal("agent", "queued-commands", 1);

/// `agent.command-outcome@1`: [`CommandOutcome`].
pub const COMMAND_OUTCOME_SCHEMA: SchemaId = SchemaId::literal("agent", "command-outcome", 1);

/// What raised a surface.
pub mod source {
    /// A durable progress timer came due; a newer check-in supersedes a waiting one.
    pub const CHECK_IN: &str = "check-in";
    /// A settled worker, an observer or the orchestrator raised advice.
    pub const PROPOSAL: &str = "proposal";
    /// The reconciler answered an edit it could not apply.
    pub const RECONCILER: &str = "reconciler";
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A recorded asker that names nobody reads as nobody, rather than refusing the
/// record around it: `onepipeline` reads it so.
fn recorded_asker<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Asker>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?
        .and_then(|name| Asker::new(&name, "the recorded asker").ok()))
}

/// One planner surface, as `surfaces.jsonl` and `queue.json` hold it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Surface {
    /// Allocated by the queue: one past the highest the log has queued.
    pub id: u64,
    /// What it is asking about.
    pub kind: String,
    /// Its text.
    pub message: String,
    /// What raised it: see [`source`].
    // llmlint: ignore[invalid_states_unrepresentable] 0.28.2's `channel::Surface.source` is a `String` its writers fill with open producer words this crate does not own, so a closed enum here would refuse recorded projections this layout must continue to read byte-for-byte.
    pub source: String,
    /// Whether the run waits on its answer.
    pub blocking: bool,
    /// When it was queued, in epoch milliseconds.
    pub queued_at: u64,
    /// The node that provoked it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workstream: Option<String>,
    /// Whether nobody is listening for its answer any more. Omitted while false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub abandoned: bool,
    /// Who raised it, when a session naming an asker did. Omitted while absent.
    #[serde(
        default,
        deserialize_with = "recorded_asker",
        skip_serializing_if = "Option::is_none"
    )]
    pub asker: Option<Asker>,
    /// The correlation a reply echoes to answer it, when it was raised as a
    /// question (`onemessagebus::ask`). Omitted while absent, so a surface
    /// raised any other way is written byte for byte as `onepipeline` writes it.
    #[serde(
        default,
        deserialize_with = "recorded_correlation",
        skip_serializing_if = "Option::is_none"
    )]
    pub correlation: Option<Correlation>,
}

/// A recorded correlation that is not one reads as none, rather than refusing
/// the record around it, as a recorded asker does.
fn recorded_correlation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Correlation>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.and_then(|text| text.parse().ok()))
}

impl Message for Surface {
    const SCHEMA: SchemaId = SURFACE_SCHEMA;
}

fn planner() -> Author {
    Author::from("planner")
}
fn is_planner(author: &Author) -> bool {
    author.as_str() == "planner"
}

/// A declared version this build reads is read as the version it writes; any
/// other is left as declared, so the refusal naming the version an edit
/// envelope requires is made where it always was.
fn read_at_a_version_this_build_reads<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u32>, D::Error> {
    Ok(Option::<u32>::deserialize(deserializer)?.map(|version| {
        if REPLY_ENVELOPE_VERSIONS_READ.contains(&version) {
            REPLY_ENVELOPE_VERSION
        } else {
            version
        }
    }))
}

/// One reply envelope: a verdict, a list of graph edits, or both.
///
/// Its commands are carried as JSON, in the order and with the fields their
/// author wrote: which ops exist and what each means are `onepipeline`'s, and
/// this crate reads only each command's `op`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyEnvelope {
    /// The version it was written against, read at [`REPLY_ENVELOPE_VERSION`].
    #[serde(
        default,
        deserialize_with = "read_at_a_version_this_build_reads",
        skip_serializing_if = "Option::is_none"
    )]
    pub version: Option<u32>,
    /// Who wrote it. Omitted, the planner.
    #[serde(default = "planner", skip_serializing_if = "is_planner")]
    pub author: Author,
    /// The verdict: whether the author considers the run complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<bool>,
    /// The verdict's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Why the author reached that verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The graph edits, each an object naming its `op`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Map<String, Value>>,
}

impl Default for ReplyEnvelope {
    fn default() -> Self {
        Self {
            version: None,
            author: planner(),
            completion: None,
            message: None,
            reason: None,
            commands: Vec::new(),
        }
    }
}

impl ReplyEnvelope {
    /// Whether it carries a verdict half: any of `completion`, `message` and
    /// `reason`.
    #[must_use]
    pub fn carries_verdict(&self) -> bool {
        self.completion.is_some() || self.message.is_some() || self.reason.is_some()
    }

    /// Whether it carries edits and no verdict: the one shape with nothing in it
    /// for the reply queue.
    #[must_use]
    pub fn carries_edits_without_a_verdict(&self) -> bool {
        !self.commands.is_empty() && !self.carries_verdict()
    }
}

/// One reply as `replies.jsonl` holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueuedReply {
    /// The number of replies before it.
    pub id: u64,
    /// The envelope.
    pub reply: ReplyEnvelope,
    /// When it was written, in epoch milliseconds.
    pub at: u64,
    /// The correlation of the question it answers, when it answers one asked
    /// through `onemessagebus::ask`. Omitted while absent.
    #[serde(
        default,
        deserialize_with = "recorded_correlation",
        skip_serializing_if = "Option::is_none"
    )]
    pub correlation: Option<Correlation>,
}

impl Message for QueuedReply {
    const SCHEMA: SchemaId = QUEUED_REPLY_SCHEMA;
}

/// One command envelope as `commands.jsonl` holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueuedCommands {
    /// The number of envelopes before it.
    pub id: u64,
    /// Who submitted it.
    #[serde(default = "planner")]
    pub author: Author,
    /// The commands, each an object naming its `op`.
    pub commands: Vec<Map<String, Value>>,
}

impl Message for QueuedCommands {
    const SCHEMA: SchemaId = QUEUED_COMMANDS_SCHEMA;
}

/// What became of one command of an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CommandVerdict {
    /// Validated and committed.
    Applied,
    /// Validated, and nothing of it happened: something else in the envelope
    /// refused.
    Validated,
    /// A note a conversation took, in an envelope refused after that.
    Delivered,
    /// Refused.
    Refused,
}

/// The answer to one command of an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandResult {
    /// Its index in the envelope's `commands`.
    pub index: u64,
    /// Its op.
    // llmlint: ignore[invalid_states_unrepresentable] 0.28.2's `CommandResult.op` is the command's op as the envelope spelled it, including an op the reconciler refused as unknown; a closed `Op` here would refuse the very outcome record that reports that refusal.
    pub op: String,
    /// What became of it.
    pub outcome: CommandVerdict,
    /// Why it refused, or what refused around it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The reconciler's answer to one envelope, as `command-outcomes.jsonl` holds
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandOutcome {
    /// The envelope it answers.
    pub id: u64,
    /// Whether every command in it was applied.
    pub applied: bool,
    /// Why not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// One entry per command, in order. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<CommandResult>,
}

impl Message for CommandOutcome {
    const SCHEMA: SchemaId = COMMAND_OUTCOME_SCHEMA;
}

/// The planner channel's operations: `onepipeline`'s `Command` ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Op {
    /// Add a node.
    Add,
    /// Remove a node.
    Drop,
    /// Replace an unstarted node's dependencies.
    Reparent,
    /// Supersede a node with a fresh lineage.
    Retry,
    /// Park a node.
    Cancel,
    /// Return a parked node to the frontier.
    Requeue,
    /// Journal the planner's completion request.
    Complete,
    /// Complete a waiting human action.
    Attest,
    /// Raise a finding to the planner.
    Finding,
    /// Amend what a node is judged against.
    Amend,
    /// Deliver a note into a node's dispatch.
    Note,
    /// Settle a node from evidence.
    Settle,
}

impl Op {
    /// Every op, in the order the contract lists them.
    pub const ALL: [Op; 12] = [
        Op::Add,
        Op::Drop,
        Op::Reparent,
        Op::Retry,
        Op::Cancel,
        Op::Requeue,
        Op::Complete,
        Op::Attest,
        Op::Finding,
        Op::Amend,
        Op::Note,
        Op::Settle,
    ];

    /// The op's wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Op::Add => "add",
            Op::Drop => "drop",
            Op::Reparent => "reparent",
            Op::Retry => "retry",
            Op::Cancel => "cancel",
            Op::Requeue => "requeue",
            Op::Complete => "complete",
            Op::Attest => "attest",
            Op::Finding => "finding",
            Op::Amend => "amend",
            Op::Note => "note",
            Op::Settle => "settle",
        }
    }

    /// The op a wire word names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Op> {
        Op::ALL.into_iter().find(|op| op.word() == word)
    }
}

impl Operation for Op {
    fn name(&self) -> &str {
        self.word()
    }
}

/// The planner channel's allowlist: the planner is its only built-in author and
/// is granted every operation.
#[must_use]
pub fn allowlist() -> Allowlist<Op> {
    let planner = planner();
    let mut allowlist = Allowlist::new(Op::ALL);
    for op in Op::ALL {
        allowlist.grant(planner.clone(), op);
    }
    allowlist
}

/// Whether `author` may issue the op `word`, refused in `onepipeline`'s words:
/// `'<op>' is not an op the <author> may issue: <reason>. Surface it to the
/// planner instead`.
///
/// # Errors
///
/// The refusal text, for an op the allowlist does not grant `author` or a word
/// that is no op of the channel.
pub fn allows<O: Operation>(
    allowlist: &Allowlist<O>,
    author: &Author,
    word: &str,
) -> Result<(), String> {
    let Some(op) = allowlist.vocabulary().iter().find(|op| op.name() == word) else {
        return Err(format!(
            "'{word}' is not an op of the planner channel; the ops are: {}",
            Op::ALL.map(Op::word).join(", ")
        ));
    };
    allowlist.allows(author, op).map_err(|refusal| {
        format!(
            "'{}' is not an op the {} may issue: {}. Surface it to the planner instead",
            refusal.op, refusal.author, refusal.reason
        )
    })
}

fn ensure_declared<O: Operation>(allowlist: &Allowlist<O>, author: &Author) -> Result<(), String> {
    if allowlist.declares(author) {
        return Ok(());
    }
    Err(format!(
        "the envelope's author `{author}` is not declared; the declared authors are: {}",
        allowlist
            .authors()
            .iter()
            .map(Author::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Whether `author` may declare the run finished through a verdict's
/// `completion: true` — the legacy spelling of `complete`, granted or refused as
/// that op is.
///
/// # Errors
///
/// The refusal text, for a verdict carrying `completion: true` from an author
/// not granted `complete`.
pub fn allows_completion<O: Operation>(
    allowlist: &Allowlist<O>,
    author: &Author,
    completion: Option<bool>,
) -> Result<(), String> {
    if completion != Some(true) {
        return Ok(());
    }
    let Some(complete) = allowlist
        .vocabulary()
        .iter()
        .find(|op| op.name() == Op::Complete.word())
    else {
        return Ok(());
    };
    allowlist.allows(author, complete).map_err(|refusal| {
        format!(
            "declaring the run complete is not something the {} may do: {}. Surface it to the planner instead",
            refusal.author, refusal.reason
        )
    })
}

fn name(text: &str) -> QueueName {
    QueueName::try_from(text).unwrap_or_else(|_| unreachable!("{text} is a queue name"))
}

fn field(text: &str) -> onemessagebus::FieldPath {
    text.parse()
        .unwrap_or_else(|_| unreachable!("{text} is a field path"))
}

/// A reply a claim on the reply queue hands out: anything but a commands-only
/// envelope, whose reader is the command queue.
fn claims_a_verdict() -> Predicate {
    let verdict = Predicate::Any(
        ["reply.completion", "reply.message", "reply.reason"]
            .into_iter()
            .map(|path| Predicate::Present {
                field: field(path),
                present: true,
            })
            .collect(),
    );
    Predicate::Not(Box::new(Predicate::All(vec![
        Predicate::NonEmpty {
            field: field("reply.commands"),
            non_empty: true,
        },
        Predicate::Not(Box::new(verdict)),
    ])))
}

/// The layout's four queues, as it declares them.
#[must_use]
pub fn queues() -> Vec<QueueSpec> {
    let mut surfaces = QueueSpec::new(
        name(SURFACES),
        Policy {
            // Supersedes on `source`, as 0.28.2 does: see the module's note.
            supersede_on: Some(Supersede {
                key: field("source"),
                when: Some(Predicate::equals(field("source"), source::CHECK_IN)),
            }),
            hold_pending: true,
            blocking_first: true,
            projection: Some(
                PROJECTION
                    .parse()
                    .unwrap_or_else(|_| unreachable!("queue.json is a document name")),
            ),
            ..Policy::default()
        },
    );
    surfaces.schema = Some(SURFACE_SCHEMA);
    surfaces.answers = Some(name(REPLIES));

    let mut replies = QueueSpec::new(name(REPLIES), Policy::default());
    replies.schema = Some(QUEUED_REPLY_SCHEMA);
    replies.claims = Some(claims_a_verdict());
    replies.numbered = true;

    let mut commands = QueueSpec::new(name(COMMANDS), Policy::default());
    commands.schema = Some(QUEUED_COMMANDS_SCHEMA);
    commands.numbered = true;

    let mut outcomes = QueueSpec::new(name(COMMAND_OUTCOMES), Policy::default());
    outcomes.schema = Some(COMMAND_OUTCOME_SCHEMA);

    vec![surfaces, replies, commands, outcomes]
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The `planner-channel` layout.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlannerChannel;

impl PlannerChannel {
    /// The records one reply envelope becomes: its commands on the command
    /// queue, and its verdict on the reply queue — a commands-only envelope is
    /// the command queue's alone. Checked against `allowlist` first: a verdict
    /// declaring the run complete as `complete` is, and each command's op.
    fn route_envelope<O: Operation>(
        envelope: Value,
        allowlist: &Allowlist<O>,
    ) -> Result<Vec<(QueueName, Value)>, String> {
        // Checked first: serde reads an array into a struct field by field, so
        // `[3]` would otherwise pass as an envelope of version 3.
        if !envelope.is_object() {
            return Err("the reply is malformed: a reply envelope is a JSON object".to_owned());
        }
        let envelope: ReplyEnvelope = serde_json::from_value(envelope)
            .map_err(|failure| format!("the reply is malformed: {failure}"))?;
        let author = envelope.author.clone();
        ensure_declared(allowlist, &author)?;
        allows_completion(allowlist, &author, envelope.completion)?;
        let mut routed = Vec::new();
        if !envelope.commands.is_empty() {
            if envelope.version != Some(REPLY_ENVELOPE_VERSION) {
                return Err(format!(
                    "an edit envelope requires version {REPLY_ENVELOPE_VERSION}"
                ));
            }
            for command in &envelope.commands {
                let word = command
                    .get("op")
                    .and_then(Value::as_str)
                    .ok_or("a command names its `op`")?;
                allows(allowlist, &author, word)?;
            }
            routed.push((
                name(COMMANDS),
                serde_json::json!({
                    "id": 0,
                    "author": envelope.author,
                    "commands": envelope.commands,
                }),
            ));
        }
        if envelope.carries_verdict() || envelope.commands.is_empty() {
            routed.push((
                name(REPLIES),
                serde_json::json!({ "id": 0, "reply": envelope, "at": now_millis() }),
            ));
        }
        Ok(routed)
    }
}

/// The planner channel's router: a reply offered to the reply queue is routed
/// by the halves it carries, as `onepipeline` routes it.
///
/// A bare envelope carrying a verdict and edits becomes its commands on
/// [`COMMANDS`] and its verdict on [`REPLIES`], so it reaches both the pending
/// ask and the command path; one carrying only commands reaches [`COMMANDS`]
/// alone, and leaves the pending ask standing. A reply already framed
/// (`{id, reply, at}`) is checked and kept whole. Every half is checked against
/// the allowlist first, and what the edits mean stays `onepipeline`'s.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplyRouter;

impl Router for ReplyRouter {
    fn route(
        &self,
        queue: &QueueName,
        record: Value,
        allowlist: &Allowlist<OpWord>,
    ) -> Result<Vec<(QueueName, Value)>, String> {
        match record.get("reply") {
            Some(envelope) => {
                PlannerChannel::route_envelope(envelope.clone(), allowlist)?;
                Ok(vec![(queue.clone(), record)])
            }
            None => PlannerChannel::route_envelope(record, allowlist),
        }
    }
}

impl Layout for PlannerChannel {
    fn name(&self) -> &str {
        PLANNER_CHANNEL
    }

    fn queues(&self) -> Vec<QueueSpec> {
        queues()
    }

    fn allowlist(&self) -> Allowlist<OpWord> {
        allowlist().words()
    }

    fn registry(&self) -> Registry {
        crate::registry()
    }

    /// What the writers `onepipeline` has stamp and check:
    ///
    /// - a surface is stamped `queued_at` now when it names none;
    /// - a reply offered as a bare envelope — to the reply queue, or as the
    ///   answer to a surface — is routed by its halves, checked against the
    ///   allowlist, and stamped `at` now; one offered already framed
    ///   (`{id, reply, at}`) is checked and kept whole;
    /// - a command envelope is checked op by op against its author.
    fn prepare(
        &self,
        queue: &QueueName,
        record: Value,
        allowlist: &Allowlist<OpWord>,
    ) -> Result<Vec<(QueueName, Value)>, String> {
        match queue.as_str() {
            SURFACES => {
                let Value::Object(fields) = record else {
                    return Ok(vec![(queue.clone(), record)]);
                };
                // What a question is `about` is the node it names, which a
                // surface carries as its `workstream`. Rebuilt rather than
                // taken out with `remove`, which reorders the record.
                let about = fields.get(onemessagebus::ask::ABOUT).cloned();
                let mut fields: Map<String, Value> = fields
                    .into_iter()
                    .filter(|(key, _)| key != onemessagebus::ask::ABOUT)
                    .collect();
                if let Some(about) = about {
                    fields.entry("workstream").or_insert(about);
                }
                fields
                    .entry("queued_at")
                    .or_insert_with(|| Value::from(now_millis()));
                Ok(vec![(queue.clone(), Value::Object(fields))])
            }
            REPLIES => ReplyRouter.route(queue, record, allowlist),
            COMMANDS => {
                let author: Author = record
                    .get("author")
                    .map(|author| serde_json::from_value(author.clone()))
                    .transpose()
                    .map_err(|failure| format!("the envelope's author: {failure}"))?
                    .unwrap_or_else(planner);
                ensure_declared(allowlist, &author)?;
                for command in record
                    .get("commands")
                    .and_then(Value::as_array)
                    .ok_or("a command envelope carries `commands`")?
                {
                    let word = command
                        .get("op")
                        .and_then(Value::as_str)
                        .ok_or("a command names its `op`")?;
                    allows(allowlist, &author, word)?;
                }
                Ok(vec![(queue.clone(), record)])
            }
            _ => Ok(vec![(queue.clone(), record)]),
        }
    }
}

/// The planner channel over one transport, through its typed queues: what
/// `onepipeline`'s `ChannelState` does, on the bus.
#[derive(Debug, Clone)]
pub struct Channel {
    surfaces: Queue<Surface>,
    replies: Queue<QueuedReply>,
    commands: Queue<QueuedCommands>,
    outcomes: Queue<CommandOutcome>,
}

impl Channel {
    /// The channel kept on `transport`.
    ///
    /// # Errors
    ///
    /// A record type whose schema cannot be registered.
    pub fn open(transport: &Arc<dyn Transport>) -> Result<Self, QueueError> {
        let mut specs = queues().into_iter();
        let mut next = || specs.next().unwrap_or_else(|| unreachable!("four queues"));
        Ok(Self {
            surfaces: Queue::open(Arc::clone(transport), next())?,
            replies: Queue::open(Arc::clone(transport), next())?,
            commands: Queue::open(Arc::clone(transport), next())?,
            outcomes: Queue::open(Arc::clone(transport), next())?,
        })
    }

    /// The surfaces queue.
    #[must_use]
    pub fn surfaces(&self) -> &Queue<Surface> {
        &self.surfaces
    }

    /// The reply queue.
    #[must_use]
    pub fn replies(&self) -> &Queue<QueuedReply> {
        &self.replies
    }

    /// The command queue.
    #[must_use]
    pub fn commands(&self) -> &Queue<QueuedCommands> {
        &self.commands
    }

    /// The command outcome queue.
    #[must_use]
    pub fn outcomes(&self) -> &Queue<CommandOutcome> {
        &self.outcomes
    }

    /// Queue a surface; the queue allocates its id.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn push(&self, surface: &Surface) -> Result<Pushed<Surface>, QueueError> {
        self.surfaces.push(surface)
    }

    /// Claim the next surface, a blocking one first.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn claim(&self) -> Result<Option<Surface>, QueueError> {
        Ok(self
            .surfaces
            .claim(&ConsumerName::default_consumer())?
            .map(|claimed| claimed.record))
    }

    /// The surface waiting for an answer, abandoned ones passed over.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn pending(&self) -> Result<Option<Surface>, QueueError> {
        Ok(self
            .surfaces
            .pending(&ConsumerName::default_consumer())?
            .map(|claimed| claimed.record))
    }

    /// Mark the surfaces in `raised` abandoned.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn abandon(&self, raised: &[u64]) -> Result<Vec<Surface>, QueueError> {
        self.typed_surfaces(self.surfaces.raw().abandon(raised)?)
    }

    /// Take back what `asker` abandoned.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn attend(&self, asker: &Asker) -> Result<Vec<Surface>, QueueError> {
        self.typed_surfaces(self.surfaces.raw().attend(asker)?)
    }

    fn typed_surfaces(&self, records: Vec<Value>) -> Result<Vec<Surface>, QueueError> {
        records
            .into_iter()
            .map(|record| {
                serde_json::from_value(record).map_err(|failure| QueueError::Shape {
                    queue: name(SURFACES),
                    why: failure.to_string(),
                })
            })
            .collect()
    }

    /// Record that `reply` answered whatever was pending — the slot released
    /// first, then the reply appended, so a reader that finds the reply finds
    /// the slot already released — and answer the reply's id.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn answer(&self, reply: &ReplyEnvelope, at: u64) -> Result<u64, QueueError> {
        self.surfaces.raw().answer_pending()?;
        let pushed = self.replies.push(&QueuedReply {
            id: 0,
            reply: reply.clone(),
            at,
            correlation: None,
        })?;
        Ok(pushed.id.unwrap_or_default())
    }

    /// Answer the pending surface with `reply` when it carries a verdict; a
    /// commands-only envelope answers nothing.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn answer_if_verdict(
        &self,
        reply: &ReplyEnvelope,
        at: u64,
    ) -> Result<Option<u64>, QueueError> {
        if reply.carries_verdict() {
            return self.answer(reply, at).map(Some);
        }
        Ok(None)
    }

    /// Claim the next reply no reader has taken, passing over a commands-only
    /// envelope.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn claim_reply(&self) -> Result<Option<QueuedReply>, QueueError> {
        Ok(self
            .replies
            .claim(&ConsumerName::default_consumer())?
            .map(|claimed| claimed.record))
    }

    /// Append one command envelope; answers its id.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn submit(
        &self,
        author: Author,
        commands: Vec<Map<String, Value>>,
    ) -> Result<u64, QueueError> {
        let pushed = self.commands.push(&QueuedCommands {
            id: 0,
            author,
            commands,
        })?;
        Ok(pushed.id.unwrap_or_default())
    }

    /// Claim every command envelope the reconciler has not drained.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn claim_commands(&self) -> Result<Vec<QueuedCommands>, QueueError> {
        let mut claimed = Vec::new();
        while let Some(envelope) = self.commands.claim(&ConsumerName::default_consumer())? {
            claimed.push(envelope.record);
        }
        Ok(claimed)
    }

    /// Answer one claimed envelope.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn answer_commands(&self, outcome: &CommandOutcome) -> Result<Position, QueueError> {
        Ok(self.outcomes.push(outcome)?.position)
    }

    /// The reconciler's answer to envelope `id`, if it has given one.
    ///
    /// # Errors
    ///
    /// A queue failure.
    pub fn outcome_of(&self, id: u64) -> Result<Option<CommandOutcome>, QueueError> {
        Ok(self
            .outcomes
            .raw()
            .log(None)?
            .into_iter()
            .filter_map(|(record, _)| serde_json::from_value::<CommandOutcome>(record).ok())
            .find(|outcome| outcome.id == id))
    }
}
