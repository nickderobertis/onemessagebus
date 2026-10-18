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
//! Past the courier, a receiver that advertises it in `admission.json` says
//! when it has queued a message (`<id>.admitted.json`), and the receiver's take
//! and its sender's withdrawal race to create one claim file (`<id>.claimed`),
//! which only one of them can: so the guarantee holds up to [`Inbox::take`]
//! rather than up to the courier. A sender in `wait_until` publishes its
//! deadline (`<id>.deadline.json`) before it waits, on the monotonic clock it
//! verified it shares with the receiver, and a release or take past
//! it drops the message and says so (`<id>.expired`) rather than queueing or
//! taking it. `docs/inbox.md` states the
//! on-disk shape.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::inbox::{
    Acknowledged, BackendError, Claimed, Closed, Disposition, Gate, Inbox, InboxBackend, Offer,
    Pending, Released, Reply, Sender, Shared, Unanswered, Undelivered,
};
use crate::schema::{Message, SchemaId};
use crate::shared_clock::{ClockIdentity, SharedClock};

/// How long a sender waits for its message to be admitted before it withdraws
/// it and reports it lost. A taken message is waited on for as long as its receiver
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
/// The receiver's advertisement that it acknowledges admission and honours a
/// withdrawal after it, bound to the `spool.json` it was written beside.
const ADMISSION: &str = "admission.json";
/// Held locked by a receiver that advertises, for as long as it is bound: an
/// advertisement whose lock nobody holds is a dead receiver's.
const ADMISSION_LOCK: &str = "admission.lock";
/// `<id>.admitted.json`: a message the receiver's inbox has queued.
const ADMITTED: &str = ".admitted.json";
/// `<id>.claimed`: created by whichever of the receiver's take and its
/// sender's withdrawal comes first; the other cannot create it.
const CLAIMED: &str = ".claimed";
/// `<id>.deadline.json`: the deadline a sender in `wait_until` published before
/// it waited, which a release or take past it expires the message at.
const DEADLINE: &str = ".deadline.json";
/// `<id>.expired`: created by the receiver as it dropped the message from its
/// inbox, past its sender's deadline.
const EXPIRED: &str = ".expired";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    schema_version: u32,
    schema: SchemaId,
}

/// What a receiver that withdraws after admission writes beside its
/// declaration. Unknown fields are read past, so a later build can say more.
#[derive(Serialize, Deserialize)]
struct Admission {
    schema_version: u32,
    /// The identity of the `spool.json` file this was written beside. Every
    /// declaration is a new file renamed into place, so a receiver rebinding
    /// the spool leaves one this no longer names.
    declaration: Identity,
    /// The monotonic clock the receiver reads a published deadline on. A
    /// sender that cannot verify it shares that clock still waits for the
    /// acknowledgement, and reads every admitted message as taken, so no
    /// expiry it reports rests on a clock the two do not share. Absent where
    /// the receiver identifies none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clock: Option<ClockIdentity>,
}

/// The courier's acknowledgement that the inbox has queued a message.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Admitted {
    schema_version: u32,
}

