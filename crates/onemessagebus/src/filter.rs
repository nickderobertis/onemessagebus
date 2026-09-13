//! The one filter grammar: `{include: [matcher...], exclude: [matcher...]}`.
//!
//! A matcher's fields are all optional and conjoin. `source` and the
//! vocabulary's dimensions and reserved labels match by exact equality; `kind`
//! is a glob over the kebab-case wire string. An absent or empty `include`
//! admits everything; a match in `exclude` rejects whatever `include` said.
//! `stream` and payload fields are deliberately not matchable.
//!
//! Filtering decides what is *emitted*, never what a producer acts on, and
//! `seq` numbers what the stream carries — so a filtered stream has no gaps.

use std::path::Path;

use schemars::JsonSchema;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::envelope::{take_or_default, Envelope};
use crate::vocabulary::{Admits, Vocabulary};

/// Which envelopes pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(bound = "", deny_unknown_fields)]
#[schemars(
    bound = "V::Source: JsonSchema, V::Fields: JsonSchema",
    rename = "Filter"
)]
pub struct Filter<V: Vocabulary> {
    /// Matchers an envelope satisfies one of to pass. Absent or empty admits
    /// every envelope, so a filter that only rejects need name nothing here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<Matcher<V>>,
    /// Matchers that reject. A match here rejects whatever
    /// [`include`](Self::include) said, so a broad include beside a narrow
    /// exclude is how "all of this except that" is written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<Matcher<V>>,
}

impl<V: Vocabulary> Default for Filter<V> {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

/// One matcher: every field it names must hold of an envelope, and a field it
/// does not name is not consulted.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(bound = "")]
#[schemars(
    bound = "V::Source: JsonSchema, V::Fields: JsonSchema",
    rename = "Matcher"
)]
pub struct Matcher<V: Vocabulary> {
    /// The producer, by exact equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<V::Source>,
    /// A glob over the kind's kebab-case wire string, where `*` stands for any
    /// run of characters including none and every other character is itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The vocabulary's dimensions and reserved labels the matcher names, by
    /// exact equality against what the envelope carries. A matcher naming a
    /// label the envelope did not stamp does not match it.
    #[serde(flatten)]
    pub fields: V::Fields,
}

/// Read by hand for the reason [`Envelope`] is: a derive with a flattened
/// field would drop a key the vocabulary does not admit rather than refuse it.
impl<'de, V: Vocabulary> Deserialize<'de> for Matcher<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields: Map<String, Value> = Map::deserialize(deserializer)?;
        let source = take_or_default(&mut fields, "source")?;
        let kind = take_or_default(&mut fields, "kind")?;
        let fields = serde_json::from_value(Value::Object(fields)).map_err(de::Error::custom)?;
        Ok(Self {
            source,
            kind,
            fields,
        })
    }
}

impl<V: Vocabulary> Default for Matcher<V> {
    fn default() -> Self {
        Self {
            source: None,
            kind: None,
            fields: V::Fields::default(),
        }
    }
}

/// A matcher's label asks under a vocabulary that reserves nothing: any key,
/// matched as text against what the envelope stamped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct LabelMatch(pub Map<String, Value>);

impl LabelMatch {
    /// Ask that `key` be stamped exactly `value`.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }
}

/// Why a filter document was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FilterError(pub String);

impl<V: Vocabulary> Filter<V> {
    /// The filter that admits everything.
    #[must_use]
    pub fn everything() -> Self {
        Self::default()
    }

    /// Read a filter from the text of a spec: JSON, or the YAML the grammar is
    /// written in, of which JSON is a subset. Validated before it is handed
    /// back, so a filter nobody could honour is refused where it is read.
    ///
    /// # Errors
    ///
    /// A [`FilterError`] naming the fault: a field the grammar does not have,
    /// a matcher naming no field, or one naming an empty field.
    pub fn parse(spec: &str) -> Result<Self, FilterError> {
        let filter: Self = serde_norway::from_str(spec)
            .map_err(|failure| FilterError(format!("the event filter is unusable: {failure}")))?;
        filter.validate().map_err(FilterError)?;
        Ok(filter)
    }

    /// The filter a spec names: the document itself inline as JSON when its
    /// first non-space character is `{`, and otherwise a path to a file holding
    /// one — the `--event-filter` spelling the stack's command lines accept.
    ///
    /// # Errors
    ///
    /// A [`FilterError`] for a file that cannot be read, or for anything
    /// [`parse`](Self::parse) refuses.
    pub fn read(spec: &str) -> Result<Self, FilterError> {
        if spec.trim_start().starts_with('{') {
            return Self::parse(spec);
        }
        let document = std::fs::read_to_string(Path::new(spec)).map_err(|failure| {
            FilterError(format!("cannot read the event filter {spec}: {failure}"))
        })?;
        Self::parse(&document)
    }

    /// Whether an envelope passes: no `exclude` matcher holds, and either
    /// `include` is empty or one of its matchers holds.
    #[must_use]
    pub fn matches(&self, envelope: &Envelope<V>) -> bool {
        self.allows(
            &envelope.source,
            envelope.kind.as_str(),
            &envelope.dimensions,
            &envelope.labels,
        )
    }

