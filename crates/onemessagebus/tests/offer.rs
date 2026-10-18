//! The offer handle, driven through the public API over both backends that can
//! withdraw after admission: the in-process pair, and a spool bound by a
//! receiver in a process of its own. That receiver is this test binary re-run
//! as a child, told what to do one line at a time over its stdin; it is stopped,
//! resumed and killed by the handle that started it, and never by name.
//!
//! Each journey is shown on both backends: admission, the answer, a withdrawal
//! before and after a take, a deadline that expires and one that is outlived, a
//! receiver that goes away, a release, and take racing withdraw.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use onemessagebus::{
    BackendError, Closed, Delivered, Disposition, Inbox, InboxBackend, Message, Offer, SchemaId,
    Sender, Spool, Unanswered, Undelivered, SPOOL_SCHEMA_VERSION,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Order {
    sku: String,
    quantity: u32,
}

impl Message for Order {
    const SCHEMA: SchemaId = SchemaId::literal("shop", "order", 1);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Receipt {
    Filled { by: String },
}

impl Disposition for Receipt {}

fn order(sku: &str) -> Order {
    Order {
        sku: sku.to_owned(),
        quantity: 1,
    }
}

fn filled(by: &str) -> Receipt {
    Receipt::Filled { by: by.to_owned() }
}

/// Long enough that a message the receiver can take is always taken in it.
const TAKE: Duration = Duration::from_secs(10);

/// Long enough that a message still to come would have come: twenty courier
/// polls.
const SETTLE: Duration = Duration::from_millis(400);

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the spool is a directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The files of the spool at `dir` that belong to one message rather than to
/// the binding.
fn message_files(dir: &Path) -> Vec<String> {
    files_in(dir)
        .into_iter()
        .filter(|name| {
            ![
                "admission.json",
                "admission.lock",
                "receiver.lock",
                "spool.json",
                "closed.json",
            ]
            .contains(&name.as_str())
        })
        .collect()
}

/// The environment variable that makes this binary's `receiver_child` a
/// receiver bound to the spool it names.
const RECEIVER_SPOOL: &str = "ONEMESSAGEBUS_OFFER_RECEIVER_SPOOL";

/// The child half of every spool journey: a receiver bound to the spool
/// `RECEIVER_SPOOL` names, doing one thing per line of its stdin and saying
/// what came of it on a line of stdout led by `@ `. Ignored, so the suite never
/// runs it on its own; the journeys run it with `--ignored`.
#[test]
#[ignore = "the child half of the spool journeys, which run it with --ignored"]
fn receiver_child() {
    let address = std::env::var_os(RECEIVER_SPOOL)
        .expect("run only as a spool journey's child, which names the spool");
    let inbox = Inbox::<Order, Receipt>::new();
    let _spool = Spool::bind(PathBuf::from(address), &inbox).expect("the child binds");
    let mut held: HashMap<String, Delivered<Order, Receipt>> = HashMap::new();
    println!("@ bound");
    for line in std::io::stdin().lines() {
        let line = line.expect("a command");
        let words: Vec<&str> = line.split_whitespace().collect();
        let reply = match words.as_slice() {
            ["take", millis] => {
                let millis: u64 = millis.parse().expect("milliseconds");
                let taken = if millis == 0 {
                    inbox.take()
                } else {
                    inbox.take_within(Duration::from_millis(millis))
                };
                match taken {
                    Some(delivered) => {
                        let sku = delivered.message().sku.clone();
                        held.insert(sku.clone(), delivered);
                        format!("took {sku}")
                    }
                    None => "none".to_owned(),
                }
            }
            ["answer", sku, by] => {
                held.remove(*sku)
                    .expect("a held message")
                    .answer(filled(by));
                "ok".to_owned()
            }
            ["release", sku] => {
                held.remove(*sku).expect("a held message").release();
                "ok".to_owned()
            }
            // Release, and take at once, with nothing between the two.
            ["release-and-take", sku] => {
                held.remove(*sku).expect("a held message").release();
                match inbox.take() {
                    Some(delivered) => {
                        let sku = delivered.message().sku.clone();
                        held.insert(sku.clone(), delivered);
                        format!("took {sku}")
                    }
                    None => "none".to_owned(),
                }
            }
            ["drop", sku] => {
                drop(held.remove(*sku).expect("a held message"));
                "ok".to_owned()
            }
            ["close", reason @ ..] => {
                inbox.close(Closed::new(reason.join(" ")));
                "ok".to_owned()
            }
            ["answered"] => {
                let skus: Vec<String> = inbox
                    .answered()
                    .into_iter()
                    .map(|answered| answered.message.sku)
                    .collect();
                format!("answered [{}]", skus.join(","))
            }
            other => panic!("the child was told {other:?}"),
        };
        println!("@ {reply}");
    }
}

/// A receiver bound to a spool in a process of its own.
struct Receiver {
    child: Child,
    /// Closed to end the receiver cleanly, so its process exits as it would.
    commands: Option<ChildStdin>,
    replies: BufReader<ChildStdout>,
}

impl Receiver {
    fn bind(address: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("this test binary"))
            .args([
                "receiver_child",
                "--exact",
                "--nocapture",
                "--ignored",
                "--test-threads=1",
            ])
            .env(RECEIVER_SPOOL, address)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the receiver process starts");
        let commands = Some(child.stdin.take().expect("its stdin"));
        let replies = BufReader::new(child.stdout.take().expect("its stdout"));
        let mut receiver = Self {
            child,
            commands,
            replies,
        };
        assert_eq!(receiver.reply(), "bound");
        receiver
    }

    fn reply(&mut self) -> String {
        let mut line = String::new();
        loop {
            line.clear();
            let read = self.replies.read_line(&mut line).expect("a reply");
            assert!(read > 0, "the receiver process ended without replying");
            // The harness's own words can lead the first reply's line.
            if let Some((_, reply)) = line.trim_end().split_once("@ ") {
                return reply.to_owned();
            }
        }
    }

    /// Tell the receiver one thing, without waiting for what came of it.
    fn tell(&mut self, command: &str) {
        let commands = self.commands.as_mut().expect("the receiver is listening");
        writeln!(commands, "{command}").expect("told");
        commands.flush().expect("flushed");
    }

    fn say(&mut self, command: &str) -> String {
        self.tell(command);
        self.reply()
    }

    fn take(&mut self, within: Duration) -> Option<String> {
        let reply = self.say(&format!("take {}", within.as_millis()));
        if reply == "none" {
            return None;
        }
        Some(
            reply
                .strip_prefix("took ")
                .unwrap_or_else(|| panic!("{reply}"))
                .to_owned(),
        )
    }

