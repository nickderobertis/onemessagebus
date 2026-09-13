//! Serving a member's judge side over a bus: frames in, one response out per
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
/// `codecs` block. Every key is optional; every one names a constant the host
/// would otherwise pass on the command line or leave at its default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// The variable the run is read from, when a frame does not name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_env: Option<EnvName>,
    /// The variable what the member's questions are about is read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about_env: Option<EnvName>,
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
        std::thread::spawn(move || {
            for line in input.lines() {
                if frames.send(line).is_err() {
                    break;
                }
            }
        });
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
