//! The onejudge codec: `onejudge`'s command-provider frames, served over the
//! bus as a member's judge side.
//!
//! `onejudge` spawns a judge-side command once per operation, writes **one**
//! request frame to its stdin, and reads **one** response object from its
//! stdout (`docs/protocol.md` in `onejudge`, protocol v6, as `crates/onejudge/src/command.rs`
//! writes it at 0.8.1). This codec is that command, replacing the pair of
//! `onepipeline channel serve` and the host's `channel-serve.py` with one process.
//! Every frame, response field and transcript shape it reads or writes is
//! declared once in this module, and the five frames are registered as
//! `agent.onejudge-frame.<op>@6` ([`schemas`]) so a crate adopting the bus
//! reconciles its own frame types against them.
//!
//! What each operation gets:
//!
//! - **`supervisor` is liveness, and only liveness.** Any assistant content in
//!   the turn means the member took its turn: nothing is raised, and the member
//!   is answered with a non-completion it can act on. A turn with no assistant
//!   content, or whose last assistant message is a machine transcript proving the
//!   turn was lost — every line a JSON object, ending in an `error` frame or a
//!   `turn/completed` whose status is `failed` — is a failure: one bounded,
//!   non-blocking `monitor-failed` surface naming the cause and the harness
//!   identity is raised, and the process exits non-zero. Nothing else in a turn's
//!   prose is ever raised: a monitor reports through the `finding` op it issues
//!   itself.
//! - **`judge` is the planner's score.** The criterion is raised as its own
//!   non-blocking question; the ruling that answers it is the score — its
//!   `completion` the boolean, its prose the `reason`. A ruling that never comes
//!   is the conservative `unsatisfied`, never a fabricated pass. A live edit
//!   arriving where a ruling was expected — commands and no verdict, which the
//!   reply router keeps on the command path, so only a regressed transport
//!   delivers one — is recognised, answered as a non-completion naming the edits,
//!   and applied nowhere.
//! - **`assess`, a non-boolean `judge`, `respond` and `user`** are refused by
//!   name: a planner rules with a `completion` boolean, which is no free-text
//!   judgement and no score on a scale.
//!
//! `docs/codecs.md` keeps the account of why each rule is what it is.

use std::path::Path;

use onemessagebus::{
    Address, Answer, Codec, CodecFailure, CodecName, EnvName, Message, SchemaId, ServeSession,
};
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::channel::source::PROPOSAL;
use crate::note::DeliveredNote;

static CODEC_NAME: std::sync::LazyLock<CodecName> = std::sync::LazyLock::new(|| {
    CODEC
        .parse()
        .expect("the linked onejudge codec has a valid name")
});

/// The codec's name, as `serve --codec` gives it.
pub const CODEC: &str = "onejudge";

/// The protocol version the frames are transcribed at.
pub const PROTOCOL_VERSION: u32 = 6;

/// The `onejudge` release the frames are transcribed from.
pub const TRANSCRIBED_FROM: &str = "onejudge 0.8.1";

/// The field every frame names its operation in.
pub const OP: &str = "op";

/// The operations, by their wire words.
pub mod op {
    /// Run one skill turn.
    pub const RESPOND: &str = "respond";
    /// Produce one simulated-user turn.
    pub const USER: &str = "user";
    /// Decide completion, or produce the next user turn.
    pub const SUPERVISOR: &str = "supervisor";
    /// Score a criterion against the transcript.
    pub const JUDGE: &str = "judge";
    /// Write a free-text judgement.
    pub const ASSESS: &str = "assess";
    /// Every operation, in `docs/protocol.md`'s order.
    pub const ALL: [&str; 5] = [RESPOND, USER, SUPERVISOR, JUDGE, ASSESS];
    /// The operations this codec serves.
    pub const SERVED: [&str; 2] = [SUPERVISOR, JUDGE];
}

/// The response fields this codec writes and reads.
pub mod field {
    /// A supervisor ruling's boolean.
    pub const COMPLETION: &str = "completion";
    /// A supervisor ruling's next user message; a reply envelope's prose.
    pub const MESSAGE: &str = "message";
    /// A ruling's or a score's justification.
    pub const REASON: &str = "reason";
    /// A judge score's value.
    pub const VALUE: &str = "value";
    /// A reply envelope's graph edits.
    pub const COMMANDS: &str = "commands";
    /// A graph edit's operation.
    pub const EDIT_OP: &str = "op";
    /// A graph edit's target.
    pub const EDIT_TARGET: &str = "id";
    /// The field a framed reply record carries its envelope in.
    pub const REPLY: &str = "reply";
}

// llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] These frame shapes are transcribed field for field from pinned onejudge 0.8.1, and every fixed string this codec reads or writes is declared once under `onemessagebus_agent::codec::onejudge`, which is this repository's source. The cross-repository drift gate belongs to the onejudge-adopt-bus node, where onejudge registers its frame protocol and reconciles it against the schemas exposed here.
/// Who produced a message of the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The real or simulated user.
    User,
    /// The skill or agent under test.
    Assistant,
    /// System framing a provider surfaced.
    System,
}

/// One normalized tool event of an assistant turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolEvent {
    /// `tool_call` or `tool_result`.
    pub kind: String,
    /// The normalized tool name, where knowable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The structured tool arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// The result text, when the transcript exposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Its position within the run.
    #[serde(default)]
    pub index: u64,
    /// The harness's own identity for the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// One turn of the conversation a frame carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationMessage {
    /// Who produced it.
    pub role: Role,
    /// Its text.
    pub content: String,
    /// The tool events taken producing it; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<ToolEvent>,
}

/// The skill a `respond` frame runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Skill {
    /// Its name.
    pub name: String,
    /// Its directory.
    pub path: String,
    /// Its instructions.
    pub instructions: String,
}

/// The evidence a `judge` frame names (protocol v6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// The skill's working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// The exact history artifact paths.
    pub history_files: Vec<String>,
}

/// A `respond` frame: run one skill turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RespondFrame {
    /// The skill.
    pub skill: Skill,
    /// The transcript so far.
    pub messages: Vec<ConversationMessage>,
    /// The caller-owned session name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// A `user` frame: produce one simulated-user turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserFrame {
    /// The persona to play.
    pub persona: String,
    /// The transcript so far.
    pub messages: Vec<ConversationMessage>,
    /// The caller-owned session name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// A `supervisor` frame: decide completion, or produce the next user turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SupervisorFrame {
    /// The task the conversation is about; its opening line names the run.
    pub task: String,
    /// The supervisor's persona.
    pub persona: String,
    /// The criterion actually in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_when: Option<String>,
    /// The worktree the recording is kept under.
    pub worktree: String,
    /// The recording's name.
    pub history_name: String,
    /// Every note delivered so far (v5); omitted when none has been.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<DeliveredNote>,
    /// The transcript so far.
    pub messages: Vec<ConversationMessage>,
    /// The judge's caller-owned session name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// A `judge` frame: score a criterion against the transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum JudgeFrame {
    /// A boolean completion criterion.
    Boolean {
        /// The criterion.
        criterion: String,
        /// The transcript.
        messages: Vec<ConversationMessage>,
        /// The evidence (v6); omitted without context.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<Evidence>,
    },
    /// A numeric score with required bounds.
    Numeric {
        /// The criterion.
        criterion: String,
        /// The score's floor.
        min: f64,
        /// The score's ceiling.
        max: f64,
        /// The transcript.
        messages: Vec<ConversationMessage>,
        /// The evidence (v6); omitted without context.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<Evidence>,
    },
}

/// An `assess` frame: write a free-text judgement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssessFrame {
    /// What to judge.
    pub prompt: String,
    /// The transcript.
    pub messages: Vec<ConversationMessage>,
}
// llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate] The five protocol frame declarations and their shared frame types end here.

impl Message for RespondFrame {
    const SCHEMA: SchemaId = SchemaId::literal("agent", "onejudge-frame.respond", 6);
}

impl Message for UserFrame {
    const SCHEMA: SchemaId = SchemaId::literal("agent", "onejudge-frame.user", 6);
}

impl Message for SupervisorFrame {
    const SCHEMA: SchemaId = SchemaId::literal("agent", "onejudge-frame.supervisor", 6);
}

impl Message for JudgeFrame {
    const SCHEMA: SchemaId = SchemaId::literal("agent", "onejudge-frame.judge", 6);
}

impl Message for AssessFrame {
    const SCHEMA: SchemaId = SchemaId::literal("agent", "onejudge-frame.assess", 6);
}

/// One request frame, discriminated by its `op`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Frame {
    /// `respond`.
    Respond(RespondFrame),
    /// `user`.
    User(UserFrame),
    /// `supervisor`.
    Supervisor(SupervisorFrame),
    /// `judge`.
    Judge(JudgeFrame),
    /// `assess`.
    Assess(AssessFrame),
}