/// The deadline a waiting sender published, on the monotonic clock it shares
/// with the receiver: the clock its `Instant` was taken on.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeadlineDocument {
    schema_version: u32,
    /// The clock `deadline` is a reading of.
    clock: ClockIdentity,
    /// Nanoseconds on that clock.
    deadline: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosedRecord {
    schema_version: u32,
    reason: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfferDocument {
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
        // Opened before the receiver's lock, so a process that ends with both
        // held lets this one go first.
        let admission_lock = open_lock(&dir.join(ADMISSION_LOCK))?;
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
        // Waited for rather than tried: only a sender's look holds it now, and
        // only for a moment.
        admission_lock
            .lock()
            .map_err(|failure| BackendError::io("lock", &dir.join(ADMISSION_LOCK), &failure))?;
        let declaration = dir.join(DECLARATION);
        write_document(
            &declaration,
            &Declaration {
                schema_version: SPOOL_SCHEMA_VERSION,
                schema: M::SCHEMA,
            },
        )?;
        write_document(
            &dir.join(ADMISSION),
            &Admission {
                schema_version: SPOOL_SCHEMA_VERSION,
                declaration: Identity::of(&declaration).ok_or_else(|| BackendError::Io {
                    action: "read the identity of",
                    path: declaration.clone(),
                    why: "it is gone as soon as it was written".to_owned(),
                })?,
                clock: SharedClock::here().map(|clock| clock.identity().clone()),
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
                .spawn(move || serve(&serving, &shared, &stop, [admission_lock, lock]))
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
    /// message to be admitted.
    #[must_use]
    pub fn connect<M: Message + 'static, D: Disposition>(
        address: impl Into<PathBuf>,
    ) -> Sender<M, D> {
        Self::connect_within(address, SPOOL_WAIT)
    }

    /// A sender into the spool at `address`, waiting `wait` for a message to be
    /// admitted before withdrawing it.
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
        Exchange::offer(address, schema, message.clone(), wait)?
            .wait()
            .map(|(disposition, _)| disposition)
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
        self.offer(message)?.wait()
    }

    fn offer(&self, message: M) -> Result<Offer<D>, Undelivered> {
        let message = serde_json::to_value(&message).map_err(|failure| BackendError::Encoding {
            what: "message",
            why: failure.to_string(),
        })?;
        let exchange = Exchange::offer(&self.dir, Some(M::SCHEMA), message, self.wait)?;
        Ok(Offer::over(Decoding {
            exchange,
            types: PhantomData,
        }))
    }
}

/// A spool offer whose answer is read as `D`.
struct Decoding<D> {
    exchange: Exchange,
    types: PhantomData<fn() -> D>,
}

impl<D: Disposition> Decoding<D> {
    fn decode(&self, (disposition, file): (Value, PathBuf)) -> Result<D, Undelivered> {
        serde_json::from_value(disposition).map_err(|failure| {
            Undelivered::Backend(BackendError::Unreadable {
                at: self.exchange.dir.clone(),
                file,
                why: format!("its disposition is not one this sender reads: {failure}"),
            })
        })
    }
}

impl<D: Disposition> Pending<D> for Decoding<D> {
    fn withdraw(&mut self) -> bool {
        self.exchange.withdraw()
    }

    fn wait(mut self: Box<Self>) -> Result<D, Undelivered> {
        let answered = self.exchange.wait()?;
        self.decode(answered)
    }

    fn wait_until(mut self: Box<Self>, deadline: Instant) -> Result<D, Unanswered> {
        let answered = self.exchange.wait_until(deadline)?;
        self.decode(answered).map_err(Unanswered::Undelivered)
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

/// One message a sender offered into a spool, admitted to its receiver.
struct Exchange {
    dir: PathBuf,
    id: MessageId,
    /// What admission means to the receiver that took the message: read once
    /// the courier has it.
    admits: Admits,
    /// The answer, when it came before the offer was handed back.
    answered: Option<(Value, PathBuf)>,
}

impl Exchange {
    /// Offer `message` and return once the receiver has admitted it, or
    /// withdraw it and say why it was not.
    fn offer(
        dir: &Path,
        schema: Option<SchemaId>,
        message: Value,
        wait: Duration,
    ) -> Result<Self, Undelivered> {
        spool_dir(dir)?;
        // Before anything is offered: a closed receiver never services its
        // spool again, and a message left in it would be an offer nothing reads.
        if let Some(closed) = closed_record(dir)? {
            return Err(Undelivered::Closed(closed));
        }
        let mut exchange = Self {
            dir: dir.to_path_buf(),
            id: mint(),
            admits: Admits::OnRename,
            answered: None,
        };
        let offer = exchange.file(OFFER);
        write_document(
            &offer,
            &OfferDocument {
                schema_version: SPOOL_SCHEMA_VERSION,
                schema,
                message,
            },
        )?;
        let started = Instant::now();
        let mut advertised = None;
        // An acknowledgement this build cannot read acknowledges nothing; it is
        // what a withdrawal at the end of the wait reports, by name.
        let mut unreadable = None;
        loop {
            if let Some(answered) = exchange.read_answer()? {
                exchange.answered = Some(answered);
                return Ok(exchange);
            }
            let elapsed = started.elapsed() >= wait;
            let lost = || BackendError::Elapsed {
                spool: dir.to_path_buf(),
                waited: wait,
            };
            if offer.exists() {
                if let Some(closed) = closed_record(dir)? {
                    if withdraw(dir, &exchange.id) {
                        return Err(Undelivered::Closed(closed));
                    }
                    continue;
                }
                if elapsed {
                    if withdraw(dir, &exchange.id) {
                        return Err(Undelivered::Backend(lost()));
                    }
                    continue;
                }
            } else {
                // The courier has it, so the receiver that took it is the one
                // whose advertisement decides what admission means.
                exchange.admits = *advertised.get_or_insert_with(|| advertises(dir));
                if exchange.admits == Admits::OnRename {
                    return Ok(exchange);
                }
                match read_document::<Admitted>(dir, &exchange.file(ADMITTED)) {
                    Ok(Some(_)) => return Ok(exchange),
                    Ok(None) => {}
                    Err(failure) => unreadable = Some(failure),
                }
                if !exchange.file(TAKEN).exists() || !receiver_bound(dir) {
                    exchange.answered = Some(exchange.ended()?);
                    return Ok(exchange);
                }
                if elapsed {
                    match exchange.withdraw_unadmitted() {
                        Withdrawal::Withdrawn => {
                            return Err(Undelivered::Backend(unreadable.unwrap_or_else(lost)));
                        }
                        // Taken, beside an acknowledgement this build could
                        // not read: admitted.
                        Withdrawal::Kept if exchange.file(ADMITTED).is_file() => {
                            return Ok(exchange);
                        }
                        // Claimed beside no acknowledgement: a receiver that
                        // could not write it, refusing the message, whose
                        // answer the next look reads. Or not claimable at
                        // all, which decides nothing: neither side has the
                        // message until a claim can be made.
                        Withdrawal::Kept | Withdrawal::Undecided => {}
                    }
                }
            }
            std::thread::sleep(POLL);
        }
    }

    fn file(&self, suffix: &str) -> PathBuf {
        self.id.file(&self.dir, suffix)
    }

    fn read_answer(&self) -> Result<Option<(Value, PathBuf)>, Undelivered> {
        let answer = self.file(ANSWER);
        Ok(read_answer(&self.dir, &answer)?.map(|disposition| (disposition, answer)))
    }

    /// The receiver no longer holds the message: its answer, if it wrote one
    /// between the caller's look and this one, or why there is none.
    fn ended(&self) -> Result<(Value, PathBuf), Undelivered> {
        if let Some(answered) = self.read_answer()? {
            return Ok(answered);
        }
        // Closing records its reason before moving each offer from offered to
        // taken and writing its answer. An unbound sender can observe that
        // middle state; the recorded close still decides the result.
        let taken = self.file(TAKEN);
        let _ = fs::remove_file(&taken);
        if let Some(closed) = closed_record(&self.dir)? {
            return Err(Undelivered::Closed(closed));
        }
        Err(Undelivered::Backend(BackendError::Abandoned {
            spool: self.dir.clone(),
            file: taken,
        }))
    }

    /// Take the admitted message back, if the receiver has not taken it: by
    /// creating its claim before the receiver's take can.
    fn withdraw(&mut self) -> bool {
        if self.admits != Admits::OnAcknowledgement || self.answered.is_some() {
            return false;
        }
        // Not claimable, the file system refusing, is never reported withdrawn.
        self.contest() == Withdrawal::Withdrawn
    }

    /// Create the message's claim before the receiver's take can, and keep it
    /// unless the receiver had already answered.
    fn contest(&self) -> Withdrawal {
        let claimed = self.file(CLAIMED);
        match claim(&claimed) {
            Contest::Won => {}
            Contest::Lost => return Withdrawal::Kept,
            Contest::Refused => return Withdrawal::Undecided,
        }
        // The receiver answers before it lets its claim go, so a claim made
        // after that — or made when the answer was already there — finds the
        // answer: the message was taken, and is answered.
        if self.file(ANSWER).exists() {
            let _ = fs::remove_file(claimed);
            return Withdrawal::Kept;
        }
        Withdrawal::Withdrawn
    }

    /// Take back a message whose admission was never acknowledged, when the
    /// bounded wait passes. Past admission only a receiver on a clock this
    /// sender verified it shares is withdrawn from; any other's inbox queues
    /// the message only once `<id>.admitted.json` is written, under one lock,
    /// before any take, so a claim made while that file is absent came before
    /// the message could be taken. One made after it is given back, and the
    /// message read as taken.
    fn withdraw_unadmitted(&mut self) -> Withdrawal {
        let withdrawal = self.contest();
        if self.admits == Admits::AcknowledgedThenTaken
            && withdrawal == Withdrawal::Withdrawn
            && self.file(ADMITTED).is_file()
        {
            let _ = fs::remove_file(self.file(CLAIMED));
            return Withdrawal::Kept;
        }
        withdrawal
    }

    /// The answer and the file it was read from, once there is one.
    fn wait(&mut self) -> Result<(Value, PathBuf), Undelivered> {
        if let Some(answered) = self.answered.take() {
            return Ok(answered);
        }
        loop {
            if let Some(answered) = self.read_answer()? {
                return Ok(answered);
            }
            if let Some(ended) = self.gone() {
                return ended;
            }
            std::thread::sleep(POLL);
        }
    }

    fn wait_until(&mut self, deadline: Instant) -> Result<(Value, PathBuf), Unanswered> {
        if let Some(answered) = self.answered.take() {
            return Ok(answered);
        }
        // Shared with the receiver: it advertises only where it is.
        let clock = SharedClock::here();
        let mut published = Published::Nothing;
        loop {
            if let Some(answered) = self.read_answer()? {
                return Ok(answered);
            }
            if published != Published::Nothing && self.file(EXPIRED).exists() {
                // Released past the deadline: the receiver dropped it from its
                // inbox and holds nothing of it, so what is left is ours.
                discard(&self.dir, &self.id);
                return Err(Unanswered::Expired);
            }
            if let Some(ended) = self.gone() {
                return Ok(ended?);
            }
            let now = Instant::now();
            // Published before any wait, on the clock the deadline's `Instant`
            // was taken on, so a release or take past it expires the message
            // however long this sender goes without running, and no step of the
            // wall clock moves it. One the file system refuses is published at
            // a later look.
            if let (Published::Nothing, Admits::OnAcknowledgement, Some(clock)) =
                (published, self.admits, &clock)
            {
                let document = DeadlineDocument {
                    schema_version: SPOOL_SCHEMA_VERSION,
                    clock: clock.identity().clone(),
                    deadline: clock.at(deadline),
                };
                if write_document(&self.file(DEADLINE), &document).is_ok() {
                    published = Published::Deadline;
                }
            }
            // Past the deadline every look tries to withdraw it, for a message
            // untaken there.
            if now >= deadline && self.withdraw() {
                return Err(Unanswered::Expired);
            }
            // Woken at the deadline itself, not up to a poll after it.
            std::thread::sleep(match deadline.checked_duration_since(now) {
                Some(left) if !left.is_zero() => left.min(POLL),
                _ => POLL,
            });
        }
    }

    /// What became of a taken message whose receiver no longer holds it —
    /// answered between two looks, or held by a receiver that is gone — or
    /// `None` while it still does.
    fn gone(&self) -> Option<Result<(Value, PathBuf), Undelivered>> {
        if self.file(TAKEN).exists() && receiver_bound(&self.dir) {
            return None;
        }
        Some(self.ended())
    }
}

impl Drop for Exchange {
    /// Its sender stops waiting: without its taken file, the receiver writes
    /// no answer for nobody to read, and clears what is left of the message.
    fn drop(&mut self) {
        let _ = fs::remove_file(self.file(TAKEN));
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
    held(&dir.join(RECEIVER_LOCK))
}

/// Whether some process holds the lock file at `path`.
fn held(path: &Path) -> bool {
    let Ok(lock) = OpenOptions::new().write(true).open(path) else {
        return false;
    };
    // Taking the lock proves nobody holds it; the handle is dropped at once, so
    // a receiver binding next is not refused by this look.
    !matches!(lock.try_lock(), Ok(()))
}

fn open_lock(path: &Path) -> Result<File, BackendError> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|failure| BackendError::io("open", path, &failure))
}

/// What admission means to the receiver bound to `dir`: acknowledged only
/// while the receiver that wrote `admission.json` is alive and bound — it
/// holds `admission.lock` — and the `spool.json` there now is the file it was
/// written beside, neither of which depends on a clock; and withdrawn from
/// after it only where the sender also verifies that the two read one
/// monotonic clock (`admission.json`'s `clock`), the one a published deadline
/// is read on. An older receiver never takes the lock, so one rebinding after a
/// killed newer one finds it free; and it renames a new declaration into place,
/// so for the moment a dying receiver's locks take to be let go of, the
/// identity differs.
fn advertises(dir: &Path) -> Admits {
    let Ok(Some(admission)) = read_document::<Admission>(dir, &dir.join(ADMISSION)) else {
        return Admits::OnRename;
    };
    if !held(&dir.join(ADMISSION_LOCK))
        || Identity::of(&dir.join(DECLARATION)).as_ref() != Some(&admission.declaration)
    {
        return Admits::OnRename;
    }
    // A receiver whose clock this sender cannot verify it shares could not
    // read a deadline the way this sender took it, so no withdrawal or expiry
    // after admission is claimed against it; its admission is still waited for.
    let shares_a_clock = matches!(
        (SharedClock::here(), &admission.clock),
        (Some(here), Some(theirs)) if here.is(theirs)
    );
    if shares_a_clock {
        Admits::OnAcknowledgement
    } else {
        Admits::AcknowledgedThenTaken
    }
}

/// A file's identity, written `unix:<device>:<inode>` or `unidentified`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
enum Identity {
    /// Its device and inode, which no other file shares while it exists.
    Unix { device: u64, inode: u64 },
    /// The standard library gives a file no identity on this platform, so
    /// every file reads the same, and the advertisement's lock alone decides.
    Unidentified,
}

impl Identity {
    /// The identity of the file at `path`, or `None` when nothing is there.
    #[cfg(unix)]
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(path).ok()?;
        Some(Self::Unix {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    #[cfg(not(unix))]
    fn of(path: &Path) -> Option<Self> {
        fs::metadata(path).ok().map(|_| Self::Unidentified)
    }
}

impl From<Identity> for String {
    fn from(identity: Identity) -> Self {
        match identity {
            Identity::Unix { device, inode } => format!("unix:{device}:{inode}"),
            Identity::Unidentified => "unidentified".to_owned(),
        }
    }
}

impl TryFrom<String> for Identity {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        if text == "unidentified" {
            return Ok(Self::Unidentified);
        }
        let unix = text
            .strip_prefix("unix:")
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(device, inode)| Some((device.parse().ok()?, inode.parse().ok()?)));
        match unix {
            Some((device, inode)) => Ok(Self::Unix { device, inode }),
            None => Err(format!(
                "{text:?} is not a file identity: `unix:<device>:<inode>` or `unidentified`"
            )),
        }
    }
}

/// What admission means to a spool's receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admits {
    /// A receiver that does not advertise it: the message is admitted once its
    /// courier renames the offer, and is its receiver's from then on.
    OnRename,
    /// A receiver that does: the message is admitted once the receiver says its
    /// inbox has queued it, and can be withdrawn until a take claims it.
    OnAcknowledgement,
    /// A receiver that does, on a clock this sender cannot verify it shares:
    /// the message is admitted once the receiver says its inbox has queued it,
    /// and is its receiver's from then on.
    AcknowledgedThenTaken,
}

/// How a sender's attempt to take its message back went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withdrawal {
    /// It holds the claim: the message is never taken.
    Withdrawn,
    /// The receiver has it, or has answered it.
    Kept,
    /// The file system would not create the claim, which decides nothing.
    Undecided,
}

