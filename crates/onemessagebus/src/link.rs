//! Schema links: a bundle of registry documents another program publishes,
//! named by a URL or a path pinned to a version, and the on-disk cache a pinned
//! remote link resolves through (Contract L, `docs/schema-links.md`).
//!
//! Three types keep the steps apart. A [`SchemaBundle`] is the document a link
//! serves, refused on read naming what is wrong. A [`SchemaLink`] is the text a
//! configuration's `schemas` key names, parsed — and nothing more: parsing
//! touches neither the network nor the cache. A [`LinkResolver`] is the explicit
//! call that turns a link into its bundle, through the cache for a pinned remote
//! link, and answers how it got there ([`Outcome`]).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::OffsetDateTime;

use crate::schema::{Registry, RegistryError, SchemaId};
use crate::sdk_schema::RegistryDocument;

/// The variable naming the schema cache directory, over every other source.
pub const SCHEMA_CACHE_DIR_ENV: &str = "ONEMESSAGEBUS_SCHEMA_CACHE_DIR";
/// The variable naming the freshness window, in whole seconds.
pub const SCHEMA_TTL_ENV: &str = "ONEMESSAGEBUS_SCHEMA_TTL";
/// The variable that, set to `1`, forces an unconditional fetch.
pub const SCHEMA_REFRESH_ENV: &str = "ONEMESSAGEBUS_SCHEMA_REFRESH";
/// The freshness window when [`SCHEMA_TTL_ENV`] is unset: one hour.
pub const DEFAULT_SCHEMA_TTL: Duration = Duration::from_secs(3600);
/// How long establishing a connection may take — TCP, a proxy's `CONNECT`, and
/// the TLS handshake together — before the fetch is refused.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the response's headers may take to arrive once the request is
/// sent, and, separately, how long its body may take once they have.
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// The bound on a bundle's text, so an origin cannot hand a session an
/// unbounded document.
const MAX_BUNDLE_BYTES: u64 = 16 * 1024 * 1024;

/// A version a bundle declares, or a pin asks for: one to three dot-separated
/// non-negative integers (`^\d+(\.\d+){0,2}$`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BundleVersion {
    text: String,
    parts: Vec<u64>,
}

/// Why text is not a [`BundleVersion`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{text:?} is not a version: one to three dot-separated non-negative integers, e.g. 8, 8.1 or 8.1.2")]
pub struct VersionError {
    /// What was offered.
    pub text: String,
}

impl BundleVersion {
    /// The components, as written.
    #[must_use]
    pub fn parts(&self) -> &[u64] {
        &self.parts
    }

    /// The component at `index`, a missing one read as 0.
    fn at(&self, index: usize) -> u64 {
        self.parts.get(index).copied().unwrap_or(0)
    }

    /// Whether this version, read as a pin, admits `declared`: every component
    /// the pin names equals the declared version's, a missing declared
    /// component read as 0. `@8` admits `8`, `8.1` and `8.1.2`; `@8.0` admits
    /// `8`; `@8.1.2` admits exactly `8.1.2`.
    #[must_use]
    pub fn admits(&self, declared: &BundleVersion) -> bool {
        (0..self.parts.len()).all(|index| self.at(index) == declared.at(index))
    }
}

impl FromStr for BundleVersion {
    type Err = VersionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refuse = || VersionError {
            text: text.to_owned(),
        };
        let pieces: Vec<&str> = text.split('.').collect();
        if pieces.len() > 3 {
            return Err(refuse());
        }
        let parts = pieces
            .iter()
            .map(|piece| {
                if piece.is_empty() || !piece.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(refuse());
                }
                piece.parse::<u64>().map_err(|_| refuse())
            })
            .collect::<Result<Vec<u64>, VersionError>>()?;
        Ok(Self {
            text: text.to_owned(),
            parts,
        })
    }
}

impl Ord for BundleVersion {
    /// Newer is greater: component by component with missing ones read as 0,
    /// then the more specific spelling, then the text.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (0..3)
            .map(|index| self.at(index).cmp(&other.at(index)))
            .find(|order| order.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.parts.len().cmp(&other.parts.len()))
            .then_with(|| self.text.cmp(&other.text))
    }
}

impl PartialOrd for BundleVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for BundleVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for BundleVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for BundleVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for BundleVersion {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("BundleVersion")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "One to three dot-separated non-negative integers: the version a bundle declares.",
            "pattern": "^[0-9]+(\\.[0-9]+){0,2}$"
        })
    }
}

/// The document a schema link serves: a declared version and the registry
/// documents it publishes.
///
/// Its fields are held to Contract L wherever one is made — read from JSON or
/// built with [`new`](Self::new) — so a publisher generating its bundle through
/// this type cannot write one a consumer would refuse.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaBundle {
    version: BundleVersion,
    description: Option<String>,
    schemas: Vec<RegistryDocument>,
}

/// Why a document is not a [`SchemaBundle`], naming what is wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a schema bundle: {why}")]
pub struct BundleError {
    /// What is wrong, at which key.
    pub why: String,
}

