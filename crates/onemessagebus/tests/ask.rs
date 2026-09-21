//! Contract A in the library: a question asked on a queue, and the only
//! answers it can have.
//!
//! Every journey here asks through a `Bus` a configuration file resolves, over a
//! local transport in a scratch directory, with typed questions and rulings of
//! this test's own — and answers, or does not, through the same `Bus`, the way
//! a second process would.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use onemessagebus::{
    Answer, AskOptions, Asker, Bus, BusError, Config, Correlation, Layouts, Lifetime, Message,
    Position, QueueError, QueueName, RefusalKind, SchemaId, TransportKinds, ValidationContext,
    Validator, Verdict,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Question {
    kind: String,
    message: String,
}

impl Message for Question {
    const SCHEMA: SchemaId = SchemaId::literal("test", "question", 1);
}

fn question(message: &str) -> Question {
    Question {
        kind: "question".to_owned(),
        message: message.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Ruling {
    completion: bool,
    reason: String,
    correlation: Correlation,
}

impl Message for Ruling {
    const SCHEMA: SchemaId = SchemaId::literal("test", "ruling", 1);
}

fn ruling(reason: &str) -> Value {
    json!({"completion": true, "reason": reason})
}

struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a scratch directory"),
        }
    }

    fn config(&self) -> Config {
        Config::parse(&format!(
            "version: 1\ntransport: {{kind: local, dir: {}}}\nqueues:\n  questions: {{policy: {{hold_pending: true, blocking_first: true}}, answers: replies}}\n  replies: {{}}\n  notes: {{}}\n  unanswered: {{policy: {{hold_pending: true}}}}\n",
            serde_json::to_string(&self.dir.path().join("channel")).expect("a path")
        ))
        .expect("the configuration loads")
    }

    fn bus(&self) -> Bus {
        self.config()
            .resolve(&Layouts::new(), &TransportKinds::builtin())
            .expect("the configuration resolves")
    }

    fn lines(&self, queue: &str) -> Vec<Value> {
        std::fs::read_to_string(
            self.dir
                .path()
                .join("channel")
                .join(format!("{queue}.jsonl")),
        )
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
    }
}

fn queue(name: &str) -> QueueName {
    name.parse().expect("a queue name")
}

fn asker(name: &str) -> Asker {
    Asker::new(name, "the test").expect("an asker")
}

const SHORT: Duration = Duration::from_millis(300);
const LONG: Duration = Duration::from_secs(20);

#[test]
fn an_ask_is_stamped_with_a_minted_correlation_nothing_else_carries() {
    let rig = Rig::new();
    let bus = rig.bus();
    let first = bus
        .ask::<Question, Ruling>(
            &queue("questions"),
            question("is the base right?"),
            AskOptions {
                blocking: true,
                asker: Some(asker("worker-1")),
                about: Some("build".parse().expect("an address")),
            },
        )
        .expect("asked");
    let second = bus
        .ask::<Question, Ruling>(
            &queue("questions"),
            question("which port?"),
            AskOptions::default(),
        )
        .expect("asked");
    assert_ne!(first.correlation(), second.correlation());
    assert_eq!(first.queue(), &queue("questions"));
    assert_eq!((first.id(), second.id()), (0, 1));
    assert_eq!(first.asker(), Some(&asker("worker-1")));
    assert_ne!(first.position(), second.position());
    let shown = format!("{first:?}");
    assert!(
        shown.contains(first.correlation().as_str())
            && shown.contains("questions")
            && shown.contains("id: 0"),
        "a pending ask's debug form does not say which ask it is: {shown}"
    );
    let logged = rig.lines("questions");
    assert_eq!(
        logged[0],
        json!({
            "event": "queued",
            "id": 0,
            "kind": "question",
            "message": "is the base right?",
            "blocking": true,
            "asker": "worker-1",
            "about": "build",
            "correlation": first.correlation().as_str(),
        })
    );
    assert_eq!(logged[1]["blocking"], json!(false));
    assert!(logged[1].get("asker").is_none());

    let minted = Correlation::mint();
    assert!(minted.as_str().starts_with("c-") && minted.as_str().len() == 34);
    assert_eq!(
        minted
            .as_str()
            .parse::<Correlation>()
            .expect("a minted one parses"),
        minted
    );
    for (text, names) in [
        ("", "it is empty"),
        ("-leading", "does not start with a letter or a digit"),
        ("has space", "carries a character"),
        (&"x".repeat(129), "longer than"),
    ] {
        let refused = text.parse::<Correlation>().expect_err(text);
        assert!(refused.to_string().contains(names), "{text}: {refused}");
    }
    for (text, names) in [
        ("  ", "blank"),
        ("a\nb", "control character"),
        (&"x".repeat(513), "longer than"),
    ] {
        let refused = text.parse::<onemessagebus::Address>().expect_err(text);
        assert!(refused.to_string().contains(names), "{text:?}: {refused}");
    }
    // An address read from a document is held to the same rules.
    let read: onemessagebus::Address = serde_json::from_value(json!("build")).expect("an address");
    assert_eq!(read.as_str(), "build");
    let refused = serde_json::from_value::<onemessagebus::Address>(json!("a\nb"))
        .expect_err("a control character");
    assert!(
        refused.to_string().contains("control character"),
        "{refused}"
    );
}

