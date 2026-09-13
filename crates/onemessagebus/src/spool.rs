//! The spool backend: a directory a receiver's process binds, which a sender in
//! another process offers messages into and reads each answer back out of.
//!
//! The receiver binds the directory with [`Spool::bind`], which holds its lock
//! and starts a courier thread moving each offered message into the in-process
//! [`Inbox`] and writing the receiver's answer back beside it. A sender reaches
//! it through [`Spool::connect`] — or [`Spool::deliver`] for a message that is
//! JSON rather than a Rust type — by the path [`Spool::address`] answers.
//!
//! Every file is written beside its final name and renamed onto it, so neither
//! side ever reads half a document, and every hand-over is a rename, so the
//! courier taking a message and its sender withdrawing it cannot both succeed.
//! `docs/inbox.md` states the on-disk shape.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::inbox::{
    BackendError, Closed, Disposition, Inbox, InboxBackend, Reply, Sender, Shared, Undelivered,
};
use crate::schema::{Message, SchemaId};

/// How long a sender waits for its message to be taken before it withdraws it
/// and reports it lost. A taken message is waited on for as long as its receiver
/// stays bound.
pub const SPOOL_WAIT: Duration = Duration::from_secs(30);

/// The shape every document in a spool is written under. A document naming
/// another version was written by a build that knew something this one does
/// not, and is refused rather than guessed at.
pub const SPOOL_SCHEMA_VERSION: u32 = 1;

/// How often the courier looks for offers and a sender looks for its answer.
const POLL: Duration = Duration::from_millis(20);

/// The receiver's declaration: which message schema it takes.
const DECLARATION: &str = "spool.json";
/// Held locked by the bound receiver for as long as it is bound.
const RECEIVER_LOCK: &str = "receiver.lock";
/// Written when the receiver closes its inbox.
const CLOSED_RECORD: &str = "closed.json";
/// `<id>.offer.json`: a message waiting to be taken.
const OFFER: &str = ".offer.json";
/// `<id>.taken.json`: a message the courier has taken.
const TAKEN: &str = ".taken.json";
/// `<id>.answer.json`: what became of it.
const ANSWER: &str = ".answer.json";
/// `<id>.withdrawn`: a message its sender took back, on its way to removal.
const WITHDRAWN: &str = ".withdrawn";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    schema_version: u32,
    schema: SchemaId,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosedRecord {
    schema_version: u32,
    reason: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema: Option<SchemaId>,
    message: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerDocument {
    schema_version: u32,
    answer: AnswerBody,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum AnswerBody {
    /// The receiver's disposition.
    Disposition(Value),
    /// The inbox closed before the receiver answered.
    Closed(Closed),
    /// The receiver could not read the offer.
    Refused {
        /// Why.
        reason: String,
    },
}

/// A receiver's binding of one spool directory.
///
/// Dropping it stops the courier and releases the lock: offers are no longer
/// taken, and a sender whose message was already taken learns the receiver went
/// away rather than waiting on it.
pub struct Spool {
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    courier: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Spool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spool")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Spool {
    /// Bind `dir` as the spool `inbox` receives through: create it, hold its
    /// lock, declare the schema `M` is, and start the courier. A spool closed
    /// by an earlier receiver is reopened.
    ///
    /// # Errors
    ///
    /// [`BackendError::Bound`] when another receiver holds the spool, and
    /// [`BackendError::Io`] when the directory or its files cannot be made.
    pub fn bind<M, D>(dir: impl Into<PathBuf>, inbox: &Inbox<M, D>) -> Result<Self, BackendError>
    where
        M: Message + Send + 'static,
        D: Disposition,
    {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|failure| BackendError::io("create", &dir, &failure))?;
        let lock_path = dir.join(RECEIVER_LOCK);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|failure| BackendError::io("open", &lock_path, &failure))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(BackendError::Bound { path: dir }),
            Err(TryLockError::Error(failure)) => {
                return Err(BackendError::io("lock", &lock_path, &failure))
            }
        }
        write_document(
            &dir.join(DECLARATION),
            &Declaration {
                schema_version: SPOOL_SCHEMA_VERSION,
                schema: M::SCHEMA,
            },
        )?;
        remove_if_present(&dir.join(CLOSED_RECORD))?;
        let shared = inbox.shared();
        let closing = dir.clone();
        shared.on_close(move |closed| close_spool(&closing, closed));
        let stop = Arc::new(AtomicBool::new(false));
        let courier = {
            let serving = dir.clone();
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("onemessagebus-spool".to_owned())
                .spawn(move || serve(&serving, &shared, &stop, lock))
                .map_err(|failure| BackendError::io("start the courier for", &dir, &failure))?
        };
        Ok(Self {
            dir,
            stop,
            courier: Some(courier),
        })
    }

    /// The path a sender elsewhere reaches this spool by.
    #[must_use]
    pub fn address(&self) -> &Path {
        &self.dir
    }

    /// A sender into the spool at `address`, waiting [`SPOOL_WAIT`] for a
    /// message to be taken.
    #[must_use]
    pub fn connect<M: Message + 'static, D: Disposition>(
        address: impl Into<PathBuf>,
    ) -> Sender<M, D> {
        Self::connect_within(address, SPOOL_WAIT)
    }

    /// A sender into the spool at `address`, waiting `wait` for a message to be
    /// taken before withdrawing it.
    #[must_use]
    pub fn connect_within<M: Message + 'static, D: Disposition>(
        address: impl Into<PathBuf>,
        wait: Duration,
    ) -> Sender<M, D> {
        Sender::over(Connection::<M, D> {
            dir: address.into(),
            wait,
            types: PhantomData,
        })
    }

    /// The schema the spool at `address` declares its receiver takes, or `None`
    /// when no receiver has ever bound it.
    ///
    /// # Errors
    ///
    /// [`BackendError::Absent`] when `address` is not a directory, and
    /// [`BackendError::Unreadable`] for a declaration this build cannot read.
    pub fn declared(address: &Path) -> Result<Option<SchemaId>, BackendError> {
        spool_dir(address)?;
        let path = address.join(DECLARATION);
        Ok(read_document::<Declaration>(address, &path)?.map(|declaration| declaration.schema))
    }

    /// Offer one JSON message to the spool at `address` under the schema its
    /// receiver declared, and answer the receiver's disposition as JSON.
    ///
    /// # Errors
    ///
    /// As [`Sender::send`]: [`Undelivered::Closed`] with the closer's reason, or
    /// [`Undelivered::Backend`] naming the spool and the file or the wait.
    pub fn deliver(address: &Path, message: &Value, wait: Duration) -> Result<Value, Undelivered> {
        let schema = Self::declared(address)?;
        exchange(address, schema, message.clone(), wait).map(|(disposition, _)| disposition)
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(courier) = self.courier.take() {
            let _ = courier.join();
        }
    }
}

