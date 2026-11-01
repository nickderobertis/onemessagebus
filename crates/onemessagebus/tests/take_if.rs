//! Claiming only what a receiver can present: `Inbox::take_if` and
//! `Inbox::take_if_within`, driven through the public API over both backends
//! that can withdraw a queued message — the in-process pair, and a spool bound
//! in a real directory with a sender connected to it by its address.
//!
//! A receiver here presents only urgent notes (`interrupt-…`) and leaves every
//! note that waits for its turn to end (`turn-end-…`) queued. Each journey is
//! shown on both backends: the first accepted note is claimed past the rejected
//! ones ahead of it, every skipped note keeps its place and stays withdrawable by
//! its sender, a wait ends on an accepted arrival or at its timeout, and a note
//! whose spool claim the file system refuses blocks nothing behind it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use onemessagebus::{Disposition, Inbox, Message, Offer, SchemaId, Sender, Spool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Note {
    text: String,
}

impl Message for Note {
    const SCHEMA: SchemaId = SchemaId::literal("steer", "note", 1);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Shown {
    Presented { to: String },
}

impl Disposition for Shown {}

fn note(text: &str) -> Note {
    Note {
        text: text.to_owned(),
    }
}

fn presented(to: &str) -> Shown {
    Shown::Presented { to: to.to_owned() }
}

/// What this receiver can present now: an interrupting note, never one that
/// waits for the turn to end.
fn urgent(note: &Note) -> bool {
    note.text.starts_with("interrupt")
}

/// Long enough that a message the receiver can take is always taken in it.
const TAKE: Duration = Duration::from_secs(10);

/// Long enough that a message still to come would have come: twenty spool
/// courier polls.
const SETTLE: Duration = Duration::from_millis(400);

/// A receiver and a sender into it, over one backend. The spool, when there is
/// one, stays bound for as long as this is held.
struct Pair {
    inbox: Arc<Inbox<Note, Shown>>,
    sender: Sender<Note, Shown>,
    spool: Option<(tempfile::TempDir, Spool)>,
}

impl Pair {
    fn in_process() -> Self {
        let inbox = Arc::new(Inbox::new());
        let sender = inbox.sender();
        Self {
            inbox,
            sender,
            spool: None,
        }
    }

    fn spool() -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let inbox = Arc::new(Inbox::new());
        let spool = Spool::bind(dir.path().join("notes"), &inbox).expect("binds");
        let sender = Spool::connect::<Note, Shown>(spool.address());
        Self {
            inbox,
            sender,
            spool: Some((dir, spool)),
        }
    }

    fn address(&self) -> &Path {
        self.spool
            .as_ref()
            .map(|(_, spool)| spool.address())
            .expect("a spool pair")
    }

    fn offer(&self, text: &str) -> Offer<Shown> {
        self.sender.offer(note(text)).expect("admitted")
    }

    /// Take every message still queued, answering each, and say what they were
    /// in the order they came.
    fn drain(&self) -> Vec<String> {
        std::iter::from_fn(|| self.inbox.take())
            .map(|delivered| {
                let text = delivered.message().text.clone();
                delivered.answer(presented(&text));
                text
            })
            .collect()
    }
}

/// The predicate's own call record: what it has been asked about, counted.
#[derive(Clone, Default)]
struct Asked(Arc<AtomicUsize>);

