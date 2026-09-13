//! The carry backend: messages kept for a receiver that is not running now.
//!
//! A sender from [`Carry::sender`] appends its message to a durable store and is
//! answered at once with the consumer's [`Carried::carried`] disposition, since
//! nothing has read the message yet. The receiver's next session drains the
//! store as it opens, through [`Inbox::adopt_carried`], and takes each message
//! exactly once. [`Carry::read`] lists a store without draining it.
//!
//! A store is one NDJSON file: a header line naming it a carry store, then one
//! [`CarriedEntry`] per line in the order they were carried. Every writer and
//! the draining receiver hold an exclusive lock on the file across what they do
//! with it, so two processes carrying at once, or carrying while a receiver
//! drains, leave every message in the store or in the inbox and never in both.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::clock::now_rfc3339;
use crate::inbox::{BackendError, Carried, Disposition, Inbox, InboxBackend, Sender, Undelivered};
use crate::schema::{Message, SchemaId};

/// The shape of a carry store's header and records.
pub const CARRY_SCHEMA_VERSION: u32 = 1;

/// The word a store's header names itself with.
const STORE_KIND: &str = "onemessagebus-carry-store";

/// What a path is expected to be, in a refusal.
const WHAT: &str = "a carry store";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema_version: u32,
    kind: String,
}

/// One carried message, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CarriedEntry {
    /// When it was carried: RFC 3339, millisecond precision, UTC.
    // llmlint: ignore[invalid_states_unrepresentable] the stamp is carried as the bytes `now_rfc3339` wrote, exactly as the envelope's `ts` is, and reading a store refuses a record whose `ts` is not that form (`is_stamp`), so a malformed one never leaves `Carry::read` or `adopt_carried`.
    pub ts: String,
    /// The schema the message is.
    pub schema: SchemaId,
    /// The message.
    pub message: Value,
}

/// The carry backend's entry points.
#[derive(Debug, Clone, Copy)]
pub struct Carry;

impl Carry {
    /// A sender that carries each message into the store at `store`, creating
    /// the store on the first message.
    #[must_use]
    pub fn sender<M: Message + 'static, D: Carried>(store: impl Into<PathBuf>) -> Sender<M, D> {
        Sender::over(Carrier::<M, D> {
            store: store.into(),
            types: PhantomData,
        })
    }

    /// Every message the store at `store` holds, in the order they were carried.
    ///
    /// # Errors
    ///
    /// [`BackendError::Absent`] for a path that is no carry store, naming it,
    /// and [`BackendError::Unreadable`] for a record this build cannot read.
    pub fn read(store: &Path) -> Result<Vec<CarriedEntry>, BackendError> {
        if store.is_dir() {
            return Err(absent(store, "it is a directory"));
        }
        let mut file = match File::open(store) {
            Ok(file) => file,
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                return Err(absent(store, "nothing is there"))
            }
            Err(failure) => return Err(BackendError::io("open", store, &failure)),
        };
        file.lock_shared()
            .map_err(|failure| BackendError::io("lock", store, &failure))?;
        let contents = contents_of(&mut file, store)?;
        let (_, entries) = parse(store, &contents)?;
        Ok(entries)
    }
}

/// A sender's end of one carry store.
struct Carrier<M, D> {
    store: PathBuf,
    types: PhantomData<fn(M) -> D>,
}

impl<M: Message, D: Carried> InboxBackend<M, D> for Carrier<M, D> {
    fn send(&self, message: M) -> Result<D, Undelivered> {
        let entry = CarriedEntry {
            ts: now_rfc3339(),
            schema: M::SCHEMA,
            message: serde_json::to_value(&message).map_err(|failure| BackendError::Encoding {
                what: "message",
                why: failure.to_string(),
            })?,
        };
        let mut line = serde_json::to_string(&entry).map_err(|failure| BackendError::Encoding {
            what: "message",
            why: failure.to_string(),
        })?;
        line.push('\n');
        let store = &self.store;
        if store.is_dir() {
            return Err(absent(store, "it is a directory").into());
        }
        create_store(store)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store)
            .map_err(|failure| BackendError::io("open", store, &failure))?;
        file.lock()
            .map_err(|failure| BackendError::io("lock", store, &failure))?;
        let contents = contents_of(&mut file, store)?;
        parse(store, &contents)?;
        file.seek(SeekFrom::End(0))
            .and_then(|_| file.write_all(line.as_bytes()))
            .and_then(|()| file.sync_data())
            .map_err(|failure| BackendError::io("append to", store, &failure))?;
        Ok(D::carried())
    }
}

impl<M: Message + Send + 'static, D: Disposition> Inbox<M, D> {
    /// Drain the carry store at `store` into this inbox, oldest first, and
    /// answer how many messages it held. Each is taken exactly once: the store
    /// is emptied in the same step the inbox takes them. A store nothing was
    /// ever carried into is an empty one.
    ///
    /// Nobody is waiting on a carried message — its sender was answered when it
    /// was carried — so an answer to one is recorded in
    /// [`answered`](Self::answered) and sent nowhere.
    ///
    /// # Errors
    ///
    /// [`Undelivered::Closed`] when this inbox is closed, and
    /// [`Undelivered::Backend`] for a path that is no carry store or a record
    /// that is not a message of this inbox; the store is left untouched either
    /// way.
    pub fn adopt_carried(&self, store: impl AsRef<Path>) -> Result<usize, Undelivered> {
        let store = store.as_ref();
        if let Some(closed) = self.closed() {
            return Err(Undelivered::Closed(closed));
        }
        if store.is_dir() {
            return Err(absent(store, "it is a directory").into());
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(store) {
            Ok(file) => file,
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(failure) => return Err(BackendError::io("open", store, &failure).into()),
        };
        file.lock()
            .map_err(|failure| BackendError::io("lock", store, &failure))?;
        let contents = contents_of(&mut file, store)?;
        let (header, entries) = parse(store, &contents)?;
        let messages = entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| message_of::<M>(store, index, entry))
            .collect::<Result<Vec<M>, BackendError>>()?;
        if messages.is_empty() {
            return Ok(0);
        }
        self.shared()
            .offer_all(messages, || {
                file.set_len(header as u64)
                    .and_then(|()| file.sync_data())
                    .map_err(|failure| BackendError::io("drain", store, &failure))
            })
            .map_err(|refused| match refused {
                Ok(closed) => Undelivered::Closed(closed),
                Err(failure) => Undelivered::Backend(failure),
            })
    }
}

