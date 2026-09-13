//! The inbox contract, driven through the public API over a message and a
//! disposition of this test's own: an order a shop's till sends to its stock
//! room, answered with a receipt. Every backend is exercised the way a consumer
//! wires it — the in-process pair, a spool bound in a real directory, and a carry
//! store in a real file — and nothing is stood in for.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use onemessagebus::{
    Answered, BackendError, Carried, Carry, Closed, Disposition, Inbox, InboxBackend, Message,
    SchemaId, Sender, Spool, Undelivered, CARRY_SCHEMA_VERSION, SPOOL_SCHEMA_VERSION, SPOOL_WAIT,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Order {
    sku: String,
    quantity: u32,
}

impl Message for Order {
    const SCHEMA: SchemaId = SchemaId::literal("shop", "order", 1);
}

/// A second message, for a receiver that takes the other one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
struct Refund {
    sku: String,
}

impl Message for Refund {
    const SCHEMA: SchemaId = SchemaId::literal("shop", "refund", 1);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Receipt {
    Filled { by: String },
    Backordered,
    Deferred,
}

impl Disposition for Receipt {}

impl Carried for Receipt {
    fn carried() -> Self {
        Receipt::Deferred
    }
}

/// A disposition no receipt reads as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Tally(u64);

impl Disposition for Tally {}

fn order(sku: &str) -> Order {
    Order {
        sku: sku.to_owned(),
        quantity: 1,
    }
}

fn filled(by: &str) -> Receipt {
    Receipt::Filled { by: by.to_owned() }
}

/// Take one message within a generous bound, failing the test by name if none
/// arrives.
fn next<M: Message + Send + 'static, D: Disposition>(
    inbox: &Inbox<M, D>,
) -> onemessagebus::Delivered<M, D> {
    inbox
        .take_within(Duration::from_secs(10))
        .expect("a message arrives within ten seconds")
}

// --- In process -------------------------------------------------------------

#[test]
fn each_sender_is_answered_exactly_the_disposition_its_receiver_gave_it() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let senders: Vec<_> = ["a-1", "b-2", "c-3", "d-4"]
        .into_iter()
        .map(|sku| {
            let sender = sender.clone();
            std::thread::spawn(move || (sku, sender.send(order(sku))))
        })
        .collect();
    for _ in 0..4 {
        let delivered = next(&inbox);
        let by = delivered.message().sku.clone();
        delivered.answer(filled(&by));
    }
    for handle in senders {
        let (sku, answer) = handle.join().expect("the sender finishes");
        assert_eq!(
            answer,
            Ok(filled(sku)),
            "a sender was answered a disposition given to another"
        );
    }
    let answered = inbox.answered();
    assert_eq!(answered.len(), 4);
    for Answered {
        message,
        disposition,
    } in answered
    {
        assert_eq!(disposition, filled(&message.sku));
    }
}

#[test]
fn send_stays_blocked_for_as_long_as_the_receiver_takes_to_answer() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let started = Instant::now();
    let sending = std::thread::spawn(move || {
        let answer = sender.send(order("slow"));
        (answer, started.elapsed())
    });
    let delivered = next(&inbox);
    let delay = Duration::from_millis(400);
    std::thread::sleep(delay);
    delivered.answer(Receipt::Backordered);
    let (answer, waited) = sending.join().expect("the sender finishes");
    assert_eq!(answer, Ok(Receipt::Backordered));
    assert!(
        waited >= delay,
        "send returned after {waited:?}, before the receiver answered at {delay:?}"
    );
}

#[test]
fn a_close_before_the_message_arrives_refuses_the_sender_with_the_closers_reason() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    inbox.close(Closed::new("the till is closed for the night"));
    assert_eq!(
        sender.send(order("late")),
        Err(Undelivered::Closed(Closed::new(
            "the till is closed for the night"
        )))
    );
    assert_eq!(
        inbox.closed(),
        Some(Closed::new("the till is closed for the night"))
    );
    assert!(inbox.take().is_none(), "a refused message was queued");
}

