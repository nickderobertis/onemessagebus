//! The inbox: a typed channel into a running process, whose sender learns what
//! the receiver did with the message.
//!
//! A [`Sender`] hands one message to an [`Inbox`] and blocks until the receiver
//! has taken it and answered it with a [`Disposition`] — the consumer's own type
//! — or until the inbox is closed. [`Sender::offer`] splits that in two: it
//! returns an [`Offer`] once the message is admitted to the receiver's queue,
//! and the offer waits for the answer, waits up to a deadline, or withdraws the
//! message — and taking a message and withdrawing it never both succeed. What carries the message is an
//! [`InboxBackend`]: [`InProcess`] within one process, [`Spool`](crate::Spool)
//! across processes through a directory the receiver binds, and
//! [`Carry`](crate::Carry) into a durable store for a receiver that is not
//! running. The receiving end is an [`Inbox`] whichever backend it is, so a
//! consumer writes against the pair and chooses the backend where it wires them.
//!
//! The promise is narrow on purpose: a message reaches [`Inbox::take`] or its
//! sender learns why not, and a disposition reaches exactly the sender that
//! asked. Nothing here answers on a receiver's behalf — there is no timeout that
//! makes up a disposition — and where a taken message goes next is the
//! consumer's to decide. `docs/inbox.md` states the contract.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::schema::Message;

/// What a receiver may answer a delivered message with.
///
/// The consumer's own type: the core requires only that it serializes, so a
/// disposition crosses a spool or a socket as readily as a thread.
pub trait Disposition: Serialize + DeserializeOwned + Send + 'static {}

/// A disposition with an answer for a message carried to a receiver that is not
/// running: what [`Carry`](crate::Carry) answers its sender, since nothing has
/// read the message yet.
pub trait Carried: Disposition {
    /// The disposition a carried message's sender is answered with.
    fn carried() -> Self;
}

/// Why an inbox takes no more messages: the closer's words, carried to every
/// sender verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("the inbox is closed: {reason}")]
pub struct Closed {
    /// What the closer said.
    pub reason: String,
}

impl Closed {
    /// A close carrying `reason`.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Why a sender was not answered with a disposition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Undelivered {
    /// The receiver closed the inbox, before or after the message arrived.
    #[error(transparent)]
    Closed(Closed),
    /// The backend cannot produce the receiver's answer, and says why.
    #[error(transparent)]
    Backend(BackendError),
}

impl From<Closed> for Undelivered {
    fn from(closed: Closed) -> Self {
        Self::Closed(closed)
    }
}

impl From<BackendError> for Undelivered {
    fn from(failure: BackendError) -> Self {
        Self::Backend(failure)
    }
}

/// Why a wait bounded by a deadline ended without the receiver's answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Unanswered {
    /// The message was still untaken at the deadline, and was withdrawn: the
    /// receiver never takes it.
    #[error("the message was not taken by its deadline, and was withdrawn")]
    Expired,
    /// The receiver took the message, and no answer will come.
    #[error(transparent)]
    Undelivered(Undelivered),
}

impl From<Undelivered> for Unanswered {
    fn from(undelivered: Undelivered) -> Self {
        Self::Undelivered(undelivered)
    }
}

