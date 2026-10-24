//! A long-lived queue's cost: a claim, a cursor read and a submit on a local
//! queue read what they need, never the history before it.
//!
//! Every count here is the transport's own ([`LocalTransport::reads`]): the
//! bytes it read off the queue's files and the records it examined in them. A
//! clock would measure the host, which is shared.
//!
//! A queue is built the way onemessagebus 0.9.3 leaves one — its records, and a
//! cursor holding how many of them the consumer has claimed — so the first read
//! of a position is the one-time recovery from a line-count cursor alone, and
//! every operation after it is the steady state.

use std::path::Path;
use std::sync::Arc;

use onemessagebus::{
    ConsumerName, LocalTransport, Policy, QueueName, QueueSpec, RawQueue, Reads, Registry,
    Transport,
};
use serde_json::{json, Value};

/// The two histories compared: a hundred times apart in envelope count.
const SMALL: usize = 40;
const LARGE: usize = SMALL * 100;

/// How many submit-then-claim rounds the steady state is measured over.
const ROUNDS: usize = 3;

/// How many bytes two histories' steady operations may differ by: the digits of
/// a larger id and a larger count, and nothing that grows with the history.
const DIGITS: u64 = 64;

/// What one steady operation may read at most, whatever the history: a cursor,
/// a checkpoint, the record before a position, and the records not yet claimed.
const BOUND: Reads = Reads {
    bytes: 4096,
    records: 8,
};

fn commands_queue() -> QueueName {
    "commands".parse().expect("a queue name")
}

/// The run's command queue as onepipeline declares it: plain and numbered.
fn commands(transport: &LocalTransport) -> RawQueue {
    let mut spec = QueueSpec::new(commands_queue(), Policy::default());
    spec.numbered = true;
    RawQueue::open(Arc::new(transport.clone()), spec, Arc::new(Registry::new()))
}

/// One envelope as a submitter offers it.
fn envelope(n: usize) -> Value {
    json!({"author": "planner", "commands": [{"op": "note", "text": format!("edit {n}")}]})
}

/// A command queue as 0.9.3 leaves it after `answered` envelopes were submitted
/// and claimed: one numbered record per line, and the default consumer's cursor
/// holding their count, pretty-printed.
fn answered_history(dir: &Path, answered: usize) {
    let mut records = String::new();
    for id in 0..answered {
        let mut record = envelope(id);
        record["id"] = json!(id);
        records.push_str(&serde_json::to_string(&record).expect("serializes"));
        records.push('\n');
    }
    std::fs::write(dir.join("commands.jsonl"), records).expect("the records");
    std::fs::write(
        dir.join("commands-cursor.json"),
        serde_json::to_string_pretty(&answered).expect("serializes"),
    )
    .expect("the cursor");
}

/// What `op` read, as the transport counted it.
fn cost<T>(transport: &LocalTransport, op: impl FnOnce() -> T) -> (Reads, T) {
    let before = transport.reads();
    let answered = op();
    let after = transport.reads();
    (
        Reads {
            bytes: after.bytes - before.bytes,
            records: after.records - before.records,
        },
        answered,
    )
}

/// The most of each count over several runs of one operation.
fn most(costs: &[Reads]) -> Reads {
    Reads {
        bytes: costs.iter().map(|cost| cost.bytes).max().unwrap_or(0),
        records: costs.iter().map(|cost| cost.records).max().unwrap_or(0),
    }
}

/// What each operation cost over one history.
#[derive(Debug)]
struct Costs {
    history_bytes: u64,
    recovering_cursor: Reads,
    recovering_submit: Reads,
    cursor: Reads,
    submit: Reads,
    claim: Reads,
    empty_claim: Reads,
    reopened_cursor: Reads,
    reopened_submit: Reads,
    reopened_claim: Reads,
}