    fn answered(&mut self) -> String {
        self.say("answered")
    }

    /// Send the receiver's process `signal`, by the pid of the handle that
    /// started it.
    fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([signal, &self.child.id().to_string()])
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill {signal} failed");
    }

    /// End the receiver's process without closing anything.
    fn kill(mut self) {
        self.child.kill().expect("the receiver process is killed");
        self.child.wait().expect("the receiver process is reaped");
    }
}

impl Drop for Receiver {
    /// End the receiver as a receiver ends: its commands run out, it unbinds,
    /// and its process exits. Killed only if it does not within ten seconds.
    fn drop(&mut self) {
        drop(self.commands.take());
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spool_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("orders");
    (dir, address)
}

#[test]
fn an_in_process_offer_returns_admitted_and_waits_for_the_receivers_answer() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let offer = sender.offer(order("a-1")).expect("admitted");
    // Admitted means queued: a take that does not block finds it.
    let delivered = inbox.take().expect("the admitted message is queued");
    assert_eq!(delivered.message(), &order("a-1"));
    delivered.answer(filled("stock-room"));
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
}

#[test]
fn a_spool_offer_returns_once_the_receivers_inbox_has_queued_it_and_waits_for_its_answer() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("a-1")).expect("admitted");
    // Admitted means in the receiver's in-process inbox: a take that does not
    // block, told only now, finds it.
    assert_eq!(receiver.take(Duration::ZERO).as_deref(), Some("a-1"));
    assert_eq!(receiver.say("answer a-1 stock-room"), "ok");
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[test]
fn an_offer_to_a_closed_inbox_is_undelivered_at_once() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    inbox.close(Closed::new("closed for stock-take"));
    let started = Instant::now();
    assert_eq!(
        sender.offer(order("late")).map(drop),
        Err(Undelivered::Closed(Closed::new("closed for stock-take")))
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(unix)]
#[test]
fn a_spool_offer_whose_courier_never_queues_it_is_withdrawn_when_the_wait_passes() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    // Bound, but its courier stopped: nothing moves the message into its inbox.
    receiver.signal("-STOP");
    let wait = Duration::from_millis(500);
    let sender = Spool::connect_within::<Order, Receipt>(&address, wait);
    let started = Instant::now();
    assert_eq!(
        sender.offer(order("unqueued")).map(drop),
        Err(Undelivered::Backend(BackendError::Elapsed {
            spool: address.clone(),
            waited: wait,
        }))
    );
    assert!(started.elapsed() >= wait, "{:?}", started.elapsed());
    assert_eq!(
        message_files(&address),
        Vec::<String>::new(),
        "a file of the withdrawn message is left to be admitted"
    );
    receiver.signal("-CONT");
    // Neither a take the courier's resumption races nor one long after it
    // finds the withdrawn message.
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");
}

#[cfg(unix)]
#[test]
fn a_spool_offer_a_courier_renamed_but_never_queued_is_not_admitted_and_is_withdrawn() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    receiver.signal("-STOP");
    let wait = Duration::from_millis(500);
    let offering = {
        let address = address.clone();
        std::thread::spawn(move || {
            Spool::connect_within::<Order, Receipt>(&address, wait)
                .offer(order("renamed"))
                .map(drop)
        })
    };
    // Stand in for a courier stopped between its two steps: the offer renamed
    // to taken, and never queued in the inbox.
    let offered = loop {
        if let Some(name) = files_in(&address)
            .into_iter()
            .find(|name| name.ends_with(".offer.json"))
        {
            break name;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let id = offered.trim_end_matches(".offer.json");
    std::fs::rename(
        address.join(&offered),
        address.join(format!("{id}.taken.json")),
    )
    .expect("taken by the stand-in");
    // A renamed file is not admission: the sender waits out its wait, and
    // withdraws the message.
    assert_eq!(
        offering.join().expect("the offer finishes"),
        Err(Undelivered::Backend(BackendError::Elapsed {
            spool: address.clone(),
            waited: wait,
        }))
    );
    receiver.signal("-CONT");
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");

    // Renamed, and its receiver gone before it was queued: lost, by name.
    receiver.signal("-STOP");
    let offering = {
        let address = address.clone();
        std::thread::spawn(move || {
            // Whether the offer or its wait says so depends on whether the
            // sender looked at the advertisement before or after the receiver
            // died; the sender is told the same thing either way.
            Spool::connect::<Order, Receipt>(&address)
                .offer(order("orphan"))
                .and_then(Offer::wait)
        })
    };
    let offered = loop {
        if let Some(name) = files_in(&address)
            .into_iter()
            .find(|name| name.ends_with(".offer.json"))
        {
            break name;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let id = offered.trim_end_matches(".offer.json");
    let taken = address.join(format!("{id}.taken.json"));
    std::fs::rename(address.join(&offered), &taken).expect("taken by the stand-in");
    receiver.kill();
    assert_eq!(
        offering.join().expect("the offer finishes"),
        Err(Undelivered::Backend(BackendError::Abandoned {
            spool: address.clone(),
            file: taken,
        }))
    );
}

#[test]
fn an_in_process_withdrawal_before_a_take_keeps_it_from_the_receiver_and_after_one_is_refused() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let offer = sender.offer(order("obsolete")).expect("admitted");
    assert!(offer.withdraw().is_ok());
    assert!(inbox.take_within(SETTLE).is_none());
    assert!(inbox.answered().is_empty());

    let offer = sender.offer(order("current")).expect("admitted");
    let delivered = inbox.take().expect("queued");
    let offer = offer
        .withdraw()
        .expect_err("a taken message is not withdrawn");
    delivered.answer(filled("stock-room"));
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
    let answered: Vec<String> = inbox
        .answered()
        .into_iter()
        .map(|answered| answered.message.sku)
        .collect();
    assert_eq!(answered, ["current"]);
}

#[test]
fn a_spool_withdrawal_before_a_take_keeps_it_from_the_receiver_and_after_one_is_refused() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);

    let offer = sender.offer(order("obsolete")).expect("admitted");
    assert!(offer.withdraw().is_ok());
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");
    assert_eq!(message_files(&address), Vec::<String>::new());

    let offer = sender.offer(order("current")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("current"));
    let offer = offer
        .withdraw()
        .expect_err("a taken message is not withdrawn");
    assert_eq!(receiver.say("answer current stock-room"), "ok");
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
    assert_eq!(receiver.answered(), "answered [current]");
    assert_eq!(message_files(&address), Vec::<String>::new());

    // Withdrawn and still queued when the receiver closes: its sender is not
    // told the close, and nothing of it is left.
    let offer = sender.offer(order("withdrawn")).expect("admitted");
    assert!(offer.withdraw().is_ok());
    assert_eq!(receiver.say("close for stock-take"), "ok");
    assert_eq!(message_files(&address), Vec::<String>::new());
    assert!(address.join("closed.json").exists());
}