fn bundle_error(why: impl Into<String>) -> BundleError {
    BundleError { why: why.into() }
}

/// The shape a bundle serializes to, and the JSON Schema it is described by.
#[derive(Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BundleShape<'a> {
    /// The version this document declares: what a link's pin is asserted
    /// against, and what a cache entry is keyed by.
    version: &'a BundleVersion,
    /// What the bundle is, for a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    /// The registry documents it publishes, each id once.
    #[schemars(length(min = 1))]
    schemas: &'a [RegistryDocument],
}

impl SchemaBundle {
    /// The top-level keys a bundle has.
    const KEYS: [&'static str; 3] = ["version", "description", "schemas"];

    /// A bundle of `schemas` at `version`.
    ///
    /// # Errors
    ///
    /// A [`BundleError`] for an empty `schemas`, an id declared twice, or a
    /// document that is not a JSON Schema object.
    pub fn new(
        version: BundleVersion,
        description: Option<String>,
        schemas: Vec<RegistryDocument>,
    ) -> Result<Self, BundleError> {
        if schemas.is_empty() {
            return Err(bundle_error(
                "schemas: is empty; a bundle publishes at least one registry document",
            ));
        }
        let mut seen: BTreeMap<&SchemaId, usize> = BTreeMap::new();
        for (index, document) in schemas.iter().enumerate() {
            if !document.schema.is_object() {
                return Err(bundle_error(format!(
                    "schemas[{index}].schema: is not a JSON Schema object"
                )));
            }
            if let Some(first) = seen.insert(&document.id, index) {
                return Err(bundle_error(format!(
                    "schemas[{index}].id: `{}` is already declared by schemas[{first}]; each id \
                     appears once in a bundle",
                    document.id
                )));
            }
        }
        Ok(Self {
            version,
            description,
            schemas,
        })
    }

    /// Read a bundle from its JSON text.
    ///
    /// # Errors
    ///
    /// A [`BundleError`] naming what is wrong: text that is not a JSON object,
    /// an unknown top-level key, a missing or malformed `version`, a
    /// `description` that is not text, and whatever [`new`](Self::new) refuses —
    /// each entry of `schemas` by its index.
    pub fn from_json(text: &str) -> Result<Self, BundleError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|failure| bundle_error(format!("the document is not JSON: {failure}")))?;
        Self::from_value(value)
    }

    /// Read a bundle from a JSON value; see [`from_json`](Self::from_json).
    ///
    /// # Errors
    ///
    /// As [`from_json`](Self::from_json).
    pub fn from_value(value: Value) -> Result<Self, BundleError> {
        let Value::Object(mut object) = value else {
            return Err(bundle_error("the document is not a JSON object"));
        };
        if let Some(unknown) = object
            .keys()
            .find(|key| !Self::KEYS.contains(&key.as_str()))
        {
            return Err(bundle_error(format!(
                "`{unknown}` is not a key of a bundle; it has `version`, `schemas` and, \
                 optionally, `description`"
            )));
        }
        let version = match object.remove("version") {
            None => return Err(bundle_error("version: is missing")),
            Some(Value::String(text)) => text
                .parse::<BundleVersion>()
                .map_err(|failure| bundle_error(format!("version: {failure}")))?,
            Some(_) => return Err(bundle_error("version: is not a string")),
        };
        let description = match object.remove("description") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text),
            Some(_) => return Err(bundle_error("description: is not a string")),
        };
        let entries = match object.remove("schemas") {
            None => return Err(bundle_error("schemas: is missing")),
            Some(Value::Array(entries)) => entries,
            Some(_) => return Err(bundle_error("schemas: is not an array")),
        };
        let schemas = entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                serde_json::from_value::<RegistryDocument>(entry).map_err(|failure| {
                    bundle_error(format!(
                        "schemas[{index}]: is not a registry document {{\"id\", \"schema\"}}: \
                         {failure}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(version, description, schemas)
    }

    /// The version the bundle declares.
    #[must_use]
    pub fn version(&self) -> &BundleVersion {
        &self.version
    }

    /// What the bundle is, for a person.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The registry documents it publishes.
    #[must_use]
    pub fn schemas(&self) -> &[RegistryDocument] {
        &self.schemas
    }

    /// Register every document into `registry`, as
    /// [`Registry::register_schema`] does.
    ///
    /// # Errors
    ///
    /// The first refusal: [`RegistryError::Conflict`] for an id `registry`
    /// already holds a different document under.
    pub fn register_into(&self, registry: &mut Registry) -> Result<(), RegistryError> {
        for document in &self.schemas {
            registry.register_schema(document.id.clone(), document.schema.clone())?;
        }
        Ok(())
    }

    fn shape(&self) -> BundleShape<'_> {
        BundleShape {
            version: &self.version,
            description: self.description.as_deref(),
            schemas: &self.schemas,
        }
    }
}

impl Serialize for SchemaBundle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.shape().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SchemaBundle {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SchemaBundle {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("SchemaBundle")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = BundleShape::json_schema(generator);
        schema.insert(
            "description".to_owned(),
            Value::String(
                "The document a schema link serves: a declared version and the registry documents it publishes."
                    .to_owned(),
            ),
        );
        schema
    }
}

/// Where a link's bundle is read from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LinkLocation {
    /// An `https://` URL, or an `http://` one on a loopback host: fetched.
    Remote(String),
    /// A `file://` URL or a bare path: read on every resolution, never cached.
    File(PathBuf),
}