#[test]
fn only_a_reply_echoing_the_correlation_answers_and_a_wait_that_elapses_is_a_timeout_that_appends_nothing(
) {
    let rig = Rig::new();
    let bus = rig.bus();
    let unanswered = bus
        .ask::<Question, Ruling>(
            &queue("questions"),
            question("anyone?"),
            AskOptions::default(),
        )
        .expect("asked");
    let answered = bus
        .ask::<Question, Ruling>(&queue("questions"), question("you?"), AskOptions::default())
        .expect("asked");
    let bound = bus
        .reply(
            &queue("questions"),
            Some(answered.correlation()),
            ruling("yes, me"),
        )
        .expect("bound");
    assert_eq!(bound.correlation.as_ref(), Some(answered.correlation()));
    assert!(bound.answered);
    assert_eq!(bound.question.id, Some(1));
    assert_eq!(rig.lines("replies").len(), 1);

    assert_eq!(unanswered.wait(SHORT), Answer::Timeout);
    assert_eq!(
        rig.lines("replies").len(),
        1,
        "a wait appended to, or took from, the reply queue"
    );
    assert!(!unanswered.is_abandoned().expect("a read"));
    let status = bus
        .queue(&queue("questions"))
        .expect("a queue")
        .status()
        .expect("a status");
    assert_eq!(
        status.unread, 2,
        "an elapsed wait stopped counting its question"
    );

    match answered.wait(LONG) {
        Answer::Reply(reply) => {
            assert_eq!(reply.reason, "yes, me");
            assert_eq!(&reply.correlation, answered.correlation());
        }
        other => panic!("not the reply: {other:?}"),
    }

    // A session bound is a wait that ends where the session does: the same
    // timeout, and the question stays counted.
    let session_left = Duration::ZERO;
    assert_eq!(unanswered.wait(session_left), Answer::Timeout);
    assert!(!unanswered.is_abandoned().expect("a read"));
    assert_eq!(Answer::<Ruling>::Timeout.word(), "timeout");
    assert_eq!(Answer::<Ruling>::Abandoned.word(), "abandoned");
}