/// What a backend could not do, naming the path it could not do it at.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BackendError {
    /// The path is not the backend a caller named.
    #[error("{} is not {what}: {why}", path.display())]
    Absent {
        /// What the path was expected to be: `a spool`, `a carry store`.
        what: &'static str,
        /// The path.
        path: PathBuf,
        /// What is there instead.
        why: String,
    },
    /// Another receiver holds the spool.
    #[error(
        "{} is already bound by another receiver; a spool has one receiver at a time",
        path.display()
    )]
    Bound {
        /// The spool.
        path: PathBuf,
    },
    /// A document the backend reads is not one this build wrote.
    #[error("{} is not a document this build reads: {why}", file.display())]
    Unreadable {
        /// The spool or store the document belongs to.
        at: PathBuf,
        /// The document.
        file: PathBuf,
        /// What is wrong with it.
        why: String,
    },
    /// Nothing took the message within the spool's bounded wait, so it was
    /// withdrawn: no receiver waking later delivers it.
    #[error(
        "nothing took the message offered to the spool at {} within {}; it was withdrawn",
        spool.display(),
        wait_text(*waited)
    )]
    Elapsed {
        /// The spool.
        spool: PathBuf,
        /// The bounded wait that passed.
        waited: Duration,
    },
    /// The receiver took the message and is no longer bound to the spool, and it
    /// answered nothing before it went.
    #[error(
        "the receiver of the spool at {} took the message ({}) and is no longer bound, with no answer written",
        spool.display(),
        file.display()
    )]
    Abandoned {
        /// The spool.
        spool: PathBuf,
        /// The message's file in it.
        file: PathBuf,
    },
    /// The receiver refused the offered document.
    #[error("the receiver of the spool at {} refused the message: {why}", spool.display())]
    Refused {
        /// The spool.
        spool: PathBuf,
        /// What the receiver said.
        why: String,
    },
    /// The file system refused.
    #[error("cannot {action} {}: {why}", path.display())]
    Io {
        /// What was being done.
        action: &'static str,
        /// Where.
        path: PathBuf,
        /// What the system said.
        why: String,
    },
    /// A value did not serialize.
    #[error("the {what} does not serialize: {why}")]
    Encoding {
        /// What did not: `message`, `disposition`.
        what: &'static str,
        /// What serde said.
        why: String,
    },
}

impl BackendError {
    pub(crate) fn io(
        action: &'static str,
        path: &std::path::Path,
        failure: &std::io::Error,
    ) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            why: failure.to_string(),
        }
    }
}

/// A wait as a person reads it: whole seconds when it is whole, milliseconds
/// otherwise.
fn wait_text(wait: Duration) -> String {
    if wait.subsec_nanos() == 0 {
        format!("{}s", wait.as_secs())
    } else {
        format!("{}ms", wait.as_millis())
    }
}

/// One message's answer, as it travels back.
pub(crate) type Answer<D> = Result<D, Closed>;

/// The reason a sender is given when its message was taken and then dropped
/// without an answer — the one way a receiver could otherwise leave it waiting
/// on an inbox that is still open.
const LET_GO: &str = "the receiver took the message and let it go without answering";

/// The reason an inbox dropped without being closed gives its senders.
const DROPPED: &str = "the inbox was dropped without being closed";

