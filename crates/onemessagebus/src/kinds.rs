//! Which transport a configuration names, and how it is opened.
//!
//! A transport is chosen at runtime by its **kind**, and a kind resolves in one
//! order: the kinds built into this crate (`local`, `memory`), then the kinds
//! registered in-process with [`TransportKinds::register`], then a plugin
//! executable named `onemessagebus-transport-<kind>` on `PATH`. The first that
//! knows the kind opens it; a kind none of them knows is refused, naming the
//! kinds that exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::plugin::{ProcessTransport, PLUGIN_PREFIX};
use crate::transport::{LocalTransport, MemoryTransport, Transport, TransportError};

/// The `transport` block of a configuration: the kind, the directory a
/// directory-backed transport keeps its queues in, and whatever else the kind
/// takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransportConfig {
    /// The kind: `local`, `memory`, or a kind registered in-process or served
    /// by a plugin.
    pub kind: String,
    /// The directory a directory-backed transport keeps its queues in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Every other key, handed to the kind that takes it. The built-in kinds
    /// take none, and refuse one by name.
    #[serde(flatten)]
    pub options: Map<String, Value>,
}

impl TransportConfig {
    /// The configuration of a local transport over `dir`.
    #[must_use]
    pub fn local(dir: impl Into<PathBuf>) -> Self {
        Self {
            kind: LOCAL.to_owned(),
            dir: Some(dir.into()),
            options: Map::new(),
        }
    }

    fn refuse_options(&self) -> Result<(), TransportError> {
        match self.options.keys().next() {
            Some(key) => Err(TransportError::Config {
                kind: self.kind.clone(),
                why: format!(
                    "transport.{key} is not a key the {} transport takes",
                    self.kind
                ),
            }),
            None => Ok(()),
        }
    }
}

/// The built-in kind over a directory.
pub const LOCAL: &str = "local";

/// The built-in kind in memory.
pub const MEMORY: &str = "memory";

/// What opens a transport of one kind from its configuration.
pub type TransportFactory =
    Arc<dyn Fn(&TransportConfig) -> Result<Arc<dyn Transport>, TransportError> + Send + Sync>;

/// Where a kind comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KindOrigin {
    /// Built into this crate.
    Builtin,
    /// Registered in-process.
    Registered,
    /// Served by a plugin executable.
    Plugin,
}

/// One kind a build can open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KindEntry {
    /// The kind.
    pub kind: String,
    /// Where it comes from.
    pub origin: KindOrigin,
    /// The plugin executable, for a kind a plugin serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// The kinds this process can open a transport of.
#[derive(Clone)]
pub struct TransportKinds {
    registered: BTreeMap<String, (KindOrigin, TransportFactory)>,
    /// Where plugins are looked for: `PATH`'s directories unless told otherwise.
    search: Option<Vec<PathBuf>>,
}

