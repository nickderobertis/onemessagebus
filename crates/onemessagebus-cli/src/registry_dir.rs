//! The registry the command line keeps under `--registry <dir>`: one file per
//! schema, named by its id, holding a [`RegistryDocument`].
//!
//! `<dir>/<namespace>.<name>@<version>.json`, each `{"id": ..., "schema": ...}`
//! — self-describing, so a file moved or copied still says what it is, and
//! inspectable with nothing but `cat`. The profile's own schemas are always
//! registered first; the directory adds to them and may not contradict them.

use std::path::{Path, PathBuf};

use onemessagebus::sdk_schema::RegistryDocument;
use onemessagebus::{LinkError, Registry, RegistryError, Resolved, SchemaId};
use serde_json::Value;

/// A registry file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum RegistryDirError {
    /// The directory could not be listed or created.
    #[error("cannot use the registry directory {dir}: {source}")]
    Dir {
        /// The directory.
        dir: PathBuf,
        /// What the filesystem said.
        source: std::io::Error,
    },
    /// The path names something that is not a directory.
    #[error(
        "the registry {dir} is not a directory; --registry names a directory of <id>.json documents"
    )]
    NotADirectory {
        /// The path.
        dir: PathBuf,
    },
    /// A file in the directory is not a registry document.
    #[error("{path} is not a registry document: {why}")]
    Document {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        why: String,
    },
    /// A file's id disagrees with its name.
    #[error("{path} claims to be {claimed}, but a registry file is named by its id")]
    Misnamed {
        /// The file.
        path: PathBuf,
        /// The id inside it.
        claimed: SchemaId,
    },
    /// The registry refused the document.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// A file could not be written.
    #[error("cannot write {path}: {source}")]
    Write {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: std::io::Error,
    },
}

/// A registry backed by a directory of documents, on top of a base registry.
#[derive(Debug)]
pub struct RegistryDir {
    dir: Option<PathBuf>,
    registry: Registry,
}

impl RegistryDir {
    /// The base registry, plus every document under `dir` when there is one.
    ///
    /// A directory that does not exist yet holds nothing and is not an error:
    /// `schema register` creates it. A path that exists and is not a directory
    /// is refused, since reading it as an empty registry would answer every
    /// verb as though the caller had registered nothing.
    pub fn load(base: Registry, dir: Option<&Path>) -> Result<Self, RegistryDirError> {
        let mut registry = base;
        if let Some(dir) = dir {
            let exists = match std::fs::metadata(dir) {
                Ok(metadata) if metadata.is_dir() => true,
                Ok(_) => {
                    return Err(RegistryDirError::NotADirectory {
                        dir: dir.to_path_buf(),
                    })
                }
                // Windows answers a path beneath a file as not found, where Unix
                // answers not a directory: the nearest ancestor that exists says
                // which it is, so both platforms refuse it alike.
                Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
                    if let Some(file) = dir
                        .ancestors()
                        .skip(1)
                        .find(|ancestor| ancestor.exists())
                        .filter(|ancestor| !ancestor.is_dir())
                    {
                        return Err(RegistryDirError::Dir {
                            dir: dir.to_path_buf(),
                            source: std::io::Error::new(
                                std::io::ErrorKind::NotADirectory,
                                format!("{} is not a directory", file.display()),
                            ),
                        });
                    }
                    false
                }
                Err(source) => {
                    return Err(RegistryDirError::Dir {
                        dir: dir.to_path_buf(),
                        source,
                    })
                }
            };
            if exists {
                let entries = std::fs::read_dir(dir).map_err(|source| RegistryDirError::Dir {
                    dir: dir.to_path_buf(),
                    source,
                })?;
                // An entry that cannot be read is refused rather than skipped: a
                // registry read with a document missing would answer as though
                // that schema had never been registered.
                let mut paths = Vec::new();
                for entry in entries {
                    let path = entry
                        .map_err(|source| RegistryDirError::Dir {
                            dir: dir.to_path_buf(),
                            source,
                        })?
                        .path();
                    if path.extension().is_some_and(|ext| ext == "json") {
                        paths.push(path);
                    }
                }
                paths.sort();
                for path in paths {
                    let document = read_document(&path)?;
                    let expected = format!("{}.json", document.id);
                    if path.file_name().and_then(|name| name.to_str()) != Some(expected.as_str()) {
                        return Err(RegistryDirError::Misnamed {
                            path,
                            claimed: document.id,
                        });
                    }
                    registry.register_schema(document.id, document.schema)?;
                }
            }
        }
        Ok(Self {
            dir: dir.map(Path::to_path_buf),
            registry,
        })
    }

    /// Register every document of every `linked` bundle beside the base's and
    /// the directory's, in memory only.
    pub fn add_linked(&mut self, linked: &[Resolved]) -> Result<(), LinkError> {
        linked
            .iter()
            .try_for_each(|resolved| resolved.register_into(&mut self.registry))
    }

    /// The registry, base and directory together.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Record `schema` under `id`: in memory, and as a file in the directory.
    ///
    /// Refused when `id` already holds a different document, in the base or
    /// in the directory, before anything is written.
    pub fn register(&mut self, id: SchemaId, schema: Value) -> Result<PathBuf, RegistryDirError> {
        let Some(dir) = &self.dir else {
            return Err(RegistryDirError::Dir {
                dir: PathBuf::from("<none>"),
                source: std::io::Error::other(
                    "no registry directory: pass --registry <dir> or set ONEMESSAGEBUS_REGISTRY",
                ),
            });
        };
        self.registry.register_schema(id.clone(), schema.clone())?;
        std::fs::create_dir_all(dir).map_err(|source| RegistryDirError::Dir {
            dir: dir.clone(),
            source,
        })?;
        let path = dir.join(format!("{id}.json"));
        let document = RegistryDocument { id, schema };
        let mut text = serde_json::to_string_pretty(&document).unwrap_or_default();
        text.push('\n');
        std::fs::write(&path, text).map_err(|source| RegistryDirError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }
}

fn read_document(path: &Path) -> Result<RegistryDocument, RegistryDirError> {
    let text = std::fs::read_to_string(path).map_err(|source| RegistryDirError::Document {
        path: path.to_path_buf(),
        why: source.to_string(),
    })?;
    serde_json::from_str(&text).map_err(|failure| RegistryDirError::Document {
        path: path.to_path_buf(),
        why: failure.to_string(),
    })
}
