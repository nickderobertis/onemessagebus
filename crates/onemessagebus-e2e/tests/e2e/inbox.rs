//! The inbox verbs, through the built binary.
//!
//! `deliver` is the sender, in a process of its own, against a receiver this
//! test binds on a real spool — the agent profile's note inbox — and `inbox
//! carried` lists a carry store the carry backend wrote. Where a receiver has to
//! die without closing, it is this test binary re-run as a child and killed by
//! the handle that started it.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use onemessagebus::{Carry, Closed, Spool};
use onemessagebus_agent::note::{Accepted, Addressee, Note, NoteInbox, Notes, Party};
use serde_json::{json, Value};

use crate::support::{run_in, Run};

/// The environment variable that makes this binary's `receiver_child` a
/// receiver bound to the spool it names.
const RECEIVER_SPOOL: &str = "ONEMESSAGEBUS_E2E_RECEIVER_SPOOL";

/// What the in-test receiver answers a note with: decided by the note alone, so
/// a journey knows exactly which disposition the receiver gave.
fn answer_for(note: &Note) -> Accepted {
    match note.addressee {
        Addressee::Worker => Accepted::Interrupted {
            party: Party::Worker,
        },
        Addressee::Supervisor => Accepted::JudgedWith {
            completion_reason: format!("passed with \"{}\" in hand", note.text),
        },
        Addressee::Both => Accepted::Queued,
    }
}

/// A receiver bound to a spool in this process, answering each note it takes
/// after `delay`.
struct Receiver {
    inbox: Arc<NoteInbox>,
    /// Held so the receiver stays bound for as long as it serves.
    _spool: Spool,
    stop: Arc<AtomicBool>,
    serving: Option<JoinHandle<Vec<Note>>>,
}

impl Receiver {
    fn bind(address: &Path, delay: Duration) -> Self {
        let inbox = Arc::new(NoteInbox::new());
        let spool = Spool::bind(address, &inbox).expect("the receiver binds its spool");
        let stop = Arc::new(AtomicBool::new(false));
        let serving = {
            let inbox = Arc::clone(&inbox);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut taken = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    if let Some(delivered) = inbox.take_within(Duration::from_millis(20)) {
                        std::thread::sleep(delay);
                        taken.push(delivered.message().clone());
                        let answer = answer_for(delivered.message());
                        delivered.answer(answer);
                    }
                }
                taken
            })
        };
        Self {
            inbox,
            _spool: spool,
            stop,
            serving: Some(serving),
        }
    }

    /// Stop serving, and hand back every note taken.
    fn stop(mut self) -> Vec<Note> {
        self.stop.store(true, Ordering::SeqCst);
        self.serving
            .take()
            .expect("serving")
            .join()
            .expect("the receiver stops")
    }
}

fn note_json(addressee: &str, text: &str) -> String {
    json!({"addressee": addressee, "text": text}).to_string()
}

/// `deliver` with `args` after the verb, from `cwd`, timed.
fn deliver(cwd: &Path, args: &[&str], stdin: Option<&str>) -> (Run, Duration) {
    let mut argv = vec!["deliver"];
    argv.extend_from_slice(args);
    let started = Instant::now();
    let run = run_in(cwd, &argv, stdin, &[]);
    (run, started.elapsed())
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("a directory")
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

/// A spool with no message offered, taken, answered or withdrawn in it: only
/// its receiver's own files (and, once the receiver has gone, its close).
fn assert_nothing_written(address: &Path) {
    let files = files_in(address);
    assert!(
        files
            .iter()
            .all(|name| ["receiver.lock", "spool.json", "closed.json"].contains(&name.as_str())),
        "a message was written to the spool: {files:?}"
    );
}

#[test]
fn deliver_reaches_a_bound_receiver_from_stdin_file_and_message_alike_and_prints_its_answer() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    let receiver = Receiver::bind(&address, Duration::ZERO);
    let spool = address.to_str().expect("UTF-8");

    let on_stdin = note_json("worker", "the reviewer asked for a smaller diff");
    let (stdin_run, _) = deliver(dir.path(), &[spool], Some(&on_stdin));
    assert_eq!(stdin_run.code, 0, "{}", stdin_run.stderr);
    assert_eq!(
        stdin_run.stdout,
        "{\"interrupted\":{\"party\":\"worker\"}}\n"
    );
    assert!(stdin_run.stderr.is_empty(), "{}", stdin_run.stderr);

    std::fs::write(
        dir.path().join("note.json"),
        note_json("supervisor", "hold the bar where it is"),
    )
    .expect("written");
    let (file_run, _) = deliver(dir.path(), &[spool, "--file", "note.json"], None);
    assert_eq!(file_run.code, 0, "{}", file_run.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&file_run.stdout).expect("one JSON document"),
        json!({"judged_with": {"completion_reason": "passed with \"hold the bar where it is\" in hand"}})
    );

    let inline = note_json("both", "the ruling applies to both of you");
    let (message_run, _) = deliver(dir.path(), &[spool, "--message", &inline], None);
    assert_eq!(message_run.code, 0, "{}", message_run.stderr);
    assert_eq!(message_run.stdout, "\"queued\"\n");

    let taken = receiver.stop();
    assert_eq!(
        taken,
        [
            Note::to(Addressee::Worker, "the reviewer asked for a smaller diff"),
            Note::to(Addressee::Supervisor, "hold the bar where it is"),
            Note::to(Addressee::Both, "the ruling applies to both of you"),
        ],
        "the receiver did not take exactly the notes the three invocations sent"
    );
    assert_nothing_written(&address);
}

