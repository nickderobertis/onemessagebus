//! The configuration file, and the bus it resolves to.
//!
//! `onemessagebus.yaml` names a transport, optionally a layout a profile crate
//! declares, queues added to it or overriding its own, and configured authors.
//! Reading it is two steps, and the types keep them apart:
//!
//! 1. [`Config::load`] reads the file and refuses what the file alone decides —
//!    YAML that is not one document, an unknown key, a version other than
//!    [`CONFIG_VERSION`], a name, schema id or predicate that does not parse —
//!    each by the key it is at. A [`Config`] opens nothing.
//! 2. [`Config::resolve`] binds it to the [`Layouts`] a process links and the
//!    [`TransportKinds`] it can open, and refuses what only those decide — a
//!    profile no layout declares, a grant the profile does not give (a
//!    widening), a queue key naming what is not there, a transport its kind
//!    refuses — each by the key it is at. What it answers, a [`Bus`], is the one
//!    type that opens a queue or authors a record.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ask::Correlation;
use crate::author::{Allowlist, Author, NarrowingRefused, OpWord};
use crate::codec::{CodecConfig, CodecName};
use crate::kinds::{TransportConfig, TransportKinds};
use crate::link::{Freshness, LinkError, LinkResolver, Resolved, SchemaLink};
use crate::queue::{
    shape_word, Delivery, Ordering, Policy, Predicate, Pushed, QueueError, QueueSpec, RawQueue,
    Retention, Supersede,
};
use crate::schema::{Message, Registry, SchemaId};
use crate::transport::{ConsumerName, DocumentName, QueueName, Transport, TransportError};
use crate::validate::{
    combined, CommandValidator, OnRecords, PassCache, ValidationContext, Validator, Validators,
    Verdict, When,
};

/// The configuration version this build reads and writes.
pub const CONFIG_VERSION: u32 = 1;

/// The reason an operation a configuration narrowed away is refused with.
pub const NARROWED: &str = "the configuration does not grant it";

/// What a configuration file says, read and checked on its own.
///
/// It opens no queue and authors no record: [`resolve`](Self::resolve) binds it
/// to the layouts and transports a process has, and answers the [`Bus`] that
/// does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Always [`CONFIG_VERSION`].
    pub version: u32,
    /// The transport the queues are kept on.
    pub transport: TransportConfig,
    /// A layout a linked profile declares, by name: its queues, policies,
    /// authors, operations and schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Queues added to the layout's, or overriding one of the layout's by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub queues: BTreeMap<QueueName, QueueConfig>,
    /// Authors declared by the configuration. The built-in planner may only be narrowed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub authors: BTreeMap<Author, AuthorConfig>,
    /// Validators judging what is offered to a queue before anything is
    /// appended, in the order each queue judges by them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<ValidatorConfig>,
    /// What a host configures for each codec `serve` runs, by the codec's name.
    /// Which names there are is the binary's: one no linked codec answers to is
    /// refused where `serve` resolves it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub codecs: BTreeMap<CodecName, CodecConfig>,
    /// Schema bundles another program publishes, each linked by a URL or path
    /// and pinned to a version (`docs/schema-links.md`). [`load`](Self::load)
    /// parses each link and resolves nothing; [`resolve_links`](Self::resolve_links)
    /// is the call that does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schemas: Vec<SchemaLink>,
}

/// One validator of a configuration: the external kind, which a Rust
/// validator a linking consumer registers is not — that one is code
/// ([`Bus::with_validator`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidatorConfig {
    /// The queue whose offered messages it judges.
    pub on: QueueName,
    /// Which of them it judges; every one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<When>,
    /// Which kind of validator it is.
    pub kind: ValidatorKind,
    /// The command's argv, the program first: the message on its stdin, exit 0
    /// a pass, 1 a refusal with its stderr as the reason, anything else
    /// unjudged.
    pub command: Vec<String>,
    /// Where its passes are recorded; nowhere when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheConfig>,
}

/// The kinds of validator a configuration declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ValidatorKind {
    /// [`CommandValidator`]: an external command.
    Command,
}