/// Where one message's answer goes: back to a sender blocked in `send`, into a
/// spool's answer document, or nowhere for a message adopted from a carry store.
///
/// Dropped unanswered, it answers [`LET_GO`], so no path that loses a message
/// leaves its sender blocked.
pub(crate) struct Reply<D> {
    deliver: Option<Box<dyn FnOnce(Answer<D>) + Send>>,
    /// What tells its sender a taken message was handed back untaken.
    released: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl<D> Reply<D> {
    pub(crate) fn to(deliver: impl FnOnce(Answer<D>) + Send + 'static) -> Self {
        Self {
            deliver: Some(Box::new(deliver)),
            released: None,
        }
    }

    /// A reply with no sender behind it.
    pub(crate) fn nobody() -> Self {
        Self {
            deliver: None,
            released: None,
        }
    }

    /// The same reply, which also runs `released` each time its message is
    /// handed back untaken.
    fn on_release(mut self, released: impl Fn() + Send + Sync + 'static) -> Self {
        self.released = Some(Arc::new(released));
        self
    }

    fn answer(mut self, answer: Answer<D>) {
        if let Some(deliver) = self.deliver.take() {
            deliver(answer);
        }
    }

    /// Drop the reply without answering: its message was withdrawn, and its
    /// sender already knows.
    fn forget(mut self) {
        self.deliver = None;
    }
}

/// What decides, for a message carried in from elsewhere, whether the receiver
/// or its sender has it: the in-process queue's lock settles a message sent in
/// this process, and a gate settles one whose sender can withdraw it from
/// outside.
pub(crate) trait Gate: Send {
    /// Say the message is queued: run as it joins the queue, before any take.
    fn admitted(&mut self);
    /// Claim the message for the receiver as it is taken: `false` when its
    /// sender withdrew it first, which leaves the message to be dropped.
    fn claim(&mut self) -> bool;
    /// Give a claimed message back, so its sender can withdraw it again.
    fn release(&mut self);
}

impl<D> Drop for Reply<D> {
    fn drop(&mut self) {
        if let Some(deliver) = self.deliver.take() {
            deliver(Err(Closed::new(LET_GO)));
        }
    }
}

/// What a backend does when its inbox closes.
type CloseHook = Box<dyn FnOnce(&Closed) + Send>;

struct Queued<M, D> {
    id: u64,
    message: M,
    reply: Reply<D>,
    gate: Option<Box<dyn Gate>>,
}

/// A taken message's way back: to its sender, and through its gate.
struct Taken<D> {
    reply: Reply<D>,
    gate: Option<Box<dyn Gate>>,
}

struct State<M, D> {
    closed: Option<Closed>,
    /// Waiting to be taken, in id order: the order they arrived.
    queue: VecDeque<Queued<M, D>>,
    /// Taken and not yet answered, by id: what a close answers.
    taken: HashMap<u64, Taken<D>>,
    answered: Vec<Answered<M, D>>,
    /// What a backend does when the inbox closes, run once, in order.
    on_close: Vec<CloseHook>,
    next: u64,
}

/// The state one inbox's backends share.
pub(crate) struct Shared<M, D> {
    state: Mutex<State<M, D>>,
    arrived: Condvar,
}

impl<M, D> Shared<M, D> {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                closed: None,
                queue: VecDeque::new(),
                taken: HashMap::new(),
                answered: Vec::new(),
                on_close: Vec::new(),
                next: 0,
            }),
            arrived: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<M, D>> {
        // A panicking receiver still has to answer every waiting sender, so a
        // poisoned lock is read through rather than propagated.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue `message` for the receiver, answering its id, or answer `reply`
    /// with the close that refuses it.
    pub(crate) fn offer(
        &self,
        message: M,
        reply: Reply<D>,
        gate: Option<Box<dyn Gate>>,
    ) -> Result<u64, Closed> {
        let mut state = self.lock();
        if let Some(closed) = state.closed.clone() {
            drop(state);
            reply.answer(Err(closed.clone()));
            return Err(closed);
        }
        let id = state.next;
        state.next += 1;
        let mut gate = gate;
        if let Some(gate) = gate.as_mut() {
            // Under the lock, so no take can come before the word that the
            // message is queued.
            gate.admitted();
        }
        state.queue.push_back(Queued {
            id,
            message,
            reply,
            gate,
        });
        drop(state);
        self.arrived.notify_all();
        Ok(id)
    }

    /// Take queued message `id` back, if the receiver has not taken it: under
    /// the lock every take holds, so the two never both succeed.
    fn withdraw(&self, id: u64) -> bool {
        let mut state = self.lock();
        let Some(at) = state.queue.iter().position(|queued| queued.id == id) else {
            return false;
        };
        let withdrawn = state.queue.remove(at);
        drop(state);
        if let Some(withdrawn) = withdrawn {
            withdrawn.reply.forget();
        }
        true
    }

    /// Queue every one of `messages` at once, after `commit` succeeds, or none of
    /// them: the inbox's lock is held across both, so a close cannot fall between
    /// a store giving its messages up and the inbox taking them.
    pub(crate) fn offer_all<E>(
        &self,
        messages: Vec<M>,
        commit: impl FnOnce() -> Result<(), E>,
    ) -> Result<usize, Result<Closed, E>> {
        let mut state = self.lock();
        if let Some(closed) = state.closed.clone() {
            return Err(Ok(closed));
        }
        commit().map_err(Err)?;
        let count = messages.len();
        for message in messages {
            let id = state.next;
            state.next += 1;
            state.queue.push_back(Queued {
                id,
                message,
                reply: Reply::nobody(),
                gate: None,
            });
        }
        drop(state);
        self.arrived.notify_all();
        Ok(count)
    }

    pub(crate) fn closed(&self) -> Option<Closed> {
        self.lock().closed.clone()
    }

    /// Run `hook` when the inbox closes, or now if it already has.
    pub(crate) fn on_close(&self, hook: impl FnOnce(&Closed) + Send + 'static) {
        let mut state = self.lock();
        match state.closed.clone() {
            Some(closed) => {
                drop(state);
                hook(&closed);
            }
            None => state.on_close.push(Box::new(hook)),
        }
    }

    fn close(&self, closed: Closed) {
        let mut state = self.lock();
        if state.closed.is_some() {
            // The first close's reason is the one every sender is told.
            return;
        }
        state.closed = Some(closed.clone());
        let hooks = std::mem::take(&mut state.on_close);
        let mut replies: Vec<Reply<D>> = state.queue.drain(..).map(|queued| queued.reply).collect();
        replies.extend(state.taken.drain().map(|(_, taken)| taken.reply));
        drop(state);
        self.arrived.notify_all();
        for hook in hooks {
            hook(&closed);
        }
        for reply in replies {
            reply.answer(Err(closed.clone()));
        }
    }
}