#[test]
fn deliver_stays_blocked_for_the_whole_time_the_receiver_takes_and_prints_exactly_its_answer() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    let delay = Duration::from_millis(2500);
    let receiver = Receiver::bind(&address, delay);
    // The bounded wait is shorter than the receiver's delay: it bounds how long a
    // message waits to be taken, never how long a taken one waits for its answer.
    let (run, elapsed) = deliver(
        dir.path(),
        &[
            address.to_str().expect("UTF-8"),
            "--wait",
            "1",
            "--message",
            &note_json("worker", "take your time"),
        ],
        None,
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        elapsed >= delay,
        "deliver returned after {elapsed:?}, before the receiver answered at {delay:?}"
    );
    assert_eq!(run.stdout, "{\"interrupted\":{\"party\":\"worker\"}}\n");
    assert_eq!(receiver.stop().len(), 1);
}

#[test]
fn deliver_against_a_closed_spool_exits_non_zero_with_the_closers_reason() {
    // Closed before the message is offered.
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    let receiver = Receiver::bind(&address, Duration::ZERO);
    receiver.inbox.close(Closed::new(
        "the member settled: its supervisor passed the work",
    ));
    let (run, _) = deliver(
        dir.path(),
        &[
            address.to_str().expect("UTF-8"),
            "--message",
            &note_json("worker", "too late"),
        ],
        None,
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    assert!(
        run.stderr
            .contains("the member settled: its supervisor passed the work"),
        "the closer's reason did not reach the sender: {}",
        run.stderr
    );
    assert!(run.stderr.contains("was not delivered"), "{}", run.stderr);
    assert!(receiver.stop().is_empty());
    assert!(files_in(&address)
        .iter()
        .all(|name| !name.contains(".offer.") && !name.contains(".answer.")));

    // Closed after the message arrived: taken, and the inbox closed instead of
    // answering it.
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    let inbox = NoteInbox::new();
    let spool = Spool::bind(&address, &inbox).expect("binds");
    let sending = {
        let cwd = dir.path().to_path_buf();
        let spool = spool.address().to_str().expect("UTF-8").to_owned();
        std::thread::spawn(move || {
            deliver(
                &cwd,
                &[&spool, "--message", &note_json("worker", "in flight")],
                None,
            )
        })
    };
    let held = inbox
        .take_within(Duration::from_secs(30))
        .expect("the spawned sender's note arrives");
    assert_eq!(held.message(), &Note::to(Addressee::Worker, "in flight"));
    inbox.close(Closed::new(
        "the conversation ended before the note was read",
    ));
    let (run, _) = sending.join().expect("the sender finishes");
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("the conversation ended before the note was read"),
        "{}",
        run.stderr
    );
}

#[test]
fn deliver_refuses_a_message_passed_as_a_second_positional_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    let receiver = Receiver::bind(&address, Duration::ZERO);
    let (run, _) = deliver(
        dir.path(),
        &[
            address.to_str().expect("UTF-8"),
            &note_json("worker", "as a positional"),
        ],
        None,
    );
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert!(run.stderr.contains("unexpected argument"), "{}", run.stderr);
    assert!(run.stdout.is_empty());
    assert!(
        receiver.stop().is_empty(),
        "a refused message reached the receiver"
    );
    assert_nothing_written(&address);
}

