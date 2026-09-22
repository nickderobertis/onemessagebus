//! The message family the inbox journeys deliver: `demo.memo@1`, declared by
//! these tests over the core's public API, so the inbox is proven over a family
//! no product owns and this build does not register.

use onemessagebus::{Carried, Disposition, Message, SchemaId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which desk a memo is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Desk {
    Front,
    Back,
    Both,
}

/// A memo's prose, refused when blank, so a receiver refuses a memo whose text
/// nobody can read in its own words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct MemoText(String);

impl TryFrom<String> for MemoText {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if text.trim().is_empty() {
            return Err("a memo's text is blank".to_owned());
        }
        Ok(Self(text))
    }
}

impl From<MemoText> for String {
    fn from(text: MemoText) -> Self {
        text.0
    }
}

/// One memo: the desk it is for, what it says, and what it refers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Memo {
    pub to: Desk,
    pub text: MemoText,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl Memo {
    /// A memo for `to` saying `text`.
    pub fn to(to: Desk, text: &str) -> Self {
        Self {
            to,
            text: MemoText::try_from(text.to_owned()).expect("a memo's text"),
            reference: None,
        }
    }

    /// The same memo, referring to `reference`.
    pub fn referencing(mut self, reference: &str) -> Self {
        self.reference = Some(reference.to_owned());
        self
    }

    /// The memo's prose.
    pub fn text(&self) -> &str {
        &self.text.0
    }
}

impl Message for Memo {
    const SCHEMA: SchemaId = SchemaId::literal("demo", "memo", 1);
}

/// What a receiver did with a memo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filed {
    /// Kept for later: what a carried memo is answered with.
    Queued,
    /// Handed to a desk.
    Routed { desk: Desk },
    /// Signed for, with the receipt.
    Signed { receipt: String },
}

impl Disposition for Filed {}

impl Carried for Filed {
    fn carried() -> Self {
        Filed::Queued
    }
}

/// A receiver's inbox of memos.
pub type MemoInbox = onemessagebus::Inbox<Memo, Filed>;

/// A sender of memos.
pub type Memos = onemessagebus::Sender<Memo, Filed>;

/// A memo as JSON, the way `deliver` is handed one.
pub fn memo_json(to: &str, text: &str) -> String {
    serde_json::json!({"to": to, "text": text}).to_string()
}
