//! The agent profile over the `onemessagebus` core: the vocabulary the agent
//! stack shares, declared once here and re-exported by every consumer.
//!
//! The core knows the shape of an envelope and nothing about agents. This
//! crate supplies the words: the three [`Source`]s, the four [`Phase`]s, the
//! six reserved [`Labels`], and the schema families the stack registers — the
//! event envelope at its two versions among them.
//! [`Agent`] is the [`Vocabulary`] those types make up, and [`Envelope`] is
//! the core's envelope over it, serializing to the same bytes the stack's
//! producers write today.
//!
//! The [`note`] module is the agent note contract — a role-addressed correction
//! into a running conversation — declared as the profile's first message family
//! over the core's inbox.
//!
//! The dependency runs one way: this crate depends on the core, and the core
//! never on this crate.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod event;
pub mod note;
pub mod registry;

use onemessagebus::{Reserved, Vocabulary};

pub use event::{
    AgentEnvelope, AgentFilter, ArtifactRef, Dimensions, Emitter, Envelope, EventFilter, Labels,
    MatchFields, Matcher, Merge, Phase, Reader, Source,
};
/// The note contract's own refusal, named apart from the core's
/// [`onemessagebus::Undelivered`] where both are in scope.
pub use note::Undelivered as NoteUndelivered;
pub use registry::{registry, EVENT_ENVELOPE_FAMILY};

/// The agent stack's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Agent;

/// The reserved label keys, in wire order.
pub const RESERVED_LABELS: &[Reserved] = &[
    Reserved::text("run_id"),
    Reserved::integer("round"),
    Reserved::text("node"),
    Reserved::text("step"),
    Reserved::text("member"),
    Reserved::text("persona"),
];

/// The reserved top-level dimensions: `phase` alone.
pub const DIMENSIONS: &[Reserved] = &[Reserved::word("phase")];

impl Vocabulary for Agent {
    type Source = Source;
    type Dimensions = Dimensions;
    type Labels = Labels;
    type Fields = MatchFields;

    const NAME: &'static str = "agent";
    const RESERVED: &'static [Reserved] = RESERVED_LABELS;
    const DIMENSIONS: &'static [Reserved] = DIMENSIONS;
    const DEFAULT_SOURCE: &'static str = "pipeline";

    /// The envelope version each producer writes against: `pipeline` moved to
    /// 2 when its journal's record shapes did; `agentgraph` and `vcs` write 1.
    /// A relayed envelope keeps its producer's number.
    fn write_version(source: &Self::Source) -> u32 {
        match source {
            Source::Agentgraph | Source::Vcs => 1,
            Source::Pipeline => 2,
        }
    }
}

/// The README's sample, compiled by `cargo test --doc` so a sample naming an
/// item this crate no longer has fails rather than reaching crates.io.
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