#[test]
fn an_in_process_deadline_expires_an_untaken_message_and_outlives_a_taken_one() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let offer = sender.offer(order("stale")).expect("admitted");
    let deadline = Instant::now() + Duration::from_millis(200);
    assert_eq!(offer.wait_until(deadline), Err(Unanswered::Expired));
    assert!(Instant::now() >= deadline);
    assert!(inbox.take_within(SETTLE).is_none());

    let offer = sender.offer(order("slow")).expect("admitted");
    let delivered = inbox.take().expect("queued");
    let deadline = Instant::now() + Duration::from_millis(100);
    let waiting = std::thread::spawn(move || offer.wait_until(deadline));
    std::thread::sleep(Duration::from_millis(300));
    // Answered after the deadline, and still the answer.
    delivered.answer(filled("late"));
    assert_eq!(waiting.join().expect("waits"), Ok(filled("late")));
}

#[test]
fn a_spool_deadline_expires_an_untaken_message_and_outlives_a_taken_one() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);

    let offer = sender.offer(order("stale")).expect("admitted");
    let deadline = Instant::now() + Duration::from_millis(200);
    assert_eq!(offer.wait_until(deadline), Err(Unanswered::Expired));
    assert!(Instant::now() >= deadline);
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(message_files(&address), Vec::<String>::new());

    let offer = sender.offer(order("slow")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("slow"));
    let deadline = Instant::now() + Duration::from_millis(100);
    let waiting = std::thread::spawn(move || offer.wait_until(deadline));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(receiver.say("answer slow late"), "ok");
    assert_eq!(waiting.join().expect("waits"), Ok(filled("late")));
}

#[test]
fn an_in_process_offer_whose_receiver_closes_or_drops_its_claim_is_undelivered() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let waited = sender.offer(order("w")).expect("admitted");
    let bounded = sender.offer(order("b")).expect("admitted");
    let held = (inbox.take().expect("w"), inbox.take().expect("b"));
    let deadline = Instant::now() + Duration::from_secs(60);
    let waiting = std::thread::spawn(move || (waited.wait(), bounded.wait_until(deadline)));
    std::thread::sleep(Duration::from_millis(100));
    drop(inbox);
    let (waited, bounded) = waiting.join().expect("both finish");
    let dropped = Closed::new("the inbox was dropped without being closed");
    assert_eq!(waited, Err(Undelivered::Closed(dropped.clone())));
    assert_eq!(
        bounded,
        Err(Unanswered::Undelivered(Undelivered::Closed(dropped)))
    );
    drop(held);

    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let waited = sender.offer(order("w")).expect("admitted");
    let bounded = sender.offer(order("b")).expect("admitted");
    drop(inbox.take().expect("w"));
    drop(inbox.take().expect("b"));
    let let_go = Closed::new("the receiver took the message and let it go without answering");
    assert_eq!(waited.wait(), Err(Undelivered::Closed(let_go.clone())));
    assert_eq!(
        bounded.wait_until(Instant::now() + Duration::from_secs(60)),
        Err(Unanswered::Undelivered(Undelivered::Closed(let_go)))
    );
}

#[test]
fn a_spool_offer_whose_receiver_process_ends_or_drops_its_claim_is_undelivered() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let waited = sender.offer(order("w")).expect("admitted");
    let bounded = sender.offer(order("b")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("w"));
    assert_eq!(receiver.take(TAKE).as_deref(), Some("b"));
    let deadline = Instant::now() + Duration::from_secs(60);
    let waiting = std::thread::spawn(move || (waited.wait(), bounded.wait_until(deadline)));
    std::thread::sleep(Duration::from_millis(100));
    receiver.kill();
    let (waited, bounded) = waiting.join().expect("both finish");
    assert!(
        matches!(
            waited,
            Err(Undelivered::Backend(BackendError::Abandoned { .. }))
        ),
        "{waited:?}"
    );
    assert!(
        matches!(
            bounded,
            Err(Unanswered::Undelivered(Undelivered::Backend(
                BackendError::Abandoned { .. }
            )))
        ),
        "{bounded:?}"
    );
    assert!(Instant::now() < deadline);

    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let waited = sender.offer(order("w")).expect("admitted");
    let bounded = sender.offer(order("b")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("w"));
    assert_eq!(receiver.take(TAKE).as_deref(), Some("b"));
    assert_eq!(receiver.say("drop w"), "ok");
    assert_eq!(receiver.say("drop b"), "ok");
    let let_go = Closed::new("the receiver took the message and let it go without answering");
    assert_eq!(waited.wait(), Err(Undelivered::Closed(let_go.clone())));
    assert_eq!(
        bounded.wait_until(Instant::now() + Duration::from_secs(60)),
        Err(Unanswered::Undelivered(Undelivered::Closed(let_go)))
    );
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[test]
fn an_in_process_release_hands_the_message_back_untaken_in_its_place() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();

    // Released, then withdrawn: never taken again.
    let offer = sender.offer(order("withdrawn")).expect("admitted");
    inbox.take().expect("queued").release();
    assert!(offer.withdraw().is_ok());
    assert!(inbox.take_within(SETTLE).is_none());

    // Released after its deadline: expired at once, never taken again.
    let offer = sender.offer(order("expired")).expect("admitted");
    let delivered = inbox.take().expect("queued");
    let deadline = Instant::now() + Duration::from_millis(100);
    let waiting = std::thread::spawn(move || offer.wait_until(deadline));
    std::thread::sleep(Duration::from_millis(300));
    let released = Instant::now();
    delivered.release();
    assert_eq!(waiting.join().expect("waits"), Err(Unanswered::Expired));
    assert!(released.elapsed() < Duration::from_secs(1));
    assert!(inbox.take_within(SETTLE).is_none());

    // Released and left: taken again, and answered exactly once.
    let offer = sender.offer(order("again")).expect("admitted");
    inbox.take().expect("queued").release();
    let delivered = inbox.take().expect("back in the queue");
    assert_eq!(delivered.message(), &order("again"));
    delivered.answer(filled("second look"));
    assert_eq!(offer.wait(), Ok(filled("second look")));
    let answered: Vec<String> = inbox
        .answered()
        .into_iter()
        .map(|answered| answered.message.sku)
        .collect();
    assert_eq!(answered, ["again"]);

    // In its place: ahead of every message that arrived after it.
    let offers: Vec<Offer<Receipt>> = ["m1", "m2", "m3"]
        .into_iter()
        .map(|sku| sender.offer(order(sku)).expect("admitted"))
        .collect();
    inbox.take().expect("m1").release();
    let taken: Vec<String> = (0..3)
        .map(|_| {
            let delivered = inbox.take().expect("queued");
            let sku = delivered.message().sku.clone();
            delivered.answer(filled(&sku));
            sku
        })
        .collect();
    assert_eq!(taken, ["m1", "m2", "m3"]);
    for (offer, sku) in offers.into_iter().zip(["m1", "m2", "m3"]) {
        assert_eq!(offer.wait(), Ok(filled(sku)));
    }

    // Released into a closed inbox: gone, and its sender told the close.
    let offer = sender.offer(order("late")).expect("admitted");
    let delivered = inbox.take().expect("queued");
    inbox.close(Closed::new("closed for stock-take"));
    delivered.release();
    assert!(inbox.take().is_none());
    assert_eq!(
        offer.wait(),
        Err(Undelivered::Closed(Closed::new("closed for stock-take")))
    );
}