/// Record `index` of a store as a message of `M`.
fn message_of<M: Message>(
    store: &Path,
    index: usize,
    entry: CarriedEntry,
) -> Result<M, BackendError> {
    let unreadable = |why: String| BackendError::Unreadable {
        at: store.to_path_buf(),
        file: store.to_path_buf(),
        why: format!("record {}: {why}", index + 1),
    };
    if entry.schema != M::SCHEMA {
        return Err(unreadable(format!(
            "it carries a {}, and this inbox takes {}",
            entry.schema,
            M::SCHEMA
        )));
    }
    serde_json::from_value(entry.message)
        .map_err(|failure| unreadable(format!("it is not a {}: {failure}", M::SCHEMA)))
}

fn absent(store: &Path, why: &str) -> BackendError {
    BackendError::Absent {
        what: WHAT,
        path: store.to_path_buf(),
        why: why.to_owned(),
    }
}

fn header_line() -> Result<String, BackendError> {
    let mut line = serde_json::to_string(&Header {
        schema_version: CARRY_SCHEMA_VERSION,
        kind: STORE_KIND.to_owned(),
    })
    .map_err(|failure| BackendError::Encoding {
        what: "store header",
        why: failure.to_string(),
    })?;
    line.push('\n');
    Ok(line)
}

fn contents_of(file: &mut File, store: &Path) -> Result<String, BackendError> {
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|failure| BackendError::io("read", store, &failure))?;
    Ok(contents)
}

/// The byte length of a store's header line and every record after it.
fn parse(store: &Path, contents: &str) -> Result<(usize, Vec<CarriedEntry>), BackendError> {
    let Some((header, records)) = contents.split_once('\n') else {
        return Err(absent(
            store,
            if contents.is_empty() {
                "it is empty, and a carry store starts with its header line"
            } else {
                "it has no header line"
            },
        ));
    };
    match serde_json::from_str::<Header>(header) {
        Ok(read) if read.kind == STORE_KIND && read.schema_version == CARRY_SCHEMA_VERSION => {}
        Ok(read) if read.kind == STORE_KIND => {
            return Err(BackendError::Unreadable {
                at: store.to_path_buf(),
                file: store.to_path_buf(),
                why: format!(
                    "it declares schema_version {}, and this build reads {CARRY_SCHEMA_VERSION}",
                    read.schema_version
                ),
            })
        }
        _ => {
            return Err(absent(
                store,
                "its first line is not a carry store's header",
            ))
        }
    }
    if !records.is_empty() && !records.ends_with('\n') {
        return Err(BackendError::Unreadable {
            at: store.to_path_buf(),
            file: store.to_path_buf(),
            why: "it ends in a torn record".to_owned(),
        });
    }
    let entries = records
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let unreadable = |why: String| BackendError::Unreadable {
                at: store.to_path_buf(),
                file: store.to_path_buf(),
                why: format!("record {}: {why}", index + 1),
            };
            let entry = serde_json::from_str::<CarriedEntry>(line)
                .map_err(|failure| unreadable(failure.to_string()))?;
            if !is_stamp(&entry.ts) {
                return Err(unreadable(format!(
                    "its ts {:?} is not RFC 3339 with millisecond precision in UTC",
                    entry.ts
                )));
            }
            Ok(entry)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((header.len() + 1, entries))
}

/// Whether `ts` is the stamp `now_rfc3339` writes: `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn is_stamp(ts: &str) -> bool {
    const SHAPE: &[u8; 24] = b"dddd-dd-ddTdd:dd:dd.dddZ";
    ts.len() == SHAPE.len()
        && ts
            .bytes()
            .zip(SHAPE.iter())
            .all(|(byte, shape)| match shape {
                b'd' => byte.is_ascii_digit(),
                literal => byte == *literal,
            })
}

/// Create the store with its header line unless something is already there, in
/// one step: the header is written beside the store and linked into place, so no
/// reader or drain ever finds a store that exists without its header.
fn create_store(store: &Path) -> Result<(), BackendError> {
    static STAGED: AtomicU64 = AtomicU64::new(0);
    if store.exists() {
        return Ok(());
    }
    let mut staging = store.as_os_str().to_owned();
    staging.push(format!(
        ".{}-{}.staging",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    ));
    let staging = PathBuf::from(staging);
    fs::write(&staging, header_line()?)
        .map_err(|failure| BackendError::io("write", &staging, &failure))?;
    let linked = fs::hard_link(&staging, store);
    let _ = fs::remove_file(&staging);
    match linked {
        Ok(()) => Ok(()),
        Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(failure) => Err(BackendError::io("create", store, &failure)),
    }
}