/// A link a configuration's `schemas` key names: `<location>` or
/// `<location>@<pin>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaLink {
    written: String,
    location: LinkLocation,
    pin: Option<BundleVersion>,
}

/// Why a link did not resolve, or could not be parsed, naming it.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The text is not a link.
    #[error("{link:?} is not a schema link: {why}")]
    Malformed {
        /// What was offered.
        link: String,
        /// What is wrong with it.
        why: String,
    },
    /// A variable the resolver reads holds what it cannot use.
    #[error("{name}: {why}")]
    Environment {
        /// The variable.
        name: &'static str,
        /// What is wrong with its value.
        why: String,
    },
    /// The bundle could not be fetched or read.
    #[error("{link}: cannot {doing} the bundle: {why}")]
    Unreachable {
        /// The link.
        link: String,
        /// `fetch` or `read`.
        doing: &'static str,
        /// Why not.
        why: String,
    },
    /// What the link served is not a bundle.
    #[error("{link}: {source}")]
    NotABundle {
        /// The link.
        link: String,
        /// What is wrong with the document.
        source: BundleError,
    },
    /// The bundle declares a version the link's pin does not admit.
    #[error("{link}: the bundle declares version {declared}, which the pin @{pin} does not admit")]
    PinUnsatisfied {
        /// The link.
        link: String,
        /// The pin.
        pin: BundleVersion,
        /// The version the bundle declared.
        declared: BundleVersion,
    },
    /// No cache directory can be named.
    #[error("no schema cache directory: set {SCHEMA_CACHE_DIR_ENV}, XDG_CACHE_HOME or HOME")]
    NoCacheDir,
    /// The cache directory could not be read or written.
    #[error("the schema cache {}: {why}", dir.display())]
    Cache {
        /// The path.
        dir: PathBuf,
        /// What the filesystem said.
        why: String,
    },
    /// A linked document contradicts one already registered.
    #[error("{link}: {source}")]
    Register {
        /// The link.
        link: String,
        /// The registry's refusal.
        source: RegistryError,
    },
}

/// Whether `text` is a pin: the version grammar.
fn is_pin(text: &str) -> bool {
    text.parse::<BundleVersion>().is_ok()
}

impl SchemaLink {
    /// Parse a link. A relative bare path stays relative:
    /// [`rebased`](Self::rebased) resolves it against a configuration's
    /// directory, which [`Config::load`](crate::Config::load) does.
    ///
    /// # Errors
    ///
    /// [`LinkError::Malformed`] naming the link: empty text, a scheme a link
    /// does not take, an `http://` host that is not loopback, a URL with no
    /// host, or a `file://` URL whose path is not absolute.
    pub fn parse(text: &str) -> Result<Self, LinkError> {
        let refuse = |why: &str| LinkError::Malformed {
            link: text.to_owned(),
            why: why.to_owned(),
        };
        if text.trim().is_empty() {
            return Err(refuse("it is empty"));
        }
        if text.trim() != text {
            return Err(refuse("it has leading or trailing whitespace"));
        }
        let last_segment = text.rfind(['/', '\\']).map_or(0, |slash| slash + 1);
        let (location, pin) = match text[last_segment..].rfind('@') {
            Some(at) if is_pin(&text[last_segment + at + 1..]) => (
                &text[..last_segment + at],
                text[last_segment + at + 1..].parse().ok(),
            ),
            _ => (text, None),
        };
        if location.is_empty() {
            return Err(refuse("it names a pin and no location"));
        }
        let lower = location.to_ascii_lowercase();
        let location = if lower.starts_with("https://") {
            host_of(&location["https://".len()..])
                .ok_or_else(|| refuse("its URL names no host"))?;
            LinkLocation::Remote(location.to_owned())
        } else if lower.starts_with("http://") {
            let host = host_of(&location["http://".len()..])
                .ok_or_else(|| refuse("its URL names no host"))?;
            if !matches!(
                host.to_ascii_lowercase().as_str(),
                "localhost" | "127.0.0.1" | "[::1]"
            ) {
                return Err(refuse(
                    "http:// is taken only for a loopback host (localhost, 127.0.0.1 or [::1]); \
                     use https://",
                ));
            }
            LinkLocation::Remote(location.to_owned())
        } else if lower.starts_with("file://") {
            let rest = &location["file://".len()..];
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            let path = PathBuf::from(drive_letter_path(rest));
            if !path.is_absolute() {
                return Err(refuse(
                    "a file:// URL names an absolute path, e.g. file:///srv/frames.json",
                ));
            }
            LinkLocation::File(path)
        } else if location.contains("://") {
            return Err(refuse(
                "its scheme is not one a link takes: https://, http:// on a loopback host, \
                 file://, or a bare path",
            ));
        } else {
            LinkLocation::File(PathBuf::from(location))
        };
        Ok(Self {
            written: text.to_owned(),
            location,
            pin,
        })
    }

