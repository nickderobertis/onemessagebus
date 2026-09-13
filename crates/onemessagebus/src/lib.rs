//! A typed NDJSON message bus, generic over the vocabulary a consumer declares.
//!
//! One envelope shape ([`Envelope`]), one filter grammar ([`Filter`],
//! [`Matcher`]), the payload bounds and redaction every producer applies
//! ([`bound_text`], [`bound_payload`], [`bound_detail`], [`Redactor`]), a schema
//! registry with version read-sets ([`Registry`], [`SchemaId`]), and an emitter
//! and reader over NDJSON streams ([`Emitter`], [`Reader`], [`Merge`]).
//!
//! Nothing here names an agent. Which source words exist, which label keys are
//! reserved, and which top-level dimensions an envelope carries are a
//! [`Vocabulary`]'s to declare: the `onemessagebus-agent` crate declares the
//! agent stack's, and [`Open`] is the vocabulary that reserves nothing. The
//! [`conformance`] module is the one journey every vocabulary is driven
//! through, so a vocabulary of your own is proven the same way the agent one is.
//!
//! The wire shape — one JSON object per line, `(ts, stream, seq)` merge order,
//! per-stream `seq` gaps as loss detection — is stated in `docs/wire.md` and
//! held by `docs/contract.md`.
//!
//! Beside the streams, an [`Inbox`] is a typed channel into a running process
//! whose [`Sender`] learns what the receiver did with each message: in one
//! process ([`InProcess`]), across processes through a directory ([`Spool`]),
//! or carried to a receiver that is not running ([`Carry`]). The disposition a
//! receiver answers with is the consumer's own type ([`Disposition`]); the
//! inbox contract is stated in `docs/inbox.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bounds;
mod capability;
mod carry;
mod clock;
pub mod conformance;
mod emit;
mod envelope;
mod filter;
mod inbox;
mod read;
mod redact;
mod schema;
pub mod sdk_schema;
mod spool;
mod vocabulary;

pub use bounds::{
    bound_detail, bound_payload, bound_text, MAX_ACTIVITY_DETAIL_CHARS, MAX_PAYLOAD_TEXT_BYTES,
    TRUNCATED_KEY,
};
pub use capability::{
    Capability, FlagKind, OptionBinding, StdoutShape, UncoveredFlag, CAPABILITIES,
};
pub use carry::{CarriedEntry, Carry, CARRY_SCHEMA_VERSION};
pub use clock::now_rfc3339;
pub use emit::{Emitter, EmitterError, Unrecorded};
pub use envelope::{ArtifactRef, Envelope, Kind, Labels, NoDimensions, Source};
pub use filter::{glob, Filter, FilterError, LabelMatch, Matcher};
pub use inbox::{
    Answered, BackendError, Carried, Closed, Delivered, Disposition, InProcess, Inbox,
    InboxBackend, Sender, Undelivered,
};
pub use read::{Merge, Reader, Reading, Readings, Record, Refused, Torn};
pub use redact::{Redactor, CREDENTIAL_PREFIXES, CREDENTIAL_WORDS, REDACTED};
pub use schema::{
    CheckError, Message, Read, Registry, RegistryError, SchemaId, SchemaIdError, SchemaViolation,
    UnknownVersion,
};
pub use spool::{Spool, SPOOL_SCHEMA_VERSION, SPOOL_WAIT};
pub use vocabulary::{Admits, Open, Reserved, Vocabulary, Wire};

/// The README's sample, compiled by `cargo test --doc` so a sample naming an
/// item this crate no longer has fails rather than reaching crates.io.
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