/// One message and the disposition its receiver answered it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered<M, D> {
    /// The message.
    pub message: M,
    /// What the receiver answered.
    pub disposition: D,
}

/// The receiving end: where every backend's messages arrive, in the order they
/// arrived.
///
/// Dropping it closes it, so a receiver that goes away for any reason — a clean
/// end, an error, a panic — answers every sender still waiting.
pub struct Inbox<M: Message, D: Disposition> {
    shared: Arc<Shared<M, D>>,
}

impl<M: Message + Send + 'static, D: Disposition> Default for Inbox<M, D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Message, D: Disposition> fmt::Debug for Inbox<M, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.shared.lock();
        f.debug_struct("Inbox")
            .field("closed", &state.closed)
            .field("queued", &state.queue.len())
            .field("taken", &state.taken.len())
            .field("answered", &state.answered.len())
            .finish()
    }
}

impl<M: Message + Send + 'static, D: Disposition> Inbox<M, D> {
    /// An open inbox with nothing in it.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared::new()),
        }
    }

    /// An in-process sender into this inbox.
    #[must_use]
    pub fn sender(&self) -> Sender<M, D> {
        Sender::over(InProcess {
            shared: Arc::clone(&self.shared),
        })
    }

    /// The next delivered message, if one is waiting; non-blocking.
    #[must_use]
    pub fn take(&self) -> Option<Delivered<M, D>> {
        let mut state = self.shared.lock();
        self.pop(&mut state)
    }

    /// Blocks up to `timeout` for the next delivered message. `None` when none
    /// arrived in time, or the inbox is closed.
    #[must_use]
    pub fn take_within(&self, timeout: Duration) -> Option<Delivered<M, D>> {
        let until = Instant::now().checked_add(timeout);
        let mut state = self.shared.lock();
        loop {
            if let Some(delivered) = self.pop(&mut state) {
                return Some(delivered);
            }
            if state.closed.is_some() {
                return None;
            }
            state = match until {
                Some(until) => {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return None;
                    }
                    self.shared
                        .arrived
                        .wait_timeout(state, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .shared
                    .arrived
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    fn pop(&self, state: &mut State<M, D>) -> Option<Delivered<M, D>> {
        let (id, message) = loop {
            let Queued {
                id,
                message,
                reply,
                mut gate,
            } = state.queue.pop_front()?;
            if let Some(gate) = gate.as_mut() {
                if !gate.claim() {
                    // Its sender withdrew it first: it is nobody's to take.
                    reply.forget();
                    continue;
                }
            }
            state.taken.insert(id, Taken { reply, gate });
            break (id, message);
        };
        Some(Delivered {
            message,
            claim: Claim {
                id,
                shared: Arc::clone(&self.shared),
                settled: false,
            },
        })
    }

    pub(crate) fn shared(&self) -> Arc<Shared<M, D>> {
        Arc::clone(&self.shared)
    }
}

impl<M: Message, D: Disposition> Inbox<M, D> {
    /// Close: every blocked sender, and every later one, gets
    /// [`Undelivered::Closed`] with `reason`. A second close changes nothing —
    /// the first reason is the one every sender is told.
    pub fn close(&self, reason: Closed) {
        self.shared.close(reason);
    }

    /// The close in force, if the inbox is closed.
    #[must_use]
    pub fn closed(&self) -> Option<Closed> {
        self.shared.closed()
    }
}

impl<M: Message + Clone, D: Disposition + Clone> Inbox<M, D> {
    /// Every message answered so far, with its disposition, oldest first.
    #[must_use]
    pub fn answered(&self) -> Vec<Answered<M, D>> {
        self.shared.lock().answered.clone()
    }
}

impl<M: Message, D: Disposition> Drop for Inbox<M, D> {
    fn drop(&mut self) {
        self.shared.close(Closed::new(DROPPED));
    }
}

/// One message a receiver has taken, and the way back to its sender.
pub struct Delivered<M: Message, D: Disposition> {
    message: M,
    claim: Claim<M, D>,
}

/// A taken message's hold on its sender: dropped unsettled, it lets the message
/// go, which answers the sender rather than leaving it blocked.
struct Claim<M, D> {
    id: u64,
    shared: Arc<Shared<M, D>>,
    settled: bool,
}

impl<M, D> Drop for Claim<M, D> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // Removed under the lock and dropped after it, since dropping a reply
        // answers its sender.
        let taken = self.shared.lock().taken.remove(&self.id);
        drop(taken);
    }
}

impl<M: Message, D: Disposition> fmt::Debug for Delivered<M, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Delivered")
            .field("id", &self.claim.id)
            .finish_non_exhaustive()
    }
}