#[test]
fn a_spool_release_hands_the_message_back_untaken_in_its_place() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);

    // Released, then withdrawn: never taken again.
    let offer = sender.offer(order("withdrawn")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("withdrawn"));
    assert_eq!(receiver.say("release withdrawn"), "ok");
    assert!(offer.withdraw().is_ok());
    assert_eq!(receiver.take(SETTLE), None);

    // Released after its deadline: expired at once, never taken again.
    let offer = sender.offer(order("expired")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("expired"));
    let deadline = Instant::now() + Duration::from_millis(100);
    let waiting = std::thread::spawn(move || offer.wait_until(deadline));
    std::thread::sleep(Duration::from_millis(300));
    let released = Instant::now();
    assert_eq!(receiver.say("release expired"), "ok");
    assert_eq!(waiting.join().expect("waits"), Err(Unanswered::Expired));
    assert!(released.elapsed() < Duration::from_secs(1));
    assert_eq!(receiver.take(SETTLE), None);

    // Released and left: taken again, and answered exactly once.
    let offer = sender.offer(order("again")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("again"));
    assert_eq!(receiver.say("release again"), "ok");
    assert_eq!(receiver.take(Duration::ZERO).as_deref(), Some("again"));
    assert_eq!(receiver.say("answer again second-look"), "ok");
    assert_eq!(offer.wait(), Ok(filled("second-look")));
    assert_eq!(receiver.answered(), "answered [again]");

    // In its place: ahead of every message that arrived after it.
    let offers: Vec<Offer<Receipt>> = ["m1", "m2", "m3"]
        .into_iter()
        .map(|sku| sender.offer(order(sku)).expect("admitted"))
        .collect();
    assert_eq!(receiver.take(TAKE).as_deref(), Some("m1"));
    assert_eq!(receiver.say("release m1"), "ok");
    let mut taken = Vec::new();
    for _ in 0..3 {
        let sku = receiver.take(Duration::ZERO).expect("queued");
        assert_eq!(receiver.say(&format!("answer {sku} {sku}")), "ok");
        taken.push(sku);
    }
    assert_eq!(taken, ["m1", "m2", "m3"]);
    for (offer, sku) in offers.into_iter().zip(["m1", "m2", "m3"]) {
        assert_eq!(offer.wait(), Ok(filled(sku)));
    }
    assert_eq!(receiver.answered(), "answered [again,m1,m2,m3]");
    assert_eq!(message_files(&address), Vec::<String>::new());
}

/// How many times each journey releases a message past its deadline and takes
/// at once.
const EXPIRIES: usize = 20;

#[test]
fn an_in_process_release_past_the_deadline_expires_the_message_before_any_take_can_find_it() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    for round in 0..EXPIRIES {
        let offer = sender.offer(order("late")).expect("admitted");
        let delivered = inbox.take().expect("queued");
        let deadline = Instant::now() + Duration::from_millis(20);
        let waiting = std::thread::spawn(move || offer.wait_until(deadline));
        std::thread::sleep(Duration::from_millis(60));
        delivered.release();
        // Before the waiting sender has been scheduled to look.
        assert!(
            inbox.take().is_none(),
            "round {round}: a take found a message released past its deadline"
        );
        assert_eq!(waiting.join().expect("waits"), Err(Unanswered::Expired));
        assert!(inbox.take_within(SETTLE).is_none());
    }
    // Released before the deadline, it is untaken and queued again.
    let offer = sender.offer(order("early")).expect("admitted");
    let delivered = inbox.take().expect("queued");
    let deadline = Instant::now() + Duration::from_secs(60);
    let waiting = std::thread::spawn(move || offer.wait_until(deadline));
    std::thread::sleep(Duration::from_millis(20));
    delivered.release();
    let delivered = inbox.take().expect("queued again");
    delivered.answer(filled("in time"));
    assert_eq!(waiting.join().expect("waits"), Ok(filled("in time")));
}

#[test]
fn a_spool_release_past_the_deadline_expires_the_message_before_any_take_can_find_it() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    for round in 0..EXPIRIES {
        let offer = sender.offer(order("late")).expect("admitted");
        assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
        let deadline = Instant::now() + Duration::from_millis(20);
        let waiting = std::thread::spawn(move || offer.wait_until(deadline));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            receiver.say("release-and-take late"),
            "none",
            "round {round}: a take found a message released past its deadline"
        );
        assert_eq!(waiting.join().expect("waits"), Err(Unanswered::Expired));
        assert_eq!(receiver.take(SETTLE), None);
        assert_eq!(message_files(&address), Vec::<String>::new());
    }
    // Released before the deadline, it is untaken and queued again.
    let offer = sender.offer(order("early")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("early"));
    let deadline = Instant::now() + Duration::from_secs(60);
    let waiting = std::thread::spawn(move || offer.wait_until(deadline));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(receiver.say("release-and-take early"), "took early");
    assert_eq!(receiver.say("answer early in-time"), "ok");
    assert_eq!(waiting.join().expect("waits"), Ok(filled("in-time")));
    assert_eq!(receiver.answered(), "answered [early]");
    assert_eq!(message_files(&address), Vec::<String>::new());
}