    /// The same link, a relative bare path resolved against `dir`.
    #[must_use]
    pub fn rebased(self, dir: &Path) -> Self {
        match &self.location {
            LinkLocation::File(path) if path.is_relative() && !self.is_file_url() => {
                let path = dir.join(path);
                let written = match &self.pin {
                    Some(pin) => format!("{}@{pin}", path.display()),
                    None => path.display().to_string(),
                };
                Self {
                    written,
                    location: LinkLocation::File(path),
                    pin: self.pin,
                }
            }
            _ => self,
        }
    }

    fn is_file_url(&self) -> bool {
        self.written.to_ascii_lowercase().starts_with("file://")
    }

    /// Where the bundle is read from.
    #[must_use]
    pub fn location(&self) -> &LinkLocation {
        &self.location
    }

    /// The pin, when the link names one.
    #[must_use]
    pub fn pin(&self) -> Option<&BundleVersion> {
        self.pin.as_ref()
    }

    /// The link as text: the location, and `@<pin>` when it names one.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.written
    }
}

/// The host of the authority that begins `rest` (what follows `scheme://`), or
/// `None` when it names none.
fn host_of(rest: &str) -> Option<&str> {
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, after)| after);
    let host = if host_port.starts_with('[') {
        &host_port[..=host_port.find(']')?]
    } else {
        &host_port[..host_port.find(':').unwrap_or(host_port.len())]
    };
    (!host.is_empty()).then_some(host)
}

/// On Windows a `file://` URL for an absolute path is `file:///C:/dir/x`: the
/// slash before the drive letter is dropped. A no-op elsewhere.
#[cfg(windows)]
fn drive_letter_path(text: &str) -> &str {
    let bytes = text.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        &text[1..]
    } else {
        text
    }
}

#[cfg(not(windows))]
fn drive_letter_path(text: &str) -> &str {
    text
}

impl fmt::Display for SchemaLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.written)
    }
}

impl FromStr for SchemaLink {
    type Err = LinkError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl Serialize for SchemaLink {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.written)
    }
}

impl<'de> Deserialize<'de> for SchemaLink {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SchemaLink {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("SchemaLink")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A schema bundle's location — an https:// URL, an http:// URL on a loopback host, a file:// URL, or a path relative to the configuration's directory — and, after a final @, the version pin it is held to: https://example.org/frames.json@8.",
            "minLength": 1
        })
    }
}

/// How much a resolution may lean on the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// A satisfying entry confirmed within the freshness window is used with no
    /// request; an older one is revalidated. What every verb but `serve` does.
    Window,
    /// Every satisfying entry is revalidated, whatever its age: `schemas fetch`.
    Revalidate,
    /// A satisfying entry is used with no request, whatever its age; only a link
    /// with nothing cached is fetched: `serve`, which never waits on the network
    /// at start when the cache holds what it needs.
    CachedFirst,
}

/// How a link came to resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A `file://` or bare-path bundle, read.
    Read,
    /// Fetched: nothing usable was cached, the origin answered a newer document,
    /// the link is unpinned, or a fetch was forced.
    Fetched,
    /// A cached entry used with no request.
    Cached,
    /// A cached entry the origin confirmed current (`304`).
    Confirmed,
    /// A cached entry used because its revalidation could not be made.
    Reused {
        /// Why the revalidation failed.
        why: String,
    },
}

impl Outcome {
    /// The word a report names it by.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Fetched => "fetched",
            Self::Cached => "cached",
            Self::Confirmed => "confirmed",
            Self::Reused { .. } => "reused",
        }
    }
}

/// A link resolved to its bundle.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The link.
    pub link: SchemaLink,
    /// The bundle it resolved to.
    pub bundle: SchemaBundle,
    /// How it got there.
    pub outcome: Outcome,
}

impl Resolved {
    /// Register every document of the bundle into `registry`.
    ///
    /// # Errors
    ///
    /// [`LinkError::Register`] naming the link and the id already registered
    /// with a different document.
    pub fn register_into(&self, registry: &mut Registry) -> Result<(), LinkError> {
        self.bundle
            .register_into(registry)
            .map_err(|source| LinkError::Register {
                link: self.link.to_string(),
                source,
            })
    }
}

/// One entry of the cache, as `schemas` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CachedBundle {
    /// The link's location, without its pin.
    pub url: String,
    /// The version the cached bundle declares.
    pub version: BundleVersion,
    /// When the origin last confirmed it: RFC 3339, UTC.
    pub confirmed_at: String,
}

/// What a cache entry records beside its body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryMeta {
    url: String,
    version: BundleVersion,
    /// Seconds since the Unix epoch.
    confirmed_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
}

/// One readable cache entry: its metadata, its bundle, and the files holding
/// them.
struct Entry {
    meta: EntryMeta,
    bundle: SchemaBundle,
    meta_path: PathBuf,
    body_path: PathBuf,
}