impl<M: Message, D: Disposition> Delivered<M, D> {
    /// The message.
    #[must_use]
    pub fn message(&self) -> &M {
        &self.message
    }

    /// Hand the message back untaken, for a receiver that took it and cannot
    /// present it: it returns to the inbox's queue at the position it arrived
    /// in, ahead of every message that arrived after it. To its sender it is
    /// untaken again — an [`Offer`] can withdraw it, and one whose deadline has
    /// passed expires it at once. A message released into a closed inbox is
    /// gone, and its sender was already told the close.
    pub fn release(self) {
        let Self { message, mut claim } = self;
        claim.settled = true;
        let released = {
            let mut state = claim.shared.lock();
            let Some(Taken { reply, mut gate }) = state.taken.remove(&claim.id) else {
                return;
            };
            if let Some(gate) = gate.as_mut() {
                gate.release();
            }
            let released = reply.released.clone();
            let at = state.queue.partition_point(|queued| queued.id < claim.id);
            state.queue.insert(
                at,
                Queued {
                    id: claim.id,
                    message,
                    reply,
                    gate,
                },
            );
            released
        };
        claim.shared.arrived.notify_all();
        if let Some(released) = released {
            released();
        }
    }
}

impl<M: Message, D: Disposition + Clone> Delivered<M, D> {
    /// Hands the disposition back to the sender blocked in `send`, and records
    /// the answer in [`Inbox::answered`]. A sender the inbox's close already
    /// answered is not answered twice.
    pub fn answer(self, disposition: D) {
        let Self { message, mut claim } = self;
        claim.settled = true;
        let reply = {
            let mut state = claim.shared.lock();
            state.answered.push(Answered {
                message,
                disposition: disposition.clone(),
            });
            state.taken.remove(&claim.id)
        };
        if let Some(taken) = reply {
            taken.reply.answer(Ok(disposition));
        }
    }
}

/// What carries a message from a [`Sender`] to an [`Inbox`].
pub trait InboxBackend<M, D>: Send + Sync {
    /// Hand `message` over and block until its receiver answers it, or the
    /// backend knows it never will.
    ///
    /// # Errors
    ///
    /// [`Undelivered`], naming why no disposition came back.
    fn send(&self, message: M) -> Result<D, Undelivered>;

    /// Hand `message` over and return once its receiver has admitted it.
    ///
    /// Provided for a backend that cannot report admission on its own: it
    /// sends the message and holds the answer, so the offer returns only once
    /// the message was admitted — and answered — and its
    /// [`withdraw`](Offer::withdraw) always reads the message as taken, since
    /// this backend could not guarantee a withdrawal.
    ///
    /// # Errors
    ///
    /// [`Undelivered`], as [`send`](InboxBackend::send).
    fn offer(&self, message: M) -> Result<Offer<D>, Undelivered>
    where
        D: Disposition,
    {
        self.send(message).map(Offer::answered)
    }
}

/// One offered message, admitted to its receiver's queue: what waits for its
/// answer, or takes it back.
///
/// Taking it and withdrawing it never both succeed and never both fail: either
/// the receiver took it, and an answer or the reason there is none follows, or
/// it was withdrawn, and the receiver never takes it. Dropping an offer
/// withdraws nothing; the message stays offered and is answered to nobody.
pub struct Offer<D: Disposition> {
    pending: Box<dyn Pending<D>>,
}