impl Asked {
    fn urgent(&self) -> impl FnMut(&Note) -> bool + Send + 'static {
        let asked = Arc::clone(&self.0);
        move |note: &Note| {
            asked.fetch_add(1, Ordering::SeqCst);
            urgent(note)
        }
    }

    /// Wait until the predicate has been asked at least `times` times; a guard
    /// against a hang, never a measure.
    fn until(&self, times: usize) {
        let gave_up = Instant::now() + TAKE;
        while self.0.load(Ordering::SeqCst) < times {
            assert!(
                Instant::now() < gave_up,
                "the predicate was never asked {times} times"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn the_first_accepted_note_is_claimed_and_every_other_keeps_its_place(pair: &Pair) {
    // In their place: the later takes return the three skipped notes in the
    // order they arrived.
    let offers: Vec<Offer<Shown>> = ["turn-end-1", "interrupt-1", "turn-end-2", "interrupt-2"]
        .into_iter()
        .map(|text| pair.offer(text))
        .collect();
    let claimed = pair
        .inbox
        .take_if(urgent)
        .expect("an urgent note is queued");
    assert_eq!(claimed.message(), &note("interrupt-1"));
    claimed.answer(presented("interrupt-1"));
    assert_eq!(pair.drain(), ["turn-end-1", "turn-end-2", "interrupt-2"]);
    for (offer, text) in
        offers
            .into_iter()
            .zip(["turn-end-1", "interrupt-1", "turn-end-2", "interrupt-2"])
    {
        assert_eq!(offer.wait(), Ok(presented(text)));
    }

    // Withdrawable: the first skipped note's sender takes it back while the
    // receiver still holds the urgent one it claimed.
    let skipped = pair.offer("turn-end-3");
    let first = pair.offer("interrupt-3");
    let behind = pair.offer("turn-end-4");
    let second = pair.offer("interrupt-4");
    let claimed = pair
        .inbox
        .take_if(urgent)
        .expect("an urgent note is queued");
    assert_eq!(claimed.message(), &note("interrupt-3"));
    assert!(
        skipped.withdraw().is_ok(),
        "a note the receiver skipped was refused as taken"
    );
    claimed.answer(presented("interrupt-3"));
    assert_eq!(first.wait(), Ok(presented("interrupt-3")));
    assert_eq!(pair.drain(), ["turn-end-4", "interrupt-4"]);
    assert_eq!(behind.wait(), Ok(presented("turn-end-4")));
    assert_eq!(second.wait(), Ok(presented("interrupt-4")));
}

#[test]
fn an_in_process_take_if_claims_the_first_accepted_message_and_leaves_the_rest_in_place() {
    the_first_accepted_note_is_claimed_and_every_other_keeps_its_place(&Pair::in_process());
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_spool_take_if_claims_the_first_accepted_message_and_leaves_the_rest_in_place() {
    the_first_accepted_note_is_claimed_and_every_other_keeps_its_place(&Pair::spool());
}

fn a_skipped_note_is_never_claimed_while_selecting(pair: &Pair) {
    // While a wait is selecting over nothing but notes it rejects, each one's
    // sender withdraws it from this thread.
    let offers: Vec<Offer<Shown>> = ["turn-end-1", "turn-end-2", "turn-end-3"]
        .into_iter()
        .map(|text| pair.offer(text))
        .collect();
    let asked = Asked::default();
    let waiting = {
        let inbox = Arc::clone(&pair.inbox);
        let urgent = asked.urgent();
        std::thread::spawn(move || inbox.take_if_within(TAKE, urgent))
    };
    asked.until(3);
    for (at, offer) in offers.into_iter().enumerate() {
        assert!(
            offer.withdraw().is_ok(),
            "skipped note {at} was refused as taken while the wait was selecting"
        );
    }
    let urgent_offer = pair.offer("interrupt-1");
    let claimed = waiting.join().expect("the wait ends").expect("claimed");
    assert_eq!(claimed.message(), &note("interrupt-1"));
    claimed.answer(presented("interrupt-1"));
    assert_eq!(urgent_offer.wait(), Ok(presented("interrupt-1")));

    // And after a take that skipped them has returned.
    let offers: Vec<Offer<Shown>> = ["turn-end-4", "turn-end-5"]
        .into_iter()
        .map(|text| pair.offer(text))
        .collect();
    let urgent_offer = pair.offer("interrupt-2");
    let claimed = pair
        .inbox
        .take_if(urgent)
        .expect("an urgent note is queued");
    assert_eq!(claimed.message(), &note("interrupt-2"));
    let withdrawing = std::thread::spawn(move || {
        offers
            .into_iter()
            .map(|offer| offer.withdraw().is_ok())
            .collect::<Vec<bool>>()
    });
    assert_eq!(
        withdrawing.join().expect("withdraws"),
        [true, true],
        "a note the take skipped was refused as taken"
    );
    claimed.answer(presented("interrupt-2"));
    assert_eq!(urgent_offer.wait(), Ok(presented("interrupt-2")));
    assert_eq!(pair.drain(), Vec::<String>::new());
}

#[test]
fn an_in_process_note_skipped_by_take_if_is_never_claimed_and_its_sender_can_withdraw_it() {
    a_skipped_note_is_never_claimed_while_selecting(&Pair::in_process());
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_spool_note_skipped_by_take_if_is_never_claimed_and_its_sender_can_withdraw_it() {
    let pair = Pair::spool();
    a_skipped_note_is_never_claimed_while_selecting(&pair);
    // Every withdrawal went through the spool's claim contest, and left nothing.
    let leftover: Vec<String> = std::fs::read_dir(pair.address())
        .expect("the spool is a directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.contains('-'))
        .collect();
    assert_eq!(leftover, Vec::<String>::new());
}

fn a_wait_ends_on_an_accepted_arrival_or_at_its_timeout(pair: &Pair) {
    // An urgent note arriving after the wait began is returned before its
    // timeout.
    let turn_end = pair.offer("turn-end-1");
    let asked = Asked::default();
    let started = Instant::now();
    let waiting = {
        let inbox = Arc::clone(&pair.inbox);
        let urgent = asked.urgent();
        std::thread::spawn(move || inbox.take_if_within(TAKE, urgent))
    };
    asked.until(1);
    let urgent_offer = pair.offer("interrupt-1");
    let claimed = waiting.join().expect("the wait ends").expect("claimed");
    assert!(started.elapsed() < TAKE, "the wait ran to its timeout");
    assert_eq!(claimed.message(), &note("interrupt-1"));
    claimed.answer(presented("interrupt-1"));
    assert_eq!(urgent_offer.wait(), Ok(presented("interrupt-1")));

    // With only rejected notes queued, the wait ends empty at its timeout and
    // leaves every one of them untaken.
    let more = pair.offer("turn-end-2");
    let started = Instant::now();
    assert!(pair.inbox.take_if_within(SETTLE, urgent).is_none());
    assert!(
        started.elapsed() >= SETTLE,
        "the wait ended before its timeout"
    );
    assert!(turn_end.withdraw().is_ok(), "a rejected note was taken");
    assert!(more.withdraw().is_ok(), "a rejected note was taken");
    assert_eq!(pair.drain(), Vec::<String>::new());
}

#[test]
fn an_in_process_take_if_within_returns_an_accepted_arrival_or_nothing_at_its_timeout() {
    a_wait_ends_on_an_accepted_arrival_or_at_its_timeout(&Pair::in_process());
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_spool_take_if_within_returns_an_accepted_arrival_or_nothing_at_its_timeout() {
    a_wait_ends_on_an_accepted_arrival_or_at_its_timeout(&Pair::spool());
}

#[test]
fn a_take_if_within_on_a_closed_inbox_ends_empty_and_a_close_ends_one_waiting() {
    let pair = Pair::in_process();
    let turn_end = pair.offer("turn-end-1");
    let asked = Asked::default();
    let waiting = {
        let inbox = Arc::clone(&pair.inbox);
        let urgent = asked.urgent();
        std::thread::spawn(move || inbox.take_if_within(TAKE, urgent).is_none())
    };
    asked.until(1);
    let started = Instant::now();
    pair.inbox
        .close(onemessagebus::Closed::new("the worker stopped"));
    assert!(
        waiting.join().expect("the wait ends"),
        "a closed wait claimed"
    );
    assert!(started.elapsed() < TAKE, "the close did not end the wait");
    assert!(pair.inbox.take_if_within(TAKE, urgent).is_none());
    assert!(pair.inbox.take_if(|_| true).is_none());
    assert!(turn_end.wait().is_err());
}

/// The spool at `address` refusing every file created in it, while held: `None`
/// for a user whom permissions bind nothing, such as root.
#[cfg(unix)]
fn refuse_claims(address: &Path, refuse: bool) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let mode = if refuse { 0o555 } else { 0o755 };
    std::fs::set_permissions(address, std::fs::Permissions::from_mode(mode))
        .expect("the spool's mode is set");
    if !refuse {
        return true;
    }
    let probe = address.join("probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return false;
    }
    true
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_spool_note_whose_claim_is_undecided_is_left_in_place_and_blocks_no_accepted_note_behind_it() {
    let pair = Pair::spool();
    let address: PathBuf = pair.address().to_path_buf();
    let contested = pair.offer("interrupt-contested");
    let behind = pair.offer("interrupt-behind");
    // The receiver would present both; the file system refuses the head's claim
    // — the spool turns read-only as it is accepted — and lets the next be
    // claimed.
    let mut permissions_bind = true;
    let claimed = pair
        .inbox
        .take_if(|note: &Note| {
            let contested = note.text == "interrupt-contested";
            permissions_bind &= refuse_claims(&address, contested);
            true
        })
        .expect("the note behind the contested one is claimed");
    refuse_claims(&address, false);
    if !permissions_bind {
        eprintln!("skipped: a read-only directory refuses nothing to this user");
        return;
    }
    assert_eq!(claimed.message(), &note("interrupt-behind"));
    claimed.answer(presented("behind"));
    assert_eq!(behind.wait(), Ok(presented("behind")));
    // Untaken and in its place: the next take finds it at the head.
    let again = pair
        .inbox
        .take()
        .expect("the contested note is still queued");
    assert_eq!(again.message(), &note("interrupt-contested"));
    again.answer(presented("contested"));
    assert_eq!(contested.wait(), Ok(presented("contested")));
}