/// Resolves links to bundles, through the cache for a pinned remote link.
#[derive(Debug, Clone)]
pub struct LinkResolver {
    cache_dir: Option<PathBuf>,
    ttl: Duration,
    refresh: bool,
}

impl LinkResolver {
    /// A resolver over `cache_dir` — no cache at all when `None`, so a pinned
    /// remote link is fetched on every resolution — with the default freshness
    /// window and no forced fetch.
    #[must_use]
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            cache_dir,
            ttl: DEFAULT_SCHEMA_TTL,
            refresh: false,
        }
    }

    /// The same resolver with freshness window `ttl`; zero revalidates on every
    /// resolution.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// The same resolver, forcing an unconditional fetch that replaces what the
    /// cache holds when `refresh`.
    #[must_use]
    pub fn with_refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// The resolver the environment describes: the cache directory from
    /// [`SCHEMA_CACHE_DIR_ENV`], else `$XDG_CACHE_HOME/onemessagebus/schemas`,
    /// else `$HOME/.cache/onemessagebus/schemas`; the window from
    /// [`SCHEMA_TTL_ENV`]; a forced fetch from [`SCHEMA_REFRESH_ENV`].
    ///
    /// # Errors
    ///
    /// [`LinkError::Environment`] for a window that is not a whole number of
    /// seconds, or a refresh that is not `1`, `0` or empty.
    pub fn from_env() -> Result<Self, LinkError> {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let cache_dir = var(SCHEMA_CACHE_DIR_ENV)
            .map(PathBuf::from)
            .or_else(|| {
                var("XDG_CACHE_HOME").map(|home| PathBuf::from(home).join("onemessagebus/schemas"))
            })
            .or_else(|| {
                var("HOME").map(|home| PathBuf::from(home).join(".cache/onemessagebus/schemas"))
            });
        let text = |name: &'static str| {
            std::env::var_os(name)
                .map(|value| {
                    value.into_string().map_err(|_| LinkError::Environment {
                        name,
                        why: "is not text".to_owned(),
                    })
                })
                .transpose()
        };
        let ttl = match text(SCHEMA_TTL_ENV)?.as_deref().map(str::trim) {
            None | Some("") => DEFAULT_SCHEMA_TTL,
            Some(seconds) => {
                Duration::from_secs(seconds.parse().map_err(|_| LinkError::Environment {
                    name: SCHEMA_TTL_ENV,
                    why: format!(
                        "{seconds:?} is not a whole number of seconds; 0 revalidates on every \
                         resolution"
                    ),
                })?)
            }
        };
        let refresh = match text(SCHEMA_REFRESH_ENV)?.as_deref().map(str::trim) {
            None | Some("" | "0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(LinkError::Environment {
                    name: SCHEMA_REFRESH_ENV,
                    why: format!("{other:?} is not 1 (force a fetch) or 0"),
                })
            }
        };
        Ok(Self {
            cache_dir,
            ttl,
            refresh,
        })
    }

    /// The cache directory, when one can be named.
    #[must_use]
    pub fn cache_dir(&self) -> Option<&Path> {
        self.cache_dir.as_deref()
    }

    /// Resolve `link` to its bundle.
    ///
    /// A `file://` or bare-path link is read, never cached. An unpinned remote
    /// link is fetched, never cached. A pinned remote link resolves through the
    /// cache as `freshness` and the resolver's window and refresh say.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unreachable`] for a bundle that could not be read, or
    /// fetched with nothing cached to fall back on; [`LinkError::NotABundle`];
    /// [`LinkError::PinUnsatisfied`] for a declared version the pin does not
    /// admit, including one a revalidation answered; and [`LinkError::Cache`]
    /// for an entry that could not be written.
    pub fn resolve(&self, link: &SchemaLink, freshness: Freshness) -> Result<Resolved, LinkError> {
        let url = match link.location() {
            LinkLocation::File(path) => {
                let text =
                    std::fs::read_to_string(path).map_err(|failure| LinkError::Unreachable {
                        link: link.to_string(),
                        doing: "read",
                        why: format!("{}: {failure}", path.display()),
                    })?;
                let bundle = held_to_pin(link, parse_bundle(link, &text)?)?;
                return Ok(resolved(link, bundle, Outcome::Read));
            }
            LinkLocation::Remote(url) => url,
        };
        let (Some(pin), Some(cache)) = (link.pin(), &self.cache_dir) else {
            let body = fetch(link, url, None)?.ok_or_else(|| unanswered(link))?;
            let bundle = held_to_pin(link, parse_bundle(link, &body.text)?)?;
            return Ok(resolved(link, bundle, Outcome::Fetched));
        };
        let dir = url_dir(cache, url);
        if self.refresh && freshness != Freshness::CachedFirst {
            let body = fetch(link, url, None)?.ok_or_else(|| unanswered(link))?;
            let bundle = held_to_pin(link, parse_bundle(link, &body.text)?)?;
            for entry in read_entries(&dir, url) {
                let _ = std::fs::remove_file(&entry.body_path);
                let _ = std::fs::remove_file(&entry.meta_path);
            }
            store(&dir, url, &bundle, &body, now_secs())?;
            return Ok(resolved(link, bundle, Outcome::Fetched));
        }
        let mut satisfying: Vec<Entry> = read_entries(&dir, url)
            .into_iter()
            .filter(|entry| pin.admits(&entry.meta.version))
            .collect();
        satisfying.sort_by(|a, b| b.meta.version.cmp(&a.meta.version));
        let Some(entry) = satisfying.into_iter().next() else {
            let body = fetch(link, url, None)?.ok_or_else(|| unanswered(link))?;
            let bundle = held_to_pin(link, parse_bundle(link, &body.text)?)?;
            store(&dir, url, &bundle, &body, now_secs())?;
            return Ok(resolved(link, bundle, Outcome::Fetched));
        };
        let now = now_secs();
        let fresh = now.saturating_sub(entry.meta.confirmed_at) < self.ttl.as_secs();
        if freshness == Freshness::CachedFirst || (freshness == Freshness::Window && fresh) {
            return Ok(resolved(link, entry.bundle, Outcome::Cached));
        }
        match fetch(link, url, Some(&entry.meta)) {
            Ok(None) => {
                let meta = EntryMeta {
                    confirmed_at: now,
                    ..entry.meta
                };
                write_meta(&entry.meta_path, &meta)?;
                Ok(resolved(link, entry.bundle, Outcome::Confirmed))
            }
            Ok(Some(body)) => match SchemaBundle::from_json(&body.text) {
                Ok(bundle) => {
                    let bundle = held_to_pin(link, bundle)?;
                    store(&dir, url, &bundle, &body, now)?;
                    Ok(resolved(link, bundle, Outcome::Fetched))
                }
                Err(failure) => Ok(resolved(
                    link,
                    entry.bundle,
                    Outcome::Reused {
                        why: format!("the origin answered a document that is {failure}"),
                    },
                )),
            },
            Err(failure) => Ok(resolved(
                link,
                entry.bundle,
                Outcome::Reused {
                    why: match failure {
                        LinkError::Unreachable { why, .. } => why,
                        other => other.to_string(),
                    },
                },
            )),
        }
    }

    /// Every readable entry of the cache, by URL then version.
    ///
    /// # Errors
    ///
    /// [`LinkError::NoCacheDir`] when no directory can be named, and
    /// [`LinkError::Cache`] for one that exists and cannot be read. A directory
    /// that does not exist yet is an empty cache.
    pub fn cached(&self) -> Result<Vec<CachedBundle>, LinkError> {
        let cache = self.cache_dir.as_deref().ok_or(LinkError::NoCacheDir)?;
        let mut entries: Vec<CachedBundle> = subdirs(cache)?
            .iter()
            .flat_map(|dir| read_entries(dir, ""))
            .map(|entry| CachedBundle {
                url: entry.meta.url,
                version: entry.meta.version,
                confirmed_at: rfc3339_seconds(entry.meta.confirmed_at),
            })
            .collect();
        entries.sort_by(|a, b| a.url.cmp(&b.url).then_with(|| a.version.cmp(&b.version)));
        Ok(entries)
    }

    /// Remove every entry of the cache, and answer how many there were. Only
    /// entries this build wrote are removed.
    ///
    /// # Errors
    ///
    /// As [`cached`](Self::cached), and [`LinkError::Cache`] for an entry that
    /// could not be removed.
    pub fn clear(&self) -> Result<usize, LinkError> {
        let cache = self.cache_dir.as_deref().ok_or(LinkError::NoCacheDir)?;
        let mut removed = 0;
        for dir in subdirs(cache)? {
            for entry in read_entries(&dir, "") {
                for path in [&entry.body_path, &entry.meta_path] {
                    std::fs::remove_file(path).map_err(|failure| LinkError::Cache {
                        dir: path.clone(),
                        why: format!("cannot remove it: {failure}"),
                    })?;
                }
                removed += 1;
            }
            let _ = std::fs::remove_dir(&dir);
        }
        Ok(removed)
    }
}