/// Whether a sender in `wait_until` has published its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Published {
    /// Not yet: the file system refused it so far.
    Nothing,
    /// It is there, for a release or a take to read.
    Deadline,
}

/// What a receiver found of a message's deadline as it released or took it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expiring {
    /// No deadline published, or one not yet passed.
    NotDue,
    /// Past it: the message is dropped, and its sender told.
    Expired,
    /// Past it, and the file system refused to tell its sender.
    Refused,
}

/// Whether the receiver holds a message's claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holding {
    /// It does not: the message is untaken, or was released.
    Unclaimed,
    /// It created `<id>.claimed`, and has not removed it.
    Claimed,
}

/// One message's id in a spool: what every file of it is named by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MessageId(String);

impl MessageId {
    /// The message's file in `dir` with `suffix`: `<id><suffix>`.
    fn file(&self, dir: &Path, suffix: &str) -> PathBuf {
        dir.join(format!("{}{suffix}", self.0))
    }
}

/// How one side's attempt on a message's claim went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Contest {
    /// This side created the claim, and has the message.
    Won,
    /// The other side created it first.
    Lost,
    /// The file system would not create it, which decides nothing: neither
    /// side has the message until a claim can be made.
    Refused,
}

/// Try to create the claim at `path`.
fn claim(path: &Path) -> Contest {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => Contest::Won,
        Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => Contest::Lost,
        Err(_) => Contest::Refused,
    }
}

