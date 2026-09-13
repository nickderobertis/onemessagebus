//! Redaction of credential-shaped values, before anything leaves a producer.
//!
//! Two sources, because neither covers the other. A value this process was
//! handed in a credential-shaped environment variable is a credential whatever
//! it looks like; a value spelled like a host's own token is one whatever it
//! was named. Both tables are the ones the stack's producers already hold, so
//! a consumer that redacts through the bus redacts exactly what it redacts now.

use serde_json::Value;

/// What a redacted value is replaced with.
pub const REDACTED: &str = "[redacted]";

/// The words an environment variable's name makes it credential-shaped by.
pub const CREDENTIAL_WORDS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "APIKEY",
    "API_KEY",
    "PRIVATE_KEY",
];

/// The prefixes a value is a credential by, whatever it was named.
pub const CREDENTIAL_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "AKIA",
];

/// A value under a credential-shaped name shorter than this is not treated as
/// a secret: replacing every `yes` a `TOKEN_ENABLED` variable holds would
/// redact prose rather than credentials.
const MIN_SECRET_LEN: usize = 8;

/// The characters a prefixed token has to carry past its prefix to count as
/// one, so the prefix alone in prose is left alone.
const MIN_PREFIXED_TAIL: usize = 8;

/// Replaces credential-shaped values with [`REDACTED`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redactor {
    /// Literal values to replace wherever they appear.
    secrets: Vec<String>,
}

impl Redactor {
    /// A redactor that applies only the prefix rule: no literal values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A redactor holding every value this process's environment carries under
    /// a credential-shaped name — the rule the stack's producers apply — on top
    /// of the prefix rule.
    ///
    /// Read once, here: a redactor is built where an emitter is, and what the
    /// environment held then is what is redacted.
    #[must_use]
    pub fn from_env() -> Self {
        let mut redactor = Self::new();
        for (name, value) in std::env::vars() {
            let upper = name.to_ascii_uppercase();
            if CREDENTIAL_WORDS.iter().any(|word| upper.contains(word)) {
                redactor.add_secret(value);
            }
        }
        redactor
    }

    /// The same redactor, also replacing `secret` wherever it appears.
    #[must_use]
    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.add_secret(secret.into());
        self
    }

    fn add_secret(&mut self, secret: String) {
        if secret.len() >= MIN_SECRET_LEN && !self.secrets.contains(&secret) {
            self.secrets.push(secret);
        }
    }

    /// `text` with every credential-shaped value replaced.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        let mut clean = text.to_owned();
        for secret in &self.secrets {
            clean = clean.replace(secret, REDACTED);
        }
        clean
            .split_inclusive(|c: char| c.is_whitespace())
            .map(|word| {
                let trimmed = word.trim_end();
                let spacing = &word[trimmed.len()..];
                let bare = trimmed.trim_end_matches(['"', '\'', ',', ';', ')']);
                let punctuation = &trimmed[bare.len()..];
                if CREDENTIAL_PREFIXES.iter().any(|prefix| {
                    bare.starts_with(prefix) && bare.len() >= prefix.len() + MIN_PREFIXED_TAIL
                }) {
                    format!("{REDACTED}{punctuation}{spacing}")
                } else {
                    word.to_owned()
                }
            })
            .collect()
    }

    /// `value` with every string in it — however deeply nested — redacted.
    #[must_use]
    pub fn redact_value(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.redact(&text)),
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|item| self.redact_value(item))
                    .collect(),
            ),
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, self.redact_value(value)))
                    .collect(),
            ),
            other => other,
        }
    }
}