/// Where a command validator records its passes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    /// The directory records are kept in, relative to the working directory
    /// when relative.
    pub dir: PathBuf,
    /// The argv whose output fingerprints the bar: a pass recorded under one
    /// fingerprint is not a pass under another.
    pub bar_fingerprint: Vec<String>,
}

impl ValidatorConfig {
    /// The validator this declares.
    ///
    /// # Errors
    ///
    /// The refusal, naming the key under `validators[index]`.
    fn build(&self, index: usize) -> Result<CommandValidator, String> {
        let ValidatorKind::Command = self.kind;
        let validator = CommandValidator::new(self.command.iter().cloned())
            .map_err(|failure| format!("validators[{index}].command: {failure}"))?;
        Ok(match &self.cache {
            Some(cache) => validator.with_cache(
                PassCache::new(&cache.dir, cache.bar_fingerprint.iter().cloned()).map_err(
                    |failure| format!("validators[{index}].cache.bar_fingerprint: {failure}"),
                )?,
            ),
            None => validator,
        })
    }
}

/// A configured validator: judged only for the messages its `when` admits.
struct Gated {
    when: Option<When>,
    validator: CommandValidator,
}

impl Validator<Value> for Gated {
    fn validate(&self, message: &Value, context: &ValidationContext) -> Verdict {
        if self.when.as_ref().is_none_or(|when| when.admits(message)) {
            Validator::<Value>::validate(&self.validator, message, context)
        } else {
            Verdict::Pass
        }
    }
}

/// One queue of a configuration: an addition, or an override of a layout's
/// queue. Every key left out keeps the layout's value — or, for an addition,
/// the plain default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueConfig {
    /// The policy's keys to set.
    #[serde(default, skip_serializing_if = "PolicyConfig::is_empty")]
    pub policy: PolicyConfig,
    /// The schema records are validated against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaId>,
    /// The queue a reply to a pending record is appended to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<QueueName>,
    /// Which records a claim hands out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<Predicate>,
    /// The consumers whose cursors `status` reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumers: Option<Vec<ConsumerName>>,
    /// Whether a push numbers each record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numbered: Option<bool>,
}

/// The policy keys a configuration sets on a queue.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// See [`Policy::delivery`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    /// See [`Policy::ordering`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordering: Option<Ordering>,
    /// See [`Policy::supersede_on`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersede_on: Option<Supersede>,
    /// See [`Policy::hold_pending`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_pending: Option<bool>,
    /// See [`Policy::blocking_first`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_first: Option<bool>,
    /// See [`Policy::retention`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    /// See [`Policy::projection`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<DocumentName>,
}

impl PolicyConfig {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    fn apply(&self, policy: &mut Policy) {
        if let Some(delivery) = self.delivery {
            policy.delivery = delivery;
        }
        if let Some(ordering) = self.ordering {
            policy.ordering = ordering;
        }
        if let Some(supersede) = &self.supersede_on {
            policy.supersede_on = Some(supersede.clone());
        }
        if let Some(hold) = self.hold_pending {
            policy.hold_pending = hold;
        }
        if let Some(first) = self.blocking_first {
            policy.blocking_first = first;
        }
        if let Some(retention) = self.retention {
            policy.retention = retention;
        }
        if let Some(projection) = &self.projection {
            policy.projection = Some(projection.clone());
        }
    }
}

/// One author of a configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthorConfig {
    /// The operations the author may issue.
    pub capabilities: Vec<String>,
    /// Reasons ungranted operations are refused, by operation word.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub refusals: BTreeMap<String, String>,
}