const RACES: usize = 200;

/// Which side of race `race` waits before it moves, and for how long: a sweep
/// from the taker a millisecond ahead to the withdrawer a millisecond ahead,
/// so each side wins at one end whatever the machine's speed, and the middle
/// is a true race.
fn head_start(race: usize) -> (Duration, Duration) {
    let step = i64::try_from(race % 21).expect("small") - 10;
    let lead = Duration::from_micros(step.unsigned_abs() * 100);
    if step > 0 {
        (Duration::ZERO, lead)
    } else {
        (lead, Duration::ZERO)
    }
}

/// Busy for `span`, which a sleep is too coarse to give.
fn spin(span: Duration) {
    let until = Instant::now() + span;
    while Instant::now() < until {
        std::hint::spin_loop();
    }
}

#[test]
fn an_in_process_take_and_withdraw_racing_for_one_message_settle_it_exactly_one_way() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let inbox = Arc::new(inbox);
    let mut outcomes = (0, 0);
    for race in 0..RACES {
        let sku = format!("race-{race}");
        let offer = sender.offer(order(&sku)).expect("admitted");
        let start = Arc::new(Barrier::new(2));
        let (taker_waits, withdrawer_waits) = head_start(race);
        let taking = {
            let inbox = Arc::clone(&inbox);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                spin(taker_waits);
                inbox.take_within(Duration::from_millis(50))
            })
        };
        start.wait();
        spin(withdrawer_waits);
        let withdrawn = offer.withdraw();
        let taken = taking.join().expect("the take finishes");
        assert!(
            inbox.take().is_none(),
            "race {race} left the message queued"
        );
        match (taken, withdrawn) {
            (Some(delivered), Err(offer)) => {
                assert_eq!(delivered.message(), &order(&sku));
                delivered.answer(filled(&sku));
                assert_eq!(offer.wait(), Ok(filled(&sku)));
                outcomes.0 += 1;
            }
            (None, Ok(())) => outcomes.1 += 1,
            (Some(_), Ok(())) => panic!("race {race}: taken and withdrawn"),
            (None, Err(_)) => panic!("race {race}: neither taken nor withdrawn"),
        }
    }
    assert_eq!(outcomes.0 + outcomes.1, RACES);
    assert!(
        outcomes.0 > 0 && outcomes.1 > 0,
        "the races never raced: {outcomes:?} (taken, withdrawn)"
    );
    assert_eq!(inbox.answered().len(), outcomes.0);
}

#[test]
fn a_spool_take_and_withdraw_racing_for_one_message_settle_it_exactly_one_way() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let mut answered = Vec::new();
    let mut withdrawn_count = 0;
    for race in 0..RACES {
        let sku = format!("race-{race}");
        let offer = sender.offer(order(&sku)).expect("admitted");
        // The take is told, and the withdrawal made, with neither waiting on
        // the other; the withdrawal is held back a little more each race, so
        // the take, a process away, wins some of them.
        receiver.tell("take 50");
        spin(Duration::from_micros(
            u64::try_from(race % 11).expect("small") * 200,
        ));
        let withdrawn = offer.withdraw();
        let taken = receiver.reply();
        assert_eq!(
            receiver.take(Duration::ZERO),
            None,
            "race {race} left the message queued"
        );
        match (taken.strip_prefix("took "), withdrawn) {
            (Some(took), Err(offer)) => {
                assert_eq!(took, sku);
                assert_eq!(receiver.say(&format!("answer {sku} {sku}")), "ok");
                assert_eq!(offer.wait(), Ok(filled(&sku)));
                answered.push(sku);
            }
            (None, Ok(())) => {
                assert_eq!(taken, "none");
                withdrawn_count += 1;
            }
            (Some(_), Ok(())) => panic!("race {race}: taken and withdrawn"),
            (None, Err(_)) => panic!("race {race}: neither taken nor withdrawn"),
        }
    }
    assert_eq!(answered.len() + withdrawn_count, RACES);
    assert!(
        !answered.is_empty() && withdrawn_count > 0,
        "the races never raced: {} taken, {withdrawn_count} withdrawn",
        answered.len()
    );
    assert_eq!(
        receiver.answered(),
        format!("answered [{}]", answered.join(","))
    );
    assert_eq!(message_files(&address), Vec::<String>::new());
}