/// Remove what is left of message `id` once its sender withdrew it.
fn discard(dir: &Path, id: &MessageId) {
    for suffix in [TAKEN, ADMITTED, CLAIMED, DEADLINE, EXPIRED] {
        let _ = fs::remove_file(id.file(dir, suffix));
    }
}

/// Take back offer `id`, if the courier has not taken it first.
fn withdraw(dir: &Path, id: &MessageId) -> bool {
    let withdrawn = id.file(dir, WITHDRAWN);
    if fs::rename(id.file(dir, OFFER), &withdrawn).is_err() {
        return false;
    }
    let _ = fs::remove_file(withdrawn);
    true
}

/// The courier: take each offer into the inbox until the inbox closes or the
/// spool is dropped, holding the advertisement's lock and the receiver's lock
/// throughout, and letting them go in that order.
fn serve<M, D>(dir: &Path, shared: &Arc<Shared<M, D>>, stop: &AtomicBool, locks: [File; 2])
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
    // Removed while the locks are still held, so it is never a later
    // receiver's.
    let _ = fs::remove_file(dir.join(ADMISSION));
    let [admission_lock, lock] = locks;
    drop(admission_lock);
    drop(lock);
}

/// The ids of every offer waiting, in the order of the instants they were
/// minted at.
fn offered(dir: &Path) -> Vec<MessageId> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<MessageId> = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(OFFER))
                .map(|id| MessageId(id.to_owned()))
        })
        .collect();
    ids.sort();
    ids
}

