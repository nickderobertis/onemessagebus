//! The schema registry: which message shapes exist, at which versions, and what
//! this build reads and writes of each family.
//!
//! A [`SchemaId`] is `<namespace>.<name>@<version>`. A [`Message`] is a Rust
//! type that says which id it is; [`Registry::register`] records its generated
//! JSON Schema under that id, and [`Registry::register_schema`] records one
//! handed in as JSON — the door a Python or TypeScript definition comes through.
//! A *family* is an id without its version, and the registry answers what it
//! reads of one, what it writes, and how a declared version is read
//! ([`Registry::read_at`]).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `<namespace>.<name>@<version>`: which schema a message is.
///
/// Namespace and name are non-empty runs of ASCII letters, digits, `-` and
/// `_`; the first `.` separates them, so a name may not carry one. The version
/// is a positive integer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaId {
    namespace: Cow<'static, str>,
    name: Cow<'static, str>,
    version: u32,
}

/// Why text is not a [`SchemaId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not a schema id: {why}; a schema id is <namespace>.<name>@<version>, e.g. billing.invoice@1")]
pub struct SchemaIdError {
    /// What was offered.
    pub text: String,
    /// What is wrong with it.
    pub why: String,
}

impl SchemaId {
    /// An id from its literal parts, held to the grammar [`FromStr`] enforces:
    /// use [`FromStr`] for text from outside, which refuses rather than panics.
    ///
    /// # Panics
    ///
    /// When the namespace or the name is empty or carries anything but ASCII
    /// letters, digits, `-` and `_`, or the version is 0. In a `const` — a
    /// [`Message::SCHEMA`] — that is a compile error, so a malformed literal
    /// never reaches a registry:
    ///
    /// ```compile_fail
    /// const EMPTY_NAME: onemessagebus::SchemaId = onemessagebus::SchemaId::literal("billing", "", 1);
    /// ```
    #[must_use]
    pub const fn literal(namespace: &'static str, name: &'static str, version: u32) -> Self {
        assert!(
            is_part(namespace),
            "SchemaId::literal: the namespace is not a non-empty run of ASCII letters, digits, `-` and `_`"
        );
        assert!(
            is_part(name),
            "SchemaId::literal: the name is not a non-empty run of ASCII letters, digits, `-` and `_`"
        );
        assert!(
            version > 0,
            "SchemaId::literal: the version is not a positive integer"
        );
        Self {
            namespace: Cow::Borrowed(namespace),
            name: Cow::Borrowed(name),
            version,
        }
    }

    /// An id from its parts, refused as text would be.
    ///
    /// # Errors
    ///
    /// A [`SchemaIdError`] naming the empty or malformed part.
    pub fn new(namespace: &str, name: &str, version: u32) -> Result<Self, SchemaIdError> {
        format!("{namespace}.{name}@{version}").parse()
    }

    /// The namespace.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The name within the namespace.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The version.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The id without its version: `billing.invoice`.
    #[must_use]
    pub fn family(&self) -> String {
        format!("{}.{}", self.namespace, self.name)
    }

    /// The same family at another version.
    ///
    /// # Panics
    ///
    /// When `version` is 0, which the grammar refuses: the family is already
    /// well-formed, so the version is the one part left to fault.
    #[must_use]
    pub fn at(&self, version: u32) -> Self {
        assert!(
            version > 0,
            "SchemaId::at: the version is not a positive integer"
        );
        Self {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            version,
        }
    }
}

/// A `while` loop rather than an iterator, because iterators are not callable
/// in a `const fn` and [`SchemaId::literal`] checks at compile time.
const fn is_part(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') {
            return false;
        }
        at += 1;
    }
    true
}

