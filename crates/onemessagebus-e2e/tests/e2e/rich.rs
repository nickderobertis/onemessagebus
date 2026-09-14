//! A document exercising every construct the Rust renderer covers, declared by
//! hand so its schema is what schemars emits for real Rust — and held, through
//! the binary, to the declaration `schema gen --lang rust` renders for it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Every construct the renderer covers, in one document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rich {
    /// Plain text.
    pub name: String,
    /// A small count.
    pub count: u32,
    /// A big count.
    pub total: u64,
    /// A signed one.
    pub delta: i64,
    /// A ratio.
    pub ratio: f64,
    /// A switch.
    pub enabled: bool,
    /// Optional text, omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Some tags.
    pub tags: Vec<String>,
    /// A closed word.
    pub level: Level,
    /// An optional closed word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mood: Option<Level>,
    /// A nested document.
    pub inner: Inner,
    /// Anything at all.
    pub extra: serde_json::Value,
    /// An open map.
    pub bag: serde_json::Map<String, serde_json::Value>,
    /// A typed map.
    pub counts: std::collections::BTreeMap<String, u64>,
    /// A key spelled differently on the wire.
    #[serde(rename = "kebab-key")]
    pub kebab_key: String,
    /// Defaulted, so not required.
    #[serde(default)]
    pub label: String,
    /// A named scalar: a newtype over text.
    pub code: Code,
    /// An optional named scalar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Code>,
}

/// A code: text with a name of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Code(pub String);

/// A closed set of words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Level {
    /// The low one.
    #[serde(rename = "low")]
    Low,
    /// The high one.
    #[serde(rename = "high")]
    High,
}

/// A nested document, open to extra keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Inner {
    /// Its id.
    pub id: String,
}