#[test]
fn a_close_after_the_message_arrived_answers_the_blocked_sender_with_the_closers_reason() {
    // Taken and not yet answered.
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let sending = std::thread::spawn(move || sender.send(order("taken")));
    let delivered = next(&inbox);
    inbox.close(Closed::new("stock-take"));
    assert_eq!(
        sending.join().expect("the sender finishes"),
        Err(Undelivered::Closed(Closed::new("stock-take")))
    );
    // The receiver answering afterwards records its answer and sends nothing a
    // second time.
    delivered.answer(Receipt::Backordered);
    assert_eq!(inbox.answered().len(), 1);

    // Queued and never taken.
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let queued = sender.clone();
    let sending = std::thread::spawn(move || queued.send(order("queued")));
    // A second sender proves the first has arrived: its message is taken, and
    // it is behind the first in the queue.
    let prove = std::thread::spawn(move || sender.send(order("behind")));
    let first = next(&inbox);
    let first_sku = first.message().sku.clone();
    first.answer(Receipt::Backordered);
    inbox.close(Closed::new("stock-take"));
    let answers = [
        sending.join().expect("the sender finishes"),
        prove.join().expect("the sender finishes"),
    ];
    let skus = ["queued", "behind"];
    for (sku, answer) in skus.iter().zip(answers) {
        if *sku == first_sku {
            assert_eq!(answer, Ok(Receipt::Backordered));
        } else {
            assert_eq!(answer, Err(Undelivered::Closed(Closed::new("stock-take"))));
        }
    }
}

#[test]
fn a_closed_inbox_stays_closed_with_its_first_reason() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    inbox.close(Closed::new("the first reason"));
    inbox.close(Closed::new("a second reason"));
    assert_eq!(inbox.closed(), Some(Closed::new("the first reason")));
    assert_eq!(
        sender.send(order("x")),
        Err(Undelivered::Closed(Closed::new("the first reason")))
    );
    assert!(inbox.take_within(Duration::from_secs(5)).is_none());
}

#[test]
fn a_dropped_inbox_answers_every_blocked_sender() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let blocked: Vec<_> = ["one", "two", "three"]
        .into_iter()
        .map(|sku| {
            let sender = sender.clone();
            std::thread::spawn(move || sender.send(order(sku)))
        })
        .collect();
    // One taken and held, the others queued behind it.
    let held = next(&inbox);
    drop(held);
    drop(inbox);
    for handle in blocked {
        match handle.join().expect("the sender finishes") {
            Err(Undelivered::Closed(closed)) => assert!(
                closed.reason.contains("dropped") || closed.reason.contains("let it go"),
                "{closed:?}"
            ),
            other => panic!("a dropped inbox left a sender with {other:?}"),
        }
    }
    assert!(sender.send(order("after")).is_err());
}