/// What one backend does for an admitted message's [`Offer`].
pub(crate) trait Pending<D>: Send {
    /// Withdraw the message if its receiver has not taken it: `true` when it
    /// was withdrawn, and the receiver never takes it.
    fn withdraw(&mut self) -> bool;
    /// Block until the receiver answers, or the backend knows it never will.
    fn wait(&mut self) -> Result<D, Undelivered>;
    /// As `wait`, but withdraw the message if it is untaken at `deadline`.
    fn wait_until(&mut self, deadline: Instant) -> Result<D, Unanswered>;
}

/// An offer whose answer is already known: what a backend that cannot report
/// admission on its own answers.
struct Settled<D> {
    answer: Option<Result<D, Undelivered>>,
}

impl<D: Send> Pending<D> for Settled<D> {
    fn withdraw(&mut self) -> bool {
        false
    }

    fn wait(&mut self) -> Result<D, Undelivered> {
        self.answer.take().unwrap_or_else(|| {
            Err(Undelivered::Closed(Closed::new(
                "the offer's answer was already read",
            )))
        })
    }

    fn wait_until(&mut self, _deadline: Instant) -> Result<D, Unanswered> {
        self.wait().map_err(Unanswered::Undelivered)
    }
}

impl<D: Disposition> fmt::Debug for Offer<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Offer").finish_non_exhaustive()
    }
}

impl<D: Disposition> Offer<D> {
    pub(crate) fn over(pending: impl Pending<D> + 'static) -> Self {
        Self {
            pending: Box::new(pending),
        }
    }

    /// An offer already answered `disposition`.
    fn answered(disposition: D) -> Self {
        Self::over(Settled {
            answer: Some(Ok(disposition)),
        })
    }

    /// Block until the receiver has answered the message, or the inbox is
    /// closed, as [`Sender::send`] does after admission.
    ///
    /// # Errors
    ///
    /// [`Undelivered::Closed`] with the closer's reason, including a receiver
    /// that let the message go unanswered; [`Undelivered::Backend`] when the
    /// backend cannot produce the answer.
    pub fn wait(mut self) -> Result<D, Undelivered> {
        self.pending.wait()
    }

    /// Wait for the answer, withdrawing the message if it is still untaken at
    /// `deadline`. A message taken before the deadline is waited on past it,
    /// for as long as its receiver holds it, and its answer returned however
    /// late it comes; one handed back with [`Delivered::release`] after the
    /// deadline is withdrawn at once.
    ///
    /// # Errors
    ///
    /// [`Unanswered::Expired`] when the message was withdrawn at the deadline
    /// — it was never taken, and never will be — and
    /// [`Unanswered::Undelivered`] for every outcome [`wait`](Self::wait)
    /// reports.
    pub fn wait_until(mut self, deadline: Instant) -> Result<D, Unanswered> {
        self.pending.wait_until(deadline)
    }

    /// Take the message back: `Ok` only when the receiver had not taken it,
    /// and then it never will. A message already taken is answered `Err` with
    /// this offer, still able to [`wait`](Self::wait) for its answer.
    ///
    /// # Errors
    ///
    /// The offer itself, when the receiver took the message first — or when
    /// the backend cannot guarantee a withdrawal, which reads as taken.
    pub fn withdraw(mut self) -> Result<(), Self> {
        if self.pending.withdraw() {
            Ok(())
        } else {
            Err(self)
        }
    }
}

/// The sending end; `Clone`, and usable from any thread.
pub struct Sender<M: Message, D: Disposition> {
    backend: Arc<dyn InboxBackend<M, D>>,
}

impl<M: Message, D: Disposition> Clone for Sender<M, D> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
        }
    }
}

impl<M: Message, D: Disposition> fmt::Debug for Sender<M, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender").finish_non_exhaustive()
    }
}

impl<M: Message, D: Disposition> Sender<M, D> {
    /// A sender over `backend`.
    #[must_use]
    pub fn over(backend: impl InboxBackend<M, D> + 'static) -> Self {
        Self {
            backend: Arc::new(backend),
        }
    }

    /// Blocks until the receiver has taken the message and answered it, or the
    /// inbox is closed. Never returns a disposition the receiver did not answer.
    ///
    /// # Errors
    ///
    /// [`Undelivered::Closed`] with the closer's reason when the inbox is closed
    /// before or after the message arrived; [`Undelivered::Backend`] when the
    /// backend cannot produce the answer.
    pub fn send(&self, message: M) -> Result<D, Undelivered> {
        self.backend.send(message)
    }

