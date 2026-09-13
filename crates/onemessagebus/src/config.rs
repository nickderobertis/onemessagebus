//! The configuration file, and the bus it resolves to.
//!
//! `onemessagebus.yaml` names a transport, optionally a layout a profile crate
//! declares, queues added to it or overriding its own, and authors narrowed from
//! its grants. Reading it is two steps, and the types keep them apart:
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

use crate::author::{Allowlist, Author, NarrowingRefused, OpWord};
use crate::kinds::{TransportConfig, TransportKinds};
use crate::queue::{
    Delivery, Ordering, Policy, Predicate, Pushed, QueueError, QueueSpec, RawQueue, Retention,
    Supersede,
};
use crate::schema::{Registry, SchemaId};
use crate::transport::{ConsumerName, DocumentName, QueueName, Transport, TransportError};

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
    /// Authors whose grants the configuration narrows. It may never widen them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub authors: BTreeMap<Author, AuthorConfig>,
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
    /// The operations the author keeps: a subset of what the layout grants.
    pub capabilities: Vec<String>,
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
    /// # Errors
    ///
    /// [`ConfigError::Read`] for a file that cannot be read,
    /// [`ConfigError::Parse`] for one that is not a configuration — naming the
    /// unknown key, or the key a malformed value is at — and
    /// [`ConfigError::Version`] for a version other than [`CONFIG_VERSION`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text).map_err(|failure| match failure {
            ConfigError::Parse { why, .. } => ConfigError::Parse {
                path: path.to_path_buf(),
                why,
            },
            other => other,
        })
    }

    /// Read and check a configuration from its text; see [`load`](Self::load).
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load), with the path empty.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Self = serde_norway::from_str(text).map_err(|failure| ConfigError::Parse {
            path: PathBuf::new(),
            why: failure.to_string(),
        })?;
        if config.version != CONFIG_VERSION {
            return Err(ConfigError::Version {
                found: config.version,
            });
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
        }
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
        let registry = layout.map(|layout| layout.registry()).unwrap_or_default();
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
        for (author, narrowed) in &self.authors {
            allowlist.narrow(
                &format!("authors.{author}.capabilities"),
                author,
                &narrowed.capabilities,
                NARROWED,
            )?;
        }
        let transport = kinds.open(&self.transport)?;
        Ok(Bus {
            transport,
            kind: self.transport.kind.to_string(),
            layout: layout.cloned(),
            queues: specs,
            allowlist,
            registry: Arc::new(registry),
        })
    }
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
        ))
    }

    /// What offering `record` to `queue` pushes, in order: the layout's
    /// [`prepare`](Layout::prepare), under the narrowed grants.
    ///
    /// # Errors
    ///
    /// [`BusError::UnknownQueue`], or [`BusError::Refused`] in the layout's words.
    pub fn prepare(
        &self,
        queue: &QueueName,
        record: Value,
    ) -> Result<Vec<(QueueName, Value)>, BusError> {
        self.queue(queue)?;
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

    /// Offer `record` to `queue`: prepare it, then push each record it becomes
    /// onto its queue, validated against that queue's schema.
    ///
    /// # Errors
    ///
    /// As [`prepare`](Self::prepare), or the first push a queue refused; what was
    /// pushed before it stays pushed.
    pub fn send(
        &self,
        queue: &QueueName,
        record: Value,
    ) -> Result<Vec<(QueueName, Pushed<Value>)>, BusError> {
        let mut pushed = Vec::new();
        for (target, record) in self.prepare(queue, record)? {
            let landed = self.queue(&target)?.push(record)?;
            pushed.push((target, landed));
        }
        Ok(pushed)
    }
}