#[test]
fn a_message_taken_and_let_go_answers_its_sender_rather_than_leaving_it_blocked() {
    let (sender, inbox) = Sender::<Order, Receipt>::channel();
    let sending = std::thread::spawn(move || sender.send(order("dropped")));
    let delivered = next(&inbox);
    assert!(format!("{delivered:?}").starts_with("Delivered"));
    drop(delivered);
    match sending.join().expect("the sender finishes") {
        Err(Undelivered::Closed(closed)) => {
            assert!(closed.reason.contains("without answering"), "{closed:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        inbox.closed(),
        None,
        "letting one message go closes nothing"
    );
    assert!(inbox.answered().is_empty());
}

#[test]
fn take_does_not_block_and_take_within_waits_only_as_long_as_asked() {
    let inbox = Inbox::<Order, Receipt>::default();
    assert!(inbox.take().is_none());
    let started = Instant::now();
    assert!(inbox.take_within(Duration::from_millis(60)).is_none());
    assert!(started.elapsed() >= Duration::from_millis(60));
    // A wait too long to add to the clock waits for a message rather than
    // overflowing, and a close ends it.
    let sender = inbox.sender();
    let sending = std::thread::spawn(move || sender.send(order("eventually")));
    let delivered = inbox
        .take_within(Duration::MAX)
        .expect("the message arrives");
    delivered.answer(Receipt::Backordered);
    assert_eq!(sending.join().expect("finishes"), Ok(Receipt::Backordered));
    let debug = format!("{inbox:?}");
    assert!(debug.contains("answered: 1"), "{debug}");
    assert!(format!("{:?}", inbox.sender()).starts_with("Sender"));
}

/// A backend of a consumer's own, which a sender is built over like any other.
struct Counter;

impl InboxBackend<Order, Tally> for Counter {
    fn send(&self, message: Order) -> Result<Tally, Undelivered> {
        Ok(Tally(u64::from(message.quantity)))
    }
}

#[test]
fn a_sender_is_written_against_whichever_backend_carries_it() {
    let sender = Sender::over(Counter);
    assert_eq!(
        sender.send(Order {
            sku: "n".to_owned(),
            quantity: 7
        }),
        Ok(Tally(7))
    );
}

#[test]
fn an_undelivered_names_what_became_of_the_message() {
    let closed: Undelivered = Closed::new("gone home").into();
    assert_eq!(closed.to_string(), "the inbox is closed: gone home");
    let lost: Undelivered = BackendError::Elapsed {
        spool: PathBuf::from("/spool"),
        waited: Duration::from_millis(1500),
    }
    .into();
    assert_eq!(
        lost.to_string(),
        "nothing took the message offered to the spool at /spool within 1500ms; it was withdrawn"
    );
    let whole = BackendError::Elapsed {
        spool: PathBuf::from("/spool"),
        waited: SPOOL_WAIT,
    };
    assert!(whole.to_string().contains("within 30s"), "{whole}");
    let encoding = BackendError::Encoding {
        what: "message",
        why: "no".to_owned(),
    };
    assert_eq!(encoding.to_string(), "the message does not serialize: no");
}

// --- Spool --------------------------------------------------------------------

/// A receiver bound to a spool, answering every order it takes with `answer`
/// after `delay`, until the returned flag is set.
fn serve(
    inbox: Arc<Inbox<Order, Receipt>>,
    delay: Duration,
) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        while !stopping.load(Ordering::SeqCst) {
            if let Some(delivered) = inbox.take_within(Duration::from_millis(20)) {
                std::thread::sleep(delay);
                let by = delivered.message().sku.clone();
                delivered.answer(filled(&by));
            }
        }
    });
    (stop, handle)
}

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

fn waiting_offers(dir: &Path) -> Vec<String> {
    files_in(dir)
        .into_iter()
        .filter(|name| name.ends_with(".offer.json"))
        .collect()
}

#[test]
fn a_spool_carries_a_message_across_and_answers_exactly_what_the_receiver_said() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("spool");
    let inbox = Arc::new(Inbox::<Order, Receipt>::new());
    let spool = Spool::bind(&address, &inbox).expect("binds");
    assert_eq!(spool.address(), address);
    assert!(format!("{spool:?}").contains("spool"));
    assert_eq!(
        Spool::declared(&address).expect("declared"),
        Some(Order::SCHEMA)
    );
    let (stop, receiver) = serve(Arc::clone(&inbox), Duration::ZERO);

    let sender = Spool::connect::<Order, Receipt>(spool.address());
    for sku in ["first", "second"] {
        assert_eq!(sender.send(order(sku)), Ok(filled(sku)));
    }
    // JSON in, JSON out, under the schema the receiver declared.
    let answered = Spool::deliver(
        &address,
        &json!({"sku": "raw", "quantity": 3}),
        Duration::from_secs(10),
    )
    .expect("delivered");
    assert_eq!(answered, json!({"filled": {"by": "raw"}}));

    stop.store(true, Ordering::SeqCst);
    receiver.join().expect("the receiver stops");
    assert_eq!(
        files_in(&address),
        ["receiver.lock", "spool.json"],
        "a settled exchange left files behind"
    );
    let declaration: Value =
        serde_json::from_str(&std::fs::read_to_string(address.join("spool.json")).expect("read"))
            .expect("JSON");
    assert_eq!(
        declaration,
        json!({"schema_version": SPOOL_SCHEMA_VERSION, "schema": "shop.order@1"})
    );
}

#[test]
fn a_spool_sender_stays_blocked_while_a_receiver_that_took_its_message_takes_its_time() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let inbox = Arc::new(Inbox::<Order, Receipt>::new());
    let spool = Spool::bind(dir.path(), &inbox).expect("binds");
    let delay = Duration::from_millis(700);
    let (stop, receiver) = serve(Arc::clone(&inbox), delay);
    // The bounded wait is far shorter than the receiver's delay: once the
    // message is taken, the sender waits on the receiver, not on the clock.
    let sender =
        Spool::connect_within::<Order, Receipt>(spool.address(), Duration::from_millis(250));
    let started = Instant::now();
    assert_eq!(sender.send(order("patient")), Ok(filled("patient")));
    assert!(started.elapsed() >= delay, "{:?}", started.elapsed());
    stop.store(true, Ordering::SeqCst);
    receiver.join().expect("the receiver stops");
}