fn resolved(link: &SchemaLink, bundle: SchemaBundle, outcome: Outcome) -> Resolved {
    Resolved {
        link: link.clone(),
        bundle,
        outcome,
    }
}

fn parse_bundle(link: &SchemaLink, text: &str) -> Result<SchemaBundle, LinkError> {
    SchemaBundle::from_json(text).map_err(|source| LinkError::NotABundle {
        link: link.to_string(),
        source,
    })
}

/// `bundle`, when the link's pin admits the version it declares.
fn held_to_pin(link: &SchemaLink, bundle: SchemaBundle) -> Result<SchemaBundle, LinkError> {
    match link.pin() {
        Some(pin) if !pin.admits(bundle.version()) => Err(LinkError::PinUnsatisfied {
            link: link.to_string(),
            pin: pin.clone(),
            declared: bundle.version().clone(),
        }),
        _ => Ok(bundle),
    }
}

fn unanswered(link: &SchemaLink) -> LinkError {
    LinkError::Unreachable {
        link: link.to_string(),
        doing: "fetch",
        why: "the origin answered 304 Not Modified to a request that was not conditional"
            .to_owned(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

const STAMP: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

/// `secs` since the epoch as an RFC 3339 UTC stamp to the second.
fn rfc3339_seconds(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok())
        .and_then(|at| at.format(STAMP).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

/// The directory a URL's entries are kept in: a digest of the URL, so any URL
/// names a valid directory and two never share one.
fn url_dir(cache: &Path, url: &str) -> PathBuf {
    let digest = Sha256::digest(url.as_bytes());
    let name: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    cache.join(name)
}

fn entry_paths(dir: &Path, version: &BundleVersion) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("{version}.json")),
        dir.join(format!("{version}.meta.json")),
    )
}

fn subdirs(cache: &Path) -> Result<Vec<PathBuf>, LinkError> {
    match std::fs::read_dir(cache) {
        Ok(entries) => Ok(entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect()),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(failure) => Err(LinkError::Cache {
            dir: cache.to_path_buf(),
            why: format!("cannot read it: {failure}"),
        }),
    }
}

/// Every readable entry in one URL's directory — all of them when `url` is
/// empty. Anything else there — a half-written entry, a body that no longer
/// declares the version its metadata does, metadata for another URL — is passed
/// over, never misread: the cache is a speed-up, and resolution fetches past it.
fn read_entries(dir: &Path, url: &str) -> Vec<Entry> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for meta_path in listing.flatten().map(|entry| entry.path()) {
        let is_meta = meta_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".meta.json"));
        if !is_meta {
            continue;
        }
        let Some(meta) = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|text| serde_json::from_str::<EntryMeta>(&text).ok())
        else {
            continue;
        };
        if (!url.is_empty() && meta.url != url)
            || url_dir(dir.parent().unwrap_or(dir), &meta.url) != dir
        {
            continue;
        }
        let (body_path, expected) = entry_paths(dir, &meta.version);
        if expected != meta_path {
            continue;
        }
        let Some(bundle) = std::fs::read_to_string(&body_path)
            .ok()
            .and_then(|text| SchemaBundle::from_json(&text).ok())
            .filter(|bundle| bundle.version() == &meta.version)
        else {
            continue;
        };
        entries.push(Entry {
            meta,
            bundle,
            meta_path,
            body_path,
        });
    }
    entries
}