fn costs_over(answered: usize) -> Costs {
    let dir = tempfile::tempdir().expect("a scratch directory");
    answered_history(dir.path(), answered);
    let history_bytes = std::fs::metadata(dir.path().join("commands.jsonl"))
        .expect("the records")
        .len();
    let default = ConsumerName::default_consumer();
    let transport = LocalTransport::open(dir.path()).expect("opens");
    let queue = commands(&transport);

    let (recovering_cursor, at) = cost(&transport, || {
        transport
            .cursor(&commands_queue(), &default)
            .expect("reads")
    });
    let end = Some(onemessagebus::Position::from_token(history_bytes));
    assert_eq!(
        at, end,
        "the 0.9.3 cursor does not resume after its history"
    );
    let (recovering_submit, pushed) = cost(&transport, || {
        queue.push(envelope(answered)).expect("submits")
    });
    assert_eq!(
        pushed.id,
        Some(answered as u64),
        "the submit took the wrong id"
    );
    let (first_claim, claimed) = cost(&transport, || queue.claim(&default).expect("claims"));
    let claimed = claimed.expect("the submitted envelope is claimed");
    assert_eq!(claimed.id, Some(answered as u64));

    let mut cursors = Vec::new();
    let mut submits = Vec::new();
    let mut claims = vec![first_claim];
    for round in 1..=ROUNDS {
        let id = answered + round;
        let (submit, pushed) = cost(&transport, || queue.push(envelope(id)).expect("submits"));
        assert_eq!(pushed.id, Some(id as u64));
        submits.push(submit);
        let (cursor, _) = cost(&transport, || {
            transport
                .cursor(&commands_queue(), &default)
                .expect("reads")
        });
        cursors.push(cursor);
        let (claim, claimed) = cost(&transport, || queue.claim(&default).expect("claims"));
        assert_eq!(
            claimed.expect("the submitted envelope is claimed").id,
            Some(id as u64),
            "a claim did not resume at the first envelope it had not claimed"
        );
        claims.push(claim);
    }
    let (empty_claim, nothing) = cost(&transport, || queue.claim(&default).expect("claims"));
    assert!(
        nothing.is_none(),
        "a claim handed back an answered envelope"
    );

    // A second process over the same directory: nothing carried in memory.
    let reopened = LocalTransport::open(dir.path()).expect("opens");
    let queue = commands(&reopened);
    let (reopened_cursor, _) = cost(&reopened, || {
        reopened.cursor(&commands_queue(), &default).expect("reads")
    });
    let id = answered + ROUNDS + 1;
    let (reopened_submit, pushed) = cost(&reopened, || queue.push(envelope(id)).expect("submits"));
    assert_eq!(pushed.id, Some(id as u64));
    let (reopened_claim, claimed) = cost(&reopened, || queue.claim(&default).expect("claims"));
    assert_eq!(claimed.expect("claimed").id, Some(id as u64));

    Costs {
        history_bytes,
        recovering_cursor,
        recovering_submit,
        cursor: most(&cursors),
        submit: most(&submits),
        claim: most(&claims),
        empty_claim,
        reopened_cursor,
        reopened_submit,
        reopened_claim,
    }
}

/// Once a consumer's position is established, a claim, a cursor read and a
/// submit read the same bytes and examine the same records over a history of
/// forty envelopes and of four thousand — in this process and in a fresh one —
/// and recovering a position from 0.9.3's line-count cursor reads the history
/// once.
#[test]
fn a_claim_a_cursor_read_and_a_submit_cost_the_same_however_long_the_history() {
    let small = costs_over(SMALL);
    let large = costs_over(LARGE);
    for costs in [&small, &large] {
        for (what, recovery) in [
            ("cursor read", costs.recovering_cursor),
            ("submit", costs.recovering_submit),
        ] {
            assert!(
                recovery.bytes >= costs.history_bytes,
                "the recovering {what} read {} bytes of a {}-byte history: it did not recover \
                 the position from the line-count cursor",
                recovery.bytes,
                costs.history_bytes
            );
        }
    }
    let steady = |costs: &Costs| {
        [
            ("cursor read", costs.cursor),
            ("submit", costs.submit),
            ("claim", costs.claim),
            ("claim finding nothing", costs.empty_claim),
            ("cursor read in a fresh process", costs.reopened_cursor),
            ("submit in a fresh process", costs.reopened_submit),
            ("claim in a fresh process", costs.reopened_claim),
        ]
    };
    for ((what, small), (_, large)) in steady(&small).into_iter().zip(steady(&large)) {
        assert!(
            large.bytes <= BOUND.bytes && large.records <= BOUND.records,
            "a {what} after {LARGE} answered envelopes read {} bytes and examined {} records, \
             past the bound of {} and {}",
            large.bytes,
            large.records,
            BOUND.bytes,
            BOUND.records
        );
        assert_eq!(
            large.records, small.records,
            "a {what} examined more records after {LARGE} answered envelopes than after {SMALL}"
        );
        assert!(
            large.bytes <= small.bytes + DIGITS,
            "a {what} read {} bytes after {LARGE} answered envelopes and {} after {SMALL}",
            large.bytes,
            small.bytes
        );
    }
}

/// onemessagebus 0.9.3's own reading and writing of a local cursor — what
/// `LocalTransport::cursor` and `LocalTransport::commit` computed at v0.9.3
/// (e879a4f), restated over the files: the records are the file's non-blank
/// lines, a count maps to the position after that many of them (after the last,
/// for a count past them), and a commit writes how many of them end before the
/// position, pretty-printed. A release from before the checkpoints reads and
/// writes nothing else.
mod v0_9_3 {
    use std::path::Path;