/// A sender's end of one spool.
struct Connection<M, D> {
    dir: PathBuf,
    wait: Duration,
    types: PhantomData<fn(M) -> D>,
}

impl<M: Message, D: Disposition> InboxBackend<M, D> for Connection<M, D> {
    fn send(&self, message: M) -> Result<D, Undelivered> {
        let message = serde_json::to_value(&message).map_err(|failure| BackendError::Encoding {
            what: "message",
            why: failure.to_string(),
        })?;
        let (disposition, file) = exchange(&self.dir, Some(M::SCHEMA), message, self.wait)?;
        serde_json::from_value(disposition).map_err(|failure| {
            Undelivered::Backend(BackendError::Unreadable {
                at: self.dir.clone(),
                file,
                why: format!("its disposition is not one this sender reads: {failure}"),
            })
        })
    }
}

fn spool_dir(address: &Path) -> Result<(), BackendError> {
    if address.is_dir() {
        return Ok(());
    }
    Err(BackendError::Absent {
        what: "a spool",
        path: address.to_path_buf(),
        why: if address.exists() {
            "it is not a directory".to_owned()
        } else {
            "nothing is there".to_owned()
        },
    })
}

/// Offer `message` and wait for what became of it: the disposition and the
/// answer document it was read from.
fn exchange(
    dir: &Path,
    schema: Option<SchemaId>,
    message: Value,
    wait: Duration,
) -> Result<(Value, PathBuf), Undelivered> {
    spool_dir(dir)?;
    // Before anything is offered: a closed receiver never services its spool
    // again, and a message left in it would be an offer nothing reads.
    if let Some(closed) = closed_record(dir)? {
        return Err(Undelivered::Closed(closed));
    }
    let id = mint();
    let offer = dir.join(format!("{id}{OFFER}"));
    let taken = dir.join(format!("{id}{TAKEN}"));
    let answer = dir.join(format!("{id}{ANSWER}"));
    write_document(
        &offer,
        &Offer {
            schema_version: SPOOL_SCHEMA_VERSION,
            schema,
            message,
        },
    )?;
    let started = Instant::now();
    loop {
        if let Some(disposition) = read_answer(dir, &answer)? {
            return Ok((disposition, answer));
        }
        if offer.exists() {
            if let Some(closed) = closed_record(dir)? {
                if withdraw(dir, &id) {
                    return Err(Undelivered::Closed(closed));
                }
                continue;
            }
            if started.elapsed() >= wait {
                if withdraw(dir, &id) {
                    return Err(Undelivered::Backend(BackendError::Elapsed {
                        spool: dir.to_path_buf(),
                        waited: wait,
                    }));
                }
                continue;
            }
        } else if !taken.exists() || !receiver_bound(dir) {
            // Taken, and either answered between the two looks above or held by
            // a receiver that is gone: the answer decides which.
            if let Some(disposition) = read_answer(dir, &answer)? {
                return Ok((disposition, answer));
            }
            let _ = fs::remove_file(&taken);
            return Err(Undelivered::Backend(BackendError::Abandoned {
                spool: dir.to_path_buf(),
                file: taken,
            }));
        }
        std::thread::sleep(POLL);
    }
}