impl FromStr for SchemaId {
    type Err = SchemaIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refuse = |why: &str| SchemaIdError {
            text: text.to_owned(),
            why: why.to_owned(),
        };
        let (family, version) = text
            .rsplit_once('@')
            .ok_or_else(|| refuse("it names no version"))?;
        let (namespace, name) = family
            .split_once('.')
            .ok_or_else(|| refuse("it names no namespace"))?;
        if !is_part(namespace) {
            return Err(refuse(if namespace.is_empty() {
                "its namespace is empty"
            } else {
                "its namespace is not letters, digits, `-` and `_`"
            }));
        }
        if !is_part(name) {
            return Err(refuse(if name.is_empty() {
                "its name is empty"
            } else {
                "its name is not letters, digits, `-` and `_`"
            }));
        }
        let version: u32 = version
            .parse()
            .ok()
            .filter(|version| *version > 0)
            .ok_or_else(|| refuse("its version is not a positive integer"))?;
        Ok(Self {
            namespace: Cow::Owned(namespace.to_owned()),
            name: Cow::Owned(name.to_owned()),
            version,
        })
    }
}

impl fmt::Display for SchemaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}@{}", self.namespace, self.name, self.version)
    }
}

impl Serialize for SchemaId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SchemaId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SchemaId {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("SchemaId")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "<namespace>.<name>@<version>: which schema a message is.",
            "pattern": "^[A-Za-z0-9_-]+\\.[A-Za-z0-9_-]+@[1-9][0-9]*$"
        })
    }
}

/// A Rust type that says which schema it is.
pub trait Message: Serialize + DeserializeOwned + JsonSchema {
    /// The id this type's generated schema is registered under.
    const SCHEMA: SchemaId;
}

/// Why the registry refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// A second, different document was offered under an id already held.
    #[error(
        "{id} is already registered with a different document; register a new version instead"
    )]
    Conflict {
        /// The id both documents claimed.
        id: SchemaId,
    },
    /// The document is not a JSON Schema this registry can validate against.
    #[error("{id}: the document is not a usable JSON Schema: {why}")]
    NotASchema {
        /// The id it was offered under.
        id: SchemaId,
        /// What the validator said.
        why: String,
    },
    /// The id names nothing registered.
    #[error("{id} is not a registered schema; registered: {known}")]
    Unknown {
        /// The id asked for.
        id: SchemaId,
        /// What is registered, for the reader's next try.
        known: String,
    },
}

/// A payload that does not conform to its schema.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{id}: at {pointer}: {message}")]
pub struct SchemaViolation {
    /// The schema the payload was checked against.
    pub id: SchemaId,
    /// The JSON pointer of the first violation, `""` for the document itself.
    pub pointer: String,
    /// What the validator said of it.
    pub message: String,
}

/// How a declared version of a family is read by this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// Read as the version this build writes: every version in the read set
    /// is carried forward, since every bump within the set is additive.
    At(u32),
    /// Not a version this build reads; the caller refuses, naming the version
    /// and the set.
    Unknown(UnknownVersion),
}

/// A declared version outside a family's read set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{family}@{declared} is not a version this build reads; it reads {}",
    read_set_text(read_set)
)]
pub struct UnknownVersion {
    /// The family the version was declared for.
    pub family: String,
    /// The version the document declared.
    pub declared: u32,
    /// The versions this build reads, newest first.
    pub read_set: Vec<u32>,
}

fn read_set_text(read_set: &[u32]) -> String {
    if read_set.is_empty() {
        return "no version of it".to_owned();
    }
    let versions: Vec<String> = read_set.iter().map(u32::to_string).collect();
    format!("[{}]", versions.join(", "))
}

/// One registered document and the validator compiled from it.
struct Registered {
    document: Value,
    validator: jsonschema::Validator,
}