    fn lines(bytes: &[u8]) -> Vec<(usize, usize)> {
        let mut lines = Vec::new();
        let mut start = 0;
        while start < bytes.len() {
            match bytes[start..].iter().position(|byte| *byte == b'\n') {
                Some(offset) => {
                    let end = start + offset;
                    if !bytes[start..end].iter().all(u8::is_ascii_whitespace) {
                        lines.push((start, end));
                    }
                    start = end + 1;
                }
                None => break,
            }
        }
        lines
    }

    /// Where 0.9.3 resumes the consumer whose cursor is `cursor`.
    pub fn cursor(records: &Path, cursor: &Path) -> Option<u64> {
        let text = std::fs::read_to_string(cursor).ok()?;
        let count = serde_json::from_str::<u64>(text.trim()).ok()?;
        let bytes = std::fs::read(records).unwrap_or_default();
        let lines = lines(&bytes);
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        Some(match count {
            0 => 0,
            n => lines
                .get(n - 1)
                .or(lines.last())
                .map_or(0, |(_, end)| *end as u64 + 1),
        })
    }

    /// 0.9.3 committing the consumer whose cursor is `cursor` at `at`.
    pub fn commit(records: &Path, cursor: &Path, at: u64) {
        let bytes = std::fs::read(records).expect("the records");
        let count = lines(&bytes)
            .iter()
            .filter(|(_, stop)| (*stop as u64) < at)
            .count();
        std::fs::write(
            cursor,
            serde_json::to_string_pretty(&count).expect("serializes"),
        )
        .expect("the cursor");
    }

    /// The record 0.9.3 claims next from `at`: the first whole line after it.
    pub fn next(records: &Path, at: u64) -> Option<String> {
        let bytes = std::fs::read(records).expect("the records");
        let at = usize::try_from(at).expect("a position");
        lines(&bytes[at..])
            .first()
            .map(|(start, end)| String::from_utf8_lossy(&bytes[at + start..at + end]).into_owned())
    }
}

fn consumer(name: &str) -> ConsumerName {
    name.parse().expect("a consumer name")
}

/// The record a claim handed back, as the line it was written as.
fn line_of(record: &Value) -> String {
    serde_json::to_string(record).expect("serializes")
}

/// A queue 0.9.3 wrote — blank lines among its records, and cursors at the
/// start, part way, at the end and past the end — is read by this release with
/// every consumer resuming at exactly the position and record 0.9.3 resumes it
/// at; and the cursors this release then writes are read by 0.9.3 the same way,
/// a cursor 0.9.3 moves after that included.
#[test]
fn a_queue_and_cursors_0_9_3_wrote_and_this_release_writes_agree_in_both_directions() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let records = dir.path().join("commands.jsonl");
    std::fs::write(
        &records,
        "{\"id\":0}\n\n{\"id\":1}\n   \n{\"id\":2}\n{\"id\":3}\n\n{\"id\":4}\n{\"id\":5}\n",
    )
    .expect("the records");
    let cursors = [
        ("default", 0),
        ("first", 1),
        ("blank", 2),
        ("most", 5),
        ("all", 6),
        ("past", 40),
    ];
    let transport = LocalTransport::open(dir.path()).expect("opens");
    for (name, count) in cursors {
        let path = transport.cursor_path(&commands_queue(), &consumer(name));
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&count).expect("serializes"),
        )
        .expect("the cursor");
        let expected = v0_9_3::cursor(&records, &path).expect("0.9.3 reads it");
        for read in ["recovered", "checkpointed"] {
            assert_eq!(
                transport
                    .cursor(&commands_queue(), &consumer(name))
                    .expect("reads"),
                Some(onemessagebus::Position::from_token(expected)),
                "the {name} consumer's {read} position is not the one 0.9.3 resumes it at"
            );
        }
    }

    // This release claims for each consumer; 0.9.3 reads where each now is, and
    // claims the same record next.
    let queue = RawQueue::open(
        Arc::new(transport.clone()),
        QueueSpec::new(commands_queue(), Policy::default()),
        Arc::new(Registry::new()),
    );
    for (name, count) in cursors {
        let cursor = transport.cursor_path(&commands_queue(), &consumer(name));
        let before = v0_9_3::cursor(&records, &cursor).expect("0.9.3 reads it");
        let claimed = queue.claim(&consumer(name)).expect("claims");
        assert_eq!(
            claimed.as_ref().map(|claimed| line_of(&claimed.record)),
            v0_9_3::next(&records, before),
            "the {name} consumer (cursor {count}) claimed another record than 0.9.3 would"
        );
        let after = v0_9_3::cursor(&records, &cursor).expect("0.9.3 reads it");
        assert_eq!(
            Some(onemessagebus::Position::from_token(after)),
            transport
                .cursor(&commands_queue(), &consumer(name))
                .expect("reads"),
            "0.9.3 reads the {name} consumer's cursor at another position than this release wrote"
        );
        assert_eq!(
            claimed.map(|claimed| claimed.position.token()),
            v0_9_3::next(&records, before).map(|_| after),
            "the {name} consumer's cursor does not sit just after what it claimed"
        );
    }

    // 0.9.3 moves a cursor back and forth under the checkpoint this release kept
    // for it, and a numbered push lands after records 0.9.3 appended; this
    // release follows the cursor file, and numbers by the log.
    let most = transport.cursor_path(&commands_queue(), &consumer("most"));
    for at in [10, 0, 38, 47] {
        v0_9_3::commit(&records, &most, at);
        assert_eq!(
            transport
                .cursor(&commands_queue(), &consumer("most"))
                .expect("reads"),
            v0_9_3::cursor(&records, &most).map(onemessagebus::Position::from_token),
            "this release did not follow a cursor 0.9.3 moved to {at}"
        );
    }
    let numbered = commands(&transport);
    assert_eq!(numbered.push(envelope(6)).expect("pushes").id, Some(6));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&records)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"{\"id\":7}\n"))
        .expect("0.9.3 appends");
    let before = std::fs::metadata(&records).expect("the records").len();
    let pushed = numbered.push(envelope(8)).expect("pushes");
    assert_eq!(pushed.id, Some(8));
    assert_eq!(
        v0_9_3::next(&records, before),
        Some(line_of(&pushed.record)),
        "0.9.3 does not read the pushed record as the one line after the last"
    );
    assert!(
        std::fs::read_to_string(&records)
            .expect("the records")
            .ends_with(&format!("{{\"id\":7}}\n{}\n", line_of(&pushed.record))),
        "the records file is not the one-line-per-record file 0.9.3 reads"
    );
}