/// Every frame's registered schema: the type's own document with its `op`
/// pinned to the operation's word.
#[must_use]
pub fn schemas() -> Vec<(SchemaId, Schema)> {
    vec![
        frame_schema::<RespondFrame>(op::RESPOND),
        frame_schema::<UserFrame>(op::USER),
        frame_schema::<SupervisorFrame>(op::SUPERVISOR),
        frame_schema::<JudgeFrame>(op::JUDGE),
        frame_schema::<AssessFrame>(op::ASSESS),
    ]
}

fn frame_schema<F: Message>(word: &str) -> (SchemaId, Schema) {
    let mut document = schemars::schema_for!(F).to_value();
    if let Some(object) = document.as_object_mut() {
        stamp_frame_op(object, word);
        if let Some(Value::Array(variants)) = object.get_mut("oneOf") {
            for variant in variants {
                if let Some(variant) = variant.as_object_mut() {
                    stamp_frame_op(variant, word);
                }
            }
        }
    }
    let schema = Schema::try_from(document).unwrap_or_else(|_| schemars::schema_for!(F));
    (F::SCHEMA, schema)
}

fn stamp_frame_op(object: &mut Map<String, Value>, word: &str) {
    let mut properties = Map::new();
    properties.insert(
        OP.to_owned(),
        serde_json::json!({
            "type": "string",
            "const": word,
            "description": "The operation this frame asks for."
        }),
    );
    if let Some(Value::Object(existing)) = object.get("properties") {
        properties.extend(existing.clone());
    }
    object.insert("properties".to_owned(), Value::Object(properties));
    let mut required = vec![Value::String(OP.to_owned())];
    if let Some(Value::Array(existing)) = object.get("required") {
        required.extend(existing.iter().cloned());
    }
    object.insert("required".to_owned(), Value::Array(required));
}

/// What a provider reports it spent; any subset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    /// Prompt tokens billed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Completion tokens billed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Prompt tokens read from a cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Prompt tokens written to a cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Total cost in USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// A supervisor ruling: exactly one of two shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorResponse {
    /// `{"completion": true, "reason": ...}`: the work is complete; a reason is
    /// required and a message forbidden.
    Completed {
        /// Why.
        reason: String,
    },
    /// `{"completion": false, "message": ..., "reason": ...}`: the next user
    /// turn, verbatim, and why.
    Continue {
        /// The next user turn.
        message: String,
        /// Why.
        reason: String,
    },
}

impl Serialize for SupervisorResponse {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut out = serializer.serialize_map(None)?;
        match self {
            Self::Completed { reason } => {
                out.serialize_entry(field::COMPLETION, &true)?;
                out.serialize_entry(field::REASON, reason)?;
            }
            Self::Continue { message, reason } => {
                out.serialize_entry(field::COMPLETION, &false)?;
                out.serialize_entry(field::MESSAGE, message)?;
                out.serialize_entry(field::REASON, reason)?;
            }
        }
        out.end()
    }
}

/// A supervisor ruling as it arrives, before its shape is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisorWire {
    completion: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "protocol v6 includes usage in supervisor responses even though routing does not inspect it"
    )]
    usage: Option<Usage>,
}

impl<'de> Deserialize<'de> for SupervisorResponse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SupervisorWire::deserialize(deserializer)?;
        match (wire.completion, wire.message) {
            (true, None) if !wire.reason.trim().is_empty() => Ok(Self::Completed {
                reason: wire.reason,
            }),
            (true, _) => Err(serde::de::Error::custom(
                "a completed ruling requires a non-empty `reason` and forbids `message`",
            )),
            (false, Some(message)) if !message.trim().is_empty() => Ok(Self::Continue {
                message,
                reason: wire.reason,
            }),
            (false, _) => Err(serde::de::Error::custom(
                "a continuing ruling requires a non-empty `message`",
            )),
        }
    }
}

/// A judge score's value: a boolean or a number, as the frame's kind asks.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum JudgeValue {
    /// A boolean score.
    Bool(bool),
    /// A numeric score.
    Number(f64),
}

/// A judge score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JudgeResponse {
    /// The score.
    pub value: JudgeValue,
    /// The one-sentence justification. `onejudge` reads it from `reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the scoring spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// A skill turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RespondResponse {
    /// The assistant reply.
    pub message: String,
    /// Whether the skill considers the task complete.
    #[serde(default)]
    pub done: bool,
    /// What the turn spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// The tool events the turn took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<ToolEvent>>,
}