/// The answer written at `path`, once there is one.
fn read_answer(dir: &Path, path: &Path) -> Result<Option<Value>, Undelivered> {
    let Some(document) = read_document::<AnswerDocument>(dir, path)? else {
        return Ok(None);
    };
    let _ = fs::remove_file(path);
    match document.answer {
        AnswerBody::Disposition(disposition) => Ok(Some(disposition)),
        AnswerBody::Closed(closed) => Err(Undelivered::Closed(closed)),
        AnswerBody::Refused { reason } => Err(Undelivered::Backend(BackendError::Refused {
            spool: dir.to_path_buf(),
            why: reason,
        })),
    }
}

/// The close a receiver recorded, if it has.
fn closed_record(dir: &Path) -> Result<Option<Closed>, BackendError> {
    Ok(
        read_document::<ClosedRecord>(dir, &dir.join(CLOSED_RECORD))?
            .map(|record| Closed::new(record.reason)),
    )
}

/// Whether a receiver holds the spool's lock.
fn receiver_bound(dir: &Path) -> bool {
    let Ok(lock) = OpenOptions::new().write(true).open(dir.join(RECEIVER_LOCK)) else {
        return false;
    };
    // Taking the lock proves nobody holds it; the handle is dropped at once, so
    // a receiver binding next is not refused by this look.
    !matches!(lock.try_lock(), Ok(()))
}

/// Take back offer `id`, if the courier has not taken it first.
fn withdraw(dir: &Path, id: &str) -> bool {
    let withdrawn = dir.join(format!("{id}{WITHDRAWN}"));
    if fs::rename(dir.join(format!("{id}{OFFER}")), &withdrawn).is_err() {
        return false;
    }
    let _ = fs::remove_file(withdrawn);
    true
}

/// The courier: take each offer into the inbox until the inbox closes or the
/// spool is dropped, holding the receiver's lock throughout.
fn serve<M, D>(dir: &Path, shared: &Arc<Shared<M, D>>, stop: &AtomicBool, lock: File)
where
    M: Message + Send + 'static,
    D: Disposition,
{
    while !stop.load(Ordering::SeqCst) && shared.closed().is_none() {
        for id in offered(dir) {
            take_offer(dir, &id, shared);
        }
        std::thread::sleep(POLL);
    }
    drop(lock);
}

/// The ids of every offer waiting, in the order of the instants they were
/// minted at.
fn offered(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(OFFER))
                .map(str::to_owned)
        })
        .collect();
    ids.sort();
    ids
}

fn take_offer<M, D>(dir: &Path, id: &str, shared: &Arc<Shared<M, D>>)
where
    M: Message + Send + 'static,
    D: Disposition,
{
    let taken = dir.join(format!("{id}{TAKEN}"));
    if fs::rename(dir.join(format!("{id}{OFFER}")), &taken).is_err() {
        // Withdrawn by its sender first.
        return;
    }
    let message = match read_offer::<M>(dir, &taken) {
        Ok(message) => message,
        Err(reason) => {
            settle(dir, id, &AnswerBody::Refused { reason });
            return;
        }
    };
    let replying = dir.to_path_buf();
    let replying_id = id.to_owned();
    let _ = shared.offer(
        message,
        Reply::to(move |answer: Result<D, Closed>| {
            let body = match answer {
                Ok(disposition) => match serde_json::to_value(&disposition) {
                    Ok(value) => AnswerBody::Disposition(value),
                    Err(failure) => AnswerBody::Refused {
                        reason: format!("its disposition does not serialize: {failure}"),
                    },
                },
                Err(closed) => AnswerBody::Closed(closed),
            };
            settle(&replying, &replying_id, &body);
        }),
    );
}