/// What reading `consumer`'s position cost, and the position.
fn cursor_cost(transport: &LocalTransport, name: &str) -> (Reads, Option<onemessagebus::Position>) {
    cost(transport, || {
        transport
            .cursor(&commands_queue(), &consumer(name))
            .expect("reads")
    })
}

/// A consumer's position stays where its line cursor puts it when its offset
/// checkpoint is missing, was edited by hand, is cut short, or no longer
/// matches a cursor another release moved — each read from the cursor's count,
/// over the whole log once, after which the checkpoint holds again — and when
/// the log ends in a torn, partly written record, which no claim hands out
/// until its writer finishes it and which an append heals away uncounted.
#[test]
fn a_position_falls_back_to_its_line_cursor_whatever_became_of_its_checkpoint() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    answered_history(dir.path(), SMALL);
    let records = dir.path().join("commands.jsonl");
    let history = std::fs::metadata(&records).expect("the records").len();
    let transport = LocalTransport::open(dir.path()).expect("opens");
    let cursor = transport.cursor_path(&commands_queue(), &ConsumerName::default_consumer());
    let offset = dir.path().join("commands-cursor.json.offset");
    let at = Some(onemessagebus::Position::from_token(history));

    let recovered = |why: &str, read: (Reads, Option<onemessagebus::Position>)| {
        assert_eq!(read.1, at, "with {why}, the position moved");
        assert!(
            read.0.bytes >= history,
            "with {why}, the position was not read from the line cursor ({} bytes read)",
            read.0.bytes
        );
        let (again, position) = cursor_cost(&transport, "default");
        assert_eq!(position, at);
        assert!(
            again.bytes < history && again.records <= BOUND.records,
            "with {why}, the checkpoint was not kept again: the next read cost {again:?}"
        );
    };

    recovered("no checkpoint yet", cursor_cost(&transport, "default"));
    assert!(offset.exists(), "the recovery kept no checkpoint");

    std::fs::remove_file(&offset).expect("deletes");
    recovered("its checkpoint deleted", cursor_cost(&transport, "default"));

    let kept: Value =
        serde_json::from_slice(&std::fs::read(&offset).expect("the checkpoint")).expect("JSON");
    let mut wrong = kept.clone();
    wrong["count"] = json!(SMALL - 1);
    std::fs::write(&offset, wrong.to_string()).expect("writes");
    recovered(
        "a checkpoint edited by hand to another count",
        cursor_cost(&transport, "default"),
    );

    let mut wrong = kept.clone();
    wrong["offset"] = json!(history / 2);
    std::fs::write(&offset, wrong.to_string()).expect("writes");
    recovered(
        "a checkpoint edited by hand to another offset",
        cursor_cost(&transport, "default"),
    );

    let mut wrong = kept.clone();
    wrong["digest"] = json!(kept["digest"].as_u64().expect("a digest").wrapping_add(1));
    std::fs::write(&offset, wrong.to_string()).expect("writes");
    recovered(
        "a checkpoint edited by hand to another record's digest",
        cursor_cost(&transport, "default"),
    );

    std::fs::write(&offset, "{\"count\": ").expect("writes");
    recovered("a checkpoint cut short", cursor_cost(&transport, "default"));

    // The cursor itself moved by a release that keeps no checkpoint: the line
    // count wins over the checkpoint it no longer matches.
    std::fs::write(&cursor, "3").expect("an older release's cursor");
    let (_, position) = cursor_cost(&transport, "default");
    assert_eq!(
        position,
        v0_9_3::cursor(&records, &cursor).map(onemessagebus::Position::from_token)
    );
    std::fs::write(&cursor, SMALL.to_string()).expect("put back");

    // A torn record after the position: the position holds, a claim hands out
    // nothing, and the record is claimed once its writer finishes it.
    let queue = commands(&transport);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&records)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"{\"id\":40,\"half"))
        .expect("a torn record");
    assert_eq!(
        cursor_cost(&transport, "default").1,
        at,
        "a torn tail moved the position"
    );
    assert_eq!(
        transport.count(&commands_queue()).expect("counts"),
        SMALL as u64
    );
    let (claim, claimed) = cost(&transport, || {
        transport
            .read(&commands_queue(), at.as_ref(), usize::MAX)
            .expect("reads")
    });
    assert!(
        claimed.records.is_empty(),
        "a torn record was read as whole"
    );
    assert!(claimed.torn.is_some(), "the torn record went unreported");
    assert!(
        claim.bytes < history,
        "reading past the position read the history"
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&records)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"\":1}\n"))
        .expect("its writer finishes it");
    let finished = queue
        .claim(&ConsumerName::default_consumer())
        .expect("claims")
        .expect("the finished record is claimed");
    assert_eq!(line_of(&finished.record), "{\"id\":40,\"half\":1}");
    assert_eq!(
        transport
            .cursor(&commands_queue(), &ConsumerName::default_consumer())
            .expect("reads"),
        Some(finished.position)
    );

    // A torn record an append heals away.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&records)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"{\"id\":41,\"never"))
        .expect("a torn record");
    let pushed = queue.push(envelope(41)).expect("the push heals the tail");
    assert_eq!(pushed.id, Some(41), "the torn record was counted");
    assert_eq!(
        queue
            .claim(&ConsumerName::default_consumer())
            .expect("claims")
            .and_then(|claimed| claimed.id),
        Some(41),
        "the claim after a healed tail did not resume at the next record"
    );
}