fn cache_error(path: &Path, failure: &std::io::Error) -> LinkError {
    LinkError::Cache {
        dir: path.to_path_buf(),
        why: format!("cannot write it: {failure}"),
    }
}

/// Write an entry under the version its bundle declares.
fn store(
    dir: &Path,
    url: &str,
    bundle: &SchemaBundle,
    body: &Body,
    now: u64,
) -> Result<(), LinkError> {
    std::fs::create_dir_all(dir).map_err(|failure| cache_error(dir, &failure))?;
    let (body_path, meta_path) = entry_paths(dir, bundle.version());
    std::fs::write(&body_path, &body.text).map_err(|failure| cache_error(&body_path, &failure))?;
    write_meta(
        &meta_path,
        &EntryMeta {
            url: url.to_owned(),
            version: bundle.version().clone(),
            confirmed_at: now,
            etag: body.etag.clone(),
            last_modified: body.last_modified.clone(),
        },
    )
}

fn write_meta(path: &Path, meta: &EntryMeta) -> Result<(), LinkError> {
    let mut text = serde_json::to_string_pretty(meta).unwrap_or_default();
    text.push('\n');
    std::fs::write(path, text).map_err(|failure| cache_error(path, &failure))
}

/// A fetched document and the validators its origin answered with.
struct Body {
    text: String,
    etag: Option<String>,
    last_modified: Option<String>,
}

/// GET `url`: `Ok(None)` for a `304` to a conditional request — one carrying a
/// validator `meta` recorded — and the document for a success.
///
/// # Errors
///
/// [`LinkError::Unreachable`] for a transport failure, a timeout, or any other
/// status.
fn fetch(
    link: &SchemaLink,
    url: &str,
    meta: Option<&EntryMeta>,
) -> Result<Option<Body>, LinkError> {
    let refuse = |why: String| LinkError::Unreachable {
        link: link.to_string(),
        doing: "fetch",
        why,
    };
    let agent = agent(url).map_err(refuse)?;
    let mut request = agent.get(url);
    let mut conditional = false;
    if let Some(meta) = meta {
        if let Some(etag) = &meta.etag {
            request = request.header("If-None-Match", etag.as_str());
            conditional = true;
        }
        if let Some(modified) = &meta.last_modified {
            request = request.header("If-Modified-Since", modified.as_str());
            conditional = true;
        }
    }
    let mut response = request
        .call()
        .map_err(|failure| refuse(transport_failure(&failure)))?;
    let status = response.status().as_u16();
    if conditional && status == 304 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(refuse(format!("the origin answered HTTP {status}")));
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let etag = header("etag");
    let last_modified = header("last-modified");
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_BUNDLE_BYTES)
        .read_to_string()
        .map_err(|failure| {
            refuse(format!(
                "reading the response: {}",
                transport_failure(&failure)
            ))
        })?;
    Ok(Some(Body {
        text,
        etag,
        last_modified,
    }))
}

