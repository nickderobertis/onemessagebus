//! The one envelope: one JSON object per NDJSON line.
//!
//! Nothing here emits, orders, bounds, or redacts anything — this is the wire
//! shape and the types that fill it. `docs/wire.md` states the shape; the
//! contract tests hold these types to it.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::vocabulary::Vocabulary;

/// One event, as a producing process writes it and as a consumer reads it.
///
/// Merge order across streams is `(ts, stream, seq)`. A consumer detects loss
/// as a per-stream [`seq`](Self::seq) gap; there is no cross-stream promise
/// beyond the timestamps.
///
/// The type parameter is the [`Vocabulary`] the envelope is written over: it
/// decides the source words, the dimensions carried between `kind` and
/// `labels`, and the label set. Deserialization refuses an unknown top-level
/// field, a `seq` that is not an unsigned integer, a source word the vocabulary
/// does not admit, and a missing required field — each by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(bound = "")]
#[schemars(
    bound = "V::Source: JsonSchema, V::Dimensions: JsonSchema, V::Labels: JsonSchema",
    rename = "Envelope"
)]
pub struct Envelope<V: Vocabulary> {
    /// The envelope schema version the producer wrote against.
    pub v: u32,
    /// RFC 3339, millisecond precision, UTC.
    pub ts: String,
    /// Unique id of the producing process.
    pub stream: String,
    /// Monotonic per [`stream`](Self::stream).
    pub seq: u64,
    /// What produced the event, in the vocabulary's words.
    pub source: V::Source,
    /// What happened, as its producer named it.
    pub kind: Kind,
    /// The vocabulary's reserved top-level dimensions, carried as named fields
    /// here — between `kind` and `labels` on the wire — and omitted when
    /// absent. [`NoDimensions`] writes nothing.
    #[serde(flatten)]
    pub dimensions: V::Dimensions,
    /// The reserved keys the vocabulary declares plus free-form extras.
    /// Producers stamp what they know; enrichers never rewrite.
    #[serde(default)]
    pub labels: V::Labels,
    /// Kind-specific detail. Text fields are bounded by
    /// [`MAX_PAYLOAD_TEXT_BYTES`](crate::MAX_PAYLOAD_TEXT_BYTES); larger
    /// evidence is an [`ArtifactRef`].
    #[serde(default)]
    pub payload: Map<String, Value>,
    /// Evidence stored by the producing library and referenced by id.
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
}

impl<V: Vocabulary> Envelope<V> {
    /// The key three streams merge in: `(ts, stream, seq)`.
    #[must_use]
    pub fn order_key(&self) -> (&str, &str, u64) {
        (&self.ts, &self.stream, self.seq)
    }
}

/// What happened, as the kebab-case wire string.
///
/// Open on the wire and a string here, because a relay carries a sibling's
/// kinds without interpreting them; a producing library keeps its own closed
/// enum and converts into this with `From`.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Kind(pub String);

impl Kind {
    /// The wire spelling, which is what a filter's `kind` glob matches.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Kind {
    fn from(kind: &str) -> Self {
        Self(kind.to_owned())
    }
}

impl From<String> for Kind {
    fn from(kind: String) -> Self {
        Self(kind)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An open source word: whatever produced the event, by name.
///
/// The [`Open`](crate::Open) vocabulary's source. A vocabulary that closes the
/// set declares an enum instead, and serde is what refuses a word outside it.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Source(pub String);

impl Source {
    /// The word as it travels.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Source {
    fn from(source: &str) -> Self {
        Self(source.to_owned())
    }
}

impl From<String> for Source {
    fn from(source: String) -> Self {
        Self(source)
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An open label set: an ordered map of whatever the producer stamped.
///
/// The [`Open`](crate::Open) vocabulary's labels. A vocabulary that reserves
/// keys declares a struct with a field per key and a flattened map for the
/// rest, which serializes to the same bytes when the same keys are stamped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Labels(pub Map<String, Value>);

impl Labels {
    /// No labels at all.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What is stamped under `key`, when it is text.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    /// Stamp `value` under `key`, replacing what was there.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        self.0.insert(key.into(), value.into());
        self
    }

    /// The same labels with `value` stamped under `key`.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.insert(key, value);
        self
    }
}

/// No top-level dimensions: writes nothing, and refuses any field it is handed.
///
/// The type an envelope's `dimensions` has under a vocabulary that declares
/// none. Refusing unknown fields is what makes an envelope over such a
/// vocabulary reject an unknown top-level key by name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoDimensions {}

/// Evidence too large for a payload: stored by the producing library and
/// referenced by id, to be read back through that library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// Identifier, unique within the producing library's store.
    pub id: String,
    /// What the artifact is — a gate log, a check log, a transcript, a report.
    pub kind: String,
    /// Size of the stored artifact.
    pub bytes: u64,
}
