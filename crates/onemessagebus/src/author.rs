//! Who wrote a record, and which operations each author may carry.
//!
//! An [`Author`] is an open word in the core; a profile declares its operation
//! vocabulary and may provide built-in authors, while configuration declares
//! additional names such as `sentinel` and the operations each may issue. An
//! [`Allowlist`] is exhaustive: an operation
//! not granted to an author is refused **by omission**, so an operation added to
//! the vocabulary later is refused for every author nobody granted it to, and
//! the refusal names the author, the operation and the reason recorded for it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Who wrote a record: an open word.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Author(pub String);

impl Author {
    /// The author as its word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Author {
    fn from(word: &str) -> Self {
        Self(word.to_owned())
    }
}

impl fmt::Display for Author {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One operation an author may or may not carry: a profile's closed
/// vocabulary, named by its wire word.
pub trait Operation: Clone + Eq + fmt::Debug {
    /// The operation's wire word.
    fn name(&self) -> &str;
}

/// An operation known only by its word: what a configuration's
/// `capabilities` list names, and what an allowlist is read as by a consumer
/// that does not link the profile's own type.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct OpWord(pub String);

impl Operation for OpWord {
    fn name(&self) -> &str {
        &self.0
    }
}

/// An operation refused to an author.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("'{op}' is not an op {author} may issue: {reason}")]
pub struct Refusal {
    /// Who asked.
    pub author: Author,
    /// The operation's word.
    pub op: String,
    /// Why it is not granted: the reason recorded for it, or that nothing
    /// granted it.
    pub reason: String,
}

/// A configuration that would widen what an author may do, or names what the
/// allowlist does not have.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{key}: {why}")]
pub struct NarrowingRefused {
    /// The configuration key, e.g. `authors.sentinel.capabilities`.
    pub key: String,
    /// What is wrong with it.
    pub why: String,
}

/// The reason an operation nothing granted is refused with, when none was
/// recorded for it.
pub const NOT_GRANTED: &str = "nothing grants it to this author";

/// Which operations each author may carry, over a closed vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist<Op: Operation> {
    vocabulary: Vec<Op>,
    grants: BTreeMap<Author, Vec<Op>>,
    /// Per author, per operation word, why it is not granted.
    reasons: BTreeMap<Author, BTreeMap<String, String>>,
}

impl<Op: Operation> Allowlist<Op> {
    /// An allowlist over `vocabulary` that grants nothing to anybody.
    #[must_use]
    pub fn new(vocabulary: impl IntoIterator<Item = Op>) -> Self {
        Self {
            vocabulary: vocabulary.into_iter().collect(),
            grants: BTreeMap::new(),
            reasons: BTreeMap::new(),
        }
    }

    /// Declare `author`, granted nothing yet.
    pub fn declare(&mut self, author: Author) -> &mut Self {
        self.grants.entry(author).or_default();
        self
    }

    /// Grant `op` to `author`, declaring the author.
    pub fn grant(&mut self, author: Author, op: Op) -> &mut Self {
        let granted = self.grants.entry(author).or_default();
        if !granted.contains(&op) {
            granted.push(op);
        }
        self
    }

    /// Record why `op` is not granted to `author`, which its refusal says.
    pub fn refuse(&mut self, author: Author, op: &Op, reason: impl Into<String>) -> &mut Self {
        self.grants.entry(author.clone()).or_default();
        self.reasons
            .entry(author)
            .or_default()
            .insert(op.name().to_owned(), reason.into());
        self
    }

    /// Whether `author` may carry `op`.
    ///
    /// # Errors
    ///
    /// The [`Refusal`] naming the author, the operation and its reason, for an
    /// operation not granted — including one no reason was recorded for, and
    /// every operation of an author nobody declared.
    pub fn allows(&self, author: &Author, op: &Op) -> Result<(), Refusal> {
        if self
            .grants
            .get(author)
            .is_some_and(|granted| granted.contains(op))
        {
            return Ok(());
        }
        Err(Refusal {
            author: author.clone(),
            op: op.name().to_owned(),
            reason: self
                .reasons
                .get(author)
                .and_then(|reasons| reasons.get(op.name()))
                .cloned()
                .unwrap_or_else(|| NOT_GRANTED.to_owned()),
        })
    }

    /// The operations `author` may carry, in grant order.
    #[must_use]
    pub fn granted(&self, author: &Author) -> Vec<Op> {
        self.grants.get(author).cloned().unwrap_or_default()
    }

    /// Every declared author.
    #[must_use]
    pub fn authors(&self) -> Vec<Author> {
        self.grants.keys().cloned().collect()
    }

    /// Whether `author` is declared.
    #[must_use]
    pub fn declares(&self, author: &Author) -> bool {
        self.grants.contains_key(author)
    }

    /// The vocabulary.
    #[must_use]
    pub fn vocabulary(&self) -> &[Op] {
        &self.vocabulary
    }

    /// Narrow `author` to the operations `capabilities` names, which a
    /// configuration may do and never the reverse. An operation dropped this way
    /// is refused with `reason`.
    ///
    /// # Errors
    ///
    /// [`NarrowingRefused`] naming `key` for an author this allowlist does not
    /// declare, a word that is no operation of its vocabulary, or an operation
    /// the author is not granted — a widening. Nothing is narrowed.
    pub fn narrow(
        &mut self,
        key: &str,
        author: &Author,
        capabilities: &[String],
        reason: &str,
    ) -> Result<(), NarrowingRefused> {
        let Some(granted) = self.grants.get(author) else {
            return Err(NarrowingRefused {
                key: key.to_owned(),
                why: format!(
                    "`{author}` is not an author this profile declares, and a configuration may not add one; the authors are: {}",
                    self.authors()
                        .iter()
                        .map(Author::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        };
        let mut kept = Vec::new();
        for word in capabilities {
            let Some(op) = self.vocabulary.iter().find(|op| op.name() == word) else {
                return Err(NarrowingRefused {
                    key: key.to_owned(),
                    why: format!(
                        "`{word}` is not an op; the ops are: {}",
                        self.vocabulary
                            .iter()
                            .map(Operation::name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            };
            if !granted.contains(op) {
                return Err(NarrowingRefused {
                    key: key.to_owned(),
                    why: format!(
                        "`{word}` is not granted to {author} by the profile, and a configuration may narrow an author's grants but never widen them"
                    ),
                });
            }
            kept.push(op.clone());
        }
        let dropped: BTreeSet<String> = granted
            .iter()
            .filter(|op| !kept.contains(op))
            .map(|op| op.name().to_owned())
            .collect();
        let reasons = self.reasons.entry(author.clone()).or_default();
        for word in dropped {
            reasons.insert(word, reason.to_owned());
        }
        self.grants.insert(author.clone(), kept);
        Ok(())
    }

    /// The same allowlist with each operation known only by its word.
    #[must_use]
    pub fn words(&self) -> Allowlist<OpWord> {
        let word = |op: &Op| OpWord(op.name().to_owned());
        Allowlist {
            vocabulary: self.vocabulary.iter().map(word).collect(),
            grants: self
                .grants
                .iter()
                .map(|(author, ops)| (author.clone(), ops.iter().map(word).collect()))
                .collect(),
            reasons: self.reasons.clone(),
        }
    }
}