#[test]
fn a_closed_spool_refuses_a_sender_with_the_closers_reason_before_and_after_the_offer() {
    // Closed before anything is offered: refused at once, nothing written.
    let dir = tempfile::tempdir().expect("a temp dir");
    let inbox = Inbox::<Order, Receipt>::new();
    let spool = Spool::bind(dir.path(), &inbox).expect("binds");
    inbox.close(Closed::new("closed for stock-take"));
    let sender = Spool::connect::<Order, Receipt>(spool.address());
    assert_eq!(
        sender.send(order("late")),
        Err(Undelivered::Closed(Closed::new("closed for stock-take")))
    );
    assert!(waiting_offers(dir.path()).is_empty());
    let record: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("closed.json")).expect("a close record"),
    )
    .expect("JSON");
    assert_eq!(
        record,
        json!({"schema_version": SPOOL_SCHEMA_VERSION, "reason": "closed for stock-take"})
    );

    // Offered and taken, then closed before an answer.
    let dir = tempfile::tempdir().expect("a temp dir");
    let inbox = Inbox::<Order, Receipt>::new();
    let spool = Spool::bind(dir.path(), &inbox).expect("binds");
    let sender = Spool::connect::<Order, Receipt>(spool.address());
    let sending = std::thread::spawn(move || sender.send(order("taken")));
    let held = next(&inbox);
    inbox.close(Closed::new("the receiver was told to stop"));
    assert_eq!(
        sending.join().expect("finishes"),
        Err(Undelivered::Closed(Closed::new(
            "the receiver was told to stop"
        )))
    );
    drop(held);

    // Offered and never taken — a receiver that is not taking — then closed.
    let dir = tempfile::tempdir().expect("a temp dir");
    let inbox = Inbox::<Order, Receipt>::new();
    let spool = Spool::bind(dir.path(), &inbox).expect("binds");
    let address = spool.address().to_path_buf();
    // The courier is stopped so the offer stays offered, and the close still
    // answers it.
    drop(spool);
    let sender = Spool::connect::<Order, Receipt>(&address);
    let sending = std::thread::spawn(move || sender.send(order("waiting")));
    while waiting_offers(&address).is_empty() {
        std::thread::sleep(Duration::from_millis(5));
    }
    inbox.close(Closed::new("closed with an offer waiting"));
    assert_eq!(
        sending.join().expect("finishes"),
        Err(Undelivered::Closed(Closed::new(
            "closed with an offer waiting"
        )))
    );
    assert!(waiting_offers(&address).is_empty());
}

#[test]
fn an_answer_document_that_is_not_an_answer_is_reported_naming_the_file() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().to_path_buf();
    let sender = Spool::connect::<Order, Receipt>(&address);
    let sending = std::thread::spawn(move || sender.send(order("garbled")));
    // Stand in for a courier whose answer was overwritten: take the offer and
    // write bytes that are not an answer where the answer goes.
    let offer = loop {
        if let Some(name) = waiting_offers(&address).into_iter().next() {
            break name;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let id = offer.trim_end_matches(".offer.json");
    std::fs::rename(
        address.join(&offer),
        address.join(format!("{id}.taken.json")),
    )
    .expect("taken");
    let answer = address.join(format!("{id}.answer.json"));
    std::fs::write(&answer, "this is not an answer").expect("written");
    match sending.join().expect("finishes") {
        Err(Undelivered::Backend(failure @ BackendError::Unreadable { .. })) => {
            let BackendError::Unreadable { file, .. } = &failure else {
                unreachable!()
            };
            assert_eq!(file, &answer);
            assert!(
                failure.to_string().contains(&answer.display().to_string()),
                "{failure}"
            );
        }
        other => panic!("an unreadable answer was reported as {other:?}"),
    }
    assert!(
        answer.exists(),
        "the unreadable answer was not left for inspection"
    );
}

#[test]
fn a_spool_nothing_services_reports_the_elapsed_wait_and_withdraws_the_offer() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let wait = Duration::from_millis(200);
    let sender = Spool::connect_within::<Order, Receipt>(dir.path(), wait);
    let started = Instant::now();
    assert_eq!(
        sender.send(order("unserviced")),
        Err(Undelivered::Backend(BackendError::Elapsed {
            spool: dir.path().to_path_buf(),
            waited: wait,
        }))
    );
    assert!(started.elapsed() >= wait);
    assert!(
        files_in(dir.path()).is_empty(),
        "the withdrawn offer was left for a receiver to deliver later: {:?}",
        files_in(dir.path())
    );
    // A receiver binding afterwards takes nothing its sender was told was lost.
    let inbox = Inbox::<Order, Receipt>::new();
    let _spool = Spool::bind(dir.path(), &inbox).expect("binds");
    assert!(inbox.take_within(Duration::from_millis(150)).is_none());
}