/// A simulated-user turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UserResponse {
    /// The user's next message.
    pub message: String,
    /// Whether the conversation ends.
    #[serde(default)]
    pub stop: bool,
    /// What the turn spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// A free-text judgement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AssessResponse {
    /// The judgement.
    pub text: String,
    /// What it spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// The fixed strings of a harness's JSON-RPC stream this codec reads.
pub mod transcript {
    /// A frame's method.
    pub const METHOD: &str = "method";
    /// A reply frame's result.
    pub const RESULT: &str = "result";
    /// A notification's parameters.
    pub const PARAMS: &str = "params";
    /// The turn a notification reports on.
    pub const TURN: &str = "turn";
    /// A turn's status.
    pub const STATUS: &str = "status";
    /// The status of a turn that was lost.
    pub const FAILED: &str = "failed";
    /// The method of an error notification, and the field an error is in.
    pub const ERROR: &str = "error";
    /// How a codex stream names the home it was credentialed from.
    pub const CODEX_HOME: &str = "codexHome";
    /// A turn error's classification.
    pub const CODEX_ERROR_INFO: &str = "codexErrorInfo";
    /// A turn error's code.
    pub const CODE: &str = "code";
    /// A turn error's paragraph.
    pub const MESSAGE: &str = "message";
}

/// What a harness recorded about a turn it could not take: each field only
/// where it was text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnError {
    /// The classification a planner acts on (`usageLimitExceeded`).
    pub codex_error_info: Option<String>,
    /// A code.
    pub code: Option<String>,
    /// The paragraph beside them.
    pub message: Option<String>,
}

impl TurnError {
    fn of(recorded: &Map<String, Value>) -> Self {
        let text = |key: &str| recorded.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            codex_error_info: text(transcript::CODEX_ERROR_INFO),
            code: text(transcript::CODE),
            message: text(transcript::MESSAGE),
        }
    }
}

/// How much of a failure's cause reaches a surface.
pub const CAUSE_LIMIT: usize = 80;

/// The cause named when a transcript records a failed turn and nothing about
/// why.
pub const NO_CAUSE_RECORDED: &str = "no cause recorded";

/// The cause named for a turn with no assistant content at all.
pub const NO_ASSISTANT_CONTENT: &str = "the turn produced no assistant message";

/// The identity named when a transcript names no harness.
pub const UNIDENTIFIED_HARNESS: &str = "an unidentified harness";

/// The identity of a codex stream credentialed from the primary home.
pub const CODEX_IDENTITY: &str = "codex";

/// The identity of a codex stream credentialed from the alternate home.
pub const CODEX_ALTERNATE_IDENTITY: &str = "codex:alternate";

/// The variable naming the host's alternate codex home, which a transcript's
/// `codexHome` is compared against.
pub const CODEX_ALT_HOME_ENV: &str = "ORCHESTRATOR_CODEX_ALT_HOME";

/// The machine transcript `spoken` is — every non-blank line a JSON object — or
/// `None` when it is prose.
#[must_use]
pub fn transcript_frames(spoken: &str) -> Option<Vec<Value>> {
    let mut frames = Vec::new();
    for line in spoken.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(frame @ Value::Object(_)) => frames.push(frame),
            _ => return None,
        }
    }
    (!frames.is_empty()).then_some(frames)
}

/// What a lost turn recorded — an empty record when it recorded nothing — or
/// `None` when nothing proves the turn was lost. Read from the end backwards,
/// because a transcript ends in what became of the turn; only a failure is
/// recognised, never "this looks like a transcript".
#[must_use]
pub fn lost_turn_error(frames: &[Value]) -> Option<TurnError> {
    for frame in frames.iter().rev() {
        let params = frame.get(transcript::PARAMS);
        let turn = params.and_then(|params| params.get(transcript::TURN));
        let failed = turn
            .and_then(|turn| turn.get(transcript::STATUS))
            .and_then(Value::as_str)
            == Some(transcript::FAILED);
        if failed {
            return Some(
                turn.and_then(|turn| turn.get(transcript::ERROR))
                    .and_then(Value::as_object)
                    .map_or_else(TurnError::default, TurnError::of),
            );
        }
        if frame.get(transcript::METHOD).and_then(Value::as_str) == Some(transcript::ERROR) {
            return Some(
                params
                    .and_then(|params| params.get(transcript::ERROR))
                    .and_then(Value::as_object)
                    .map_or_else(TurnError::default, TurnError::of),
            );
        }
    }
    None
}

