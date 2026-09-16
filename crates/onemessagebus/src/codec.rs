//! Serving a member protocol over a bus: frames in, one response out per
//! frame.
//!
//! A [`Codec`] reads one frame of some member's protocol and answers it with one
//! response object — raising what it must on a queue, and asking what it must
//! ask, through the [`ServeSession`] it is handed. [`Bus::serve`] is the loop
//! every codec shares: the frame stream is read on a thread of its own, so a
//! session bound is a real deadline rather than something noticed between
//! frames; each frame's answer is written and flushed before the next is read;
//! and how the session ends decides what becomes of the questions it asked. A
//! session that reaches its **bound** with the stream still open leaves them
//! counted — the member is still there, and still owed every answer. A stream
//! that **ends** marks each one still unanswered abandoned: nothing is listening
//! for those answers now, and a later listener of the same asker takes them back.
//! That is `onepipeline`'s `Served` distinction, kept.
//!
//! What a frame means is the codec's alone; no protocol's word is named here.
//! The configuration's `codecs` block ([`CodecConfig`]) carries what a host
//! configures for one, by name. `docs/codecs.md` states the contract.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, Write};
use std::num::NonZeroU64;
use std::str::FromStr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ask::{Address, Answer, AskOptions, Correlation, Pending};
use crate::config::{Bus, BusError};
use crate::queue::{Asker, Pushed};
use crate::schema::SchemaId;
use crate::transport::QueueName;

/// How long a question a codec asks waits for its ruling, when nothing says.
pub const DEFAULT_REPLY_WINDOW: Duration = Duration::from_secs(30);

/// The longest codec name accepted, in bytes.
const CODEC_NAME_LIMIT: usize = 64;

/// The longest environment variable name accepted, in bytes.
const ENV_NAME_LIMIT: usize = 128;

/// A codec's name, as a configuration's `codecs` block and `serve --codec`
/// give it: a lowercase ASCII letter, then lowercase letters, digits and `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct CodecName(String);

/// The name of an environment variable a codec reads: ASCII letters, digits
/// and `_`, not starting with a digit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct EnvName(String);

/// Why text is not a [`CodecName`] or an [`EnvName`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not {what}: {why}")]
pub struct NameRefused {
    /// What was offered.
    pub text: String,
    /// What it was offered as.
    pub what: &'static str,
    /// What is wrong with it.
    pub why: &'static str,
}

impl CodecName {
    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl EnvName {
    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CodecName {
    type Err = NameRefused;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refuse = |why| NameRefused {
            text: text.to_owned(),
            what: "a codec name",
            why,
        };
        let Some(first) = text.bytes().next() else {
            return Err(refuse("it is empty"));
        };
        if text.len() > CODEC_NAME_LIMIT {
            return Err(refuse("it is longer than a codec name can be"));
        }
        if !first.is_ascii_lowercase() {
            return Err(refuse("it does not start with a lowercase letter"));
        }
        if !text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(refuse(
                "it carries something other than lowercase letters, digits and `-`",
            ));
        }
        Ok(Self(text.to_owned()))
    }
}

impl FromStr for EnvName {
    type Err = NameRefused;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refuse = |why| NameRefused {
            text: text.to_owned(),
            what: "an environment variable name",
            why,
        };
        let Some(first) = text.bytes().next() else {
            return Err(refuse("it is empty"));
        };
        if text.len() > ENV_NAME_LIMIT {
            return Err(refuse("it is longer than a variable name can be"));
        }
        if first.is_ascii_digit() {
            return Err(refuse("it starts with a digit"));
        }
        if !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(refuse(
                "it carries something other than ASCII letters, digits and `_`",
            ));
        }
        Ok(Self(text.to_owned()))
    }
}

macro_rules! name_traits {
    ($name:ident, $schema:literal, $pattern:literal, $description:literal) => {
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed($schema)
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "description": $description,
                    "pattern": $pattern
                })
            }
        }
    };
}