/// A receiver of a build before admission was acknowledged, bound to `dir`: it
/// holds the lock, declares the schema in the old shape, and moves each offer
/// to taken, answering it once `answer` is set.
struct OlderReceiver {
    stop: Arc<std::sync::atomic::AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

impl OlderReceiver {
    /// Bound, writing its declaration as every build does when `declare`, or
    /// leaving the one there — the file an earlier receiver wrote — when not.
    fn bind(dir: &Path, answer: Arc<std::sync::Mutex<Option<Receipt>>>, declare: bool) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        std::fs::create_dir_all(dir).expect("made");
        let lock = std::fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("receiver.lock"))
            .expect("opened");
        lock.try_lock().expect("the older receiver binds");
        if declare {
            // As every build writes a document: beside its name, then renamed on.
            let staging = dir.join("spool.json.staging");
            std::fs::write(
                &staging,
                json!({"schema_version": SPOOL_SCHEMA_VERSION, "schema": "shop.order@1"})
                    .to_string(),
            )
            .expect("staged");
            std::fs::rename(&staging, dir.join("spool.json")).expect("declared");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let serving = {
            let dir = dir.to_path_buf();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let _lock = lock;
                let mut taken: Vec<String> = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    for name in files_in(&dir) {
                        if let Some(id) = name.strip_suffix(".offer.json") {
                            if std::fs::rename(
                                dir.join(&name),
                                dir.join(format!("{id}.taken.json")),
                            )
                            .is_ok()
                            {
                                taken.push(id.to_owned());
                            }
                        }
                    }
                    if let Some(receipt) = answer.lock().expect("unpoisoned").clone() {
                        for id in taken.drain(..) {
                            let answer = dir.join(format!("{id}.answer.json"));
                            let staging = dir.join(format!("{id}.answer.json.staging"));
                            std::fs::write(
                                &staging,
                                json!({"schema_version": SPOOL_SCHEMA_VERSION,
                                       "answer": {"disposition": receipt}})
                                .to_string(),
                            )
                            .expect("staged");
                            std::fs::rename(&staging, &answer).expect("answered");
                            std::fs::remove_file(dir.join(format!("{id}.taken.json")))
                                .expect("settled");
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        };
        Self {
            stop,
            serving: Some(serving),
        }
    }
}

impl Drop for OlderReceiver {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// An older receiver has the message once its courier has it, and a sender
/// never claims a withdrawal that receiver would not honour.
fn assert_withdrawal_after_admission_is_refused(address: &Path) {
    assert_withdrawal_refused_once(address, true, |_| {});
}

/// As `assert_withdrawal_after_admission_is_refused`, with `beside` writing
/// what else is in the spool once the older receiver has bound it.
fn assert_withdrawal_refused_once(address: &Path, declare: bool, beside: impl FnOnce(&Path)) {
    let answer = Arc::new(std::sync::Mutex::new(None));
    let _older = OlderReceiver::bind(address, Arc::clone(&answer), declare);
    beside(address);
    let sender = Spool::connect::<Order, Receipt>(address);
    let offer = sender
        .offer(order("held"))
        .expect("admitted by the older receiver");
    let offer = offer
        .withdraw()
        .expect_err("a receiver that does not advertise it cannot be withdrawn from");
    let offer = offer
        .withdraw()
        .expect_err("and asking again changes nothing");
    *answer.lock().expect("unpoisoned") = Some(filled("older"));
    assert_eq!(
        offer.wait_until(Instant::now() + Duration::from_millis(1)),
        Ok(filled("older"))
    );
    // Answered before its sender saw it admitted: the offer holds the answer.
    for bounded in [false, true] {
        let offer = sender.offer(order("prompt")).expect("answered");
        let offer = offer.withdraw().expect_err("answered, so taken");
        let answered = if bounded {
            offer
                .wait_until(Instant::now())
                .map_err(|unanswered| panic!("{unanswered:?}"))
        } else {
            offer
                .wait()
                .map_err(|undelivered| panic!("{undelivered:?}"))
        };
        assert_eq!(answered, Ok::<_, ()>(filled("older")));
    }
}

#[test]
fn a_spool_receiver_that_does_not_advertise_withdrawal_reads_every_admitted_message_as_taken() {
    let (_dir, address) = spool_dir();
    assert_withdrawal_after_admission_is_refused(&address);
}

#[test]
fn an_older_receiver_rebinding_after_a_newer_one_leaves_its_advertisement_unread() {
    let (_dir, address) = spool_dir();
    // A newer receiver binds and is killed, so its advertisement is left
    // behind rather than removed as it unbinds.
    let newer = Receiver::bind(&address);
    newer.kill();
    assert!(
        address.join("admission.json").exists(),
        "a killed receiver removed its advertisement"
    );
    assert_withdrawal_after_admission_is_refused(&address);
}

/// A spool a newer receiver bound and was killed in, leaving its advertisement
/// behind.
fn left_by_a_killed_receiver() -> (tempfile::TempDir, PathBuf) {
    let (dir, address) = spool_dir();
    Receiver::bind(&address).kill();
    assert!(
        address.join("admission.json").exists(),
        "a killed receiver removed its advertisement"
    );
    (dir, address)
}

#[test]
fn an_older_receiver_whose_declaration_carries_the_stale_timestamp_still_refuses_withdrawal() {
    let (_dir, address) = left_by_a_killed_receiver();
    // The instant the declaration the advertisement was written beside carries.
    let stale = std::fs::metadata(address.join("spool.json"))
        .and_then(|metadata| metadata.modified())
        .expect("a declaration");
    // An older receiver rebinds, and its declaration comes to carry that same
    // instant — a clock set back, or two writes in one timestamp tick.
    assert_withdrawal_refused_once(&address, true, |address| {
        std::fs::File::options()
            .write(true)
            .open(address.join("spool.json"))
            .and_then(|declaration| declaration.set_modified(stale))
            .expect("the older declaration is stamped");
    });
}

#[test]
fn an_advertisement_whose_receiver_is_gone_is_not_trusted_beside_the_declaration_it_names() {
    // The declaration is the very file the advertisement names — as when a
    // file system hands a rewritten declaration a freed file's identity — and
    // the receiver that wrote it holds nothing any more.
    let (_dir, address) = left_by_a_killed_receiver();
    assert_withdrawal_refused_once(&address, false, |_| {});
}

#[test]
fn an_advertisement_beside_a_rewritten_declaration_is_not_trusted_while_its_lock_is_held() {
    let (_dir, address) = left_by_a_killed_receiver();
    // Stand in for a dying receiver whose advertisement's lock is let go of
    // last: still held while an older receiver rebinds and declares anew.
    let lingering = std::fs::File::options()
        .write(true)
        .open(address.join("admission.lock"))
        .expect("the advertisement's lock");
    lingering
        .try_lock()
        .expect("free once its receiver is gone");
    assert_withdrawal_refused_once(&address, true, |_| {});
    drop(lingering);
}

#[test]
fn a_senders_look_at_the_advertisements_lock_never_refuses_or_misleads_a_binding_receiver() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (_dir, address) = spool_dir();
    std::fs::create_dir_all(&address).expect("made");
    let stop = Arc::new(AtomicBool::new(false));
    let looks = Arc::new(AtomicUsize::new(0));
    // A sender's look, over and over: open the lock, try it, let it go.
    let looking = {
        let lock = address.join("admission.lock");
        let (stop, looks) = (Arc::clone(&stop), Arc::clone(&looks));
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if let Ok(file) = std::fs::File::options().write(true).open(&lock) {
                    let _ = file.try_lock();
                    looks.fetch_add(1, Ordering::SeqCst);
                }
            }
        })
    };
    for round in 0..50 {
        let inbox = Inbox::<Order, Receipt>::new();
        let spool = Spool::bind(&address, &inbox)
            .unwrap_or_else(|failure| panic!("round {round}: a look refused the bind: {failure}"));
        let offer = Spool::connect::<Order, Receipt>(&address)
            .offer(order(&format!("round-{round}")))
            .expect("admitted");
        assert!(
            offer.withdraw().is_ok(),
            "round {round}: a bound receiver was read as not advertising"
        );
        drop(spool);
    }
    stop.store(true, Ordering::SeqCst);
    looking.join().expect("the looks stop");
    assert!(looks.load(Ordering::SeqCst) > 0, "nothing looked");
}

/// A live receiver of this build whose advertisement `rewrite` replaced: its
/// sender reads every admitted message as taken, and is still answered.
fn assert_rewritten_advertisement_refused(rewrite: impl FnOnce(&Path)) {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    rewrite(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("held")).expect("admitted");
    let offer = offer
        .withdraw()
        .expect_err("an advertisement this build cannot read advertises nothing");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("held"));
    assert_eq!(receiver.say("answer held stock-room"), "ok");
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
}