/// The schemas this build knows, by id.
#[derive(Default)]
pub struct Registry {
    schemas: BTreeMap<SchemaId, Registered>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("ids", &self.ids())
            .finish()
    }
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `M`'s generated JSON Schema under [`M::SCHEMA`](Message::SCHEMA).
    ///
    /// # Errors
    ///
    /// [`RegistryError::Conflict`] when the id already holds a different
    /// document.
    pub fn register<M: Message>(&mut self) -> Result<(), RegistryError> {
        let schema = schemars::schema_for!(M);
        self.register_schema(M::SCHEMA, schema.to_value())
    }

    /// Record a JSON Schema handed in as a document under `id`. Registering
    /// the same document twice is fine; a different one is refused.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Conflict`] for a different document under a held id,
    /// [`RegistryError::NotASchema`] for one the validator cannot compile.
    pub fn register_schema(&mut self, id: SchemaId, schema: Value) -> Result<(), RegistryError> {
        if let Some(held) = self.schemas.get(&id) {
            if held.document == schema {
                return Ok(());
            }
            return Err(RegistryError::Conflict { id });
        }
        let validator =
            jsonschema::validator_for(&schema).map_err(|failure| RegistryError::NotASchema {
                id: id.clone(),
                why: failure.to_string(),
            })?;
        self.schemas.insert(
            id,
            Registered {
                document: schema,
                validator,
            },
        );
        Ok(())
    }

    /// Every registered id, in order.
    #[must_use]
    pub fn ids(&self) -> Vec<SchemaId> {
        self.schemas.keys().cloned().collect()
    }

    /// The document registered under `id`.
    #[must_use]
    pub fn schema(&self, id: &SchemaId) -> Option<&Value> {
        self.schemas.get(id).map(|held| &held.document)
    }

    /// Validate `payload` against the document registered under `id`.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Unknown`] for an id nothing is registered under; the
    /// [`SchemaViolation`] naming the id and the JSON pointer of the first
    /// violation otherwise.
    pub fn check(&self, id: &SchemaId, payload: &Value) -> Result<(), CheckError> {
        let held = self.schemas.get(id).ok_or_else(|| RegistryError::Unknown {
            id: id.clone(),
            known: self.known(),
        })?;
        match held.validator.iter_errors(payload).next() {
            None => Ok(()),
            Some(violation) => Err(CheckError::Violation(SchemaViolation {
                id: id.clone(),
                pointer: violation.instance_path().to_string(),
                message: violation.to_string(),
            })),
        }
    }

    /// The versions of `family` this build reads, newest first.
    #[must_use]
    pub fn read_set(&self, family: &str) -> Vec<u32> {
        let mut versions: Vec<u32> = self
            .schemas
            .keys()
            .filter(|id| id.family() == family)
            .map(SchemaId::version)
            .collect();
        versions.sort_unstable_by(|a, b| b.cmp(a));
        versions
    }

    /// The version of `family` this build writes: the newest it reads. `None`
    /// for a family nothing is registered under.
    #[must_use]
    pub fn writes(&self, family: &str) -> Option<u32> {
        self.read_set(family).first().copied()
    }

    /// How a document of `family` declaring `declared` is read.
    ///
    /// **Forward-carry**: a version in the read set is read as the version
    /// this build writes, since every bump in the set is additive. Anything
    /// else is [`Read::Unknown`], naming the version and the set.
    #[must_use]
    pub fn read_at(&self, family: &str, declared: u32) -> Read {
        let read_set = self.read_set(family);
        match (read_set.contains(&declared), read_set.first()) {
            (true, Some(writes)) => Read::At(*writes),
            _ => Read::Unknown(UnknownVersion {
                family: family.to_owned(),
                declared,
                read_set,
            }),
        }
    }

    fn known(&self) -> String {
        let ids: Vec<String> = self.schemas.keys().map(ToString::to_string).collect();
        if ids.is_empty() {
            "nothing".to_owned()
        } else {
            ids.join(", ")
        }
    }
}

/// Why a check did not pass.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    /// The id names nothing registered.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The payload does not conform.
    #[error(transparent)]
    Violation(#[from] SchemaViolation),
}