#[test]
fn an_abandoned_listener_answers_abandoned_until_its_asker_rearms_and_then_the_eventual_reply() {
    let rig = Rig::new();
    let bus = rig.bus();
    let pending = bus
        .ask::<Question, Ruling>(
            &queue("questions"),
            question("still there?"),
            AskOptions {
                asker: Some(asker("worker-1")),
                ..AskOptions::default()
            },
        )
        .expect("asked");
    assert_eq!(pending.wait(SHORT), Answer::Timeout);
    pending.abandon().expect("abandoned");
    assert!(pending.is_abandoned().expect("a read"));
    let status = bus
        .queue(&queue("questions"))
        .expect("a queue")
        .status()
        .expect("a status");
    assert_eq!(
        status.abandoned.len(),
        1,
        "the abandonment is not visible: {status:?}"
    );

    let started = Instant::now();
    assert_eq!(pending.wait(LONG), Answer::Abandoned);
    assert!(
        started.elapsed() < LONG / 2,
        "an abandoned question was waited on"
    );
    for lifetime in [Lifetime::Session, Lifetime::Durable(asker("worker-2"))] {
        let listener = bus
            .listen::<Ruling>(&queue("questions"), pending.correlation(), &lifetime)
            .expect("listens");
        assert_eq!(listener.wait(LONG), Answer::Abandoned);
    }
    assert!(rig.lines("replies").is_empty());
    let unknown = bus
        .listen::<Ruling>(
            &queue("questions"),
            &Correlation::mint(),
            &Lifetime::Session,
        )
        .expect_err("a correlation nothing carries");
    assert!(matches!(unknown, BusError::Unbound { .. }), "{unknown}");

    let rearmed = bus
        .listen::<Ruling>(
            &queue("questions"),
            pending.correlation(),
            &Lifetime::Durable(asker("worker-1")),
        )
        .expect("re-arms");
    assert!(!rearmed.is_abandoned().expect("a read"));
    let replier = rig.bus();
    let correlation = pending.correlation().clone();
    let replying = std::thread::spawn(move || {
        std::thread::sleep(SHORT);
        replier
            .reply(&queue("questions"), Some(&correlation), ruling("here"))
            .expect("bound")
    });
    match rearmed.wait(LONG) {
        Answer::Reply(reply) => assert_eq!(reply.reason, "here"),
        other => panic!("the re-armed wait did not receive the reply: {other:?}"),
    }
    replying.join().expect("the replier finishes");

    pending.abandon().expect("abandoned again");
    pending.rearm().expect("re-armed");
    assert!(!pending.is_abandoned().expect("a read"));
}

#[test]
fn a_reply_for_an_abandoned_question_is_still_matched_to_it() {
    let rig = Rig::new();
    let bus = rig.bus();
    let pending = bus
        .ask::<Question, Ruling>(
            &queue("questions"),
            question("late?"),
            AskOptions::default(),
        )
        .expect("asked");
    pending.abandon().expect("abandoned");
    bus.reply(
        &queue("questions"),
        Some(pending.correlation()),
        ruling("late"),
    )
    .expect("an abandoned ask is still pending an answer");
    match pending.wait(SHORT) {
        Answer::Reply(reply) => assert_eq!(reply.reason, "late"),
        other => panic!("the late answer was dropped: {other:?}"),
    }
}

#[test]
fn a_reply_record_that_is_not_an_r_answers_refused_naming_the_id_and_pointer_never_reply() {
    let rig = Rig::new();
    let bus = rig.bus();
    let pending = bus
        .ask::<Question, Ruling>(&queue("questions"), question("yes?"), AskOptions::default())
        .expect("asked");
    let forged = json!({"completion": "yes", "reason": "forged", "correlation": pending.correlation().as_str()});
    bus.transport()
        .append(&queue("replies"), forged.to_string().as_bytes())
        .expect("a writer that bypassed the bus");
    match pending.wait(SHORT) {
        Answer::Refused(refusal) => {
            assert_eq!(refusal.kind, RefusalKind::Schema);
            assert!(
                refusal.reason.contains("test.ruling@1") && refusal.reason.contains("/completion"),
                "{refusal}"
            );
        }
        other => panic!("a record that is not a ruling answered {other:?}"),
    }
}