fn take_offer<M, D>(dir: &Path, id: &MessageId, shared: &Arc<Shared<M, D>>)
where
    M: Message + Send + 'static,
    D: Disposition,
{
    let taken = id.file(dir, TAKEN);
    if fs::rename(id.file(dir, OFFER), &taken).is_err() {
        // Withdrawn by its sender first.
        return;
    }
    // A sender that withdrew it as the courier took it holds its claim, so
    // the take — or the answer — that follows finds the message withdrawn.
    let hold = Hold {
        dir: dir.to_path_buf(),
        id: id.clone(),
        held: Arc::new(Mutex::new(Holding::Unclaimed)),
    };
    let message = match read_offer::<M>(dir, &taken) {
        Ok(message) => message,
        Err(reason) => {
            hold.settle(&AnswerBody::Refused { reason });
            return;
        }
    };
    let replying = hold.clone();
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
            replying.settle(&body);
        }),
        Some(Box::new(hold)),
    );
}

/// The receiver's side of one message carried in from the spool: what claims
/// it as it is taken, and writes what became of it.
#[derive(Clone)]
struct Hold {
    dir: PathBuf,
    id: MessageId,
    /// Whether the receiver holds the message's claim.
    held: Arc<Mutex<Holding>>,
}

impl Hold {
    fn file(&self, suffix: &str) -> PathBuf {
        self.id.file(&self.dir, suffix)
    }