name_traits!(
    CodecName,
    "CodecName",
    "^[a-z][a-z0-9-]{0,63}$",
    "A codec's name: a lowercase letter, then lowercase letters, digits and `-`."
);
name_traits!(
    EnvName,
    "EnvName",
    "^[A-Za-z_][A-Za-z0-9_]{0,127}$",
    "The name of an environment variable: ASCII letters, digits and `_`, not starting with a digit."
);

/// What a host configures for one codec, under its name in the configuration's
/// `codecs` block.
// llmlint: ignore-block[invalid_states_unrepresentable] These are the language-neutral serde and JSON Schema configuration objects Contract B requires SDKs to generate. Paths, non-empty collections, scalar equality and the exclusive raise outcome are validated together by Config::load with fully qualified codec/entry/binding errors; wrapper types or nested outcome enums would change the required YAML shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodecConfig {
    /// The queue the codec raises and asks on; `serve <queue>` must name the
    /// same one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueName>,
    /// Whole seconds a question the codec asks waits for its ruling before the
    /// codec answers without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_window_seconds: Option<NonZeroU64>,
    /// The variable the session bound is read from, in whole seconds, when
    /// `--session-seconds` is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_env: Option<EnvName>,
    /// The variable the asker is read from, when `--asker` is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker_env: Option<EnvName>,
    /// The variable what the member's questions are about is read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about_env: Option<EnvName>,
    /// The frame field whose string value selects an entry in [`Self::frames`].
    pub select: String,
    /// The protocol's entries, keyed by the selected field's value.
    pub frames: BTreeMap<String, FrameConfig>,
}

/// One selected frame in a configured codec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameConfig {
    /// The registered schema that validates this frame.
    pub schema: SchemaId,
    /// Actions tried in order; the first whose condition holds is applied.
    pub bindings: Vec<Binding>,
}

/// One optional field-equality condition and its action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Binding {
    /// The condition; absent means this binding always holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<FieldEquals>,
    /// What to do with the frame.
    #[serde(flatten)]
    pub action: BindingAction,
}

/// Equality against one frame field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldEquals {
    /// Dot-separated object keys.
    pub field: String,
    /// A JSON scalar. SDKs intentionally expose this as their arbitrary-JSON
    /// type because its runtime type participates in equality.
    pub equals: Value,
}

/// The four operations a configured codec can perform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "do", rename_all = "lowercase", deny_unknown_fields)]
pub enum BindingAction {
    /// Write a response and do nothing on the bus.
    Answer {
        /// The frame response as arbitrary JSON, preserving template value types.
        response: Value,
    },
    /// Refuse the frame and write nothing.
    Refuse {
        /// The refusal written on stderr.
        message: String,
    },
    /// Raise a record, then respond or fail.
    Raise {
        /// The record raised on the served queue as arbitrary JSON.
        record: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// The response written after raising as arbitrary JSON.
        response: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// The failure written after raising.
        fail: Option<String>,
    },
    /// Ask a question and relay its ruling.
    Ask {
        /// The question asked on the served queue as arbitrary JSON.
        record: Value,
        #[serde(default)]
        /// Whether the question blocks its queue.
        blocking: bool,
        /// The arbitrary-JSON response resolved from a ruling.
        response: Value,
        /// The arbitrary-JSON response used when no ruling can be resolved.
        unanswered: Value,
    },
}
// llmlint: ignore-end[invalid_states_unrepresentable] The load boundary above has validated every representable wire object before a Config is returned.

/// A codec interpreted entirely from one configuration entry.
#[derive(Debug, Clone)]
pub struct ConfiguredCodec {
    name: CodecName,
    config: CodecConfig,
}

impl ConfiguredCodec {
    /// Build a codec after validating the configuration rules that serde alone
    /// cannot express.
    pub fn new(name: CodecName, config: CodecConfig) -> Result<Self, String> {
        validate_codec(&name, &config)?;
        Ok(Self { name, config })
    }
}