#[test]
fn a_reply_binds_by_correlation_is_refused_unbound_and_binds_the_one_pending_ask() {
    let rig = Rig::new();
    let bus = rig.bus();
    let stranger = Correlation::mint();
    let refused = bus
        .reply(&queue("questions"), Some(&stranger), ruling("to nobody"))
        .expect_err("unbound");
    assert!(
        matches!(&refused, BusError::Unbound { .. })
            && refused.to_string().contains(stranger.as_str()),
        "{refused}"
    );
    let nothing = bus
        .reply(&queue("questions"), None, ruling("to whichever"))
        .expect_err("nothing pending");
    assert!(
        nothing.to_string().contains("no ask is pending"),
        "{nothing}"
    );

    let first = bus
        .ask::<Question, Ruling>(&queue("questions"), question("one"), AskOptions::default())
        .expect("asked");
    let second = bus
        .ask::<Question, Ruling>(&queue("questions"), question("two"), AskOptions::default())
        .expect("asked");
    let ambiguous = bus
        .reply(&queue("questions"), None, ruling("both?"))
        .expect_err("two pending");
    assert!(
        ambiguous.to_string().contains("2 asks are pending")
            && ambiguous.to_string().contains(first.correlation().as_str())
            && ambiguous
                .to_string()
                .contains(second.correlation().as_str()),
        "{ambiguous}"
    );
    assert!(
        rig.lines("replies").is_empty(),
        "a refused reply was appended"
    );

    bus.reply(
        &queue("questions"),
        Some(first.correlation()),
        ruling("one"),
    )
    .expect("bound");
    let twice = bus
        .reply(
            &queue("questions"),
            Some(first.correlation()),
            ruling("again"),
        )
        .expect_err("an answered ask is not pending");
    assert!(
        twice.to_string().contains(first.correlation().as_str()),
        "{twice}"
    );
    let only = bus
        .reply(&queue("questions"), None, ruling("the one left"))
        .expect("binds to the one pending");
    assert_eq!(only.correlation.as_ref(), Some(second.correlation()));
    assert_eq!(rig.lines("replies").len(), 2);
}

#[test]
fn a_record_that_is_not_an_object_is_refused_before_it_is_bound_judged_or_appended() {
    let rig = Rig::new();
    let bus = rig.bus();
    let not_an_object =
        |failure: &BusError| matches!(failure, BusError::Queue(QueueError::NotAnObject { .. }));
    // Nothing is pending, and the shape is what is refused, not the binding.
    let shapeless = bus
        .reply(&queue("questions"), None, json!(["to", "whichever"]))
        .expect_err("not an object");
    assert!(not_an_object(&shapeless), "{shapeless}");

    let pending = bus
        .ask::<Question, Ruling>(&queue("questions"), question("one"), AskOptions::default())
        .expect("asked");
    for reply in [json!([true, "one"]), json!("one"), Value::Null] {
        let refused = bus
            .reply(
                &queue("questions"),
                Some(pending.correlation()),
                reply.clone(),
            )
            .expect_err("not an object");
        assert!(
            not_an_object(&refused) && refused.to_string().starts_with("replies: "),
            "{reply}: {refused}"
        );
    }
    let noted = bus
        .send(&queue("notes"), json!({"note": "a plain log"}))
        .expect("sent")
        .remove(0)
        .1;
    let at = bus
        .reply_at(&queue("questions"), &noted.position, json!([1]))
        .expect_err("not an object");
    assert!(not_an_object(&at), "{at}");
    for refused in [
        bus.validate(&queue("questions"), json!(["one"]))
            .expect_err("not an object"),
        bus.send(&queue("questions"), json!(1))
            .expect_err("not an object"),
    ] {
        assert!(
            not_an_object(&refused) && refused.to_string().starts_with("questions: "),
            "{refused}"
        );
    }
    // A plain log keeps whatever JSON it is given.
    assert_eq!(
        bus.validate(&queue("notes"), json!(["one"]))
            .expect("judged"),
        Verdict::Pass
    );

    assert!(
        rig.lines("replies").is_empty(),
        "a refused reply was appended"
    );
    assert!(matches!(pending.wait(SHORT), Answer::Timeout));
}

