//! The agent stack's envelope, labels, sources, phases and filter: the core's
//! generic types over the [`Agent`] vocabulary, and the closed enums that
//! vocabulary is made of.
//!
//! Every type here serializes to the bytes the stack's producers write today —
//! `crates/onemessagebus-agent/tests/recorded/` holds a stream from each and
//! round-trips it unchanged.

use std::fmt;

use onemessagebus::{Message, SchemaId, Vocabulary};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Agent;

pub use onemessagebus::ArtifactRef;

/// One event of the agent stack: the core's envelope over the agent vocabulary.
pub type Envelope = onemessagebus::Envelope<Agent>;

/// Which envelopes pass: the core's filter over the agent vocabulary.
pub type EventFilter = onemessagebus::Filter<Agent>;

/// One matcher of an [`EventFilter`]: `source`, `kind`, and the
/// [`MatchFields`] — `phase` and the five reserved labels a matcher may name.
pub type Matcher = onemessagebus::Matcher<Agent>;

/// The library that produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// `oneagentgraph`.
    Agentgraph,
    /// `onevcs`.
    Vcs,
    /// `onepipeline`.
    Pipeline,
}

impl Source {
    /// Every source, in the order a refusal lists them.
    #[must_use]
    pub const fn every() -> [Source; 3] {
        [Source::Agentgraph, Source::Vcs, Source::Pipeline]
    }

    /// The word this source travels as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Source::Agentgraph => "agentgraph",
            Source::Vcs => "vcs",
            Source::Pipeline => "pipeline",
        }
    }

    /// The envelope version a producer of this source writes against.
    #[must_use]
    pub fn write_version(self) -> u32 {
        Agent::write_version(&self)
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which part of a change's life an event belongs to.
///
/// Four phases over one change: the work is made, it is brought together with
/// the base it is going onto, it is proposed and ruled on, and what carries it
/// is released. Stamped by the producer, never derived by a reader: one kind's
/// phase is not always a fact about the kind.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// The work is being made.
    Development,
    /// The work is being brought together with the base.
    Integrate,
    /// The change request is open and being ruled on.
    Review,
    /// What carries the landed change is being released.
    Release,
}

impl Phase {
    /// The word this phase is spelled with, in a filter and in a rendering.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Phase::Development => "development",
            Phase::Integrate => "integrate",
            Phase::Review => "review",
            Phase::Release => "release",
        }
    }

    /// Every phase, in the order a refusal lists them.
    #[must_use]
    pub const fn every() -> [Phase; 4] {
        [
            Phase::Development,
            Phase::Integrate,
            Phase::Review,
            Phase::Release,
        ]
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The agent envelope's reserved top-level dimensions: `phase`, optional and
/// omitted from the wire when absent, exactly as `onepipeline` relays it and
/// `onevcs` stamps it.
///
/// Carried between `kind` and `labels` on the wire. Refuses any other
/// top-level key, which is what makes an agent envelope reject an unknown
/// field by name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dimensions {
    /// Which part of a change's life the event belongs to, as its producer
    /// classified it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
}

impl Dimensions {
    /// No phase.
    #[must_use]
    pub const fn none() -> Self {
        Self { phase: None }
    }

    /// At `phase`.
    #[must_use]
    pub const fn at(phase: Phase) -> Self {
        Self { phase: Some(phase) }
    }
}

impl From<Phase> for Dimensions {
    fn from(phase: Phase) -> Self {
        Self::at(phase)
    }
}

impl From<Option<Phase>> for Dimensions {
    fn from(phase: Option<Phase>) -> Self {
        Self { phase }
    }
}

/// The reserved label keys, plus whatever else a producer stamped.
///
/// Reserved keys are absent rather than empty when unknown, so an enricher can
/// tell "not stamped" from "stamped empty". The extras are flattened beside
/// them, in the order they were stamped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Labels {
    /// The run this event belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The round within the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    /// The graph node being executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The step within a node that runs several in sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Which member of a conversation produced the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The persona that member is running under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Free-form extras beyond the reserved keys above, carried untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Message for Labels {
    const SCHEMA: SchemaId = SchemaId::literal("agent", "labels", 1);
}

/// What an agent matcher may name beside `source` and `kind`: `phase` and the
/// reserved labels, each by exact equality against what the envelope carries.
/// `round` is deliberately not among them, as the grammar says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatchFields {
    /// The phase the envelope was stamped at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    /// The `run_id` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The `node` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The `step` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The `member` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The `persona` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
}

/// What the agent envelope carries that the core's field names do not spell:
/// the phase, by name.
pub trait AgentEnvelope {
    /// The phase the envelope was stamped at, if any.
    fn phase(&self) -> Option<Phase>;
}

impl AgentEnvelope for Envelope {
    fn phase(&self) -> Option<Phase> {
        self.dimensions.phase
    }
}

/// [`EventFilter::allows`] spelled in the agent vocabulary's own terms.
pub trait AgentFilter {
    /// Whether an envelope of `source`, `kind`, `labels` and `phase` passes.
    fn admits(&self, source: Source, kind: &str, labels: &Labels, phase: Option<Phase>) -> bool;
}

impl AgentFilter for EventFilter {
    fn admits(&self, source: Source, kind: &str, labels: &Labels, phase: Option<Phase>) -> bool {
        self.allows(&source, kind, &Dimensions { phase }, labels)
    }
}