/// Every answer the transport gives — each id a numbered push allocates, each
/// record a claim hands out, each cursor file it writes — is the same whether
/// its checkpoints hold, are deleted after every operation, or are put back as
/// a concurrent process's older copy would leave them; and is the count a read
/// of the whole log gives.
#[test]
fn a_checkpoint_deleted_or_left_stale_changes_no_answer() {
    #[derive(Clone, Copy, Debug)]
    enum Meddling {
        None,
        Deleted,
        Stale,
    }
    let run = |meddling: Meddling| {
        let dir = tempfile::tempdir().expect("a scratch directory");
        answered_history(dir.path(), SMALL);
        let transport = LocalTransport::open(dir.path()).expect("opens");
        let queue = commands(&transport);
        let checkpoints = [
            dir.path().join("commands.jsonl.count"),
            dir.path().join("commands-cursor.json.offset"),
        ];
        let mut older: Vec<Option<Vec<u8>>> = vec![None, None];
        let mut answers = Vec::new();
        for n in 0..12 {
            let whole = transport
                .read(&commands_queue(), None, usize::MAX)
                .expect("reads")
                .records
                .len() as u64;
            let pushed = queue.push(envelope(n)).expect("pushes");
            assert_eq!(
                pushed.id,
                Some(whole),
                "{meddling:?}: an id is not the count before it"
            );
            answers.push(format!("pushed {:?}", pushed.id));
            if n % 3 != 0 {
                let claimed = queue
                    .claim(&ConsumerName::default_consumer())
                    .expect("claims")
                    .map(|claimed| (claimed.id, claimed.position));
                answers.push(format!("claimed {claimed:?}"));
            }
            answers.push(
                std::fs::read_to_string(dir.path().join("commands-cursor.json"))
                    .expect("the cursor"),
            );
            for (path, older) in checkpoints.iter().zip(older.iter_mut()) {
                match meddling {
                    Meddling::None => {}
                    Meddling::Deleted => {
                        let _ = std::fs::remove_file(path);
                    }
                    Meddling::Stale => {
                        let now = std::fs::read(path).ok();
                        if let Some(stale) = older.replace(now.clone().unwrap_or_default()) {
                            std::fs::write(path, stale).expect("a stale checkpoint");
                        }
                    }
                }
            }
        }
        answers
    };
    let kept = run(Meddling::None);
    assert_eq!(
        run(Meddling::Deleted),
        kept,
        "deleting the checkpoints changed an answer"
    );
    assert_eq!(
        run(Meddling::Stale),
        kept,
        "a stale checkpoint changed an answer"
    );
}