/// One cause as a single bounded line: runs of whitespace and control
/// characters collapsed to one space, and at most [`CAUSE_LIMIT`] characters.
#[must_use]
pub fn clipped(cause: &str) -> String {
    let mut collapsed = String::with_capacity(cause.len());
    let mut gap = false;
    for character in cause.chars() {
        if character.is_whitespace() || character.is_control() {
            gap = true;
        } else {
            if gap && !collapsed.is_empty() {
                collapsed.push(' ');
            }
            gap = false;
            collapsed.push(character);
        }
    }
    if collapsed.chars().count() <= CAUSE_LIMIT {
        return collapsed;
    }
    let kept: String = collapsed.chars().take(CAUSE_LIMIT - 1).collect();
    format!("{}\u{2026}", kept.trim_end())
}

/// The shortest true name for why a turn was lost: the classification before
/// the code before the paragraph, each normalized before it is judged empty.
#[must_use]
pub fn cause_of(error: &TurnError) -> String {
    [&error.codex_error_info, &error.code, &error.message]
        .into_iter()
        .flatten()
        .map(|stated| clipped(stated))
        .find(|named| !named.is_empty())
        .unwrap_or_else(|| NO_CAUSE_RECORDED.to_owned())
}

/// The harness identity a transcript names: the alternate codex identity when
/// its `codexHome` is `alternate_home`, the primary one when it names another,
/// and an unidentified harness when it names none.
#[must_use]
pub fn identity_in(frames: &[Value], alternate_home: Option<&str>) -> &'static str {
    for frame in frames {
        let Some(home) = frame
            .get(transcript::RESULT)
            .and_then(|result| result.get(transcript::CODEX_HOME))
            .and_then(Value::as_str)
            .filter(|home| !home.trim().is_empty())
        else {
            continue;
        };
        let alternate = alternate_home.is_some_and(|alternate| {
            Path::new(alternate)
                .components()
                .eq(Path::new(home).components())
        });
        return if alternate {
            CODEX_ALTERNATE_IDENTITY
        } else {
            CODEX_IDENTITY
        };
    }
    UNIDENTIFIED_HARNESS
}

/// What a turn was, as liveness reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    /// The member produced content: it took its turn.
    Taken,
    /// The member's turn was lost.
    Lost {
        /// Why, bounded.
        cause: String,
        /// Which harness identity it ran as.
        identity: &'static str,
        /// How long the transcript it left was, when it left one.
        transcript_chars: Option<usize>,
    },
}

/// Read a turn's liveness out of its conversation — whether there is any
/// assistant content at all, and whether the last assistant message is a
/// transcript proving the turn was lost — and nothing else.
#[must_use]
pub fn liveness(messages: &[ConversationMessage], alternate_home: Option<&str>) -> Turn {
    let Some(said) = messages
        .iter()
        .filter(|message| message.role == Role::Assistant && !message.content.trim().is_empty())
        .map(|message| message.content.as_str())
        .next_back()
    else {
        return Turn::Lost {
            cause: NO_ASSISTANT_CONTENT.to_owned(),
            identity: UNIDENTIFIED_HARNESS,
            transcript_chars: None,
        };
    };
    let Some(frames) = transcript_frames(said) else {
        return Turn::Taken;
    };
    match lost_turn_error(&frames) {
        Some(error) => Turn::Lost {
            cause: cause_of(&error),
            identity: identity_in(&frames, alternate_home),
            transcript_chars: Some(said.chars().count()),
        },
        None => Turn::Taken,
    }
}

/// How a composed task's opening line names its run: this, then the run, then
/// a closing backtick.
pub const RUN_IN_TASK_OPENS: &str = "onepipeline run `";

/// The variable naming the run when a frame does not.
pub const RUN_ENV: &str = "ONEPIPELINE_RUN_ID";

/// The variable naming the serving session's asker.
pub const ASKER_ENV: &str = crate::channel::ASKER_ENV;

/// The variable naming the serving session's bound, in whole seconds.
pub const SESSION_ENV: &str = "ONEPIPELINE_SERVE_SESSION_SECONDS";

/// The run a task's opening line names, when it names one.
#[must_use]
pub fn run_in_task(task: &str) -> Option<&str> {
    let opening = task.lines().next()?;
    let after = &opening[opening.find(RUN_IN_TASK_OPENS)? + RUN_IN_TASK_OPENS.len()..];
    let run = &after[..after.find('`')?];
    (!run.is_empty()).then_some(run)
}

/// Whether `run` is one word a run id can be: letters, digits, `_`, `.` and
/// `-`, not starting with `.` or `-`.
#[must_use]
pub fn is_safe_run(run: &str) -> bool {
    run.bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric() || first == b'_')
        && run
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