    /// [`matches`](Self::matches), for a caller holding the addressing values
    /// rather than a whole envelope — which is what an emitter holds before it
    /// has stamped one.
    #[must_use]
    pub fn allows(
        &self,
        source: &V::Source,
        kind: &str,
        dimensions: &V::Dimensions,
        labels: &V::Labels,
    ) -> bool {
        if self.include.is_empty() && self.exclude.is_empty() {
            return true;
        }
        let carried = Carried::of::<V>(dimensions, labels);
        if self
            .exclude
            .iter()
            .any(|matcher| matcher.holds(source, kind, &carried))
        {
            return false;
        }
        self.include.is_empty()
            || self
                .include
                .iter()
                .any(|matcher| matcher.holds(source, kind, &carried))
    }

    /// Whether every matcher in this filter could match anything.
    ///
    /// A spec is external input, so this is its trust boundary. A matcher
    /// naming no field matches *every* envelope — one in `exclude` silences
    /// the stream — and one naming an empty field matches nothing, since no
    /// envelope carries an empty kind or an empty label.
    ///
    /// # Errors
    ///
    /// A message naming the list, the index in it, and the matcher itself.
    pub fn validate(&self) -> Result<(), String> {
        for (list, matchers) in [("include", &self.include), ("exclude", &self.exclude)] {
            for (at, matcher) in matchers.iter().enumerate() {
                matcher.check().map_err(|why| {
                    format!(
                        "{list}[{at}] {}: {why}",
                        serde_json::to_string(matcher).unwrap_or_else(|_| "{}".to_owned())
                    )
                })?;
            }
        }
        Ok(())
    }
}

/// What an envelope carries under its dimension and label keys, read once per
/// envelope so several matchers consult one map.
struct Carried(Map<String, Value>);

impl Carried {
    fn of<V: Vocabulary>(dimensions: &V::Dimensions, labels: &V::Labels) -> Self {
        let mut carried = as_map(dimensions);
        carried.extend(as_map(labels));
        Self(carried)
    }
}

/// A wire value as the JSON object it serializes to; empty when it is not one.
fn as_map<T: Serialize>(value: &T) -> Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

impl<V: Vocabulary> Matcher<V> {
    /// A matcher naming nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one matcher from its JSON spelling.
    ///
    /// # Errors
    ///
    /// A [`FilterError`] for a document that is not a matcher, one naming a
    /// field the vocabulary does not have, or one that could match nothing.
    pub fn parse(spec: &str) -> Result<Self, FilterError> {
        let matcher: Self = serde_json::from_str(spec)
            .map_err(|failure| FilterError(format!("the matcher is unusable: {failure}")))?;
        matcher
            .check()
            .map_err(|why| FilterError(format!("{spec}: {why}")))?;
        Ok(matcher)
    }

    /// The same matcher asking for `source`.
    #[must_use]
    pub fn source(mut self, source: V::Source) -> Self {
        self.source = Some(source);
        self
    }

    /// The same matcher asking for a `kind` glob.
    #[must_use]
    pub fn kind(mut self, glob: impl Into<String>) -> Self {
        self.kind = Some(glob.into());
        self
    }

    /// The same matcher asking for `fields`.
    #[must_use]
    pub fn fields(mut self, fields: V::Fields) -> Self {
        self.fields = fields;
        self
    }

    /// Whether every field this matcher names holds.
    fn holds(&self, source: &V::Source, kind: &str, carried: &Carried) -> bool {
        if self.source.as_ref().is_some_and(|named| named != source) {
            return false;
        }
        if self
            .kind
            .as_deref()
            .is_some_and(|pattern| !glob(pattern, kind))
        {
            return false;
        }
        as_map(&self.fields)
            .iter()
            .all(|(key, asked)| carried.0.get(key) == Some(asked))
    }

    /// Whether this matcher could match anything; see [`Filter::validate`].
    fn check(&self) -> Result<(), String> {
        let mut named = usize::from(self.source.is_some());
        let mut asked: Vec<(String, Value)> = Vec::new();
        if let Some(kind) = &self.kind {
            asked.push(("kind".to_owned(), Value::String(kind.clone())));
        }
        asked.extend(as_map(&self.fields));
        for (field, value) in asked {
            named += 1;
            let empty = match &value {
                Value::String(text) => text.trim().is_empty(),
                Value::Null => true,
                _ => false,
            };
            if empty {
                return Err(format!(
                    "`{field}` is empty, and nothing on the stream carries an empty {field} — \
                     omit the field to leave it unasked"
                ));
            }
        }
        if named == 0 {
            // A matcher asks by exact text, so an integer key is not one it
            // can name.
            let keys: Vec<&str> = ["source", "kind"]
                .into_iter()
                .chain(
                    V::DIMENSIONS
                        .iter()
                        .chain(V::RESERVED)
                        .filter(|reserved| reserved.admits != Admits::Integer)
                        .map(|reserved| reserved.key),
                )
                .collect();
            return Err(format!(
                "a matcher naming no field matches every event — name at least one of `{}`",
                keys.join("`, `")
            ));
        }
        Ok(())
    }
}

/// Whether `pattern` matches `text`, where `*` stands for any run of
/// characters including none and every other character is itself.
///
/// The whole dialect, stated rather than inherited: a `?` or a `[a-z]`
/// supported here would be a grammar every consumer restates, and kebab-case
/// wire strings need neither.
#[must_use]
pub fn glob(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    // Where to resume from if the run this `*` is currently standing for turns
    // out to be one character too short.
    let (mut star, mut resume) = (None, 0);
    while t < text.len() {
        if pattern.get(p) == Some(&'*') {
            star = Some(p);
            resume = t;
            p += 1;
        } else if pattern.get(p) == Some(&text[t]) {
            p += 1;
            t += 1;
        } else if let Some(at) = star {
            p = at + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|character| *character == '*')
}