fn advertisement(address: &Path) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(address.join("admission.json")).expect("an advertisement"),
    )
    .expect("JSON")
}

#[test]
fn an_advertisement_this_build_cannot_read_advertises_nothing() {
    assert_rewritten_advertisement_refused(|address| {
        std::fs::write(address.join("admission.json"), "not an advertisement").expect("written");
    });
    assert_rewritten_advertisement_refused(|address| {
        let mut later = advertisement(address);
        later["schema_version"] = json!(SPOOL_SCHEMA_VERSION + 1);
        std::fs::write(address.join("admission.json"), later.to_string()).expect("written");
    });
    assert_rewritten_advertisement_refused(|address| {
        let mut unnamed = advertisement(address);
        unnamed["declaration"] = json!("the declaration over there");
        std::fs::write(address.join("admission.json"), unnamed.to_string()).expect("written");
    });
}

#[test]
fn an_advertisement_that_says_more_than_this_build_knows_still_advertises() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let mut more = advertisement(&address);
    more["said_by_a_later_build"] = json!(true);
    std::fs::write(address.join("admission.json"), more.to_string()).expect("written");
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("obsolete")).expect("admitted");
    assert!(offer.withdraw().is_ok());
    assert_eq!(receiver.take(SETTLE), None);
}

#[cfg(unix)]
#[test]
fn an_acknowledgement_this_build_cannot_read_is_reported_and_the_message_withdrawn() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    receiver.signal("-STOP");
    let offering = {
        let address = address.clone();
        std::thread::spawn(move || {
            Spool::connect_within::<Order, Receipt>(&address, Duration::from_millis(500))
                .offer(order("garbled"))
                .map(drop)
        })
    };
    let offered = loop {
        if let Some(name) = files_in(&address)
            .into_iter()
            .find(|name| name.ends_with(".offer.json"))
        {
            break name;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // Stand in for a courier whose acknowledgement was overwritten.
    let id = offered.trim_end_matches(".offer.json");
    let acknowledgement = address.join(format!("{id}.admitted.json"));
    std::fs::write(&acknowledgement, "not an acknowledgement").expect("written");
    std::fs::rename(
        address.join(&offered),
        address.join(format!("{id}.taken.json")),
    )
    .expect("taken by the stand-in");
    match offering.join().expect("the offer finishes") {
        Err(Undelivered::Backend(BackendError::Unreadable { file, .. })) => {
            assert_eq!(file, acknowledgement);
        }
        other => panic!("an unreadable acknowledgement was reported as {other:?}"),
    }
    receiver.signal("-CONT");
    assert_eq!(receiver.take(SETTLE), None);
}

/// A spool nobody can create or remove a file in, for as long as it is held:
/// `None` for a user permissions bind nothing to, such as root, for whom there
/// is nothing to show.
#[cfg(unix)]
struct ReadOnly<'a>(&'a Path);

#[cfg(unix)]
impl<'a> ReadOnly<'a> {
    fn hold(address: &'a Path) -> Option<Self> {
        let read_only = Self(address);
        read_only.mode(0o555);
        let probe = address.join("probe");
        if std::fs::write(&probe, "").is_ok() {
            let _ = std::fs::remove_file(&probe);
            eprintln!("skipped: a read-only directory refuses nothing to this user");
            return None;
        }
        Some(read_only)
    }

    fn mode(&self, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(mode))
            .expect("the spool's mode is set");
    }
}

#[cfg(unix)]
impl Drop for ReadOnly<'_> {
    fn drop(&mut self) {
        self.mode(0o755);
    }
}

#[cfg(unix)]
#[test]
fn a_claim_the_file_system_refuses_gives_the_message_to_neither_side_until_one_can_be_made() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("stuck")).expect("admitted");
    let Some(read_only) = ReadOnly::hold(&address) else {
        return;
    };
    let offer = offer
        .withdraw()
        .expect_err("a withdrawal that cannot be claimed is not claimed");
    assert_eq!(
        receiver.take(SETTLE),
        None,
        "a take that cannot be claimed took the message"
    );
    // Once a claim can be made, the message is where it was, and is taken.
    drop(read_only);
    assert_eq!(receiver.take(TAKE).as_deref(), Some("stuck"));
    assert_eq!(receiver.say("answer stuck at-last"), "ok");
    assert_eq!(offer.wait(), Ok(filled("at-last")));
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_release_whose_claim_the_file_system_keeps_leaves_the_message_the_receivers() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("kept")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("kept"));
    let Some(read_only) = ReadOnly::hold(&address) else {
        return;
    };
    assert_eq!(receiver.say("release kept"), "ok");
    // Its claim could not be let go of: still the receiver's, so not withdrawn.
    let offer = offer
        .withdraw()
        .expect_err("a message whose claim the receiver still holds is taken");
    drop(read_only);
    assert_eq!(receiver.take(Duration::ZERO).as_deref(), Some("kept"));
    assert_eq!(receiver.say("answer kept second-look"), "ok");
    assert_eq!(offer.wait(), Ok(filled("second-look")));
    assert_eq!(receiver.answered(), "answered [kept]");
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_deadline_the_file_system_will_not_let_its_sender_say_is_said_once_it_can_be() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("late")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
    let deadline = Instant::now() + Duration::from_millis(100);
    let Some(read_only) = ReadOnly::hold(&address) else {
        return;
    };
    let waiting = std::thread::spawn(move || offer.wait_until(deadline));
    // The deadline passes while nothing can be created in the spool.
    std::thread::sleep(Duration::from_millis(300));
    drop(read_only);
    // A look once it can be: then a release expires it.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(receiver.say("release-and-take late"), "none");
    assert_eq!(waiting.join().expect("waits"), Err(Unanswered::Expired));
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_release_the_file_system_will_not_let_expire_leaves_the_message_to_nobody_until_it_can() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("late")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
    let deadline = Instant::now() + Duration::from_millis(20);
    let waiting = std::thread::spawn(move || offer.wait_until(deadline));
    // Past the deadline, and said, before the spool turns read-only.
    std::thread::sleep(Duration::from_millis(200));
    let Some(read_only) = ReadOnly::hold(&address) else {
        return;
    };
    // The release cannot expire it, and the take cannot either: neither has it.
    assert_eq!(receiver.say("release-and-take late"), "none");
    assert_eq!(receiver.take(Duration::ZERO), None);
    assert!(
        !waiting.is_finished(),
        "its sender was told before it could be"
    );
    // Once the word can be handed over, the next take expires it.
    drop(read_only);
    assert_eq!(receiver.take(Duration::ZERO), None);
    assert_eq!(waiting.join().expect("waits"), Err(Unanswered::Expired));
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_close_the_file_system_refuses_to_record_tells_the_sender_its_message_was_lost() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("unrecorded")).expect("admitted");
    let Some(read_only) = ReadOnly::hold(&address) else {
        return;
    };
    // Neither its claim, its answer nor the close can be written.
    assert_eq!(receiver.say("close for stock-take"), "ok");
    drop(read_only);
    let started = Instant::now();
    match offer.wait_until(Instant::now() + Duration::from_secs(60)) {
        Err(Unanswered::Undelivered(Undelivered::Backend(BackendError::Abandoned { .. }))) => {}
        other => panic!("a close nothing recorded was reported as {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_withdrawal_after_the_answer_is_written_is_refused_and_the_answer_read() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let offer = sender.offer(order("answered")).expect("admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("answered"));
    assert_eq!(receiver.say("answer answered stock-room"), "ok");
    // The receiver has let its claim go, and its answer is waiting.
    assert!(message_files(&address)
        .iter()
        .all(|name| !name.ends_with(".claimed")));
    let offer = offer
        .withdraw()
        .expect_err("an answered message is not withdrawn");
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
    assert_eq!(message_files(&address), Vec::<String>::new());
}