#[test]
fn a_receiver_that_took_the_message_and_is_no_longer_bound_is_reported_abandoned() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let inbox = Inbox::<Order, Receipt>::new();
    let spool = Spool::bind(dir.path(), &inbox).expect("binds");
    let sender = Spool::connect::<Order, Receipt>(spool.address());
    let sending = std::thread::spawn(move || sender.send(order("orphan")));
    let held = next(&inbox);
    drop(spool);
    match sending.join().expect("finishes") {
        Err(Undelivered::Backend(BackendError::Abandoned { spool, file })) => {
            assert_eq!(spool, dir.path());
            assert!(file.to_string_lossy().ends_with(".taken.json"), "{file:?}");
        }
        other => panic!("{other:?}"),
    }
    // An answer given after its sender stopped waiting is written nowhere.
    held.answer(Receipt::Backordered);
    assert!(files_in(dir.path())
        .iter()
        .all(|name| !name.ends_with(".answer.json")));
}

#[test]
fn a_spool_has_one_receiver_at_a_time_and_a_new_one_reopens_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let first = Inbox::<Order, Receipt>::new();
    let bound = Spool::bind(dir.path(), &first).expect("binds");
    let second = Inbox::<Order, Receipt>::new();
    let refused = Spool::bind(dir.path(), &second).expect_err("a bound spool refuses a second");
    assert_eq!(
        refused,
        BackendError::Bound {
            path: dir.path().to_path_buf()
        }
    );
    assert!(refused.to_string().contains("already bound"), "{refused}");
    first.close(Closed::new("the first session ended"));
    drop(bound);
    drop(first);
    assert!(dir.path().join("closed.json").exists());

    let rebound = Spool::bind(dir.path(), &second).expect("the released spool binds");
    assert!(
        !dir.path().join("closed.json").exists(),
        "a new session is refused by an old close"
    );
    let (stop, receiver) = serve(Arc::new(second), Duration::ZERO);
    assert_eq!(
        Spool::connect::<Order, Receipt>(rebound.address()).send(order("again")),
        Ok(filled("again"))
    );
    stop.store(true, Ordering::SeqCst);
    receiver.join().expect("stops");

    // A spool bound for an inbox that is already closed records the close.
    let closed_dir = tempfile::tempdir().expect("a temp dir");
    let closed = Inbox::<Order, Receipt>::new();
    closed.close(Closed::new("never open"));
    let _spool = Spool::bind(closed_dir.path(), &closed).expect("binds");
    assert_eq!(
        Spool::connect::<Order, Receipt>(closed_dir.path()).send(order("x")),
        Err(Undelivered::Closed(Closed::new("never open")))
    );
}