/// A driver claiming while replies submit, as two processes over one
/// directory: every envelope gets its own id in order, and is claimed exactly
/// once, in order.
#[test]
fn a_claimer_and_a_submitter_racing_over_one_queue_agree_on_every_id_and_claim() {
    const SUBMITTED: usize = 200;
    let dir = tempfile::tempdir().expect("a scratch directory");
    answered_history(dir.path(), SMALL);
    let claimer = LocalTransport::open(dir.path()).expect("opens");
    let submitter = LocalTransport::open(dir.path()).expect("opens");
    let claimed = std::thread::scope(|scope| {
        let claiming = scope.spawn(|| {
            let queue = commands(&claimer);
            (0..SUBMITTED)
                .filter_map(|_| {
                    queue
                        .claim(&ConsumerName::default_consumer())
                        .expect("claims")
                })
                .filter_map(|claimed| claimed.id)
                .collect::<Vec<_>>()
        });
        let queue = commands(&submitter);
        let ids: Vec<_> = (0..SUBMITTED)
            .map(|n| queue.push(envelope(n)).expect("submits").id)
            .collect();
        assert_eq!(
            ids,
            (SMALL..SMALL + SUBMITTED)
                .map(|id| Some(id as u64))
                .collect::<Vec<_>>(),
            "two submits took one id, or skipped one"
        );
        claiming.join().expect("the claimer")
    });
    let queue = commands(&claimer);
    let rest = std::iter::from_fn(|| {
        queue
            .claim(&ConsumerName::default_consumer())
            .expect("claims")
    })
    .filter_map(|claimed| claimed.id);
    let all: Vec<u64> = claimed.into_iter().chain(rest).collect();
    assert_eq!(
        all,
        (SMALL..SMALL + SUBMITTED)
            .map(|id| id as u64)
            .collect::<Vec<_>>(),
        "an envelope was claimed twice, out of order, or never"
    );
}

/// `Transport::count` is the number of whole records a read from the start
/// hands back, a torn tail not among them, on the local transport and on one
/// that keeps the trait's default.
#[test]
fn a_count_is_the_whole_records_a_read_from_the_start_hands_back() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let local = LocalTransport::open(dir.path()).expect("opens");
    let memory = onemessagebus::MemoryTransport::new();
    let transports: [&dyn Transport; 2] = [&local, &memory];
    for transport in transports {
        assert_eq!(transport.count(&commands_queue()).expect("counts"), 0);
        for n in 0..3 {
            transport
                .append(&commands_queue(), format!("{{\"n\":{n}}}").as_bytes())
                .expect("appends");
            assert_eq!(
                transport.count(&commands_queue()).expect("counts"),
                transport
                    .read(&commands_queue(), None, usize::MAX)
                    .expect("reads")
                    .records
                    .len() as u64
            );
        }
    }
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("commands.jsonl"))
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"\n{\"torn"))
        .expect("a torn record");
    assert_eq!(local.count(&commands_queue()).expect("counts"), 3);
}

/// Whether `transport`'s answers about the command queue are the ones a read of
/// the whole log gives: the default consumer's position as 0.9.3 reads its
/// cursor, and the next id as the count of records.
fn answers_as_the_log_does(transport: &LocalTransport, dir: &Path, why: &str) {
    let records = dir.join("commands.jsonl");
    let cursor = transport.cursor_path(&commands_queue(), &ConsumerName::default_consumer());
    assert_eq!(
        transport
            .cursor(&commands_queue(), &ConsumerName::default_consumer())
            .expect("reads"),
        v0_9_3::cursor(&records, &cursor).map(onemessagebus::Position::from_token),
        "{why}: the position is not where the line cursor puts it"
    );
    let whole = transport
        .read(&commands_queue(), None, usize::MAX)
        .expect("reads")
        .records
        .len() as u64;
    assert_eq!(
        transport.count(&commands_queue()).expect("counts"),
        whole,
        "{why}: the count is not the log's"
    );
    let pushed = commands(transport).push(envelope(0)).expect("pushes");
    assert_eq!(
        pushed.id,
        Some(whole),
        "{why}: a push took another id than the log's count"
    );
}