/// Offer `sku` into the receiver at `address`, stopped, and stand in the way
/// of its acknowledgement — a directory where the courier writes it — before
/// the receiver resumes: the courier queues the message and cannot say so.
#[cfg(unix)]
fn offer_unacknowledged(
    receiver: &Receiver,
    address: &Path,
    sku: &'static str,
    wait: Duration,
) -> (
    std::thread::JoinHandle<Result<Offer<Receipt>, Undelivered>>,
    PathBuf,
) {
    receiver.signal("-STOP");
    let offering = {
        let address = address.to_path_buf();
        std::thread::spawn(move || {
            Spool::connect_within::<Order, Receipt>(&address, wait).offer(order(sku))
        })
    };
    let offered = loop {
        if let Some(name) = files_in(address)
            .into_iter()
            .find(|name| name.ends_with(".offer.json"))
        {
            break name;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let acknowledgement = address.join(offered.replace(".offer.json", ".admitted.json"));
    std::fs::create_dir(&acknowledgement).expect("in the acknowledgement's way");
    receiver.signal("-CONT");
    (offering, acknowledgement)
}

#[cfg(unix)]
#[test]
fn an_acknowledgement_the_courier_cannot_write_leaves_the_message_withdrawn_or_taken() {
    // Nothing takes it: when the wait passes it is withdrawn, the failure is
    // named, and the queued message is never taken.
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let (offering, acknowledgement) =
        offer_unacknowledged(&receiver, &address, "unsaid", Duration::from_millis(500));
    match offering.join().expect("the offer finishes").map(drop) {
        Err(Undelivered::Backend(failure)) => assert!(
            failure
                .to_string()
                .contains(&acknowledgement.display().to_string()),
            "{failure}"
        ),
        other => panic!("an acknowledgement never written was reported as {other:?}"),
    }
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");

    // Taken before the wait passes: admitted by the take, and answered.
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let (offering, _) = offer_unacknowledged(&receiver, &address, "taken", Duration::from_secs(30));
    assert_eq!(receiver.take(TAKE).as_deref(), Some("taken"));
    let offer = offering
        .join()
        .expect("the offer finishes")
        .expect("admitted by the take");
    assert_eq!(receiver.say("answer taken stock-room"), "ok");
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
}

#[test]
fn a_dropped_offer_withdraws_nothing_and_its_message_is_still_taken_and_answered() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    drop(sender.offer(order("left")).expect("admitted"));
    let delivered = inbox.take().expect("still queued");
    assert_eq!(delivered.message(), &order("left"));
    delivered.answer(filled("nobody"));
    assert_eq!(inbox.answered().len(), 1);

    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    drop(
        Spool::connect::<Order, Receipt>(&address)
            .offer(order("left"))
            .expect("admitted"),
    );
    assert_eq!(receiver.take(Duration::ZERO).as_deref(), Some("left"));
    assert_eq!(receiver.say("answer left nobody"), "ok");
    assert_eq!(receiver.answered(), "answered [left]");
    assert_eq!(
        message_files(&address),
        Vec::<String>::new(),
        "an answer was written for an offer nobody holds"
    );
}

#[test]
fn a_receiver_unbinding_takes_its_advertisement_with_it() {
    let (_dir, address) = spool_dir();
    let inbox = Inbox::<Order, Receipt>::new();
    let spool = Spool::bind(&address, &inbox).expect("binds");
    assert!(address.join("admission.json").exists());
    drop(spool);
    assert!(!address.join("admission.json").exists());
}

/// A backend that answers at once and says nothing of admission.
struct Counter;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Tally(u64);

impl Disposition for Tally {}

impl InboxBackend<Order, Tally> for Counter {
    fn send(&self, message: Order) -> Result<Tally, Undelivered> {
        if message.quantity == 0 {
            return Err(Undelivered::Closed(Closed::new("nothing to count")));
        }
        Ok(Tally(u64::from(message.quantity)))
    }
}

#[test]
fn a_backend_that_cannot_report_admission_offers_an_answered_message_it_never_withdraws() {
    let sender = Sender::over(Counter);
    let offer = sender
        .offer(Order {
            sku: "n".to_owned(),
            quantity: 7,
        })
        .expect("answered, so admitted");
    assert!(format!("{offer:?}").starts_with("Offer"));
    let offer = offer.withdraw().expect_err("read as taken");
    assert_eq!(offer.wait(), Ok(Tally(7)));

    let offer = sender.offer(order("m")).expect("answered");
    assert_eq!(
        offer.wait_until(Instant::now()),
        Ok(Tally(1)),
        "a deadline already passed expires nothing it cannot withdraw"
    );
    assert_eq!(
        sender
            .offer(Order {
                sku: "z".to_owned(),
                quantity: 0
            })
            .map(drop),
        Err(Undelivered::Closed(Closed::new("nothing to count")))
    );
}

#[test]
fn an_unanswered_says_why_in_words() {
    assert_eq!(
        Unanswered::Expired.to_string(),
        "the message was not taken by its deadline, and was withdrawn"
    );
    let closed: Unanswered = Undelivered::Closed(Closed::new("gone home")).into();
    assert_eq!(closed.to_string(), "the inbox is closed: gone home");
}