    /// Hands the message over and returns once the receiver has admitted it to
    /// its inbox's queue — at once in process, and over a spool once the
    /// receiver's courier has queued it and said so. A message not admitted
    /// within the backend's bounded wait is withdrawn first, as
    /// [`send`](Self::send) withdraws it.
    ///
    /// # Errors
    ///
    /// As [`send`](Self::send), for a message that was never admitted.
    pub fn offer(&self, message: M) -> Result<Offer<D>, Undelivered> {
        self.backend.offer(message)
    }
}

impl<M: Message + Send + 'static, D: Disposition> Sender<M, D> {
    /// An in-process pair: a sender and the inbox it sends into.
    #[must_use]
    pub fn channel() -> (Self, Inbox<M, D>) {
        let inbox = Inbox::new();
        (inbox.sender(), inbox)
    }
}

/// The in-process backend: a sender and its inbox in one process, meeting in
/// memory.
pub struct InProcess<M: Message, D: Disposition> {
    shared: Arc<Shared<M, D>>,
}

impl<M: Message + Send + 'static, D: Disposition> InboxBackend<M, D> for InProcess<M, D> {
    fn send(&self, message: M) -> Result<D, Undelivered> {
        self.offer(message)?.wait()
    }

    fn offer(&self, message: M) -> Result<Offer<D>, Undelivered> {
        let ticket: Arc<Ticket<D>> = Arc::new(Ticket {
            slot: Mutex::new(Slot {
                answer: None,
                releases: 0,
            }),
            changed: Condvar::new(),
        });
        let answered = Arc::clone(&ticket);
        let released = Arc::clone(&ticket);
        let reply = Reply::to(move |answer| {
            answered.lock().answer = Some(answer);
            answered.changed.notify_all();
        })
        .on_release(move || {
            released.lock().releases += 1;
            released.changed.notify_all();
        });
        let id = self.shared.offer(message, reply, None)?;
        Ok(Offer::over(Queuing {
            shared: Arc::clone(&self.shared),
            id,
            ticket,
        }))
    }
}

/// Where an in-process message's answer lands, and how often it was handed
/// back.
struct Ticket<D> {
    slot: Mutex<Slot<D>>,
    changed: Condvar,
}

struct Slot<D> {
    answer: Option<Answer<D>>,
    /// How many times the message was released: what a sender past its
    /// deadline watches for, to withdraw it the moment it is untaken again.
    releases: u64,
}

impl<D> Ticket<D> {
    fn lock(&self) -> MutexGuard<'_, Slot<D>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An in-process message's offer: its place in the queue, and its ticket.
struct Queuing<M, D> {
    shared: Arc<Shared<M, D>>,
    id: u64,
    ticket: Arc<Ticket<D>>,
}

impl<M: Send, D: Send> Pending<D> for Queuing<M, D> {
    fn withdraw(&mut self) -> bool {
        self.shared.withdraw(self.id)
    }

    fn wait(&mut self) -> Result<D, Undelivered> {
        let mut slot = self.ticket.lock();
        loop {
            if let Some(answer) = slot.answer.take() {
                return answer.map_err(Undelivered::Closed);
            }
            slot = self
                .ticket
                .changed
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn wait_until(&mut self, deadline: Instant) -> Result<D, Unanswered> {
        let mut slot = self.ticket.lock();
        loop {
            if let Some(answer) = slot.answer.take() {
                return answer.map_err(|closed| Unanswered::Undelivered(closed.into()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            slot = self
                .ticket
                .changed
                .wait_timeout(slot, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        // Past the deadline: withdraw it if it is untaken, and otherwise wait
        // for its answer — or for a release, which makes it untaken again.
        loop {
            let releases = slot.releases;
            drop(slot);
            if self.shared.withdraw(self.id) {
                return Err(Unanswered::Expired);
            }
            slot = self.ticket.lock();
            loop {
                if let Some(answer) = slot.answer.take() {
                    return answer.map_err(|closed| Unanswered::Undelivered(closed.into()));
                }
                if slot.releases != releases {
                    break;
                }
                slot = self
                    .ticket
                    .changed
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}