/// A transport over `dir` with both checkpoints in place and holding: one
/// push counted, and the default consumer committed at the end of the log, so a
/// count and a cursor read after it read none of the history.
fn checkpointed(dir: &Path) -> LocalTransport {
    let transport = LocalTransport::open(dir).expect("opens");
    let end = commands(&transport)
        .push(envelope(0))
        .expect("pushes")
        .position;
    transport
        .commit(&commands_queue(), &ConsumerName::default_consumer(), &end)
        .expect("commits");
    let (count, _) = cost(&transport, || {
        transport.count(&commands_queue()).expect("counts")
    });
    assert!(
        count.bytes < end.token(),
        "the count checkpoint does not hold"
    );
    let (cursor, _) = cursor_cost(&transport, "default");
    assert!(
        cursor.bytes < end.token(),
        "the cursor checkpoint does not hold"
    );
    transport
}

/// A checkpoint is of one records file as it stood: once the log is rewritten
/// in place, replaced by another file holding the very same bytes, or cut
/// short beneath it — and once the count checkpoint is edited by hand, or made
/// to claim every id there is — neither checkpoint is relied on, and the
/// position, the count and the next id are the log's.
#[test]
fn a_checkpoint_over_a_log_rewritten_replaced_or_cut_short_is_not_relied_on() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    answered_history(dir.path(), SMALL);
    let records = dir.path().join("commands.jsonl");

    // Rewritten in place: the same file, other records at the checkpoint.
    let transport = checkpointed(dir.path());
    let rewritten: String = (0..SMALL + 1)
        .map(|n| format!("{{\"id\":{n},\"rewritten\":true}}\n"))
        .collect();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&records)
        .expect("opens");
    file.set_len(0).expect("truncates");
    std::io::Write::write_all(&mut file, rewritten.as_bytes()).expect("rewrites");
    drop(file);
    answers_as_the_log_does(&transport, dir.path(), "a log rewritten in place");

    // Replaced: another file with every byte the same, renamed over it.
    let transport = checkpointed(dir.path());
    let copy = dir.path().join("commands.copy");
    std::fs::copy(&records, &copy).expect("copies");
    std::fs::rename(&copy, &records).expect("replaces");
    let (replaced, _) = cost(&transport, || {
        transport.count(&commands_queue()).expect("counts")
    });
    let history = std::fs::metadata(&records).expect("the records").len();
    assert!(
        replaced.bytes >= history,
        "a checkpoint was relied on over a file it was not kept for"
    );
    answers_as_the_log_does(&transport, dir.path(), "a log replaced by a copy");

    // Cut short beneath both checkpoints, at a record boundary.
    let transport = checkpointed(dir.path());
    let bytes = std::fs::read(&records).expect("the records");
    let half = bytes[..bytes.len() / 2]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .expect("a boundary")
        + 1;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&records)
        .and_then(|file| file.set_len(half as u64))
        .expect("cuts the log short");
    answers_as_the_log_does(&transport, dir.path(), "a log cut short");

    // The count checkpoint edited by hand.
    let count = dir.path().join("commands.jsonl.count");
    for (field, value) in [
        ("count", json!(3)),
        ("count", json!(u64::MAX)),
        ("offset", json!(10)),
        ("digest", json!(0)),
        ("seal", json!(0)),
    ] {
        let transport = checkpointed(dir.path());
        let mut edited: Value =
            serde_json::from_slice(&std::fs::read(&count).expect("the checkpoint")).expect("JSON");
        edited[field] = value.clone();
        std::fs::write(&count, edited.to_string()).expect("edits");
        answers_as_the_log_does(
            &transport,
            dir.path(),
            &format!("a count checkpoint whose {field} was edited to {value}"),
        );
    }
}

/// A checkpoint that cannot be read or written — a directory where its file
/// would be — costs reads, never an answer: positions, commits, claims and ids
/// are what they are with no checkpoint at all.
#[test]
fn a_checkpoint_that_cannot_be_read_or_written_changes_no_answer() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    answered_history(dir.path(), SMALL);
    for checkpoint in ["commands.jsonl.count", "commands-cursor.json.offset"] {
        let blocked = dir.path().join(checkpoint);
        std::fs::create_dir(&blocked).expect("a directory in its place");
        std::fs::write(blocked.join("occupied"), "").expect("not empty");
    }
    let transport = LocalTransport::open(dir.path()).expect("opens");
    let queue = commands(&transport);
    for n in 0..3 {
        let id = transport
            .read(&commands_queue(), None, usize::MAX)
            .expect("reads")
            .records
            .len() as u64;
        assert_eq!(queue.push(envelope(n)).expect("pushes").id, Some(id));
        let claimed = queue
            .claim(&ConsumerName::default_consumer())
            .expect("claims")
            .expect("the push is claimed");
        assert_eq!(claimed.id, Some(id));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("commands-cursor.json")).expect("the cursor"),
            (id + 1).to_string(),
            "the commit did not write the cursor"
        );
        answers_as_the_log_does(&transport, dir.path(), "an unwritable checkpoint");
        queue
            .claim(&ConsumerName::default_consumer())
            .expect("claims")
            .expect("the push answers_as_the_log_does made is claimed");
    }
}