    fn holding(&self) -> Holding {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn hold(&self, holding: Holding) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = holding;
    }

    /// Whether the deadline its sender published has passed, read here and
    /// now on the monotonic clock the two share, so it needs nothing of the
    /// sender; and if so, tell the sender the message is dropped. A deadline
    /// this build cannot read, or one on a clock this receiver does not read,
    /// is no deadline: the sender's own withdrawal at it still decides.
    fn expire(&self) -> Expiring {
        let due = match (
            read_document::<DeadlineDocument>(&self.dir, &self.file(DEADLINE)),
            SharedClock::here(),
        ) {
            (Ok(Some(published)), Some(clock)) => {
                clock.is(&published.clock) && clock.now() >= published.deadline
            }
            _ => false,
        };
        if !due {
            return Expiring::NotDue;
        }
        match claim(&self.file(EXPIRED)) {
            Contest::Won | Contest::Lost => Expiring::Expired,
            Contest::Refused => Expiring::Refused,
        }
    }

    /// Write what became of the message, if its sender is still waiting for
    /// it: claimed first, so a sender that withdrew it is never answered, and
    /// let go of only once the answer is written. One that cannot be claimed is
    /// dropped unanswered, so its sender is told it was lost.
    fn settle(&self, body: &AnswerBody) {
        if self.holding() == Holding::Unclaimed {
            match claim(&self.file(CLAIMED)) {
                Contest::Won => {}
                Contest::Lost | Contest::Refused => {
                    discard(&self.dir, &self.id);
                    return;
                }
            }
        }
        let taken = self.file(TAKEN);
        if taken.exists() {
            let _ = write_document(
                &self.file(ANSWER),
                &AnswerDocument {
                    schema_version: SPOOL_SCHEMA_VERSION,
                    answer: clone_body(body),
                },
            );
        }
        // Without its taken file, its sender stopped waiting: nobody reads an
        // answer written now.
        discard(&self.dir, &self.id);
    }
}

