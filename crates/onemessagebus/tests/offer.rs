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

/// Held shared by every child process this binary starts, as it starts it, and
/// exclusively by a journey that rebinds a spool in this process: a child
/// forked while a lock file is open holds a copy of it until it runs its
/// program, so a rebind in that moment is refused by a lock nobody means to
/// hold.
static FORKING: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// Run `start`, which starts a child process, while no in-process rebind runs.
fn forking<T>(start: impl FnOnce() -> T) -> T {
    let _forking = FORKING
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    start()
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
            // The wall clock as this process reads it, in seconds.
            ["wall"] => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("after the epoch")
                .as_secs()
                .to_string(),
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

/// The environment variable that makes this binary's `sender_child` a sender
/// into the spool it names.
const SENDER_SPOOL: &str = "ONEMESSAGEBUS_OFFER_SENDER_SPOOL";

/// The child half of the journey whose sender is suspended: one offer into the
/// spool `SENDER_SPOOL` names, then `wait <millis>` waits until a deadline that
/// far off and says what came of it.
#[test]
#[ignore = "the child half of the suspended-sender journey, which runs it with --ignored"]
fn sender_child() {
    let address = std::env::var_os(SENDER_SPOOL)
        .expect("run only as a spool journey's child, which names the spool");
    let sender = Spool::connect::<Order, Receipt>(PathBuf::from(address));
    let offer = sender.offer(order("late")).expect("admitted");
    println!("@ admitted");
    let mut command = String::new();
    std::io::stdin().read_line(&mut command).expect("a command");
    let millis: u64 = command
        .trim()
        .strip_prefix("wait ")
        .and_then(|millis| millis.parse().ok())
        .unwrap_or_else(|| panic!("the child was told {command:?}"));
    let deadline = Instant::now() + Duration::from_millis(millis);
    let outcome = match offer.wait_until(deadline) {
        Ok(Receipt::Filled { by }) => format!("answered by {by}"),
        Err(Unanswered::Expired) => "expired".to_owned(),
        Err(other) => format!("unanswered: {other}"),
    };
    println!("@ {outcome}");
}

/// A receiver bound to a spool in a process of its own — or, started as
/// `sender_child`, a sender in one.
struct Receiver {
    child: Child,
    /// Closed to end the receiver cleanly, so its process exits as it would.
    commands: Option<ChildStdin>,
    replies: BufReader<ChildStdout>,
}

impl Receiver {
    fn bind(address: &Path) -> Self {
        Self::bind_with(address, |_| {})
    }

    /// As `bind`, with `configure` setting up the receiver's process first.
    fn bind_with(address: &Path, configure: impl FnOnce(&mut Command)) -> Self {
        let mut receiver = Self::start_with("receiver_child", RECEIVER_SPOOL, address, configure);
        assert_eq!(receiver.reply(), "bound");
        receiver
    }

    /// This binary re-run as its ignored test `child`, with `variable` naming
    /// the spool at `address`.
    fn start(child: &str, variable: &str, address: &Path) -> Self {
        Self::start_with(child, variable, address, |_| {})
    }

    fn start_with(
        child: &str,
        variable: &str,
        address: &Path,
        configure: impl FnOnce(&mut Command),
    ) -> Self {
        let mut command = Command::new(std::env::current_exe().expect("this test binary"));
        configure(&mut command);
        let mut child = forking(|| {
            command
                .args([
                    child,
                    "--exact",
                    "--nocapture",
                    "--ignored",
                    "--test-threads=1",
                ])
                .env(variable, address)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
        })
        .expect("the child process starts");
        let commands = Some(child.stdin.take().expect("its stdin"));
        let replies = BufReader::new(child.stdout.take().expect("its stdout"));
        Self {
            child,
            commands,
            replies,
        }
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
        let status = forking(|| {
            Command::new("kill")
                .args([signal, &self.child.id().to_string()])
                .status()
        })
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_spool_release_past_the_deadline_of_a_suspended_sender_expires_the_message_before_any_take() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let mut sender = Receiver::start("sender_child", SENDER_SPOOL, &address);
    assert_eq!(sender.reply(), "admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
    // The sender starts waiting, and is suspended well before its deadline:
    // as soon as its wait has published anything beside the message, or after
    // a moment if it publishes nothing.
    let before = message_files(&address);
    sender.tell("wait 1500");
    let started = Instant::now();
    while message_files(&address) == before && started.elapsed() < Duration::from_millis(300) {
        std::thread::sleep(Duration::from_millis(1));
    }
    sender.signal("-STOP");
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "the sender was suspended too late to prove anything"
    );
    // Past its deadline, with the sender never having run since.
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        receiver.say("release-and-take late"),
        "none",
        "a take found a message released past a suspended sender's deadline"
    );
    assert_eq!(receiver.take(Duration::ZERO), None);
    sender.signal("-CONT");
    assert_eq!(sender.reply(), "expired");
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");
    assert_eq!(message_files(&address), Vec::<String>::new());
}

/// The one message's published deadline in the spool at `address`, once its
/// waiting sender has published it.
fn published_deadline(address: &Path) -> PathBuf {
    let started = Instant::now();
    loop {
        if let Some(name) = message_files(address)
            .into_iter()
            .find(|name| name.ends_with(".deadline.json"))
        {
            return address.join(name);
        }
        assert!(
            started.elapsed() < TAKE,
            "a waiting sender publishes its deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The preload that steps a child's wall clock and hides what identifies its
/// monotonic clock (`tests/fixtures/clock_shim.c` says how), built into `dir`.
#[cfg(target_os = "linux")]
fn clock_shim(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/clock_shim.c");
    let library = dir.join("clock_shim.so");
    let status = forking(|| {
        Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
            .args(["-shared", "-fPIC", "-o"])
            .arg(&library)
            .arg(&source)
            .arg("-ldl")
            .status()
    })
    .expect("a C compiler builds the clock shim; set CC to one");
    assert!(status.success(), "the clock shim does not build");
    library
}

/// The wall clock a child shimmed with `shift` reads, stepped `seconds` from
/// everyone else's.
#[cfg(target_os = "linux")]
fn step_wall_clock(shift: &Path, seconds: i64) {
    std::fs::write(shift, seconds.to_string()).expect("the step is written");
}

/// A child's process preloaded with `library`, its wall clock shifted by what
/// `shift` holds, and `hide` (if any) hidden from it.
#[cfg(target_os = "linux")]
fn shimmed<'a>(
    library: &'a Path,
    shift: &'a Path,
    hide: Option<&'a str>,
) -> impl FnOnce(&mut Command) + 'a {
    move |command| {
        command
            .env("LD_PRELOAD", library)
            .env("ONEMESSAGEBUS_WALL_CLOCK_SHIFT_FILE", shift);
        if let Some(hide) = hide {
            command.env("ONEMESSAGEBUS_HIDE", hide);
        }
    }
}

/// A receiver whose wall clock reads `shift` seconds off everyone else's, as
/// if the system clock had been stepped after the sender published its
/// deadline; the shift is checked to have taken.
#[cfg(target_os = "linux")]
fn receiver_with_wall_clock_shifted(dir: &Path, address: &Path, shift: i64) -> Receiver {
    let library = clock_shim(dir);
    let shift_file = dir.join("wall-clock-shift");
    step_wall_clock(&shift_file, shift);
    let mut receiver = Receiver::bind_with(address, shimmed(&library, &shift_file, None));
    let ours = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let theirs: u64 = receiver.say("wall").parse().expect("seconds");
    let off = i64::try_from(theirs).expect("small") - i64::try_from(ours).expect("small");
    assert!(
        (off - shift).abs() < 60,
        "the receiver's wall clock is {off}s off, not {shift}s"
    );
    receiver
}

#[cfg(target_os = "linux")]
#[test]
fn a_wall_clock_set_forward_never_expires_a_message_released_before_its_deadline() {
    let (dir, address) = spool_dir();
    let mut receiver = receiver_with_wall_clock_shifted(dir.path(), &address, 3600);
    let mut sender = Receiver::start("sender_child", SENDER_SPOOL, &address);
    assert_eq!(sender.reply(), "admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
    sender.tell("wait 2000");
    let started = Instant::now();
    published_deadline(&address);
    sender.signal("-STOP");
    // Before the deadline on the clock `Instant` reads, however far ahead the
    // receiver's wall clock is: released, it is untaken and taken again.
    assert_eq!(receiver.say("release-and-take late"), "took late");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "released too late to prove anything"
    );
    // Answered after the deadline passes, with the sender suspended across it.
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(receiver.say("answer late in-time"), "ok");
    sender.signal("-CONT");
    assert_eq!(sender.reply(), "answered by in-time");
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[cfg(target_os = "linux")]
#[test]
fn a_wall_clock_set_back_never_lets_a_message_released_past_its_deadline_be_taken() {
    let (dir, address) = spool_dir();
    let mut receiver = receiver_with_wall_clock_shifted(dir.path(), &address, -3600);
    let mut sender = Receiver::start("sender_child", SENDER_SPOOL, &address);
    assert_eq!(sender.reply(), "admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
    sender.tell("wait 300");
    published_deadline(&address);
    sender.signal("-STOP");
    // Past the deadline on the clock `Instant` reads, however far behind the
    // receiver's wall clock is, with the sender never having run since.
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(
        receiver.say("release-and-take late"),
        "none",
        "a take found a message released past its deadline"
    );
    assert_eq!(receiver.take(Duration::ZERO), None);
    sender.signal("-CONT");
    assert_eq!(sender.reply(), "expired");
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[cfg(target_os = "linux")]
#[test]
fn a_receiver_that_cannot_identify_its_clock_names_none_and_never_has_an_admitted_message_expire() {
    // Each a way `/proc` fails to identify the receiver's clock, so no clock is
    // identified and no wall clock stands in for one.
    for hidden in [
        "boot-id",
        "boot-id-malformed",
        "time-namespace-denied",
        "time-namespace-malformed",
    ] {
        let (dir, address) = spool_dir();
        let library = clock_shim(dir.path());
        let shift = dir.path().join("wall-clock-shift");
        step_wall_clock(&shift, 0);
        let receiver = Receiver::bind_with(&address, shimmed(&library, &shift, Some(hidden)));
        assert_no_clock_named_and_admitted_messages_taken(receiver, &address);
    }
}

// Off Linux no monotonic clock is identified, by the receiver or this sender.
#[cfg(not(target_os = "linux"))]
#[test]
fn off_linux_a_receiver_names_no_clock_and_never_has_an_admitted_message_expire() {
    let (_dir, address) = spool_dir();
    let receiver = Receiver::bind(&address);
    assert_no_clock_named_and_admitted_messages_taken(receiver, &address);
}

/// `receiver`'s advertisement names no clock, so an admitted message is read
/// as taken: its withdrawal is refused, and a deadline passing never expires
/// it before the receiver's answer comes.
fn assert_no_clock_named_and_admitted_messages_taken(mut receiver: Receiver, address: &Path) {
    assert_eq!(advertisement(address).get("clock"), None);
    let offer = Spool::connect::<Order, Receipt>(address)
        .offer(order("held"))
        .expect("admitted");
    let Err(offer) = offer.withdraw() else {
        panic!("a withdrawal on a clock no one identified was claimed");
    };
    let waiting =
        std::thread::spawn(move || offer.wait_until(Instant::now() + Duration::from_millis(100)));
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !waiting.is_finished(),
        "an expiry was claimed for an admitted message on no shared clock"
    );
    assert_eq!(receiver.take(TAKE).as_deref(), Some("held"));
    assert_eq!(receiver.say("answer held stock-room"), "ok");
    assert_eq!(waiting.join().expect("waits"), Ok(filled("stock-room")));
}

#[cfg(target_os = "linux")]
#[test]
fn a_receiver_without_a_time_namespace_names_none_and_one_with_one_does_not_trust_it() {
    let (dir, address) = spool_dir();
    let library = clock_shim(dir.path());
    let shift = dir.path().join("wall-clock-shift");
    step_wall_clock(&shift, 0);
    let mut receiver =
        Receiver::bind_with(&address, shimmed(&library, &shift, Some("time-namespace")));
    let clock = advertisement(&address)["clock"]
        .as_str()
        .expect("a clock")
        .to_owned();
    assert!(
        clock.starts_with("linux-monotonic:") && clock.ends_with(":none"),
        "{clock}"
    );
    // This process reads a time namespace, so its clock is not the one named.
    let offer = Spool::connect::<Order, Receipt>(&address)
        .offer(order("held"))
        .expect("admitted");
    let Err(offer) = offer.withdraw() else {
        panic!("a withdrawal on a clock it does not share was claimed");
    };
    assert_eq!(receiver.take(TAKE).as_deref(), Some("held"));
    assert_eq!(receiver.say("answer held stock-room"), "ok");
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
}

/// A clock identity no process here reads: a boot that never happened.
const FOREIGN_CLOCK: &str = "linux-monotonic:00000000-0000-0000-0000-000000000000:none";

/// A suspended sender's published deadline replaced with `deadline`, which
/// the receiver must not read as passed: a release queues the message again,
/// untaken, and the resumed sender's own withdrawal at its deadline decides.
fn assert_deadline_not_read(deadline: &serde_json::Value) {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let mut sender = Receiver::start("sender_child", SENDER_SPOOL, &address);
    assert_eq!(sender.reply(), "admitted");
    assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
    sender.tell("wait 300");
    let published = published_deadline(&address);
    sender.signal("-STOP");
    std::fs::write(&published, deadline.to_string()).expect("the deadline is rewritten");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(receiver.say("release late"), "ok");
    assert!(
        !message_files(&address)
            .iter()
            .any(|name| name.ends_with(".expired")),
        "{deadline} was read as a passed deadline"
    );
    sender.signal("-CONT");
    assert_eq!(sender.reply(), "expired");
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");
    assert_eq!(message_files(&address), Vec::<String>::new());
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_deadline_the_receiver_cannot_read_is_none_and_its_senders_withdrawal_still_decides() {
    // Written by a build this one cannot read.
    assert_deadline_not_read(&json!({
        "schema_version": SPOOL_SCHEMA_VERSION + 1,
        "deadline": 0,
    }));
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_deadline_on_a_clock_the_receiver_does_not_read_is_none() {
    // Long past, on a clock this receiver cannot compare its own against.
    assert_deadline_not_read(&json!({
        "schema_version": SPOOL_SCHEMA_VERSION,
        "clock": FOREIGN_CLOCK,
        "deadline": 0,
    }));
}

/// A named rewrite of a bound receiver's `admission.json`.
type Rewrite = (&'static str, fn(&mut serde_json::Value));

#[test]
fn a_receiver_on_a_clock_its_sender_cannot_verify_never_has_an_admitted_message_expire() {
    let rewrites: [Rewrite; 3] = [
        ("another boot's clock", |advertisement| {
            advertisement["clock"] = json!(FOREIGN_CLOCK);
        }),
        ("no clock", |advertisement| {
            advertisement
                .as_object_mut()
                .expect("an object")
                .remove("clock");
        }),
        ("a clock that is no identity", |advertisement| {
            advertisement["clock"] = json!("the kitchen clock");
        }),
    ];
    for (named, rewrite) in rewrites {
        let (_dir, address) = spool_dir();
        let mut receiver = Receiver::bind(&address);
        let mut rewritten = advertisement(&address);
        rewrite(&mut rewritten);
        std::fs::write(address.join("admission.json"), rewritten.to_string()).expect("written");
        let sender = Spool::connect::<Order, Receipt>(&address);
        let offer = sender.offer(order("held")).expect("admitted");
        let Err(offer) = offer.withdraw() else {
            panic!("{named}: a withdrawal it could not verify was claimed");
        };
        let waiting = std::thread::spawn(move || {
            offer.wait_until(Instant::now() + Duration::from_millis(100))
        });
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            !waiting.is_finished(),
            "{named}: an expiry was claimed for an admitted message"
        );
        assert_eq!(receiver.take(TAKE).as_deref(), Some("held"), "{named}");
        assert_eq!(receiver.say("answer held stock-room"), "ok");
        assert_eq!(
            waiting.join().expect("waits"),
            Ok(filled("stock-room")),
            "{named}"
        );
    }
}

/// The name of the one offer waiting in the spool at `address`, once there is
/// one.
#[cfg(unix)]
fn waiting_offer(address: &Path) -> String {
    loop {
        if let Some(name) = files_in(address)
            .into_iter()
            .find(|name| name.ends_with(".offer.json"))
        {
            return name;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn a_receiver_on_a_clock_its_sender_cannot_verify_still_admits_a_message_before_its_offer_returns()
{
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let mut rewritten = advertisement(&address);
    rewritten["clock"] = json!(FOREIGN_CLOCK);
    std::fs::write(address.join("admission.json"), rewritten.to_string()).expect("written");

    // Renamed to taken by a stand-in for a courier stopped between its two
    // steps, and never queued: not admitted, so the offer does not return, and
    // when its wait passes the message is withdrawn.
    receiver.signal("-STOP");
    let wait = Duration::from_millis(1500);
    let offering = {
        let address = address.clone();
        std::thread::spawn(move || {
            Spool::connect_within::<Order, Receipt>(&address, wait)
                .offer(order("renamed"))
                .map(drop)
        })
    };
    let offered = waiting_offer(&address);
    let taken = offered.replace(".offer.json", ".taken.json");
    std::fs::rename(address.join(&offered), address.join(&taken)).expect("taken by the stand-in");
    std::thread::sleep(SETTLE);
    assert!(
        !offering.is_finished(),
        "an offer returned before the receiver's inbox admitted it"
    );
    assert_eq!(
        offering.join().expect("the offer finishes"),
        Err(Undelivered::Backend(BackendError::Elapsed {
            spool: address.clone(),
            waited: wait,
        }))
    );
    receiver.signal("-CONT");
    assert_eq!(receiver.take(SETTLE), None);

    // Queued once the courier resumes: the offer returns only then, with the
    // inbox's acknowledgement written, and the message is its receiver's.
    receiver.signal("-STOP");
    let offering = {
        let address = address.clone();
        std::thread::spawn(move || Spool::connect::<Order, Receipt>(&address).offer(order("held")))
    };
    waiting_offer(&address);
    std::thread::sleep(SETTLE);
    assert!(
        !offering.is_finished(),
        "an offer returned before the receiver's inbox admitted it"
    );
    receiver.signal("-CONT");
    let offer = offering
        .join()
        .expect("the offer finishes")
        .expect("admitted");
    assert!(
        message_files(&address)
            .iter()
            .any(|name| name.ends_with(".admitted.json")),
        "the offer returned without the inbox's acknowledgement: {:?}",
        message_files(&address)
    );
    let Err(offer) = offer.withdraw() else {
        panic!("a withdrawal it could not verify was claimed");
    };
    assert_eq!(receiver.take(TAKE).as_deref(), Some("held"));
    assert_eq!(receiver.say("answer held stock-room"), "ok");
    assert_eq!(offer.wait(), Ok(filled("stock-room")));
    assert_eq!(receiver.answered(), "answered [held]");
}

// A sender that cannot identify its own clock, beside a receiver that does:
// it still waits for the inbox's acknowledgement, and then claims no expiry.
#[cfg(target_os = "linux")]
#[test]
fn a_sender_that_cannot_identify_its_clock_waits_for_admission_and_never_has_an_admitted_message_expire(
) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let address = dir.path().join("spool");
    std::fs::create_dir(&address).expect("the spool directory");
    let library = clock_shim(dir.path());
    let shift = dir.path().join("wall-clock-shift");
    step_wall_clock(&shift, 0);
    let mut receiver = Receiver::bind(&address);
    receiver.signal("-STOP");
    let sender = Receiver::start_with(
        "sender_child",
        SENDER_SPOOL,
        &address,
        shimmed(&library, &shift, Some("boot-id")),
    );
    let admitting = std::thread::spawn(move || {
        let mut sender = sender;
        let said = sender.reply();
        (sender, said)
    });
    waiting_offer(&address);
    std::thread::sleep(SETTLE);
    assert!(
        !admitting.is_finished(),
        "an offer returned before the receiver's inbox admitted it"
    );
    receiver.signal("-CONT");
    let (mut sender, said) = admitting.join().expect("the sender replies");
    assert_eq!(said, "admitted");
    assert!(
        message_files(&address)
            .iter()
            .any(|name| name.ends_with(".admitted.json")),
        "the offer returned without the inbox's acknowledgement: {:?}",
        message_files(&address)
    );
    // Its deadline passes with the message untaken: none was published, and
    // no withdrawal at it is claimed, so the receiver still takes it and the
    // answer it gives after the deadline is the one returned.
    sender.tell("wait 50");
    std::thread::sleep(SETTLE);
    assert!(
        !message_files(&address)
            .iter()
            .any(|name| name.ends_with(".deadline.json") || name.ends_with(".expired")),
        "a deadline on a clock the sender could not identify was published: {:?}",
        message_files(&address)
    );
    assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
    assert_eq!(receiver.say("answer late stock-room"), "ok");
    assert_eq!(sender.reply(), "answered by stock-room");
    assert_eq!(receiver.answered(), "answered [late]");
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_first_take_past_the_deadline_of_a_suspended_sender_expires_the_message_untaken() {
    let (_dir, address) = spool_dir();
    let mut receiver = Receiver::bind(&address);
    let mut sender = Receiver::start("sender_child", SENDER_SPOOL, &address);
    assert_eq!(sender.reply(), "admitted");
    // Never taken: the sender starts waiting and is suspended once its deadline
    // is published, before it passes.
    sender.tell("wait 300");
    published_deadline(&address);
    sender.signal("-STOP");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        receiver.take(Duration::ZERO),
        None,
        "a first take past a suspended sender's deadline took the message"
    );
    assert_eq!(receiver.take(Duration::ZERO), None);
    sender.signal("-CONT");
    assert_eq!(sender.reply(), "expired");
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn a_senders_look_at_the_advertisements_lock_never_refuses_or_misleads_a_binding_receiver() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    // Every bind here is in this process, so no child starts meanwhile.
    let _rebinding = FORKING
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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

/// Offer `sku` with a bounded wait of `wait` into the receiver at `address`,
/// stopped, and stand in for a courier that took it and wrote an
/// acknowledgement this build cannot read, and — when `claimed` — for that
/// receiver's take, holding its claim. What the offer came to once the wait
/// passed, and the message's id.
fn offer_beside_unreadable_acknowledgement(
    receiver: &Receiver,
    address: &Path,
    sku: &'static str,
    wait: Duration,
    claimed: bool,
) -> (Result<Offer<Receipt>, Undelivered>, String) {
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
    let id = offered.trim_end_matches(".offer.json").to_owned();
    std::fs::write(
        address.join(format!("{id}.admitted.json")),
        "not an acknowledgement",
    )
    .expect("written");
    if claimed {
        std::fs::write(address.join(format!("{id}.claimed")), "").expect("claimed");
    }
    std::fs::rename(
        address.join(&offered),
        address.join(format!("{id}.taken.json")),
    )
    .expect("taken by the stand-in");
    (offering.join().expect("the offer finishes"), id)
}

/// That `offer`, admitted to the receiver at `address`, is its receiver's: it
/// is not withdrawn, and once that receiver's process ends it is undelivered,
/// never answered.
fn assert_taken_until_the_receiver_goes(offer: Offer<Receipt>, receiver: Receiver) {
    let offer = offer
        .withdraw()
        .expect_err("a message its receiver has is not withdrawn");
    receiver.kill();
    assert!(
        matches!(
            offer.wait(),
            Err(Undelivered::Backend(BackendError::Abandoned { .. }))
        ),
        "a message its receiver had was not abandoned with it"
    );
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
fn an_unreadable_acknowledgement_beside_the_receivers_claim_is_admitted_when_the_wait_passes() {
    let (_dir, address) = spool_dir();
    let receiver = Receiver::bind(&address);
    let wait = Duration::from_millis(300);
    let (offered, id) =
        offer_beside_unreadable_acknowledgement(&receiver, &address, "claimed", wait, true);
    // The withdrawal at the bound lost to the receiver's claim: taken.
    let offer = offered.expect("a message its receiver claimed is admitted");
    assert!(address.join(format!("{id}.claimed")).is_file());
    assert_taken_until_the_receiver_goes(offer, receiver);
}

#[test]
fn an_unreadable_acknowledgement_on_an_unverified_clock_gives_back_the_withdrawal_claim_and_is_admitted(
) {
    let (_dir, address) = spool_dir();
    let receiver = Receiver::bind(&address);
    let mut rewritten = advertisement(&address);
    rewritten["clock"] = json!(FOREIGN_CLOCK);
    std::fs::write(address.join("admission.json"), rewritten.to_string()).expect("written");
    let wait = Duration::from_millis(300);
    let (offered, id) =
        offer_beside_unreadable_acknowledgement(&receiver, &address, "unverified", wait, false);
    // Its claim made after an acknowledgement was there, on a clock it cannot
    // verify: given back, and the message read as taken.
    let offer = offered.expect("an acknowledged message is admitted");
    assert!(
        !address.join(format!("{id}.claimed")).exists(),
        "the withdrawal's claim was kept"
    );
    assert_taken_until_the_receiver_goes(offer, receiver);
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

/// How often a spool sender looks: an offer whose bound passed with its claim
/// refused returns no later than this after its bound.
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(20);

/// Offer `sku` with a bounded wait of `wait` from a thread of its own, saying
/// what came of it and how long it took.
#[cfg(unix)]
fn offering_within(
    address: &Path,
    sku: &'static str,
    wait: Duration,
) -> std::thread::JoinHandle<(Result<(), Undelivered>, Duration)> {
    let address = address.to_path_buf();
    std::thread::spawn(move || {
        let sender = Spool::connect_within::<Order, Receipt>(&address, wait);
        let started = Instant::now();
        let offered = sender.offer(order(sku)).map(drop);
        (offered, started.elapsed())
    })
}

/// That `offered`, the offer of message `id` with the bounded wait `wait`,
/// stopped at its bound — no later than a poll after it — because the file
/// system refused the claim `<id><suffix>`, and said so apart from an ordinary
/// elapsed wait.
#[cfg(unix)]
fn assert_unclaimable(
    (offered, took): (Result<(), Undelivered>, Duration),
    address: &Path,
    id: &str,
    suffix: &str,
    wait: Duration,
) {
    assert!(
        took >= wait && took <= wait + POLL,
        "returned {took:?} into a bound of {wait:?}"
    );
    let failure = offered.expect_err("an offer neither admitted nor withdrawn is not admitted");
    let Undelivered::Backend(BackendError::Unclaimable {
        spool,
        claim,
        waited,
        why,
    }) = &failure
    else {
        panic!("not told its claim was refused: {failure:?}");
    };
    assert_eq!(spool, address);
    assert_eq!(claim, &address.join(format!("{id}{suffix}")));
    assert_eq!(waited, &wait);
    assert!(why.contains("ermission"), "{why}");
    let said = failure.to_string();
    assert!(
        said.contains(&claim.display().to_string()) && said.contains("may still admit it"),
        "{said}"
    );
}

#[cfg(unix)]
#[test]
fn a_wait_that_passes_while_the_file_system_refuses_a_claim_stops_at_its_bound_naming_the_claim() {
    let (_dir, address) = spool_dir();
    let receiver = Receiver::bind(&address);
    receiver.signal("-STOP");
    let wait = Duration::from_millis(300);
    let offering = offering_within(&address, "unclaimable", wait);
    // Stand in for a courier stopped between its two steps: renamed to taken,
    // never queued, never acknowledged.
    let offered = waiting_offer(&address);
    let id = offered.trim_end_matches(".offer.json").to_owned();
    let taken = offered.replace(".offer.json", ".taken.json");
    std::fs::rename(address.join(&offered), address.join(&taken)).expect("taken by the stand-in");
    let Some(read_only) = ReadOnly::hold(&address) else {
        receiver.signal("-CONT");
        return;
    };
    let finished = offering.join().expect("the offer finishes");
    drop(read_only);
    assert_unclaimable(finished, &address, &id, ".claimed", wait);
    receiver.signal("-CONT");
    let mut receiver = receiver;
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");
}

#[cfg(unix)]
#[test]
fn a_wait_that_passes_with_the_receivers_claim_but_no_acknowledgement_stops_at_its_bound() {
    let (_dir, address) = spool_dir();
    let receiver = Receiver::bind(&address);
    receiver.signal("-STOP");
    let wait = Duration::from_millis(300);
    let offering = offering_within(&address, "unsettled", wait);
    // Stand in for a receiver stopped between claiming the message and saying
    // anything of it: renamed to taken, claimed, never acknowledged or answered.
    let offered = waiting_offer(&address);
    let id = offered.trim_end_matches(".offer.json").to_owned();
    std::fs::write(address.join(format!("{id}.claimed")), "").expect("claimed by the stand-in");
    std::fs::rename(
        address.join(&offered),
        address.join(format!("{id}.taken.json")),
    )
    .expect("taken by the stand-in");
    let (offered, took) = offering.join().expect("the offer finishes");
    assert!(
        took >= wait && took <= wait + POLL,
        "returned {took:?} into a bound of {wait:?}"
    );
    let claim = address.join(format!("{id}.claimed"));
    let failure = offered.expect_err("a message never acknowledged is not admitted");
    assert_eq!(
        failure,
        Undelivered::Backend(BackendError::Unsettled {
            spool: address.clone(),
            claim: claim.clone(),
            waited: wait,
        })
    );
    let said = failure.to_string();
    assert!(
        said.contains("neither admitted nor withdrawn")
            && said.contains(&claim.display().to_string()),
        "{said}"
    );
    // Claimed nothing either way: the receiver's claim is left where it was.
    assert!(claim.is_file(), "the receiver's claim was removed");
    receiver.signal("-CONT");
}

#[cfg(unix)]
#[test]
fn an_offer_whose_withdrawal_before_pickup_is_refused_stops_at_its_bound_and_may_be_admitted_later()
{
    let (_dir, address) = spool_dir();
    let receiver = Receiver::bind(&address);
    // Bound, but its courier stopped: nothing moves the message on, and the
    // sender's rename taking it back is refused.
    receiver.signal("-STOP");
    let wait = Duration::from_millis(300);
    let offering = offering_within(&address, "late", wait);
    let offered = waiting_offer(&address);
    let id = offered.trim_end_matches(".offer.json").to_owned();
    let Some(read_only) = ReadOnly::hold(&address) else {
        receiver.signal("-CONT");
        let _ = offering.join();
        return;
    };
    let finished = offering.join().expect("the offer finishes");
    drop(read_only);
    assert_unclaimable(finished, &address, &id, ".withdrawn", wait);
    // The one outcome that leaves it possible: the receiver admits it later.
    receiver.signal("-CONT");
    let mut receiver = receiver;
    assert_eq!(receiver.take(TAKE).as_deref(), Some("late"));
    assert_eq!(receiver.say("answer late nobody-waiting"), "ok");
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "withdrawing an admitted message needs a monotonic clock both sides verify they share, which only Linux identifies"
)]
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
/// the receiver resumes: the courier cannot say it queued the message, so it
/// refuses it rather than queueing it.
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
fn an_acknowledgement_the_courier_cannot_write_refuses_the_message_and_nothing_takes_it() {
    let (_dir, address) = spool_dir();
    let receiver = Receiver::bind(&address);
    assert_unacknowledged_refused(receiver, &address);
}

#[cfg(unix)]
#[test]
fn an_acknowledgement_the_courier_cannot_write_on_an_unverified_clock_is_never_taken_or_released() {
    let (_dir, address) = spool_dir();
    let receiver = Receiver::bind(&address);
    let mut rewritten = advertisement(&address);
    rewritten["clock"] = json!(FOREIGN_CLOCK);
    std::fs::write(address.join("admission.json"), rewritten.to_string()).expect("written");
    assert_unacknowledged_refused(receiver, &address);
}

/// A message whose acknowledgement the courier of `receiver`, at `address`,
/// cannot write is never queued: no take finds it, so the receiver has nothing
/// to release, and its sender is told at once, by name, that it was refused.
#[cfg(unix)]
fn assert_unacknowledged_refused(mut receiver: Receiver, address: &Path) {
    // A wait far longer than the journey: the refusal comes from the receiver,
    // not from the wait passing.
    let started = Instant::now();
    let (offering, acknowledgement) =
        offer_unacknowledged(&receiver, address, "unsaid", Duration::from_secs(30));
    if let Some(taken) = receiver.take(SETTLE) {
        assert_eq!(receiver.say(&format!("release {taken}")), "ok");
        panic!("the receiver took {taken}, whose admission it never acknowledged");
    }
    match offering.join().expect("the offer finishes").map(drop) {
        Err(Undelivered::Backend(BackendError::Refused { spool, why })) => {
            assert_eq!(spool, address);
            assert!(
                why.contains(&acknowledgement.display().to_string()),
                "{why}"
            );
        }
        other => panic!("an acknowledgement never written was reported as {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(receiver.take(SETTLE), None);
    assert_eq!(receiver.answered(), "answered []");
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