#[test]
fn a_blocking_question_held_pending_is_released_by_its_reply_by_correlation_or_by_position() {
    let rig = Rig::new();
    let bus = rig.bus();
    let questions = bus.queue(&queue("questions")).expect("a queue");
    let blocking = AskOptions {
        blocking: true,
        ..AskOptions::default()
    };
    let first = bus
        .ask::<Question, Ruling>(&queue("questions"), question("hold me"), blocking.clone())
        .expect("asked");
    let claimed = questions
        .claim(&onemessagebus::ConsumerName::default_consumer())
        .expect("a claim")
        .expect("claimed");
    assert_eq!(claimed.id, Some(first.id()));
    assert!(questions.held().expect("a read").is_some());
    bus.reply(
        &queue("questions"),
        Some(first.correlation()),
        ruling("released"),
    )
    .expect("bound");
    assert!(
        questions.held().expect("a read").is_none(),
        "the slot was not released"
    );

    let second = bus
        .ask::<Question, Ruling>(&queue("questions"), question("by position"), blocking)
        .expect("asked");
    let claimed = questions
        .claim(&onemessagebus::ConsumerName::default_consumer())
        .expect("a claim")
        .expect("claimed");
    let wrong = bus
        .reply_at(&queue("questions"), &Position::from_token(1), ruling("x"))
        .expect_err("not claimed there");
    assert!(
        matches!(wrong, BusError::Queue(QueueError::NotPending { .. })),
        "{wrong}"
    );
    let bound = bus
        .reply_at(
            &queue("questions"),
            &claimed.position,
            ruling("by position"),
        )
        .expect("answered");
    assert_eq!(bound.correlation.as_ref(), Some(second.correlation()));
    assert!(questions.held().expect("a read").is_none());
    let late = bus
        .reply_at(&queue("questions"), &claimed.position, ruling("too late"))
        .expect_err("another reply answered it first");
    assert!(
        matches!(late, BusError::Unbound { .. })
            && late
                .to_string()
                .contains("was answered by another reply first"),
        "{late}"
    );
    match second.wait(SHORT) {
        Answer::Reply(reply) => assert_eq!(reply.reason, "by position"),
        other => panic!("a reply by position did not carry the correlation: {other:?}"),
    }
}

#[test]
fn a_reply_by_position_after_another_answered_an_abandoned_claim_is_appended_and_answers_nothing() {
    let rig = Rig::new();
    let bus = rig.bus();
    let questions = bus.queue(&queue("questions")).expect("a queue");
    let pending = bus
        .ask::<Question, Ruling>(
            &queue("questions"),
            question("who answers?"),
            AskOptions {
                blocking: true,
                ..AskOptions::default()
            },
        )
        .expect("asked");
    let claimed = questions
        .claim(&onemessagebus::ConsumerName::default_consumer())
        .expect("a claim")
        .expect("claimed");
    // An abandoned question is still answered, and a reply that loses the race
    // for it still learns that another reply answered first.
    pending.abandon().expect("abandoned");
    bus.reply_at(&queue("questions"), &claimed.position, ruling("first"))
        .expect("answered");
    let late = bus
        .reply_at(&queue("questions"), &claimed.position, ruling("second"))
        .expect_err("another reply answered it first");
    assert!(matches!(late, BusError::Unbound { .. }), "{late}");
    assert!(
        late.to_string()
            .contains("was answered by another reply first"),
        "{late}"
    );
    assert_eq!(
        rig.lines("replies").len(),
        2,
        "the losing reply is appended"
    );
    assert_eq!(
        rig.lines("questions")
            .iter()
            .filter(|line| line["event"] == json!("answered"))
            .count(),
        1,
        "the question was answered more than once"
    );
}