impl Gate for Hold {
    fn admitted(&mut self) -> Acknowledged {
        let written = write_document(
            &self.file(ADMITTED),
            &Admitted {
                schema_version: SPOOL_SCHEMA_VERSION,
            },
        );
        match written {
            Ok(()) => Acknowledged::Said,
            // Never queued, so no take finds a message its sender cannot
            // learn was admitted: it is refused, and its sender told why.
            Err(failure) => {
                self.settle(&AnswerBody::Refused {
                    reason: format!("its admission could not be acknowledged: {failure}"),
                });
                Acknowledged::Unsaid
            }
        }
    }

    fn claim(&mut self) -> Claimed {
        // Held already when it was released with a claim the file system would
        // not let go of: it is still the receiver's, and needs no new one.
        if self.holding() == Holding::Unclaimed {
            match claim(&self.file(CLAIMED)) {
                Contest::Won => self.hold(Holding::Claimed),
                Contest::Lost => {
                    discard(&self.dir, &self.id);
                    return Claimed::Withdrawn;
                }
                Contest::Refused => return Claimed::Undecided,
            }
        }
        // Claimed, and its sender past its deadline — untaken there, or after
        // a release that could not expire it: dropped as a release would,
        // keeping the claim, so its sender reads `<id>.expired` and clears what
        // is left.
        match self.expire() {
            Expiring::NotDue => Claimed::Taken,
            Expiring::Expired => Claimed::Withdrawn,
            Expiring::Refused => Claimed::Undecided,
        }
    }

    fn release(&mut self) -> Released {
        // Decided here, under the inbox's lock, from the deadline its sender
        // published, so the sender need not run for the release to expire it.
        match self.expire() {
            // The claim stays, so nothing takes the message, and `<id>.expired`
            // tells its sender it is dropped.
            Expiring::Expired => return Released::Expired,
            // Its sender is past its deadline and the file system refused the
            // word: the claim stays the receiver's, and the next take tries
            // again rather than taking it.
            Expiring::Refused => return Released::Untaken,
            Expiring::NotDue => {}
        }
        // Let go of only once its claim is gone: one the file system keeps is
        // still the receiver's, and its sender reads the message as taken.
        if fs::remove_file(self.file(CLAIMED)).is_ok() {
            self.hold(Holding::Unclaimed);
        }
        Released::Untaken
    }
}

/// The message in a taken offer, or why the receiver cannot read it.
fn read_offer<M: Message>(dir: &Path, path: &Path) -> Result<M, String> {
    let offer = read_document::<OfferDocument>(dir, path)
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
        if fs::rename(id.file(dir, OFFER), id.file(dir, TAKEN)).is_ok() {
            Hold {
                dir: dir.to_path_buf(),
                id,
                held: Arc::new(Mutex::new(Holding::Unclaimed)),
            }
            .settle(&AnswerBody::Closed(closed.clone()));
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
fn mint() -> MessageId {
    static MINTED: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    MessageId(format!(
        "{now:039}-{}-{:020}",
        std::process::id(),
        MINTED.fetch_add(1, Ordering::Relaxed)
    ))
}
