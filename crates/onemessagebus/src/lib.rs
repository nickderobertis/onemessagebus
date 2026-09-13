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

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bounds;
mod capability;
mod clock;
pub mod conformance;
mod emit;
mod envelope;
mod filter;
mod read;
mod redact;
mod schema;
pub mod sdk_schema;
mod vocabulary;

pub use bounds::{
    bound_detail, bound_payload, bound_text, MAX_ACTIVITY_DETAIL_CHARS, MAX_PAYLOAD_TEXT_BYTES,
    TRUNCATED_KEY,
};
pub use capability::{
    Capability, FlagKind, OptionBinding, StdoutShape, UncoveredFlag, CAPABILITIES,
};
pub use clock::now_rfc3339;
pub use emit::{Emitter, EmitterError, Unrecorded};
pub use envelope::{ArtifactRef, Envelope, Kind, Labels, NoDimensions, Source};
pub use filter::{glob, Filter, FilterError, LabelMatch, Matcher};
pub use read::{Merge, Reader, Reading, Readings, Record, Refused, Torn};
pub use redact::{Redactor, CREDENTIAL_PREFIXES, CREDENTIAL_WORDS, REDACTED};
pub use schema::{
    CheckError, Message, Read, Registry, RegistryError, SchemaId, SchemaIdError, SchemaViolation,
    UnknownVersion,
};
pub use vocabulary::{Admits, Open, Reserved, Vocabulary, Wire};