/// The kind a lost turn is raised under.
pub const SURFACE_KIND_OF_A_FAILED_TURN: &str = "monitor-failed";

/// The kind the completion criterion is raised under.
pub const SURFACE_KIND_OF_A_COMPLETION: &str = "monitor-completion";

/// What a member that took its turn is told.
pub const TURN_TAKEN_ACKNOWLEDGED: &str = "Your turn was taken and no planner surface was raised for it: prose reaches nobody. A report reaches the planner only as a `finding` op in a reply envelope, which arrives once and carries the node it is about. Your next turn opens after the graph's hold, and reads the detailed stream from your cursor: everything that landed since the timestamp `monitor.cursor` holds, not the last few lines.";

/// Why that ruling is a non-completion.
pub const TURN_TAKEN_REASON: &str =
    "the monitor took its turn; a report reaches the planner as a finding";

/// How the completion criterion is asked; `{criterion}` is replaced whole.
pub const SCORE_ASKED: &str = "The monitor's conversation has ended and onejudge is scoring that conversation against the monitor's own completion bar. You are this member's judge side, so the score is yours: did this watch meet the bar quoted below? Reply `completion: true` if it did, `false` if it did not. THE RUN IS NOT BLOCKED ON THIS and nothing waits for you: the question is non-blocking, no answer is read as `false`, and the run settles either way.\n\n{criterion}";

/// The reason a score carries when the ruling said nothing else.
pub const SCORE_UNEXPLAINED: &str =
    "the planner ruled on this member's completion bar over the channel";

/// What a member is told when the answer was a live edit; `{named}` names the
/// edits.
pub const ANSWERED_THE_ENGINE: &str = "The planner's answer to this question was a live graph edit addressed to the engine, not a ruling on your watch: {named}. It was routed to the command path when it was sent, so nothing here re-applies it. Your question has not been answered — keep watching the detailed stream and raise what you find.";

/// Appended when the planner also wrote prose beside the edits; `{said}` is it.
pub const PLANNER_ALSO_SAID: &str = " The planner also said: {said}";

/// How an edit naming no op is named.
pub const UNNAMED_EDIT: &str = "an unnamed edit";

/// A live edit recognised where a ruling was expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveEdit {
    /// The edits, named as the planner would recognise them, bounded.
    pub named: String,
    /// The prose that rode beside them, or empty.
    pub said: String,
}

/// The live edit `envelope` is — commands, and no boolean `completion` — or
/// `None` for anything else.
#[must_use]
pub fn live_edit(envelope: &Value) -> Option<LiveEdit> {
    let fields = envelope.as_object()?;
    if fields.get(field::COMPLETION).is_some_and(Value::is_boolean) {
        return None;
    }
    let commands = fields.get(field::COMMANDS)?.as_array()?;
    if commands.is_empty() {
        return None;
    }
    let named: Vec<String> = commands
        .iter()
        .map(|command| {
            let text = |key: &str| {
                command
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
            };
            match (text(field::EDIT_OP), text(field::EDIT_TARGET)) {
                (Some(op), Some(target)) => format!("{op} {target}"),
                (Some(op), None) => op.to_owned(),
                (None, Some(target)) => format!("{UNNAMED_EDIT} {target}"),
                (None, None) => UNNAMED_EDIT.to_owned(),
            }
        })
        .collect();
    let said: Vec<&str> = [field::MESSAGE, field::REASON]
        .into_iter()
        .filter_map(|key| fields.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect();
    Some(LiveEdit {
        named: clipped(&named.join(", ")),
        said: said.join(" "),
    })
}

/// A check the consumer supplies on what a session's questions are about, given
/// the run: `onepipeline` supplies "the run's graph has this node".
pub type AboutCheck = Box<dyn Fn(&str, &Address) -> Result<(), String> + Send>;

/// The onejudge codec.
pub struct Onejudge {
    run_env: EnvName,
    run_from_env: Option<String>,
    alternate_home: Option<String>,
    about_check: Option<AboutCheck>,
}

impl std::fmt::Debug for Onejudge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Onejudge")
            .field("run_env", &self.run_env)
            .field("run_from_env", &self.run_from_env)
            .field("alternate_home", &self.alternate_home)
            .field("about_check", &self.about_check.is_some())
            .finish()
    }
}

impl Default for Onejudge {
    fn default() -> Self {
        Self {
            run_env: RUN_ENV
                .parse()
                .expect("RUN_ENV is a valid environment name"),
            run_from_env: None,
            alternate_home: None,
            about_check: None,
        }
    }
}