/// The message in a taken offer, or why the receiver cannot read it.
fn read_offer<M: Message>(dir: &Path, path: &Path) -> Result<M, String> {
    let offer = read_document::<Offer>(dir, path)
        .map_err(|failure| failure.to_string())?
        .ok_or_else(|| format!("{} was removed before it was read", path.display()))?;
    if let Some(schema) = &offer.schema {
        if *schema != M::SCHEMA {
            return Err(format!(
                "the message is {schema}, and this receiver takes {}",
                M::SCHEMA
            ));
        }
    }
    serde_json::from_value(offer.message)
        .map_err(|failure| format!("the message is not a {}: {failure}", M::SCHEMA))
}

/// Write what became of message `id`, if its sender is still waiting for it.
fn settle(dir: &Path, id: &str, body: &AnswerBody) {
    let taken = dir.join(format!("{id}{TAKEN}"));
    if !taken.exists() {
        // Its sender stopped waiting: nobody reads an answer written now.
        return;
    }
    let _ = write_document(
        &dir.join(format!("{id}{ANSWER}")),
        &AnswerDocument {
            schema_version: SPOOL_SCHEMA_VERSION,
            answer: clone_body(body),
        },
    );
    let _ = fs::remove_file(taken);
}

fn clone_body(body: &AnswerBody) -> AnswerBody {
    match body {
        AnswerBody::Disposition(value) => AnswerBody::Disposition(value.clone()),
        AnswerBody::Closed(closed) => AnswerBody::Closed(closed.clone()),
        AnswerBody::Refused { reason } => AnswerBody::Refused {
            reason: reason.clone(),
        },
    }
}

/// The receiver closed: record it, so a sender arriving later is refused, and
/// answer every offer still waiting.
fn close_spool(dir: &Path, closed: &Closed) {
    let _ = write_document(
        &dir.join(CLOSED_RECORD),
        &ClosedRecord {
            schema_version: SPOOL_SCHEMA_VERSION,
            reason: closed.reason.clone(),
        },
    );
    for id in offered(dir) {
        if fs::rename(
            dir.join(format!("{id}{OFFER}")),
            dir.join(format!("{id}{TAKEN}")),
        )
        .is_ok()
        {
            settle(dir, &id, &AnswerBody::Closed(closed.clone()));
        }
    }
}

/// A document at `path`, or `None` when nothing is there yet.
fn read_document<T: DeserializeOwned>(at: &Path, path: &Path) -> Result<Option<T>, BackendError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(failure) => return Err(BackendError::io("read", path, &failure)),
    };
    let unreadable = |why: String| BackendError::Unreadable {
        at: at.to_path_buf(),
        file: path.to_path_buf(),
        why,
    };
    let document: Value =
        serde_json::from_str(&text).map_err(|failure| unreadable(failure.to_string()))?;
    let version = document.get("schema_version").and_then(Value::as_u64);
    if version != Some(u64::from(SPOOL_SCHEMA_VERSION)) {
        return Err(unreadable(format!(
            "it declares schema_version {}, and this build reads {SPOOL_SCHEMA_VERSION}",
            version.map_or_else(|| "none".to_owned(), |v| v.to_string())
        )));
    }
    serde_json::from_value(document)
        .map(Some)
        .map_err(|failure| unreadable(failure.to_string()))
}

/// Write `document` beside `path` and rename it onto it.
fn write_document<T: Serialize>(path: &Path, document: &T) -> Result<(), BackendError> {
    let text = serde_json::to_string(document).map_err(|failure| BackendError::Encoding {
        what: "document",
        why: failure.to_string(),
    })?;
    let mut staging = path.as_os_str().to_owned();
    staging.push(".staging");
    let staging = PathBuf::from(staging);
    fs::write(&staging, text).map_err(|failure| BackendError::io("write", &staging, &failure))?;
    fs::rename(&staging, path).map_err(|failure| BackendError::io("rename onto", path, &failure))
}

fn remove_if_present(path: &Path) -> Result<(), BackendError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(failure) => Err(BackendError::io("remove", path, &failure)),
    }
}

/// An id no two offers share. The wall clock leads, so the courier takes offers
/// in the order of their minting instants — the order they were made, unless
/// the system clock steps back; the process id separates two processes minting
/// in one instant, and the counter two threads of one process.
fn mint() -> String {
    static MINTED: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(
        "{now:039}-{}-{:020}",
        std::process::id(),
        MINTED.fetch_add(1, Ordering::Relaxed)
    )
}