#[test]
fn deliver_refuses_more_than_one_message_source_naming_each_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    let receiver = Receiver::bind(&address, Duration::ZERO);
    let spool = address.to_str().expect("UTF-8");
    let note = note_json("worker", "twice");
    std::fs::write(dir.path().join("note.json"), &note).expect("written");

    let cases: [(&[&str], Option<&str>, &str); 4] = [
        (
            &["--message", &note, "--file", "note.json"],
            None,
            "by --file and --message",
        ),
        (&["--message", &note], Some(&note), "by stdin and --message"),
        (&["--file", "note.json"], Some(&note), "by stdin and --file"),
        (
            &["--file", "note.json", "--message", &note],
            Some(&note),
            "by stdin and --file and --message",
        ),
    ];
    for (flags, stdin, named) in cases {
        let mut args = vec![spool];
        args.extend_from_slice(flags);
        let (run, _) = deliver(dir.path(), &args, stdin);
        assert_eq!(run.code, 2, "{flags:?}: {}", run.stderr);
        assert!(
            run.stderr.contains(named),
            "{flags:?} with stdin {stdin:?} did not name each source given: {}",
            run.stderr
        );
        assert!(run.stdout.is_empty());
        assert_nothing_written(&address);
    }

    let (none, _) = deliver(dir.path(), &[spool], None);
    assert_eq!(none.code, 2, "{}", none.stderr);
    assert!(
        none.stderr.contains("no message to deliver"),
        "{}",
        none.stderr
    );
    // Whitespace on stdin is no message either.
    let (blank, _) = deliver(dir.path(), &[spool], Some(" \n"));
    assert_eq!(blank.code, 2, "{}", blank.stderr);

    assert!(
        receiver.stop().is_empty(),
        "a refused message reached the receiver"
    );
    assert_nothing_written(&address);
}

#[test]
fn deliver_refuses_what_it_cannot_send_and_reports_what_the_receiver_refused() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    let receiver = Receiver::bind(&address, Duration::ZERO);
    let spool = address.to_str().expect("UTF-8");

    let (not_json, _) = deliver(dir.path(), &[spool, "--message", "not json"], None);
    assert_eq!(not_json.code, 2, "{}", not_json.stderr);
    assert!(
        not_json.stderr.contains("the message is not JSON"),
        "{}",
        not_json.stderr
    );

    // Checked against the schema the spool's receiver declared, before anything
    // is offered.
    let (off_schema, _) = deliver(
        dir.path(),
        &[
            spool,
            "--message",
            &note_json("manager", "not an addressee"),
        ],
        None,
    );
    assert_eq!(off_schema.code, 1, "{}", off_schema.stderr);
    assert!(
        off_schema.stderr.contains("agent.note@1") && off_schema.stderr.contains("/addressee"),
        "{}",
        off_schema.stderr
    );
    assert_nothing_written(&address);

    let (missing_file, _) = deliver(dir.path(), &[spool, "--file", "absent.json"], None);
    assert_eq!(missing_file.code, 2, "{}", missing_file.stderr);
    assert!(
        missing_file.stderr.contains("absent.json"),
        "{}",
        missing_file.stderr
    );

    let nowhere = dir.path().join("nowhere");
    let (no_spool, _) = deliver(
        dir.path(),
        &[
            nowhere.to_str().expect("UTF-8"),
            "--message",
            &note_json("worker", "x"),
        ],
        None,
    );
    assert_eq!(no_spool.code, 2, "{}", no_spool.stderr);
    assert!(
        no_spool.stderr.contains("is not a spool") && no_spool.stderr.contains("nowhere"),
        "{}",
        no_spool.stderr
    );

    // Conforming to the schema and still not a note: the receiver refuses it in
    // its own words, and that is a well-formed no.
    let (blank, _) = deliver(
        dir.path(),
        &[spool, "--message", &note_json("worker", "   ")],
        None,
    );
    assert_eq!(blank.code, 1, "{}", blank.stderr);
    assert!(
        blank.stderr.contains("refused the message") && blank.stderr.contains("blank"),
        "{}",
        blank.stderr
    );
    assert!(receiver.stop().is_empty());
}

#[test]
fn deliver_to_a_spool_nothing_services_reports_the_elapsed_wait_and_withdraws_the_message() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("unserviced");
    std::fs::create_dir(&address).expect("made");
    let (run, elapsed) = deliver(
        dir.path(),
        &[
            address.to_str().expect("UTF-8"),
            "--wait",
            "1",
            "--message",
            &note_json("worker", "into the void"),
        ],
        None,
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(elapsed >= Duration::from_secs(1), "{elapsed:?}");
    assert!(
        run.stderr.contains("within 1s")
            && run.stderr.contains("withdrawn")
            && run.stderr.contains(&address.display().to_string()),
        "the lost message was not reported naming the spool and the wait: {}",
        run.stderr
    );
    assert!(
        files_in(&address).is_empty(),
        "the withdrawn message was left in the spool: {:?}",
        files_in(&address)
    );
}

