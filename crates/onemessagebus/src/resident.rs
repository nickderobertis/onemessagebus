//! The resident core's protocol: the lines `onemessagebus serve --resident`
//! reads and writes on its unix socket.
//!
//! Each request line names a capability by its SDK method and carries that
//! capability's options — the verb set *is* [`CAPABILITIES`](crate::CAPABILITIES),
//! so the parity gate that holds the SDK clients to the manifest holds the socket
//! to it too. Each request is answered by exactly one [`ResidentAnswer`] or
//! [`ResidentFailure`] line carrying its id; a `subscribe` request streams
//! [`ResidentEvent`] lines first, until its predicate holds or a
//! [`ResidentCancel`] line names its id.
//!
//! The protocol is a registered document like any other message, under
//! [`RESIDENT_PROTOCOL`], so `schema gen --lang json bus.resident-protocol@1`
//! prints it and both SDKs generate their protocol types from it.

use std::borrow::Cow;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::schema::{Message, SchemaId};

/// The id the resident protocol's document is registered under.
pub const RESIDENT_PROTOCOL: SchemaId = SchemaId::literal("bus", "resident-protocol", 1);

/// One line of the resident protocol, in either direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(title = "ResidentLine")]
pub enum ResidentLine {
    /// Client to resident: run one capability.
    Request(ResidentRequest),
    /// Client to resident: stop a streaming request.
    Cancel(ResidentCancel),
    /// Resident to client: a request did what it was asked.
    Answer(ResidentAnswer),
    /// Resident to client: a request was refused.
    Failure(ResidentFailure),
    /// Resident to client: one line a streaming request produced.
    Event(ResidentEvent),
}

impl Message for ResidentLine {
    const SCHEMA: SchemaId = RESIDENT_PROTOCOL;
}

/// Run one capability, as its SDK method, with its options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentRequest {
    /// Chosen by the client and echoed on every line answering this request.
    pub id: u64,
    /// The capability's SDK method, camelCase, as the manifest names it.
    // llmlint: ignore[invalid_states_unrepresentable] the verb set is the capability manifest, which is data the resident reads at run time and the SDKs generate from; a closed Rust enum here would be a second list of the same methods for the manifest to drift from, and the resident refuses a method the manifest does not name by name, listing the ones it does.
    pub verb: String,
    /// The capability's options, keyed as its options root keys them (camelCase).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
    /// The bytes the verb would read on stdin: a payload, a question, a reply, a
    /// message or a codec's frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

/// Stop the streaming request `id` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentCancel {
    /// The request to stop.
    pub id: u64,
    /// Always `true`.
    pub cancel: True,
}

/// What a request that did what it was asked answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentAnswer {
    /// The request answered.
    pub id: u64,
    /// The verb's output: the document of a `json` verb, the list of lines of a
    /// `jsonl` verb, the text of a `text` verb or a `--format text` rendering —
    /// and, for `subscribe`, `"until"` when its predicate held or `"cancelled"`.
    pub ok: Value,
}

/// A request the resident refused, in the command line's own words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentFailure {
    /// The request refused; `null` for a line that named no id to answer.
    pub id: Option<u64>,
    /// Why.
    pub error: ResidentRefusal,
}

/// Why a request was refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentRefusal {
    /// The exit code the command line gives this refusal.
    pub exit: ResidentExit,
    /// The command line's refusal, as it writes it after `onemessagebus: `.
    pub message: String,
    /// The document the verb printed before refusing — `ask`'s answer,
    /// `validate`'s verdict — when it printed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

/// One line a streaming request produced: a `subscribe` log record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentEvent {
    /// The streaming request.
    pub id: u64,
    /// The line, as the verb prints it: a `{position, record}` log record, or its
    /// text rendering under `--format text`.
    pub event: Value,
}

/// The two exit codes a refusal carries, as the command line's exit-code table
/// gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentExit {
    /// `1`: well-formed input whose answer is no.
    Failed,
    /// `2`: input the verb refuses.
    Invalid,
}

impl ResidentExit {
    /// The exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Failed => 1,
            Self::Invalid => 2,
        }
    }
}

impl Serialize for ResidentExit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.code())
    }
}

impl<'de> Deserialize<'de> for ResidentExit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u8::deserialize(deserializer)? {
            1 => Ok(Self::Failed),
            2 => Ok(Self::Invalid),
            other => Err(serde::de::Error::custom(format!(
                "{other} is not an exit code a refusal carries; it is 1 or 2"
            ))),
        }
    }
}

impl JsonSchema for ResidentExit {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ResidentExit")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "enum": [1, 2],
            "description": "1: well-formed input whose answer is no; 2: input the verb refuses."
        })
    }
}

/// The literal `true`: a cancel line says nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct True;

impl Serialize for True {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for True {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(
                "a cancel line's `cancel` is `true`; leave the request running by sending nothing",
            ))
        }
    }
}

impl JsonSchema for True {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("True")
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"const": true})
    }
}