#[test]
fn a_question_that_does_not_satisfy_its_own_schema_is_refused_and_nothing_is_appended() {
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct Titled {
        kind: String,
        #[schemars(length(min = 1))]
        message: String,
    }

    impl Message for Titled {
        const SCHEMA: SchemaId = SchemaId::literal("test", "titled-question", 1);
    }

    let rig = Rig::new();
    let bus = rig.bus();
    let refused = bus
        .ask::<Titled, Ruling>(
            &queue("questions"),
            Titled {
                kind: "question".to_owned(),
                message: String::new(),
            },
            AskOptions::default(),
        )
        .expect_err("an empty message does not satisfy the question's schema");
    assert!(
        matches!(refused, BusError::Queue(QueueError::Violation { .. })),
        "{refused}"
    );
    assert!(refused.to_string().contains("message"), "{refused}");
    assert!(
        rig.lines("questions").is_empty(),
        "a refused question was appended"
    );
}

#[test]
fn a_queue_that_keeps_no_events_or_answers_nowhere_is_not_asked_on() {
    let rig = Rig::new();
    let bus = rig.bus();
    let plain = bus
        .ask::<Question, Ruling>(&queue("notes"), question("x"), AskOptions::default())
        .expect_err("a plain queue");
    assert!(
        matches!(plain, BusError::Queue(QueueError::NotAnEventQueue { .. })),
        "{plain}"
    );
    let nowhere = bus
        .ask::<Question, Ruling>(&queue("unanswered"), question("x"), AskOptions::default())
        .expect_err("no answers queue");
    assert!(matches!(nowhere, BusError::NotAskable { .. }), "{nowhere}");
    assert!(nowhere.to_string().contains("answers"), "{nowhere}");
    let undeclared = bus
        .reply(&queue("elsewhere"), None, ruling("x"))
        .expect_err("an undeclared queue");
    assert!(
        matches!(undeclared, BusError::UnknownQueue { .. }),
        "{undeclared}"
    );
}

/// Each judgement a validator made: the queue it judged for, and the
/// correlation it was told.
type Seen = Arc<Mutex<Vec<(String, Option<Correlation>)>>>;

/// A validator recording the correlation each judgement was told, refusing a
/// question whose message is `refuse me`.
struct Recording(Seen);

impl Validator<Value> for Recording {
    fn validate(&self, message: &Value, context: &ValidationContext) -> Verdict {
        self.0
            .lock()
            .expect("the record")
            .push((context.queue.to_string(), context.correlation.clone()));
        if message["message"] == json!("refuse me") {
            Verdict::Refuse {
                reason: "not this question".to_owned(),
            }
        } else {
            Verdict::Pass
        }
    }
}

#[test]
fn validators_judge_a_question_and_a_reply_before_anything_is_appended_knowing_the_correlation() {
    let rig = Rig::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let bus = rig
        .bus()
        .with_validator(&queue("questions"), Recording(Arc::clone(&seen)))
        .expect("a declared queue")
        .with_validator(&queue("replies"), Recording(Arc::clone(&seen)))
        .expect("a declared queue");
    let refused = bus
        .ask::<Question, Ruling>(
            &queue("questions"),
            question("refuse me"),
            AskOptions::default(),
        )
        .expect_err("refused");
    assert!(
        matches!(&refused, BusError::Queue(QueueError::Refused { reason, .. }) if reason == "not this question"),
        "{refused}"
    );
    assert!(
        rig.lines("questions").is_empty(),
        "a refused question was appended"
    );
    let pending = bus
        .ask::<Question, Ruling>(&queue("questions"), question("fine"), AskOptions::default())
        .expect("asked");
    bus.reply(
        &queue("questions"),
        Some(pending.correlation()),
        ruling("ok"),
    )
    .expect("bound");
    let seen = seen.lock().expect("the record").clone();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(
        seen[0].1.is_some(),
        "the refused question's judgement had no correlation"
    );
    assert_eq!(
        seen[1],
        ("questions".to_owned(), Some(pending.correlation().clone()))
    );
    assert_eq!(
        seen[2],
        ("replies".to_owned(), Some(pending.correlation().clone()))
    );
}