impl Onejudge {
    /// The codec, reading the run from no environment until told.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same codec, with `value` as what the variable `name` names the run —
    /// read at the boundary by whoever reads the environment.
    #[must_use]
    pub fn with_run_env(mut self, name: EnvName, value: Option<String>) -> Self {
        self.run_env = name;
        self.run_from_env = value;
        self
    }

    /// The same codec, comparing a transcript's `codexHome` with `home`.
    #[must_use]
    pub fn with_alternate_home(mut self, home: Option<String>) -> Self {
        self.alternate_home = home;
        self
    }

    /// The same codec, holding what a session's questions are about to `check`.
    #[must_use]
    pub fn with_about_check(mut self, check: AboutCheck) -> Self {
        self.about_check = Some(check);
        self
    }

    /// The run a frame belongs to: its task's opening line, and otherwise the
    /// environment.
    fn run(&self, task: Option<&str>) -> Result<String, CodecFailure> {
        let named = task
            .and_then(run_in_task)
            .map(str::to_owned)
            .or_else(|| self.run_from_env.clone().filter(|run| !run.is_empty()))
            .ok_or_else(|| {
                CodecFailure::Refused(format!(
                    "the frame names no run — no task opening `{RUN_IN_TASK_OPENS}<run>`` — and {} names none, so there is no run to serve",
                    self.run_env
                ))
            })?;
        if !is_safe_run(&named) {
            return Err(CodecFailure::Refused(format!(
                "the run is named {named:?}, which is not a run id: one word of letters, digits, `_`, `.` and `-`"
            )));
        }
        Ok(named)
    }

    fn about(&self, run: &str, session: &ServeSession<'_>) -> Result<(), CodecFailure> {
        match (&self.about_check, &session.options().about) {
            (Some(check), Some(about)) => check(run, about).map_err(|why| {
                CodecFailure::Refused(format!(
                    "the questions of run {run} are about {about}, which it refuses: {why}"
                ))
            }),
            _ => Ok(()),
        }
    }

    fn supervise(
        &self,
        frame: &SupervisorFrame,
        session: &mut ServeSession<'_>,
    ) -> Result<Value, CodecFailure> {
        let run = self.run(Some(&frame.task))?;
        self.about(&run, session)?;
        match liveness(&frame.messages, self.alternate_home.as_deref()) {
            Turn::Taken => serde_json::to_value(SupervisorResponse::Continue {
                message: TURN_TAKEN_ACKNOWLEDGED.to_owned(),
                reason: TURN_TAKEN_REASON.to_owned(),
            })
            .map_err(|failure| CodecFailure::Failed(failure.to_string())),
            Turn::Lost {
                cause,
                identity,
                transcript_chars,
            } => {
                let told = match transcript_chars {
                    Some(chars) => format!(
                        "It said nothing, so there is nothing to answer; its {chars}-character transcript is not repeated here."
                    ),
                    None => "It said nothing, so there is nothing to answer.".to_owned(),
                };
                let message =
                    format!("monitor turn failed: {cause} on {identity}. {told} Run {run}.");
                session
                    .raise(serde_json::json!({
                        "kind": SURFACE_KIND_OF_A_FAILED_TURN,
                        "message": message,
                        "source": PROPOSAL,
                        "blocking": false,
                    }))
                    .map_err(|failure| {
                        CodecFailure::Failed(format!(
                            "the monitor's turn on run {run} was lost ({cause} on {identity}), and the `{SURFACE_KIND_OF_A_FAILED_TURN}` surface saying so could not be raised: {failure}"
                        ))
                    })?;
                Err(CodecFailure::Failed(format!(
                    "the monitor's turn on run {run} was lost: {cause} on {identity}; a `{SURFACE_KIND_OF_A_FAILED_TURN}` surface says so on {}",
                    session.queue()
                )))
            }
        }
    }