/// Why a configuration could not be read or resolved, naming the key.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read the configuration {}: {source}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// What the host said.
        source: std::io::Error,
    },
    /// The file is not a configuration: not YAML, an unknown key, a malformed
    /// value.
    #[error("{}: {why}", path.display())]
    Parse {
        /// The file.
        path: PathBuf,
        /// What is wrong, at which key.
        why: String,
    },
    /// A version this build does not read.
    #[error("version: {found} is not a configuration version this build reads; it reads {CONFIG_VERSION}")]
    Version {
        /// The version the file declares.
        found: u32,
    },
    /// A profile no linked layout declares.
    #[error("profile: `{name}` is not a layout this build links; the layouts are: {known}")]
    Profile {
        /// The name the file gives.
        name: String,
        /// The layouts there are, comma-separated.
        known: String,
    },
    /// A grant the configuration may not make.
    #[error(transparent)]
    Narrowing(#[from] NarrowingRefused),
    /// A queue key naming what is not there.
    #[error("{key}: {why}")]
    Queue {
        /// The key.
        key: String,
        /// What is wrong with it.
        why: String,
    },
    /// The transport's kind refused its configuration, or could not be opened.
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}

impl Config {
    /// Read and check a configuration file.
    ///
    /// Each of its `schemas` links is parsed — a relative bare path resolved
    /// against the file's directory — and none is resolved: loading a
    /// configuration touches neither the network nor the schema cache.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Read`] for a file that cannot be read,
    /// [`ConfigError::Parse`] for one that is not a configuration — naming the
    /// unknown key, or the key a malformed value is at, a malformed link among
    /// them — and [`ConfigError::Version`] for a version other than
    /// [`CONFIG_VERSION`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config = Self::parse(&text).map_err(|failure| match failure {
            ConfigError::Parse { why, .. } => ConfigError::Parse {
                path: path.to_path_buf(),
                why,
            },
            other => other,
        })?;
        // A relative bare path names a bundle beside the file that links it.
        let dir = path.parent().unwrap_or_else(|| Path::new(""));
        config.schemas = std::mem::take(&mut config.schemas)
            .into_iter()
            .map(|link| link.rebased(dir))
            .collect();
        Ok(config)
    }

    /// Read and check a configuration from its text; see [`load`](Self::load).
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load), with the path empty.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: Value = serde_norway::from_str(text).map_err(|failure| ConfigError::Parse {
            path: PathBuf::new(),
            why: failure.to_string(),
        })?;
        validate_codec_keys(&raw).map_err(|why| ConfigError::Parse {
            path: PathBuf::new(),
            why,
        })?;
        validate_author_names(&raw).map_err(|why| ConfigError::Parse {
            path: PathBuf::new(),
            why,
        })?;
        let config: Self = serde_norway::from_str(text).map_err(|failure| ConfigError::Parse {
            path: PathBuf::new(),
            why: failure.to_string(),
        })?;
        if config.version != CONFIG_VERSION {
            return Err(ConfigError::Version {
                found: config.version,
            });
        }
        for (index, validator) in config.validators.iter().enumerate() {
            validator.build(index).map_err(|why| ConfigError::Parse {
                path: PathBuf::new(),
                why,
            })?;
        }
        for (name, codec) in &config.codecs {
            crate::codec::validate_codec(name, codec).map_err(|why| ConfigError::Parse {
                path: PathBuf::new(),
                why,
            })?;
        }
        Ok(config)
    }

    /// A configuration naming only a local transport over `dir` and a layout.
    #[must_use]
    pub fn local(dir: impl Into<PathBuf>, profile: Option<&str>) -> Self {
        Self {
            version: CONFIG_VERSION,
            transport: TransportConfig::local(dir),
            profile: profile.map(str::to_owned),
            queues: BTreeMap::new(),
            authors: BTreeMap::new(),
            validators: Vec::new(),
            codecs: BTreeMap::new(),
            schemas: Vec::new(),
        }
    }

    /// Resolve every link the `schemas` key names, in order, through
    /// `resolver`: the explicit call [`load`](Self::load) never makes.
    ///
    /// # Errors
    ///
    /// The first link that does not resolve, named as [`LinkResolver::resolve`]
    /// names it.
    pub fn resolve_links(
        &self,
        resolver: &LinkResolver,
        freshness: Freshness,
    ) -> Result<Vec<Resolved>, LinkError> {
        self.schemas
            .iter()
            .map(|link| resolver.resolve(link, freshness))
            .collect()
    }

    /// The same configuration with its transport keeping its queues in `dir`:
    /// how one invocation points a configuration at one directory.
    #[must_use]
    pub fn with_transport_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.transport.dir = Some(dir.into());
        self
    }

    /// Bind this configuration to the layouts a process links and the transport
    /// kinds it can open, and answer the bus it describes.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Profile`] for a profile no layout declares;
    /// [`ConfigError::Narrowing`] for an author the layout does not declare, a
    /// capability that is no operation, or one the layout does not grant — each
    /// naming `authors.<author>.capabilities`; [`ConfigError::Queue`] for a
    /// schema the registry does not hold or an `answers` naming no queue; and
    /// [`ConfigError::Transport`] for a transport its kind refuses.
    pub fn resolve(&self, layouts: &Layouts, kinds: &TransportKinds) -> Result<Bus, ConfigError> {
        self.resolve_with_registry(layouts, kinds, &Registry::new())
    }

    /// [`resolve`](Self::resolve), with every schema `added` holds registered
    /// beside the layout's: how a schema a process registered at run time — a
    /// type an SDK declared in its own language — becomes one a queue's `schema`
    /// names and every record pushed onto it is validated against.
    ///
    /// # Errors
    ///
    /// As [`resolve`](Self::resolve), and [`ConfigError::Queue`] at `registry`
    /// for an id `added` holds a different document under than the layout does.
    pub fn resolve_with_registry(
        &self,
        layouts: &Layouts,
        kinds: &TransportKinds,
        added: &Registry,
    ) -> Result<Bus, ConfigError> {
        self.bind(layouts, added, || Ok(kinds.open(&self.transport)?))
    }

    /// [`resolve_with_registry`](Self::resolve_with_registry) over a transport
    /// already open, rather than one opened from `transport`: how a process that
    /// holds its transport open across many operations — the resident core —
    /// binds each operation's bus over it with the registry as it stands then.
    ///
    /// # Errors
    ///
    /// As [`resolve_with_registry`](Self::resolve_with_registry), less the
    /// transport's own refusal: `transport` is open already.
    pub fn resolve_over(
        &self,
        layouts: &Layouts,
        transport: Arc<dyn Transport>,
        added: &Registry,
    ) -> Result<Bus, ConfigError> {
        self.bind(layouts, added, || Ok(transport))
    }

    /// The bus this configuration describes, with `added`'s schemas beside the
    /// layout's, over the transport `open` answers — asked for last, once every
    /// other key has been checked.
    fn bind(
        &self,
        layouts: &Layouts,
        added: &Registry,
        open: impl FnOnce() -> Result<Arc<dyn Transport>, ConfigError>,
    ) -> Result<Bus, ConfigError> {
        let layout = match &self.profile {
            Some(name) => Some(layouts.get(name).ok_or_else(|| ConfigError::Profile {
                name: name.clone(),
                known: layouts.names().join(", "),
            })?),
            None => None,
        };
        let mut specs: BTreeMap<QueueName, QueueSpec> = layout
            .map(|layout| {
                layout
                    .queues()
                    .into_iter()
                    .map(|spec| (spec.name.clone(), spec))
                    .collect()
            })
            .unwrap_or_default();
        for (name, queue) in &self.queues {
            let spec = specs
                .entry(name.clone())
                .or_insert_with(|| QueueSpec::new(name.clone(), Policy::default()));
            queue.policy.apply(&mut spec.policy);
            if let Some(schema) = &queue.schema {
                spec.schema = Some(schema.clone());
            }
            if let Some(answers) = &queue.answers {
                spec.answers = Some(answers.clone());
            }
            if let Some(claims) = &queue.claims {
                spec.claims = Some(claims.clone());
            }
            if let Some(consumers) = &queue.consumers {
                spec.consumers.clone_from(consumers);
            }
            if let Some(numbered) = queue.numbered {
                spec.numbered = numbered;
            }
        }
        let mut registry = layout.map(|layout| layout.registry()).unwrap_or_default();
        for id in added.ids() {
            if let Some(document) = added.schema(&id) {
                registry
                    .register_schema(id, document.clone())
                    .map_err(|failure| ConfigError::Queue {
                        key: "registry".to_owned(),
                        why: failure.to_string(),
                    })?;
            }
        }
        for spec in specs.values() {
            if let Some(schema) = &spec.schema {
                if registry.schema(schema).is_none() {
                    return Err(ConfigError::Queue {
                        key: format!("queues.{}.schema", spec.name),
                        why: format!(
                            "{schema} is not a schema this layout registers; it registers: {}",
                            registry
                                .ids()
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    });
                }
            }
            if let Some(answers) = &spec.answers {
                if !specs.contains_key(answers) {
                    return Err(ConfigError::Queue {
                        key: format!("queues.{}.answers", spec.name),
                        why: format!("`{answers}` is not a queue this configuration declares"),
                    });
                }
            }
        }
        let mut allowlist = layout
            .map(|layout| layout.allowlist())
            .unwrap_or_else(|| Allowlist::new(Vec::<OpWord>::new()));
        for (author, configured) in &self.authors {
            let capabilities_key = format!("authors.{author}.capabilities");
            if allowlist.declares(author) {
                allowlist.narrow(
                    &capabilities_key,
                    author,
                    &configured.capabilities,
                    NARROWED,
                )?;
            } else {
                allowlist.declare(author.clone());
                for word in &configured.capabilities {
                    let Some(op) = allowlist
                        .vocabulary()
                        .iter()
                        .find(|op| op.0 == *word)
                        .cloned()
                    else {
                        return Err(NarrowingRefused {
                            key: capabilities_key.clone(),
                            why: format!(
                                "`{word}` is not an op; the ops are: {}",
                                allowlist
                                    .vocabulary()
                                    .iter()
                                    .map(|op| op.0.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        }
                        .into());
                    };
                    allowlist.grant(author.clone(), op);
                }
            }
            for (word, reason) in &configured.refusals {
                let key = format!("authors.{author}.refusals.{word}");
                let Some(op) = allowlist
                    .vocabulary()
                    .iter()
                    .find(|op| op.0 == *word)
                    .cloned()
                else {
                    return Err(NarrowingRefused {
                        key,
                        why: format!("`{word}` is not an op"),
                    }
                    .into());
                };
                if configured.capabilities.contains(word) {
                    return Err(NarrowingRefused {
                        key,
                        why: "a granted op may not have a refusal".to_owned(),
                    }
                    .into());
                }
                if reason.trim().is_empty() {
                    return Err(NarrowingRefused {
                        key,
                        why: "a refusal reason must be non-empty text".to_owned(),
                    }
                    .into());
                }
                allowlist.refuse(author.clone(), &op, reason.clone());
            }
        }
        let mut validators: BTreeMap<QueueName, Validators<Value>> = BTreeMap::new();
        for (index, declared) in self.validators.iter().enumerate() {
            if !specs.contains_key(&declared.on) {
                return Err(ConfigError::Queue {
                    key: format!("validators[{index}].on"),
                    why: format!(
                        "`{}` is not a queue this configuration declares; it declares: {}",
                        declared.on,
                        specs
                            .keys()
                            .map(QueueName::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            let validator = declared.build(index).map_err(|why| ConfigError::Queue {
                key: format!("validators[{index}]"),
                why,
            })?;
            validators
                .entry(declared.on.clone())
                .or_default()
                .push(Arc::new(Gated {
                    when: declared.when.clone(),
                    validator,
                }));
        }
        let transport = open()?;
        Ok(Bus {
            transport,
            kind: self.transport.kind.to_string(),
            layout: layout.cloned(),
            queues: specs,
            allowlist,
            registry: Arc::new(registry),
            validators,
        })
    }
}

// llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] This pre-deserialization walk exists only to retain unknown binding keys that serde flattening necessarily discards. CodecConfig and BindingAction remain the schema source; core load tests enumerate every allowed action shape and reject an extra key with its full location, providing the drift gate for this diagnostic-only allowlist.
fn validate_codec_keys(raw: &Value) -> Result<(), String> {
    let Some(codecs) = raw.get("codecs").and_then(Value::as_object) else {
        return Ok(());
    };
    let codec_keys = [
        "queue",
        "reply_window_seconds",
        "session_env",
        "asker_env",
        "about_env",
        "select",
        "frames",
    ];
    for (name, codec) in codecs {
        let Some(codec) = codec.as_object() else {
            continue;
        };
        if let Some(key) = codec.keys().find(|key| !codec_keys.contains(&key.as_str())) {
            return Err(format!("codecs.{name}: unknown field `{key}`"));
        }
        let Some(frames) = codec.get("frames").and_then(Value::as_object) else {
            continue;
        };
        for (entry, frame) in frames {
            let Some(bindings) = frame.get("bindings").and_then(Value::as_array) else {
                continue;
            };
            for (index, binding) in bindings.iter().enumerate() {
                let Some(binding) = binding.as_object() else {
                    continue;
                };
                let action = binding.get("do").and_then(Value::as_str);
                let allowed: &[&str] = match action {
                    Some("answer") => &["when", "do", "response"],
                    Some("refuse") => &["when", "do", "message"],
                    Some("raise") => &["when", "do", "record", "response", "fail"],
                    Some("ask") => &["when", "do", "record", "blocking", "response", "unanswered"],
                    _ => &["when", "do"],
                };
                if let Some(key) = binding.keys().find(|key| !allowed.contains(&key.as_str())) {
                    return Err(format!(
                        "codecs.{name}.frames.{entry}.bindings[{index}]: unknown field `{key}`"
                    ));
                }
            }
        }
    }
    Ok(())
}
// llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate] The allowlist is confined to preserving precise unknown-key diagnostics.

fn validate_author_names(raw: &Value) -> Result<(), String> {
    let Some(authors) = raw.get("authors").and_then(Value::as_object) else {
        return Ok(());
    };
    for name in authors.keys() {
        let valid = name.len() <= 64
            && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            return Err(format!(
                "authors.{name}: an author name must match ^[a-z][a-z0-9-]{{0,63}}$"
            ));
        }
    }
    Ok(())
}

/// A set of queues, policies, authors, operations and schemas a profile crate
/// declares under one name, which a configuration names as its `profile`.
pub trait Layout: Send + Sync + 'static {
    /// The name a configuration's `profile` gives.
    fn name(&self) -> &str;

    /// The queues it declares.
    fn queues(&self) -> Vec<QueueSpec>;

    /// Its authors and what each may carry, over its operation vocabulary.
    fn allowlist(&self) -> Allowlist<OpWord>;

    /// The schemas its queues name.
    fn registry(&self) -> Registry;

    /// The records an offer to `queue` becomes, in the order they are pushed:
    /// what a writer of this layout stamps on a record, split where the layout
    /// routes one offer to several queues, and checked against `allowlist`. The
    /// default is the record as offered, on the queue it was offered to.
    ///
    /// # Errors
    ///
    /// The refusal, in the layout's own words, for a record its author may not
    /// write.
    fn prepare(
        &self,
        queue: &QueueName,
        record: Value,
        allowlist: &Allowlist<OpWord>,
    ) -> Result<Vec<(QueueName, Value)>, String> {
        let _ = allowlist;
        Ok(vec![(queue.clone(), record)])
    }
}

/// Where the halves of one offer go: the part of a layout that splits a record
/// offered to one queue into records on several, by its shape — so that each
/// half reaches the reader it is for, and a reader never claims a half that is
/// not its own. A profile declares one for a layout whose offers carry halves
/// for different readers, and its layout's [`prepare`](Layout::prepare) routes
/// through it.
pub trait Router: Send + Sync {
    /// The records `record`, offered to `queue`, becomes, in the order they are
    /// pushed, checked against `allowlist`.
    ///
    /// # Errors
    ///
    /// The refusal, in the router's own words, for a record its author may not
    /// write or that has no shape the router routes.
    fn route(
        &self,
        queue: &QueueName,
        record: Value,
        allowlist: &Allowlist<OpWord>,
    ) -> Result<Vec<(QueueName, Value)>, String>;
}

/// The layouts a process links, by name.
#[derive(Clone, Default)]
pub struct Layouts(Vec<Arc<dyn Layout>>);

impl fmt::Debug for Layouts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Layouts").field(&self.names()).finish()
    }
}

impl Layouts {
    /// No layouts.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same set, with `layout` added.
    #[must_use]
    pub fn with(mut self, layout: Arc<dyn Layout>) -> Self {
        self.0.push(layout);
        self
    }

    /// The layout named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Layout>> {
        self.0.iter().find(|layout| layout.name() == name)
    }

    /// Every layout's name.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|layout| layout.name().to_owned())
            .collect()
    }
}

/// Why a bus did not do what it was asked.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// A queue the configuration does not declare.
    #[error("`{queue}` is not a queue this configuration declares; it declares: {declared}")]
    UnknownQueue {
        /// The queue asked for.
        queue: QueueName,
        /// The queues there are, comma-separated.
        declared: String,
    },
    /// The layout refused the record.
    #[error("{queue}: {why}")]
    Refused {
        /// The queue it was offered to.
        queue: QueueName,
        /// The layout's words.
        why: String,
    },
    /// A queue no question can be asked or answered on.
    #[error("{queue} is not a queue a question is asked on: {why}")]
    NotAskable {
        /// The queue.
        queue: QueueName,
        /// Why not.
        why: String,
    },
    /// A reply, or a listener, that binds to no question.
    #[error("{queue}: {why}")]
    Unbound {
        /// The queue whose questions were looked through.
        queue: QueueName,
        /// What was looked for, and what is there instead.
        why: String,
    },
    /// The queue refused it, or its transport failed.
    #[error(transparent)]
    Queue(#[from] QueueError),
}

/// A resolved configuration: its transport open, its queues declared, its
/// authors' grants narrowed, and its schemas registered. The one type that
/// opens a queue or authors a record.
#[derive(Clone)]
pub struct Bus {
    transport: Arc<dyn Transport>,
    kind: String,
    layout: Option<Arc<dyn Layout>>,
    queues: BTreeMap<QueueName, QueueSpec>,
    allowlist: Allowlist<OpWord>,
    registry: Arc<Registry>,
    validators: BTreeMap<QueueName, Validators<Value>>,
}

impl fmt::Debug for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bus")
            .field("kind", &self.kind)
            .field("queues", &self.queues.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Bus {
    /// The transport the queues are kept on.
    #[must_use]
    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    /// The transport's kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Every declared queue, in name order.
    #[must_use]
    pub fn queues(&self) -> Vec<QueueName> {
        self.queues.keys().cloned().collect()
    }

    /// The authors and their grants, narrowed as the configuration says.
    #[must_use]
    pub fn allowlist(&self) -> &Allowlist<OpWord> {
        &self.allowlist
    }

    /// The schemas the queues are checked against.
    #[must_use]
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// The declared queue `name`.
    ///
    /// # Errors
    ///
    /// [`BusError::UnknownQueue`], naming every declared queue.
    pub fn queue(&self, name: &QueueName) -> Result<RawQueue, BusError> {
        let spec = self
            .queues
            .get(name)
            .ok_or_else(|| BusError::UnknownQueue {
                queue: name.clone(),
                declared: self
                    .queues
                    .keys()
                    .map(QueueName::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
            })?;
        Ok(RawQueue::open(
            Arc::clone(&self.transport),
            spec.clone(),
            Arc::clone(&self.registry),
        )
        .with_validators(self.validators.get(name).cloned().unwrap_or_default()))
    }

    /// The same bus, judging every message offered to `queue` by `validator`
    /// as well, after every validator the configuration declares for it. A Rust
    /// validator is code, so it is registered here rather than in the file; a
    /// record that does not read as an `M` is refused naming why.
    ///
    /// # Errors
    ///
    /// [`BusError::UnknownQueue`].
    pub fn with_validator<M: Message + 'static>(
        mut self,
        queue: &QueueName,
        validator: impl Validator<M> + 'static,
    ) -> Result<Self, BusError> {
        self.queue(queue)?;
        self.validators
            .entry(queue.clone())
            .or_default()
            .push(Arc::new(OnRecords::new(validator)));
        Ok(self)
    }

    /// Judge what offering `record` to `queue` would append, appending nothing:
    /// the offer by `queue`'s validators, and each record the layout routes to
    /// another queue by that queue's.
    ///
    /// # Errors
    ///
    /// As [`prepare`](Self::prepare): an unknown queue, or the layout's refusal.
    pub fn validate(&self, queue: &QueueName, record: Value) -> Result<Verdict, BusError> {
        let routed = self.prepare(queue, record.clone())?;
        Ok(self.judge(queue, &record, &routed, None))
    }

    /// The verdict on an offer and the records it was routed into, each judged
    /// in a context naming `correlation` when the offer asks or answers the
    /// question it names.
    pub(crate) fn judge(
        &self,
        queue: &QueueName,
        offered: &Value,
        routed: &[(QueueName, Value)],
        correlation: Option<&Correlation>,
    ) -> Verdict {
        let judged_by = |target: &QueueName, record: &Value| {
            self.validators.get(target).map_or(Verdict::Pass, |each| {
                let context = ValidationContext::new(target.clone());
                let context = match correlation {
                    Some(correlation) => context.with_correlation(correlation.clone()),
                    None => context,
                };
                each.judge(record, &context)
            })
        };
        combined(
            std::iter::once(judged_by(queue, offered))
                .chain(
                    routed
                        .iter()
                        .filter(|(target, _)| target != queue)
                        .map(|(target, record)| judged_by(target, record)),
                )
                .collect::<Vec<_>>(),
        )
    }

    /// What offering `record` to `queue` pushes, in order: the layout's
    /// [`prepare`](Layout::prepare), under the narrowed grants.
    ///
    /// # Errors
    ///
    /// [`BusError::UnknownQueue`]; [`QueueError::NotAnObject`] for a record
    /// that is not a JSON object offered to a queue whose records are objects —
    /// one that keeps events or numbers its records — before the layout reads
    /// it; or [`BusError::Refused`] in the layout's words.
    pub fn prepare(
        &self,
        queue: &QueueName,
        record: Value,
    ) -> Result<Vec<(QueueName, Value)>, BusError> {
        let spec = self.queue(queue)?.spec().clone();
        if (spec.policy.keeps_events() || spec.numbered) && !record.is_object() {
            return Err(QueueError::NotAnObject {
                queue: queue.clone(),
                shape: shape_word(&record),
            }
            .into());
        }
        match &self.layout {
            Some(layout) => layout
                .prepare(queue, record, &self.allowlist)
                .map_err(|why| BusError::Refused {
                    queue: queue.clone(),
                    why,
                }),
            None => Ok(vec![(queue.clone(), record)]),
        }
    }

    /// Offer `record` to `queue`: prepare it, judge it as
    /// [`validate`](Self::validate) does, then push each record it becomes onto
    /// its queue, validated against that queue's schema.
    ///
    /// # Errors
    ///
    /// As [`prepare`](Self::prepare); [`QueueError::Refused`] or
    /// [`QueueError::Unjudged`] for an offer the validators did not pass, with
    /// nothing appended anywhere; or the first push a queue refused, when what
    /// was pushed before it stays pushed.
    pub fn send(
        &self,
        queue: &QueueName,
        record: Value,
    ) -> Result<Vec<(QueueName, Pushed<Value>)>, BusError> {
        let routed = self.prepare(queue, record.clone())?;
        QueueError::of_verdict(queue, self.judge(queue, &record, &routed, None))?;
        let mut pushed = Vec::new();
        for (target, record) in routed {
            let landed = self.queue(&target)?.push_judged(record)?;
            pushed.push((target, landed));
        }
        Ok(pushed)
    }
}