#[test]
fn a_receiver_refuses_what_it_cannot_read_and_the_sender_is_told_why() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let inbox = Arc::new(Inbox::<Order, Receipt>::new());
    let spool = Spool::bind(dir.path(), &inbox).expect("binds");
    let (stop, receiver) = serve(Arc::clone(&inbox), Duration::ZERO);

    // A message of another schema.
    match Spool::connect::<Refund, Receipt>(spool.address()).send(Refund {
        sku: "r".to_owned(),
    }) {
        Err(Undelivered::Backend(BackendError::Refused { why, .. })) => {
            assert!(
                why.contains("shop.refund@1") && why.contains("shop.order@1"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    // JSON that is not an order, under the declared schema.
    match Spool::deliver(spool.address(), &json!({"sku": 1}), Duration::from_secs(10)) {
        Err(Undelivered::Backend(failure @ BackendError::Refused { .. })) => {
            assert!(
                failure.to_string().contains("is not a shop.order@1"),
                "{failure}"
            );
        }
        other => panic!("{other:?}"),
    }
    // A disposition the sender does not read, named with the answer it came in.
    match Spool::connect::<Order, Tally>(spool.address()).send(order("tally")) {
        Err(Undelivered::Backend(BackendError::Unreadable { file, why, .. })) => {
            assert!(file.to_string_lossy().ends_with(".answer.json"), "{file:?}");
            assert!(why.contains("not one this sender reads"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    stop.store(true, Ordering::SeqCst);
    receiver.join().expect("stops");
}

#[test]
fn a_path_that_is_no_spool_or_a_record_this_build_did_not_write_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let missing = dir.path().join("missing");
    match Spool::deliver(&missing, &json!({}), Duration::from_millis(10)) {
        Err(Undelivered::Backend(failure @ BackendError::Absent { .. })) => {
            assert_eq!(
                failure.to_string(),
                format!("{} is not a spool: nothing is there", missing.display())
            );
        }
        other => panic!("{other:?}"),
    }
    let file = dir.path().join("file");
    std::fs::write(&file, "x").expect("written");
    assert!(matches!(
        Spool::declared(&file),
        Err(BackendError::Absent { why, .. }) if why == "it is not a directory"
    ));
    assert_eq!(Spool::declared(dir.path()), Ok(None));

    let spool = dir.path().join("spool");
    std::fs::create_dir(&spool).expect("made");
    std::fs::write(
        spool.join("closed.json"),
        r#"{"schema_version": 9, "reason": "x"}"#,
    )
    .expect("written");
    match Spool::connect::<Order, Receipt>(&spool).send(order("x")) {
        Err(Undelivered::Backend(failure @ BackendError::Unreadable { .. })) => {
            assert!(
                failure.to_string().contains("schema_version 9"),
                "{failure}"
            );
        }
        other => panic!("{other:?}"),
    }
    std::fs::write(spool.join("spool.json"), "{").expect("written");
    assert!(matches!(
        Spool::declared(&spool),
        Err(BackendError::Unreadable { .. })
    ));
}

// --- Carry --------------------------------------------------------------------

#[test]
fn a_carried_message_is_answered_carried_and_adopted_exactly_once() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = dir.path().join("carried.ndjson");

    // No receiver is running: each sender is answered at once.
    let sender = Carry::sender::<Order, Receipt>(&store);
    for sku in ["first", "second"] {
        assert_eq!(sender.send(order(sku)), Ok(Receipt::Deferred));
    }
    let listed = Carry::read(&store).expect("a store");
    assert_eq!(
        listed
            .iter()
            .map(|entry| (entry.schema.clone(), entry.message.clone()))
            .collect::<Vec<_>>(),
        [
            (Order::SCHEMA, json!({"sku": "first", "quantity": 1})),
            (Order::SCHEMA, json!({"sku": "second", "quantity": 1})),
        ]
    );
    assert!(listed.iter().all(|entry| entry.ts.ends_with('Z')));

    // The receiver's next session takes them as it opens, oldest first.
    let session = Inbox::<Order, Receipt>::new();
    assert_eq!(session.adopt_carried(&store), Ok(2));
    let first = session.take().expect("the first carried order");
    assert_eq!(first.message(), &order("first"));
    // Nobody waits on a carried message; its answer is recorded and sent nowhere.
    first.answer(Receipt::Backordered);
    assert_eq!(
        session.take().expect("the second").message(),
        &order("second")
    );
    assert!(session.take().is_none());
    assert_eq!(session.answered().len(), 1);
    drop(session);

    // And never again.
    let later = Inbox::<Order, Receipt>::new();
    assert_eq!(later.adopt_carried(&store), Ok(0));
    assert!(later.take().is_none());
    assert_eq!(Carry::read(&store), Ok(Vec::new()));
    let header = std::fs::read_to_string(&store).expect("read");
    assert_eq!(
        header,
        format!(
            "{{\"schema_version\":{CARRY_SCHEMA_VERSION},\"kind\":\"onemessagebus-carry-store\"}}\n"
        )
    );
    // A store nothing was ever carried into is an empty one.
    assert_eq!(later.adopt_carried(dir.path().join("never")), Ok(0));
}

#[test]
fn adopting_into_a_closed_inbox_or_from_an_unreadable_store_leaves_the_store_untouched() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = dir.path().join("store");
    Carry::sender::<Order, Receipt>(&store)
        .send(order("kept"))
        .expect("carried");
    let before = std::fs::read_to_string(&store).expect("read");

    let closed = Inbox::<Order, Receipt>::new();
    closed.close(Closed::new("shut"));
    assert_eq!(
        closed.adopt_carried(&store),
        Err(Undelivered::Closed(Closed::new("shut")))
    );
    // A store of another message.
    let refunds = Inbox::<Refund, Receipt>::new();
    match refunds.adopt_carried(&store) {
        Err(Undelivered::Backend(failure @ BackendError::Unreadable { .. })) => {
            assert!(failure.to_string().contains("record 1"), "{failure}");
            assert!(failure.to_string().contains("shop.refund@1"), "{failure}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&store).expect("read"), before);

    // A record that is not the message its schema names.
    let bad = dir.path().join("bad");
    std::fs::write(
        &bad,
        format!(
            "{}\n{}\n",
            r#"{"schema_version":1,"kind":"onemessagebus-carry-store"}"#,
            r#"{"ts":"2026-09-13T00:00:00.000Z","schema":"shop.order@1","message":{"sku":2}}"#
        ),
    )
    .expect("written");
    let orders = Inbox::<Order, Receipt>::new();
    assert!(matches!(
        orders.adopt_carried(&bad),
        Err(Undelivered::Backend(BackendError::Unreadable { why, .. })) if why.contains("is not a shop.order@1")
    ));
    assert!(matches!(
        orders.adopt_carried(dir.path()),
        Err(Undelivered::Backend(BackendError::Absent { .. }))
    ));
}

#[test]
fn a_path_that_is_no_carry_store_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let absent = |path: &Path, why: &str| {
        let refused = Carry::read(path).expect_err("not a store");
        assert_eq!(
            refused.to_string(),
            format!("{} is not a carry store: {why}", path.display())
        );
    };
    absent(&dir.path().join("missing"), "nothing is there");
    absent(dir.path(), "it is a directory");
    let empty = dir.path().join("empty");
    std::fs::write(&empty, "").expect("written");
    absent(
        &empty,
        "it is empty, and a carry store starts with its header line",
    );
    let prose = dir.path().join("prose");
    std::fs::write(&prose, "a shopping list\nmilk\n").expect("written");
    absent(&prose, "its first line is not a carry store's header");
    let unended = dir.path().join("unended");
    std::fs::write(&unended, "no newline at all").expect("written");
    absent(&unended, "it has no header line");

    // Carrying into a file that is not a store refuses rather than appending.
    let sender = Carry::sender::<Order, Receipt>(&prose);
    assert!(matches!(
        sender.send(order("x")),
        Err(Undelivered::Backend(BackendError::Absent { .. }))
    ));
    assert_eq!(
        std::fs::read_to_string(&prose).expect("read"),
        "a shopping list\nmilk\n"
    );
    assert!(matches!(
        Carry::sender::<Order, Receipt>(dir.path()).send(order("x")),
        Err(Undelivered::Backend(BackendError::Absent { .. }))
    ));

    let newer = dir.path().join("newer");
    std::fs::write(
        &newer,
        "{\"schema_version\":2,\"kind\":\"onemessagebus-carry-store\"}\n",
    )
    .expect("written");
    assert!(matches!(
        Carry::read(&newer),
        Err(BackendError::Unreadable { why, .. }) if why.contains("schema_version 2")
    ));
    let torn = dir.path().join("torn");
    std::fs::write(
        &torn,
        "{\"schema_version\":1,\"kind\":\"onemessagebus-carry-store\"}\n{\"ts\":",
    )
    .expect("written");
    assert!(matches!(
        Carry::read(&torn),
        Err(BackendError::Unreadable { why, .. }) if why.contains("torn")
    ));
    let garbled = dir.path().join("garbled");
    std::fs::write(
        &garbled,
        "{\"schema_version\":1,\"kind\":\"onemessagebus-carry-store\"}\nnot a record\n",
    )
    .expect("written");
    assert!(matches!(
        Carry::read(&garbled),
        Err(BackendError::Unreadable { why, .. }) if why.starts_with("record 1")
    ));
}