/// A commit behind the consumer's checkpoint — a consumer moved back — writes
/// the count 0.9.3 writes, resumes there, and keeps a checkpoint that holds.
#[test]
fn a_commit_behind_its_checkpoint_writes_the_count_before_it() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    answered_history(dir.path(), SMALL);
    let records = dir.path().join("commands.jsonl");
    let transport = checkpointed(dir.path());
    let cursor = transport.cursor_path(&commands_queue(), &ConsumerName::default_consumer());
    let third = transport
        .read(&commands_queue(), None, 3)
        .expect("reads")
        .records
        .pop()
        .expect("a third record")
        .after;
    transport
        .commit(&commands_queue(), &ConsumerName::default_consumer(), &third)
        .expect("commits");
    assert_eq!(std::fs::read_to_string(&cursor).expect("the cursor"), "3");
    let (read, position) = cursor_cost(&transport, "default");
    assert_eq!(position, Some(third));
    assert!(
        read.records <= BOUND.records,
        "the checkpoint the commit kept does not hold: {read:?}"
    );
    let next = commands(&transport)
        .claim(&ConsumerName::default_consumer())
        .expect("claims")
        .expect("the fourth record");
    assert_eq!(
        Some(line_of(&next.record)),
        v0_9_3::next(&records, third.token())
    );
}

/// Records and torn fragments longer than one backward read: a checkpoint
/// after a record of several kilobytes holds and is read back whole, a torn
/// fragment of several kilobytes is healed back to the boundary before it, and
/// a file holding nothing but a torn fragment is healed to empty.
#[test]
fn records_and_torn_fragments_longer_than_a_backward_read_are_read_back_whole() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    answered_history(dir.path(), LARGE);
    let records = dir.path().join("commands.jsonl");
    let transport = LocalTransport::open(dir.path()).expect("opens");
    let queue = commands(&transport);
    let long = json!({"author": "planner", "text": "x".repeat(5000)});
    let pushed = queue.push(long).expect("pushes");
    let claimed = queue
        .claim(&ConsumerName::default_consumer())
        .expect("claims")
        .expect("the long record");
    assert_eq!(claimed.id, pushed.id);
    let history = std::fs::metadata(&records).expect("the records").len();
    let (read, position) = cursor_cost(&transport, "default");
    assert_eq!(position, Some(claimed.position));
    assert!(
        read.bytes > 5000 && read.bytes < 2 * 5000 + BOUND.bytes && read.bytes < history,
        "the record before the checkpoint was not read back whole, alone: {read:?}"
    );
    let (count, counted) = cost(&transport, || {
        transport.count(&commands_queue()).expect("counts")
    });
    assert_eq!(counted, LARGE as u64 + 1);
    assert!(
        count.bytes < 2 * 5000 + BOUND.bytes,
        "the count checkpoint after a long record does not hold: {count:?}"
    );

    let torn = format!("{{\"id\":{},\"torn\":\"{}", LARGE + 1, "y".repeat(3000));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&records)
        .and_then(|mut file| std::io::Write::write_all(&mut file, torn.as_bytes()))
        .expect("a long torn fragment");
    let healed = queue.push(envelope(0)).expect("the push heals the tail");
    assert_eq!(
        healed.id,
        Some(LARGE as u64 + 1),
        "the torn fragment was counted"
    );
    let loss: Value = serde_json::from_str(
        std::fs::read_to_string(dir.path().join("commands.jsonl.torn"))
            .expect("the loss is recorded")
            .trim(),
    )
    .expect("a loss record");
    assert_eq!(loss["offset"], json!(history));
    assert_eq!(loss["bytes"], json!(torn.len()));

    let only = tempfile::tempdir().expect("a scratch directory");
    std::fs::write(only.path().join("commands.jsonl"), "x".repeat(2000)).expect("a torn file");
    let transport = LocalTransport::open(only.path()).expect("opens");
    let pushed = commands(&transport).push(envelope(0)).expect("pushes");
    assert_eq!(
        pushed.id,
        Some(0),
        "a file of nothing but a torn fragment held a record"
    );
    assert_eq!(
        std::fs::read_to_string(only.path().join("commands.jsonl")).expect("the records"),
        format!("{}\n", line_of(&pushed.record)),
        "the torn fragment was not healed back to an empty file"
    );
}