/// The child half of the killed-receiver journey: a receiver bound to the spool
/// `RECEIVER_SPOOL` names, which never takes anything and never closes. Ignored,
/// so the suite never runs it on its own; that journey runs it with `--ignored`.
#[test]
#[ignore = "the child half of the killed-receiver journey, which runs it with --ignored"]
fn receiver_child() {
    let address = std::env::var_os(RECEIVER_SPOOL)
        .expect("run only as the killed-receiver journey's child, which names the spool");
    let inbox = NoteInbox::new();
    let _spool = Spool::bind(PathBuf::from(address), &inbox).expect("the child binds");
    // Bound until killed; the courier takes offers into an inbox nobody reads.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Whether a receiver holds the spool at `address`.
fn bound(address: &Path) -> bool {
    File::options()
        .write(true)
        .open(address.join("receiver.lock"))
        .is_ok_and(|lock| matches!(lock.try_lock(), Err(TryLockError::WouldBlock)))
}

#[test]
fn deliver_to_a_receiver_killed_without_closing_reports_the_elapsed_wait_and_withdraws_the_message()
{
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    let mut child: Child = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "inbox::receiver_child",
            "--exact",
            "--nocapture",
            "--ignored",
            "--test-threads=1",
        ])
        .env(RECEIVER_SPOOL, &address)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the receiver process starts");
    let until = Instant::now() + Duration::from_secs(30);
    while !bound(&address) {
        assert!(
            Instant::now() < until,
            "the receiver process never bound its spool"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Killed by the handle that started it — its own pid — without closing.
    child.kill().expect("the receiver process is killed");
    child.wait().expect("the receiver process is reaped");
    assert!(!bound(&address), "a killed receiver still holds its spool");
    assert!(
        !address.join("closed.json").exists(),
        "a killed receiver recorded a close"
    );

    let (run, elapsed) = deliver(
        dir.path(),
        &[
            address.to_str().expect("UTF-8"),
            "--wait",
            "1",
            "--message",
            &note_json("worker", "to a receiver that is gone"),
        ],
        None,
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(elapsed >= Duration::from_secs(1), "{elapsed:?}");
    assert!(
        run.stderr.contains("within 1s") && run.stderr.contains("withdrawn"),
        "{}",
        run.stderr
    );
    assert!(
        files_in(&address)
            .iter()
            .all(|name| !name.contains(".offer.")),
        "the withdrawn message was left in the spool: {:?}",
        files_in(&address)
    );
}

#[test]
fn deliver_reports_an_answer_document_that_is_not_an_answer_naming_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let address = dir.path().join("notes");
    std::fs::create_dir(&address).expect("made");
    let sending = {
        let cwd = dir.path().to_path_buf();
        let spool = address.to_str().expect("UTF-8").to_owned();
        std::thread::spawn(move || {
            deliver(
                &cwd,
                &[
                    &spool,
                    "--message",
                    &note_json("worker", "answered in garbage"),
                ],
                None,
            )
        })
    };
    // llmlint: ignore-block[e2e_not_mocked] the acceptance criterion for this node requires an answer document a test has overwritten with bytes that are not an answer; a real receiver's answer is read and removed by its sender within one poll, so only a test holding the receiver's side can overwrite it. The sender is the spawned binary, unchanged, and every other journey here drives a real receiver.
    // llmlint: ignore-block[tests_mirror_real_usage] the acceptance criterion for this node requires an answer document a test has overwritten with bytes that are not an answer; a real receiver's answer is read and removed by its sender within one poll, so only a test holding the receiver's side can overwrite it. The sender is the spawned binary, unchanged, and every other journey here drives a real receiver.
    // Stand in for the receiver's side: take the offer, and overwrite its answer
    // document with bytes that are not an answer.
    let until = Instant::now() + Duration::from_secs(30);
    let offer = loop {
        if let Some(name) = files_in(&address)
            .into_iter()
            .find(|name| name.ends_with(".offer.json"))
        {
            break name;
        }
        assert!(
            Instant::now() < until,
            "the spawned sender never offered its note"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let id = offer.trim_end_matches(".offer.json");
    std::fs::rename(
        address.join(&offer),
        address.join(format!("{id}.taken.json")),
    )
    .expect("taken");
    let answer = address.join(format!("{id}.answer.json"));
    std::fs::write(&answer, "\u{0}\u{1} not an answer").expect("overwritten");
    // llmlint: ignore-end[tests_mirror_real_usage]
    // llmlint: ignore-end[e2e_not_mocked]
    let (run, _) = sending.join().expect("the sender finishes");
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stderr.contains(&answer.display().to_string()),
        "the unreadable answer document was not named: {}",
        run.stderr
    );
}

#[test]
fn inbox_carried_lists_a_carry_store_in_order_and_a_drained_store_prints_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = dir.path().join("carried.ndjson");
    let store_arg = store.to_str().expect("UTF-8");

    // The receiver's first lifetime: nothing carried yet, and then it is gone.
    let first = NoteInbox::new();
    assert_eq!(first.adopt_carried(&store), Ok(0));
    drop(first);

    // With no receiver running, each note is carried and answered as queued.
    let notes = [
        Note::to(Addressee::Worker, "first, while nobody was running"),
        Note::to(Addressee::Supervisor, "second"),
        Note::to(Addressee::Both, "third")
            .binding("the migration is covered")
            .expect("binds"),
    ];
    let carrier: Notes = Carry::sender(&store);
    for note in &notes {
        assert_eq!(carrier.send(note.clone()), Ok(Accepted::Queued));
    }

    let listed = run_in(dir.path(), &["inbox", "carried", store_arg], None, &[]);
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    assert!(listed.stderr.is_empty(), "{}", listed.stderr);
    let entries = listed.lines();
    assert_eq!(entries.len(), notes.len());
    for (entry, note) in entries.iter().zip(&notes) {
        assert_eq!(entry["schema"], json!("agent.note@1"));
        assert_eq!(
            entry["message"],
            serde_json::to_value(note).expect("a note")
        );
        assert!(
            entry["ts"].as_str().is_some_and(|ts| ts.ends_with('Z')),
            "{entry}"
        );
    }
    let text = run_in(
        dir.path(),
        &["inbox", "carried", store_arg, "--format", "text"],
        None,
        &[],
    );
    assert_eq!(text.code, 0, "{}", text.stderr);
    let lines: Vec<&str> = text.stdout.lines().collect();
    assert_eq!(lines.len(), notes.len());
    for (line, entry) in lines.iter().zip(&entries) {
        assert_eq!(
            *line,
            format!(
                "{} agent.note@1 {}",
                entry["ts"].as_str().expect("ts"),
                entry["message"]
            )
        );
    }

    // The receiver's second lifetime takes each note exactly once, in order.
    let second = NoteInbox::new();
    assert_eq!(second.adopt_carried(&store), Ok(notes.len()));
    for note in &notes {
        assert_eq!(second.take().expect("a carried note").message(), note);
    }
    assert!(second.take().is_none());

    let drained = run_in(dir.path(), &["inbox", "carried", store_arg], None, &[]);
    assert_eq!(drained.code, 0, "{}", drained.stderr);
    assert!(drained.stdout.is_empty() && drained.stderr.is_empty());

    // And a third lifetime is handed none of them again.
    assert_eq!(NoteInbox::new().adopt_carried(&store), Ok(0));
}

#[test]
fn inbox_carried_refuses_a_path_that_is_no_carry_store_by_name() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let stream = dir.path().join("run.ndjson");
    std::fs::write(
        &stream,
        "{\"v\":1,\"ts\":\"2026-09-13T00:00:00.000Z\",\"stream\":\"s\",\"seq\":1,\"source\":\"vcs\",\"kind\":\"push\",\"payload\":{},\"artifacts\":[]}\n",
    )
    .expect("written");
    for (path, why) in [
        (dir.path().join("missing"), "nothing is there"),
        (dir.path().to_path_buf(), "it is a directory"),
        (stream, "its first line is not a carry store's header"),
    ] {
        let run = run_in(
            dir.path(),
            &["inbox", "carried", path.to_str().expect("UTF-8")],
            None,
            &[],
        );
        assert_eq!(run.code, 2, "{}", run.stderr);
        assert!(run.stdout.is_empty());
        assert!(
            run.stderr
                .contains(&format!("{} is not a carry store: {why}", path.display())),
            "{}",
            run.stderr
        );
    }
    // Takes no message: a second positional is a usage error.
    let run = run_in(dir.path(), &["inbox", "carried", "a", "b"], None, &[]);
    assert_eq!(run.code, 2, "{}", run.stderr);
}