    fn score(
        &self,
        frame: &JudgeFrame,
        session: &mut ServeSession<'_>,
    ) -> Result<Value, CodecFailure> {
        let criterion = match frame {
            JudgeFrame::Boolean { criterion, .. } => criterion,
            JudgeFrame::Numeric { .. } => {
                return Err(CodecFailure::Refused(
                    "a `numeric` score is not one this codec gives: a planner rules with a `completion` boolean, which is no score on a scale; it scores `boolean` criteria alone"
                        .to_owned(),
                ));
            }
        }
        .trim();
        if criterion.is_empty() {
            return Err(CodecFailure::Refused(
                "the `judge` frame's criterion is blank, so there is nothing to put to the planner"
                    .to_owned(),
            ));
        }
        let run = self.run(None)?;
        self.about(&run, session)?;
        let window = session.options().reply_window.as_secs();
        let (correlation, answer) = session
            .ask(
                serde_json::json!({
                    "kind": SURFACE_KIND_OF_A_COMPLETION,
                    "message": SCORE_ASKED.replace("{criterion}", criterion),
                    "source": PROPOSAL,
                }),
                false,
            )
            .map_err(|failure| {
                CodecFailure::Failed(format!(
                    "the completion criterion of run {run} could not be put to the planner: {failure}"
                ))
            })?;
        let unsatisfied = |why: String| JudgeResponse {
            value: JudgeValue::Bool(false),
            reason: Some(why),
            usage: None,
        };
        let score = match answer {
            Answer::Reply(record) => {
                let envelope = record
                    .get(field::REPLY)
                    .filter(|envelope| envelope.is_object())
                    .unwrap_or(&record);
                if let Some(edit) = live_edit(envelope) {
                    let mut told = ANSWERED_THE_ENGINE.replace("{named}", &edit.named);
                    if !edit.said.is_empty() {
                        told.push_str(&PLANNER_ALSO_SAID.replace("{said}", &edit.said));
                    }
                    unsatisfied(told)
                } else if let Some(completion) =
                    envelope.get(field::COMPLETION).and_then(Value::as_bool)
                {
                    let said = [field::REASON, field::MESSAGE]
                        .into_iter()
                        .filter_map(|key| envelope.get(key).and_then(Value::as_str))
                        .map(str::trim)
                        .find(|text| !text.is_empty())
                        .unwrap_or(SCORE_UNEXPLAINED);
                    JudgeResponse {
                        value: JudgeValue::Bool(completion),
                        reason: Some(said.to_owned()),
                        usage: None,
                    }
                } else {
                    return Err(CodecFailure::Failed(format!(
                        "the answer echoing {correlation} is not a ruling — it carries no boolean `completion` — so no score was relayed: {}",
                        clipped(&envelope.to_string())
                    )));
                }
            }
            Answer::Timeout => unsatisfied(format!(
                "no ruling on this member's completion bar arrived within {window} seconds, so it is scored unsatisfied; the question stands as {correlation}"
            )),
            Answer::Abandoned => unsatisfied(format!(
                "the question putting this member's completion bar to the planner ({correlation}) was abandoned before anyone ruled, so it is scored unsatisfied"
            )),
            Answer::Refused(refusal) => {
                return Err(CodecFailure::Failed(format!(
                    "the ruling on run {run}'s completion bar was refused, so no score was relayed: {refusal}"
                )))
            }
        };
        serde_json::to_value(score).map_err(|failure| CodecFailure::Failed(failure.to_string()))
    }
}

fn refused_op(word: &str) -> CodecFailure {
    CodecFailure::Refused(format!(
        "`{word}` is not an operation this codec serves: it serves `{}` and `{}`, since a planner rules with a `completion` boolean and nothing else",
        op::SUPERVISOR,
        op::JUDGE
    ))
}

impl Codec for Onejudge {
    fn name(&self) -> &CodecName {
        &CODEC_NAME
    }

    fn answer(
        &mut self,
        frame: &str,
        session: &mut ServeSession<'_>,
    ) -> Result<Value, CodecFailure> {
        let mut parsed: Value = serde_json::from_str(frame).map_err(|failure| {
            CodecFailure::Refused(format!("the frame is not JSON: {failure}"))
        })?;
        let word = parsed
            .get(OP)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CodecFailure::Refused(format!(
                    "the frame names no `{OP}`; a onejudge protocol v{PROTOCOL_VERSION} frame names one of {}",
                    op::ALL.join(", ")
                ))
            })?
            .to_owned();
        if !op::SERVED.contains(&word.as_str()) {
            return Err(refused_op(&word));
        }
        parsed
            .as_object_mut()
            .expect("a frame naming an op is an object")
            .remove(OP);
        let malformed = |failure| {
            CodecFailure::Refused(format!(
                "the `{word}` frame is not a onejudge protocol v{PROTOCOL_VERSION} frame: {failure}"
            ))
        };
        match word.as_str() {
            op::SUPERVISOR => serde_json::from_value::<SupervisorFrame>(parsed)
                .map_err(malformed)
                .and_then(|frame| self.supervise(&frame, session)),
            op::JUDGE => serde_json::from_value::<JudgeFrame>(parsed)
                .map_err(malformed)
                .and_then(|frame| self.score(&frame, session)),
            _ => Err(refused_op(&word)),
        }
    }
}