fn path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(root, |value, key| {
        (!key.is_empty())
            .then(|| value.as_object()?.get(key))
            .flatten()
    })
}

fn valid_path(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(|part| !part.is_empty())
}

fn placeholders(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' if bytes.get(index + 1) == Some(&b'{') => index += 2,
            b'}' if bytes.get(index + 1) == Some(&b'}') => index += 2,
            b'{' => {
                let end = text[index + 1..]
                    .find('}')
                    .map(|offset| index + 1 + offset)
                    .ok_or_else(|| "an opening `{` has no closing `}`".to_owned())?;
                let placeholder = &text[index + 1..end];
                let (root, field) = placeholder.split_once('.').ok_or_else(|| {
                    format!("placeholder `{{{placeholder}}}` has no root and path")
                })?;
                if !matches!(root, "frame" | "reply") || !valid_path(field) {
                    return Err(format!(
                        "placeholder `{{{placeholder}}}` is not a frame or reply path"
                    ));
                }
                found.push((root.to_owned(), field.to_owned()));
                index = end + 1;
            }
            b'}' => return Err("an unescaped `}` has no opening `{`".to_owned()),
            _ => index += 1,
        }
    }
    Ok(found)
}

fn visit_strings(value: &Value, allow_reply: bool) -> Result<(), String> {
    match value {
        Value::String(text) => {
            for (root, _) in placeholders(text)? {
                if root == "reply" && !allow_reply {
                    return Err(
                        "a `reply` placeholder is only allowed in an ask response".to_owned()
                    );
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                visit_strings(value, allow_reply)?;
            }
        }
        Value::Object(fields) => {
            for value in fields.values() {
                visit_strings(value, allow_reply)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn validate_codec(name: &CodecName, config: &CodecConfig) -> Result<(), String> {
    if !valid_path(&config.select) {
        return Err(format!(
            "codecs.{name}.select is not a dot-separated object path"
        ));
    }
    if config.frames.is_empty() {
        return Err(format!("codecs.{name}.frames is empty"));
    }
    for (entry, frame) in &config.frames {
        if frame.bindings.is_empty() {
            return Err(format!("codecs.{name}.frames.{entry}.bindings is empty"));
        }
        for (index, binding) in frame.bindings.iter().enumerate() {
            let at = format!("codecs.{name}.frames.{entry}.bindings[{index}]");
            if let Some(condition) = &binding.when {
                if !valid_path(&condition.field) {
                    return Err(format!(
                        "{at}.when.field is not a dot-separated object path"
                    ));
                }
                if condition.equals.is_array() || condition.equals.is_object() {
                    return Err(format!("{at}.when.equals is not a JSON scalar"));
                }
            }
            let check =
                |value, reply| visit_strings(value, reply).map_err(|why| format!("{at}: {why}"));
            match &binding.action {
                BindingAction::Answer { response } => check(response, false)?,
                BindingAction::Refuse { message } => check(&Value::String(message.clone()), false)?,
                BindingAction::Raise {
                    record,
                    response,
                    fail,
                } => {
                    check(record, false)?;
                    match (response, fail) {
                        (Some(response), None) => check(response, false)?,
                        (None, Some(fail)) => check(&Value::String(fail.clone()), false)?,
                        _ => {
                            return Err(format!(
                                "{at}: raise takes exactly one of `response` and `fail`"
                            ))
                        }
                    }
                }
                BindingAction::Ask {
                    record,
                    response,
                    unanswered,
                    ..
                } => {
                    check(record, false)?;
                    check(response, true)?;
                    check(unanswered, false)?;
                    validate_mappings(response).map_err(|why| format!("{at}.response: {why}"))?;
                }
            }
        }
    }
    Ok(())
}

fn validate_mappings(value: &Value) -> Result<(), String> {
    match value {
        Value::Array(values) => values.iter().try_for_each(validate_mappings),
        Value::Object(fields) if fields.contains_key("from") => {
            if !fields
                .keys()
                .all(|key| matches!(key.as_str(), "from" | "default"))
            {
                return Err("a mapping has a key other than `from` and `default`".to_owned());
            }
            let paths: Vec<&str> = match &fields["from"] {
                Value::String(path) => vec![path],
                Value::Array(paths) if !paths.is_empty() => paths
                    .iter()
                    .map(|path| {
                        path.as_str()
                            .ok_or_else(|| "a `from` list contains a non-string path".to_owned())
                    })
                    .collect::<Result<_, _>>()?,
                _ => return Err("`from` is not a path or non-empty list of paths".to_owned()),
            };
            if paths
                .iter()
                .any(|path| !path.starts_with("reply.") || !valid_path(&path[6..]))
            {
                return Err("a `from` path is not under `reply`".to_owned());
            }
            if let Some(default) = fields.get("default") {
                visit_strings(default, true)?;
            }
            Ok(())
        }
        Value::Object(fields) => fields.values().try_for_each(validate_mappings),
        _ => Ok(()),
    }
}

fn render_string(text: &str, frame: &Value, reply: Option<&Value>) -> Result<Value, String> {
    let fields = placeholders(text)?;
    if fields.len() == 1 {
        let (root, field) = &fields[0];
        let exact = format!("{{{root}.{field}}}");
        if text == exact {
            return Ok(match root.as_str() {
                "frame" => path(frame, field)
                    .cloned()
                    .unwrap_or(Value::String(String::new())),
                "reply" => reply
                    .and_then(|value| path(value, field))
                    .cloned()
                    .unwrap_or(Value::String(String::new())),
                _ => unreachable!(),
            });
        }
    }
    let mut rendered = String::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'{' && bytes.get(index + 1) == Some(&b'{') {
            rendered.push('{');
            index += 2;
            continue;
        }
        if bytes[index] == b'}' && bytes.get(index + 1) == Some(&b'}') {
            rendered.push('}');
            index += 2;
            continue;
        }
        if bytes[index] == b'{' {
            let end = text[index + 1..]
                .find('}')
                .map(|offset| index + 1 + offset)
                .expect("validated template");
            let placeholder = &text[index + 1..end];
            let (root, field) = placeholder.split_once('.').expect("validated placeholder");
            let value = match root {
                "frame" => path(frame, field),
                "reply" => reply.and_then(|value| path(value, field)),
                _ => None,
            };
            if let Some(value) = value {
                match value {
                    Value::String(text) => rendered.push_str(text),
                    other => rendered.push_str(&other.to_string()),
                }
            }
            index = end + 1;
        } else {
            let character = text[index..].chars().next().expect("inside a string");
            rendered.push(character);
            index += character.len_utf8();
        }
    }
    Ok(Value::String(rendered))
}

fn render(value: &Value, frame: &Value, reply: Option<&Value>) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => render_string(text, frame, reply)?,
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| render(value, frame, reply))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| Ok((key.clone(), render(value, frame, reply)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn ruling(value: &Value) -> &Value {
    value
        .as_object()
        .and_then(|fields| fields.get("reply"))
        .unwrap_or(value)
}

fn resolve_ask(value: &Value, frame: &Value, reply: &Value) -> Result<Option<Value>, String> {
    let Value::Object(fields) = value else {
        return render(value, frame, Some(reply)).map(Some);
    };
    let is_mapping = fields.contains_key("from")
        && fields
            .keys()
            .all(|key| matches!(key.as_str(), "from" | "default"));
    if is_mapping {
        let paths = match &fields["from"] {
            Value::String(path) => vec![path.as_str()],
            Value::Array(paths) => paths
                .iter()
                .map(|path| {
                    path.as_str().ok_or_else(|| {
                        "an ask response `from` list contains a non-string path".to_owned()
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("an ask response `from` is not a path or list of paths".to_owned()),
        };
        for candidate in paths {
            let candidate = candidate.strip_prefix("reply.").unwrap_or(candidate);
            if let Some(value) = path(reply, candidate).filter(|value| value.as_str() != Some("")) {
                return Ok(Some(value.clone()));
            }
        }
        return fields
            .get("default")
            .map(|value| render(value, frame, Some(reply)))
            .transpose();
    }
    let mut output = serde_json::Map::new();
    for (key, value) in fields {
        let Some(value) = resolve_ask(value, frame, reply)? else {
            return Ok(None);
        };
        output.insert(key.clone(), value);
    }
    Ok(Some(Value::Object(output)))
}

impl Codec for ConfiguredCodec {
    fn name(&self) -> &CodecName {
        &self.name
    }

    fn answer(
        &mut self,
        text: &str,
        session: &mut ServeSession<'_>,
    ) -> Result<Value, CodecFailure> {
        let frame: Value = serde_json::from_str(text).map_err(|failure| {
            CodecFailure::Refused(format!("the frame is not JSON: {failure}"))
        })?;
        if !frame.is_object() {
            return Err(CodecFailure::Refused(
                "the frame is not a JSON object".to_owned(),
            ));
        }
        let selected = path(&frame, &self.config.select);
        let word = selected.and_then(Value::as_str).ok_or_else(|| {
            CodecFailure::Refused(format!(
                "the frame's `{}` is absent or not a string; declared entries: {}",
                self.config.select,
                self.config
                    .frames
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        let entry = self.config.frames.get(word).ok_or_else(|| {
            CodecFailure::Refused(format!(
                "the frame's `{}` is `{word}`, not one of the declared entries: {}",
                self.config.select,
                self.config
                    .frames
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        session
            .bus()
            .registry()
            .check(&entry.schema, &frame)
            .map_err(|failure| {
                CodecFailure::Refused(format!(
                    "the frame does not validate against {}: {failure}",
                    entry.schema
                ))
            })?;
        let binding = entry
            .bindings
            .iter()
            .find(|binding| {
                binding.when.as_ref().is_none_or(|condition| {
                    path(&frame, &condition.field) == Some(&condition.equals)
                })
            })
            .ok_or_else(|| CodecFailure::Refused(format!("no binding holds for entry `{word}`")))?;
        let rendered = |value: &Value| render(value, &frame, None).map_err(CodecFailure::Refused);
        match &binding.action {
            BindingAction::Answer { response } => rendered(response),
            BindingAction::Refuse { message } => Err(CodecFailure::Refused(
                rendered(&Value::String(message.clone()))?
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )),
            BindingAction::Raise {
                record,
                response,
                fail,
            } => {
                session.raise(rendered(record)?).map_err(|failure| {
                    CodecFailure::Failed(format!("the queue refused the raised record: {failure}"))
                })?;
                match (response, fail) {
                    (Some(response), None) => rendered(response),
                    (None, Some(fail)) => Err(CodecFailure::Failed(
                        rendered(&Value::String(fail.clone()))?
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    )),
                    _ => unreachable!("validated configuration"),
                }
            }
            BindingAction::Ask {
                record,
                blocking,
                response,
                unanswered,
            } => {
                let (_, answer) = session
                    .ask(rendered(record)?, *blocking)
                    .map_err(|failure| {
                        CodecFailure::Failed(format!("the queue refused the question: {failure}"))
                    })?;
                match answer {
                    Answer::Reply(reply) => {
                        let reply = ruling(&reply);
                        resolve_ask(response, &frame, reply)
                            .map_err(CodecFailure::Refused)?
                            .map_or_else(|| rendered(unanswered), Ok)
                    }
                    Answer::Timeout | Answer::Abandoned => rendered(unanswered),
                    Answer::Refused(failure) => Err(CodecFailure::Failed(format!(
                        "the answer was refused: {}",
                        failure.reason
                    ))),
                }
            }
        }
    }
}

/// How a serving session runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOptions {
    /// Who the session listens for: stamped on what it raises and asks.
    pub asker: Option<Asker>,
    /// What the session's questions are about.
    pub about: Option<Address>,
    /// When the session stops of its own accord; with none, it serves until the
    /// frame stream ends.
    pub session: Option<Duration>,
    /// How long each question the codec asks waits for its ruling.
    pub reply_window: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            asker: None,
            about: None,
            session: None,
            reply_window: DEFAULT_REPLY_WINDOW,
        }
    }
}

/// Why a codec answered a frame with no response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecFailure {
    /// The frame is one this codec does not serve, or is malformed: refused
    /// input, named.
    Refused(String),
    /// The member failed, and the codec has said so where it must: the session
    /// ends with that failure.
    Failed(String),
}

/// A codec: one frame in, one response object out.
pub trait Codec: Send {
    /// The codec's name, as `serve --codec` gives it.
    fn name(&self) -> &CodecName;

    /// Answer one frame — its line, as the member wrote it — raising and asking
    /// through `session` what the answer needs.
    ///
    /// # Errors
    ///
    /// [`CodecFailure::Refused`] for a frame it does not serve;
    /// [`CodecFailure::Failed`] for a member that failed.
    fn answer(
        &mut self,
        frame: &str,
        session: &mut ServeSession<'_>,
    ) -> Result<Value, CodecFailure>;
}

/// What a codec reaches the bus through while it serves one session.
pub struct ServeSession<'a> {
    bus: &'a Bus,
    queue: &'a QueueName,
    options: &'a ServeOptions,
    asked: Vec<Pending<Value>>,
}

impl fmt::Debug for ServeSession<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServeSession")
            .field("queue", self.queue)
            .field("options", self.options)
            .field("asked", &self.asked.len())
            .finish_non_exhaustive()
    }
}

impl ServeSession<'_> {
    /// The bus served over.
    #[must_use]
    pub fn bus(&self) -> &Bus {
        self.bus
    }

    /// The queue the session raises and asks on.
    #[must_use]
    pub fn queue(&self) -> &QueueName {
        self.queue
    }

    /// How the session runs.
    #[must_use]
    pub fn options(&self) -> &ServeOptions {
        self.options
    }

    /// Raise `record` on the served queue, asking nothing: stamped with the
    /// session's asker and what it is about where it names neither, then sent as
    /// [`Bus::send`] sends.
    ///
    /// # Errors
    ///
    /// As [`Bus::send`].
    pub fn raise(&mut self, record: Value) -> Result<Vec<(QueueName, Pushed<Value>)>, BusError> {
        let record = match record {
            Value::Object(mut fields) => {
                if let Some(asker) = &self.options.asker {
                    fields
                        .entry("asker")
                        .or_insert_with(|| Value::String(asker.as_str().to_owned()));
                }
                if let Some(about) = &self.options.about {
                    fields
                        .entry(crate::ask::ABOUT)
                        .or_insert_with(|| Value::String(about.as_str().to_owned()));
                }
                Value::Object(fields)
            }
            other => other,
        };
        self.bus.send(self.queue, record)
    }

    /// Ask `question` on the served queue under the session's asker and about,
    /// wait the reply window for its answer, and keep the question: the
    /// session's ending decides whether one still unanswered is abandoned.
    ///
    /// # Errors
    ///
    /// As [`Bus::ask`]: the question was not asked.
    pub fn ask(
        &mut self,
        question: Value,
        blocking: bool,
    ) -> Result<(Correlation, Answer<Value>), BusError> {
        let pending = self.bus.ask::<Value, Value>(
            self.queue,
            question,
            AskOptions {
                blocking,
                asker: self.options.asker.clone(),
                about: self.options.about.clone(),
            },
        )?;
        let answer = pending.wait(self.options.reply_window);
        let correlation = pending.correlation().clone();
        self.asked.push(pending);
        Ok((correlation, answer))
    }
}

/// How a serving session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// The frame stream ended: nothing is listening for the answers to what the
    /// session asked, so each one still unanswered was marked abandoned.
    StreamEnded {
        /// How many questions were marked.
        abandoned: usize,
    },
    /// The session reached its bound with the stream still open: the member is
    /// still there, so every question it asked stays counted.
    SessionOver {
        /// How many questions stand unanswered.
        standing: usize,
    },
}

/// Why a serving session ended in failure.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// A frame the codec refused, named.
    #[error("{0}")]
    Refused(String),
    /// A member the codec reported failed.
    #[error("{0}")]
    Failed(String),
    /// The frame stream failed mid-read — not the same fact as its ending.
    #[error("the frame stream could not be read: {0}")]
    Stream(std::io::Error),
    /// The frame-reader thread could not be started.
    #[error("the frame reader could not be started: {0}")]
    Spawn(std::io::Error),
    /// A response could not be written.
    #[error("a response could not be written: {0}")]
    Write(std::io::Error),
    /// The bus refused, or its transport failed.
    #[error(transparent)]
    Bus(#[from] BusError),
}

impl Bus {
    /// Serve `codec` over `queue`: read frames from `input` one line at a time,
    /// write each frame's response to `output` as one line of JSON, and end when
    /// the stream ends or `options.session` elapses.
    ///
    /// The bound is asked before each frame is read and never during an
    /// exchange, so a member is never left waiting on a response this session
    /// decided not to write. Blank lines are passed over.
    ///
    /// # Errors
    ///
    /// [`BusError::UnknownQueue`] before anything is read; the codec's refusal
    /// or failure, ending the session with nothing it asked marked; a stream
    /// that failed mid-read; a response that could not be written.
    pub fn serve(
        &self,
        queue: &QueueName,
        codec: &mut dyn Codec,
        options: &ServeOptions,
        input: Box<dyn BufRead + Send>,
        output: &mut dyn Write,
    ) -> Result<Served, ServeError> {
        self.queue(queue)?;
        let deadline = options
            .session
            .and_then(|session| Instant::now().checked_add(session));
        let (frames, arriving) = mpsc::channel();
        std::thread::Builder::new()
            .name("onemessagebus-frame-reader".to_owned())
            .spawn(move || {
                for line in input.lines() {
                    if frames.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(ServeError::Spawn)?;
        let mut session = ServeSession {
            bus: self,
            queue,
            options,
            asked: Vec::new(),
        };
        let bound_reached = loop {
            let line = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break true;
                    }
                    match arriving.recv_timeout(left) {
                        Ok(line) => line,
                        Err(RecvTimeoutError::Timeout) => break true,
                        Err(RecvTimeoutError::Disconnected) => break false,
                    }
                }
                None => match arriving.recv() {
                    Ok(line) => line,
                    Err(_) => break false,
                },
            };
            let line = line.map_err(ServeError::Stream)?;
            if line.trim().is_empty() {
                continue;
            }
            let response =
                codec
                    .answer(line.trim(), &mut session)
                    .map_err(|failure| match failure {
                        CodecFailure::Refused(why) => ServeError::Refused(why),
                        CodecFailure::Failed(why) => ServeError::Failed(why),
                    })?;
            let mut written = serde_json::to_vec(&response)
                .map_err(|failure| ServeError::Write(std::io::Error::other(failure)))?;
            written.push(b'\n');
            output
                .write_all(&written)
                .and_then(|()| output.flush())
                .map_err(ServeError::Write)?;
        };
        let mut unanswered = Vec::new();
        for pending in &session.asked {
            if pending.reply_record().map_err(BusError::from)?.is_none() {
                unanswered.push(pending);
            }
        }
        if bound_reached {
            return Ok(Served::SessionOver {
                standing: unanswered.len(),
            });
        }
        for pending in &unanswered {
            pending.abandon().map_err(BusError::from)?;
        }
        Ok(Served::StreamEnded {
            abandoned: unanswered.len(),
        })
    }
}
