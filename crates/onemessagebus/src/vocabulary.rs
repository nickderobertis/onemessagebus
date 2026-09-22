//! What a consumer declares over the bus: its source words, its reserved label
//! keys, and the top-level dimensions its envelopes carry.
//!
//! The core knows the *shape* of an envelope — a version, a stamp, a stream, a
//! sequence number, a source, a kind, the labels, the payload, the artifacts —
//! and nothing about which words go in it. A [`Vocabulary`] supplies those words
//! as types, so an envelope over a billing vocabulary and one over a shipping
//! vocabulary are the same [`Envelope`](crate::Envelope) with different type
//! parameters, and neither can carry the other's keys by accident.

use std::fmt::Debug;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::envelope::{Labels, NoDimensions, Source};
use crate::filter::LabelMatch;

/// What every value that travels on the wire is: serializable both ways, with a
/// JSON Schema, and comparable — so a contract test can hold it to its document
/// and a registry can record it.
pub trait Wire:
    Serialize + DeserializeOwned + JsonSchema + Clone + PartialEq + Debug + Send + Sync + 'static
{
}

impl<T> Wire for T where
    T: Serialize
        + DeserializeOwned
        + JsonSchema
        + Clone
        + PartialEq
        + Debug
        + Send
        + Sync
        + 'static
{
}

/// What a reserved key admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Admits {
    /// A string.
    Text,
    /// A non-negative integer.
    Integer,
    /// One of a closed set of words.
    Word,
}

/// One reserved key — a label the vocabulary types, or a top-level dimension —
/// and what it admits.
///
/// Data rather than only a type, because two things outside Rust's type system
/// read it: the command line, which has to type a `--label attempt=2` as an
/// integer rather than a string, and the SDK manifest, which tells a generated
/// client which keys a matcher may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Reserved {
    /// The key as it appears on the wire.
    pub key: &'static str,
    /// What a value under it must be.
    pub admits: Admits,
}

impl Reserved {
    /// A reserved key admitting text.
    #[must_use]
    pub const fn text(key: &'static str) -> Self {
        Self {
            key,
            admits: Admits::Text,
        }
    }

    /// A reserved key admitting a non-negative integer.
    #[must_use]
    pub const fn integer(key: &'static str) -> Self {
        Self {
            key,
            admits: Admits::Integer,
        }
    }

    /// A reserved key admitting one of a closed set of words.
    #[must_use]
    pub const fn word(key: &'static str) -> Self {
        Self {
            key,
            admits: Admits::Word,
        }
    }
}

/// The words a bus carries: which sources exist, which label keys are reserved
/// and what each admits, and which top-level dimensions an envelope has.
///
/// A vocabulary is types plus a little data. The types decide the wire shape —
/// a closed `Source` enum refuses a word it does not name where an open one
/// carries anything; a typed `Labels` struct puts the reserved keys in fields
/// and flattens the rest — and the data ([`RESERVED`](Self::RESERVED),
/// [`DIMENSIONS`](Self::DIMENSIONS)) is what the command line and the SDK
/// manifest read, since neither can see a Rust type.
///
/// Matching, stamping and merging are generic over the vocabulary through serde:
/// the core serializes a label set or a matcher's fields to a JSON map when it
/// needs to look inside one, so a vocabulary declares no logic of its own.
pub trait Vocabulary: Debug + Clone + PartialEq + Send + Sync + 'static {
    /// The word naming what produced an event. Closed (an enum) or open (a
    /// string newtype); serialized as a JSON string either way.
    type Source: Wire + std::fmt::Display;
    /// The reserved top-level dimensions, carried on the wire between `kind`
    /// and `labels` as named fields. [`NoDimensions`] when there are none.
    type Dimensions: Wire + Default;
    /// The label set: the reserved keys plus free-form extras.
    type Labels: Wire + Default;
    /// What a matcher may ask of the dimensions and the reserved labels — one
    /// optional field per key, refusing any other.
    type Fields: Wire + Default;

    /// The vocabulary's name, which is what `--profile` selects one by.
    const NAME: &'static str;
    /// The reserved label keys, in the order the wire lists them.
    const RESERVED: &'static [Reserved];
    /// The top-level dimensions, in the order the wire lists them.
    const DIMENSIONS: &'static [Reserved];
    /// The source word an emitter stamps when its caller names none.
    const DEFAULT_SOURCE: &'static str;

    /// The envelope schema version a producer of `source` writes against.
    ///
    /// A profile fact rather than a bus fact: producers of one vocabulary move
    /// at their own pace, and a relayed envelope keeps its producer's number.
    fn write_version(source: &Self::Source) -> u32;
}

/// The vocabulary that reserves nothing: any source word, any labels, no
/// dimensions.
///
/// What a stream is read through when nothing more is known about it, and the
/// one profile the command line offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Open;

impl Vocabulary for Open {
    type Source = Source;
    type Dimensions = NoDimensions;
    type Labels = Labels;
    type Fields = LabelMatch;

    const NAME: &'static str = "open";
    const RESERVED: &'static [Reserved] = &[];
    const DIMENSIONS: &'static [Reserved] = &[];
    const DEFAULT_SOURCE: &'static str = "onemessagebus";

    fn write_version(_: &Self::Source) -> u32 {
        1
    }
}