/// A transport failure in words naming the bound a timeout crossed.
fn transport_failure(failure: &ureq::Error) -> String {
    match failure {
        ureq::Error::Timeout(ureq::Timeout::Connect | ureq::Timeout::Resolve) => format!(
            "the connection was not established within the {}-second connect timeout",
            CONNECT_TIMEOUT.as_secs()
        ),
        ureq::Error::Timeout(_) => format!(
            "nothing was received within the {}-second read timeout",
            READ_TIMEOUT.as_secs()
        ),
        other => other.to_string(),
    }
}

/// The client for one request to `url`: the proxy the environment names for
/// its scheme unless `NO_PROXY` exempts its host, the stated timeouts, and the
/// platform's root certificates over rustls.
fn agent(url: &str) -> Result<ureq::Agent, String> {
    let https = url.to_ascii_lowercase().starts_with("https://");
    let host = host_of(&url[url.find("://").map_or(0, |at| at + 3)..]).unwrap_or_default();
    let proxy = proxy_for(https, host)?;
    let mut tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()));
    if https {
        tls = tls.root_certs(root_certs()?);
    }
    let config = ureq::Agent::config_builder()
        .proxy(proxy)
        .http_status_as_error(false)
        .timeout_resolve(Some(CONNECT_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_send_request(Some(READ_TIMEOUT))
        .timeout_recv_response(Some(READ_TIMEOUT))
        .timeout_recv_body(Some(READ_TIMEOUT))
        .tls_config(tls.build())
        .build();
    Ok(config.into())
}

/// The platform's root certificates, as rustls-native-certs reads them — on
/// Linux and the BSDs, `SSL_CERT_FILE` and `SSL_CERT_DIR` over the system store.
fn root_certs() -> Result<ureq::tls::RootCerts, String> {
    let loaded = rustls_native_certs::load_native_certs();
    if loaded.certs.is_empty() {
        let why = loaded
            .errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "no root certificates could be loaded from the platform{}",
            if why.is_empty() {
                String::new()
            } else {
                format!(": {why}")
            }
        ));
    }
    Ok(ureq::tls::RootCerts::new_with_certs(
        &loaded
            .certs
            .iter()
            .map(|cert| ureq::tls::Certificate::from_der(cert.as_ref()).to_owned())
            .collect::<Vec<_>>(),
    ))
}

/// The proxy a request to `host` goes through: `HTTPS_PROXY` for an `https://`
/// URL and `HTTP_PROXY` for an `http://` one (either spelling, the upper case
/// first), none when `NO_PROXY` names the host.
fn proxy_for(https: bool, host: &str) -> Result<Option<ureq::Proxy>, String> {
    let var = |names: [&str; 2]| {
        names.iter().find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
    };
    let named = if https {
        var(["HTTPS_PROXY", "https_proxy"])
    } else {
        var(["HTTP_PROXY", "http_proxy"])
    };
    let Some(named) = named else {
        return Ok(None);
    };
    if let Some(exempt) = var(["NO_PROXY", "no_proxy"]) {
        if no_proxy_admits(&exempt, host) {
            return Ok(None);
        }
    }
    ureq::Proxy::new(named.trim())
        .map(Some)
        .map_err(|failure| format!("the proxy {named:?} is not usable: {failure}"))
}

/// Whether a `NO_PROXY` list exempts `host`: `*`, the host itself, or a suffix
/// written `.example.org` or `*.example.org` — which also admits the domain
/// itself — each entry trimmed and compared without case or port.
fn no_proxy_admits(list: &str, host: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    list.split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            let entry = entry.trim_start_matches('[');
            let entry = match entry.split_once(']') {
                Some((address, _)) => address.to_owned(),
                None if entry.matches(':').count() == 1 => {
                    entry.split(':').next().unwrap_or_default().to_owned()
                }
                None => entry.to_owned(),
            };
            if entry == "*" {
                return true;
            }
            let domain = entry.trim_start_matches('*').trim_start_matches('.');
            host == domain || host.ends_with(&format!(".{domain}"))
        })
}

#[cfg(test)]
mod tests {
    use super::no_proxy_admits;

    #[test]
    fn no_proxy_matches_hosts_suffixes_ports_and_everything() {
        assert!(no_proxy_admits("localhost", "localhost"));
        assert!(no_proxy_admits(" example.org , localhost ", "LOCALHOST"));
        assert!(no_proxy_admits(".example.org", "a.example.org"));
        assert!(no_proxy_admits("*.example.org", "example.org"));
        assert!(no_proxy_admits("localhost:8443", "localhost"));
        assert!(no_proxy_admits("[::1]", "[::1]"));
        assert!(no_proxy_admits("*", "anything"));
        assert!(!no_proxy_admits("example.org", "badexample.org"));
        assert!(!no_proxy_admits("", "localhost"));
    }
}