impl std::fmt::Debug for TransportKinds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportKinds")
            .field("kinds", &self.registered.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Default for TransportKinds {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Whether `kind` is a word a kind may be: lowercase ASCII letters, digits and
/// `-`, starting with a letter.
fn check_kind(kind: &str) -> Result<(), TransportError> {
    let ok = kind.starts_with(|ch: char| ch.is_ascii_lowercase())
        && kind
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if ok {
        Ok(())
    } else {
        Err(TransportError::Config {
            kind: kind.to_owned(),
            why: format!(
                "{kind:?} is not a transport kind: a kind is lowercase ASCII letters, digits and `-`, starting with a letter"
            ),
        })
    }
}

impl TransportKinds {
    /// The built-in kinds: `local` and `memory`.
    #[must_use]
    pub fn builtin() -> Self {
        let mut registered: BTreeMap<String, (KindOrigin, TransportFactory)> = BTreeMap::new();
        let local: TransportFactory = Arc::new(|config: &TransportConfig| {
            config.refuse_options()?;
            let dir = config.dir.clone().ok_or_else(|| TransportError::Config {
                kind: config.kind.clone(),
                why:
                    "the local transport needs transport.dir, the directory it keeps its queues in"
                        .to_owned(),
            })?;
            Ok(Arc::new(LocalTransport::open(dir)?) as Arc<dyn Transport>)
        });
        let memory: TransportFactory = Arc::new(|config: &TransportConfig| {
            config.refuse_options()?;
            if config.dir.is_some() {
                return Err(TransportError::Config {
                    kind: config.kind.clone(),
                    why: "transport.dir is not a key the memory transport takes".to_owned(),
                });
            }
            Ok(Arc::new(MemoryTransport::new()) as Arc<dyn Transport>)
        });
        registered.insert(LOCAL.to_owned(), (KindOrigin::Builtin, local));
        registered.insert(MEMORY.to_owned(), (KindOrigin::Builtin, memory));
        Self {
            registered,
            search: None,
        }
    }

    /// Register `kind`, opened by `factory`.
    ///
    /// # Errors
    ///
    /// [`TransportError::Config`] for a kind that is not a kind word, or one
    /// this set already has.
    pub fn register(
        &mut self,
        kind: &str,
        factory: TransportFactory,
    ) -> Result<(), TransportError> {
        check_kind(kind)?;
        if self.registered.contains_key(kind) {
            return Err(TransportError::Config {
                kind: kind.to_owned(),
                why: format!("the {kind} transport kind is already registered"),
            });
        }
        self.registered
            .insert(kind.to_owned(), (KindOrigin::Registered, factory));
        Ok(())
    }

    /// Look for plugins in `dirs` rather than in `PATH`'s directories.
    #[must_use]
    pub fn searching(mut self, dirs: Vec<PathBuf>) -> Self {
        self.search = Some(dirs);
        self
    }

    fn search_dirs(&self) -> Vec<PathBuf> {
        match &self.search {
            Some(dirs) => dirs.clone(),
            None => std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
        }
    }

    /// The plugin executable serving `kind`, the first found on the search path.
    #[must_use]
    pub fn plugin(&self, kind: &str) -> Option<PathBuf> {
        self.search_dirs()
            .into_iter()
            .flat_map(|dir| {
                plugin_names(kind)
                    .into_iter()
                    .map(move |name| dir.join(name))
            })
            .find(|candidate| is_executable(candidate))
    }

    /// Open the transport `config` names, resolving its kind built-in first,
    /// then registered, then as a plugin.
    ///
    /// # Errors
    ///
    /// [`TransportError::UnknownKind`] naming every kind there is, or whatever
    /// opening the kind refused.
    pub fn open(&self, config: &TransportConfig) -> Result<Arc<dyn Transport>, TransportError> {
        check_kind(&config.kind)?;
        if let Some((_, factory)) = self.registered.get(&config.kind) {
            return factory(config);
        }
        match self.plugin(&config.kind) {
            Some(path) => Ok(Arc::new(ProcessTransport::spawn(&path, config)?)),
            None => Err(TransportError::UnknownKind {
                kind: config.kind.clone(),
                known: self
                    .kinds()
                    .into_iter()
                    .map(|entry| entry.kind)
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }

    /// Every kind this set can open: built in, registered, then each plugin on
    /// the search path that no earlier kind shadows, in name order within each.
    #[must_use]
    pub fn kinds(&self) -> Vec<KindEntry> {
        let mut entries: Vec<KindEntry> = self
            .registered
            .iter()
            .filter(|(_, (origin, _))| *origin == KindOrigin::Builtin)
            .chain(
                self.registered
                    .iter()
                    .filter(|(_, (origin, _))| *origin == KindOrigin::Registered),
            )
            .map(|(kind, (origin, _))| KindEntry {
                kind: kind.clone(),
                origin: origin.clone(),
                path: None,
            })
            .collect();
        let mut plugins: BTreeMap<String, PathBuf> = BTreeMap::new();
        for dir in self.search_dirs() {
            let Ok(listing) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in listing.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(kind) = name
                    .strip_prefix(PLUGIN_PREFIX)
                    .map(|kind| kind.strip_suffix(".exe").unwrap_or(kind))
                else {
                    continue;
                };
                if check_kind(kind).is_err()
                    || self.registered.contains_key(kind)
                    || plugins.contains_key(kind)
                    || !is_executable(&entry.path())
                {
                    continue;
                }
                plugins.insert(kind.to_owned(), entry.path());
            }
        }
        entries.extend(plugins.into_iter().map(|(kind, path)| KindEntry {
            kind,
            origin: KindOrigin::Plugin,
            path: Some(path),
        }));
        entries
    }
}

/// The file names a plugin for `kind` may have on this platform.
fn plugin_names(kind: &str) -> Vec<String> {
    let bare = format!("{PLUGIN_PREFIX}{kind}");
    if cfg!(windows) {
        vec![format!("{bare}.exe"), bare]
    } else {
        vec![bare]
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}
