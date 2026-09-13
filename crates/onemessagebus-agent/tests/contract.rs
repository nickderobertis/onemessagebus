//! The committed contract drives the profile's types.
//!
//! Every fixture here is read out of `docs/contract.md` at compile time rather
//! than copied beside it, so the document and the wire shapes cannot drift:
//! edit one without the other and this suite fails. Each fenced block is
//! tagged with an HTML comment (`<!-- fixture: name -->`) on the line before
//! it, which is how a block is found by name rather than by position.
//!
//! The envelope example is a *shape* illustration — five of its scalars are
//! placeholders or an alternation of the legal values — so the envelope test
//! substitutes exactly those five, asserting each is still there before it
//! does. A doc edit that renames a placeholder fails here rather than silently
//! skipping the substitution.

use std::collections::BTreeMap;

use onemessagebus::{Kind, CAPABILITIES};
use onemessagebus_agent::{
    registry, AgentEnvelope, AgentFilter, Dimensions, Envelope, EventFilter, Labels, MatchFields,
    Matcher, Phase, Source, EVENT_ENVELOPE_FAMILY, REPLY_ENVELOPE_FAMILY,
};
use serde_json::{json, Value};

/// The approved contract itself.
const CONTRACT: &str = include_str!("../../../docs/contract.md");

/// The fenced block tagged `<!-- fixture: name -->`.
fn fixture(name: &str) -> String {
    let tag = format!("<!-- fixture: {name} -->");
    let mut lines = CONTRACT.lines();
    lines
        .by_ref()
        .find(|line| line.trim() == tag)
        .unwrap_or_else(|| panic!("docs/contract.md has no fixture tagged {name:?}"));
    let opening = lines.next().expect("a fence after the tag");
    assert!(
        opening.starts_with("```"),
        "the line after the {name} tag is not a fence: {opening:?}"
    );
    let mut block = String::new();
    for line in lines {
        if line.starts_with("```") {
            return block;
        }
        block.push_str(line);
        block.push('\n');
    }
    panic!("the {name} fixture's fence never closes")
}

/// Replace one placeholder scalar in the envelope example, asserting it was
/// there to replace.
fn substitute(example: &mut Value, field: &str, placeholder: &str, concrete: Value) {
    let slot = example
        .get_mut(field)
        .unwrap_or_else(|| panic!("the envelope example has no `{field}` field"));
    assert_eq!(
        slot,
        &Value::String(placeholder.to_owned()),
        "the envelope example's `{field}` placeholder moved; update this substitution"
    );
    *slot = concrete;
}

/// The envelope example with its placeholder scalars made concrete.
fn envelope_example() -> Value {
    let mut example: Value =
        serde_json::from_str(&fixture("envelope")).expect("the envelope example is JSON");
    substitute(
        &mut example,
        "ts",
        "<RFC 3339, millisecond precision, UTC>",
        json!("2026-08-07T12:34:56.789Z"),
    );
    substitute(
        &mut example,
        "stream",
        "<unique id per producing process>",
        json!("oneagentgraph-4f2a"),
    );
    substitute(
        &mut example,
        "source",
        "agentgraph|vcs|pipeline",
        json!("agentgraph"),
    );
    substitute(
        &mut example,
        "kind",
        "<kebab-case event kind>",
        json!("member-started"),
    );
    substitute(
        &mut example,
        "phase",
        "development|integrate|review|release",
        json!("development"),
    );
    example
}

#[test]
fn the_documented_envelope_round_trips_through_the_profile_type() {
    let example = envelope_example();
    let envelope: Envelope =
        serde_json::from_value(example.clone()).expect("the documented envelope parses");

    assert_eq!(envelope.v, 1);
    assert_eq!(envelope.seq, 42);
    assert_eq!(envelope.source, Source::Agentgraph);
    assert_eq!(envelope.kind, Kind::from("member-started"));
    assert_eq!(envelope.phase(), Some(Phase::Development));
    assert_eq!(envelope.labels.run_id.as_deref(), Some("R"));
    assert_eq!(envelope.labels.round, Some(2));
    assert_eq!(envelope.labels.node.as_deref(), Some("service"));
    assert_eq!(envelope.labels.step.as_deref(), Some("implement"));
    assert_eq!(envelope.labels.member.as_deref(), Some("worker"));
    assert_eq!(envelope.labels.persona.as_deref(), Some("engineer"));
    assert_eq!(
        envelope.labels.extra.get("extra"),
        Some(&json!("carried")),
        "the free-form extra rides beside the reserved keys"
    );
    assert!(envelope.payload.is_empty());
    assert_eq!(envelope.artifacts.len(), 1);
    assert_eq!(envelope.artifacts[0].id, "a-91");
    assert_eq!(envelope.artifacts[0].kind, "log");
    assert_eq!(envelope.artifacts[0].bytes, 21400);

    let round_tripped = serde_json::to_value(&envelope).expect("serializes");
    assert_eq!(
        round_tripped, example,
        "serializing the parsed envelope must reproduce the documented shape"
    );
    // And in the documented key order: v, ts, stream, seq, source, kind, phase,
    // labels, payload, artifacts.
    let written = serde_json::to_string(&envelope).expect("serializes");
    let keys: Vec<&str> = [
        "\"v\"",
        "\"ts\"",
        "\"stream\"",
        "\"seq\"",
        "\"source\"",
        "\"kind\"",
        "\"phase\"",
        "\"labels\"",
        "\"payload\"",
        "\"artifacts\"",
    ]
    .into_iter()
    .collect();
    let positions: Vec<usize> = keys
        .iter()
        .map(|key| written.find(key).expect("every key is written"))
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "the keys are not in the documented order: {written}"
    );
}

#[test]
fn a_phase_is_omitted_from_the_wire_when_absent() {
    let mut example = envelope_example();
    example.as_object_mut().expect("an object").remove("phase");
    let envelope: Envelope = serde_json::from_value(example.clone()).expect("parses");
    assert_eq!(envelope.phase(), None);
    let written = serde_json::to_value(&envelope).expect("serializes");
    assert_eq!(written, example);
    assert!(
        written.get("phase").is_none(),
        "an absent phase must not be written as null"
    );
}

/// One way an envelope goes wrong: what is wrong, the single change that makes
/// it so, and the name the refusal has to carry.
type Refusal = (&'static str, fn(&mut Value), &'static str);

/// Each way an envelope is refused, and the name the refusal has to carry.
#[test]
fn a_malformed_envelope_is_refused_by_name() {
    let refusals: [Refusal; 4] = [
        (
            "an unknown top-level field",
            |example| example["stage"] = json!("verify"),
            "stage",
        ),
        (
            "a non-integer seq",
            |example| example["seq"] = json!("42"),
            "seq",
        ),
        (
            "an unknown source word",
            |example| example["source"] = json!("orchestrator"),
            "orchestrator",
        ),
        (
            "a missing required field",
            |example| {
                example.as_object_mut().expect("an object").remove("ts");
            },
            "ts",
        ),
    ];
    for (because, mutate, names) in refusals {
        let mut example = envelope_example();
        mutate(&mut example);
        let refusal = serde_json::from_value::<Envelope>(example)
            .expect_err(&format!("{because} must be refused"));
        assert!(
            refusal.to_string().contains(names),
            "the refusal of {because} does not name {names:?}: {refusal}"
        );
    }
}

#[test]
fn the_source_alternation_names_exactly_the_source_variants() {
    let documented: Vec<&str> = "agentgraph|vcs|pipeline".split('|').collect();
    assert!(CONTRACT.contains("\"source\": \"agentgraph|vcs|pipeline\""));
    let implemented: Vec<&str> = Source::every()
        .iter()
        .map(|source| source.as_str())
        .collect();
    assert_eq!(documented, implemented);
    for name in &documented {
        let parsed: Source = serde_json::from_value(json!(name)).expect("parses");
        assert_eq!(parsed.as_str(), *name);
    }
}

#[test]
fn the_phase_alternation_names_exactly_the_phase_variants() {
    let documented: Vec<&str> = "development|integrate|review|release".split('|').collect();
    assert!(CONTRACT.contains("\"phase\": \"development|integrate|review|release\""));
    let implemented: Vec<&str> = Phase::every().iter().map(|phase| phase.as_str()).collect();
    assert_eq!(documented, implemented);
    for name in &documented {
        let parsed: Phase = serde_json::from_value(json!(name)).expect("parses");
        assert_eq!(parsed.as_str(), *name);
    }
}

fn labels(member: &str, persona: &str) -> Labels {
    Labels {
        run_id: Some("R".to_owned()),
        node: Some("service".to_owned()),
        step: Some("implement".to_owned()),
        member: Some(member.to_owned()),
        persona: Some(persona.to_owned()),
        ..Labels::default()
    }
}

/// One row of the filter table: what is asked, and what the documented filter
/// answers.
struct Row {
    because: &'static str,
    source: Source,
    kind: &'static str,
    labels: Labels,
    phase: Option<Phase>,
    admitted: bool,
}

#[test]
fn the_documented_filter_answers_the_table() {
    let filter: EventFilter =
        serde_json::from_str(&fixture("filter")).expect("the documented filter parses");
    filter.validate().expect("the documented filter is valid");
    assert_eq!(filter.include.len(), 2);
    assert_eq!(filter.exclude.len(), 2);
    assert_eq!(
        serde_json::to_value(&filter).expect("serializes"),
        serde_json::from_str::<Value>(&fixture("filter")).expect("JSON"),
        "the filter must serialize back to the documented shape"
    );

    let rows = [
        Row {
            because: "a kind glob admits every member-* kind",
            source: Source::Agentgraph,
            kind: "member-started",
            labels: labels("supervisor", "reviewer"),
            phase: None,
            admitted: true,
        },
        Row {
            because: "the glob is anchored: turn-started is not member-*",
            source: Source::Agentgraph,
            kind: "turn-started",
            labels: labels("supervisor", "reviewer"),
            phase: None,
            admitted: false,
        },
        Row {
            because: "two reserved labels conjoin and admit the worker engineer",
            source: Source::Agentgraph,
            kind: "turn-started",
            labels: labels("worker", "engineer"),
            phase: None,
            admitted: true,
        },
        Row {
            because: "one of two conjoined labels missing admits nothing",
            source: Source::Agentgraph,
            kind: "turn-started",
            labels: labels("worker", "reviewer"),
            phase: None,
            admitted: false,
        },
        Row {
            because: "exclude wins over an include that matched the labels",
            source: Source::Agentgraph,
            kind: "turn-activity",
            labels: labels("worker", "engineer"),
            phase: None,
            admitted: false,
        },
        Row {
            because: "source and phase conjoin in an exclude matcher",
            source: Source::Vcs,
            kind: "member-heartbeat",
            labels: labels("worker", "engineer"),
            phase: Some(Phase::Release),
            admitted: false,
        },
        Row {
            because: "the same source at another phase is not excluded",
            source: Source::Vcs,
            kind: "member-heartbeat",
            labels: labels("worker", "engineer"),
            phase: Some(Phase::Review),
            admitted: true,
        },
        Row {
            because: "a matcher naming a label the envelope did not stamp does not match",
            source: Source::Pipeline,
            kind: "node-ready",
            labels: Labels::default(),
            phase: None,
            admitted: false,
        },
    ];
    for row in rows {
        assert_eq!(
            filter.admits(row.source, row.kind, &row.labels, row.phase),
            row.admitted,
            "{}",
            row.because
        );
    }

    let everything = EventFilter::everything();
    assert!(everything.admits(Source::Pipeline, "anything", &Labels::default(), None));
    assert!(
        EventFilter::parse("{}").expect("empty is valid").admits(
            Source::Vcs,
            "fetch",
            &Labels::default(),
            Some(Phase::Development)
        ),
        "an absent include admits everything"
    );
}

/// Every reserved label the grammar names is matched by exact equality, alone:
/// its own value matches, another value of that label does not, and an
/// envelope that did not stamp it does not.
#[test]
fn each_reserved_label_matches_by_exact_equality() {
    let stamped = labels("worker", "engineer");
    // Every reserved text label differs from `stamped`'s, so no ask can match
    // this envelope by a key it does not name.
    let different = Labels {
        run_id: Some("S".to_owned()),
        node: Some("frontend".to_owned()),
        step: Some("verify".to_owned()),
        member: Some("other".to_owned()),
        persona: Some("other".to_owned()),
        ..Labels::default()
    };
    let asks: [(&str, MatchFields); 5] = [
        (
            "run_id",
            MatchFields {
                run_id: Some("R".to_owned()),
                ..MatchFields::default()
            },
        ),
        (
            "node",
            MatchFields {
                node: Some("service".to_owned()),
                ..MatchFields::default()
            },
        ),
        (
            "step",
            MatchFields {
                step: Some("implement".to_owned()),
                ..MatchFields::default()
            },
        ),
        (
            "member",
            MatchFields {
                member: Some("worker".to_owned()),
                ..MatchFields::default()
            },
        ),
        (
            "persona",
            MatchFields {
                persona: Some("engineer".to_owned()),
                ..MatchFields::default()
            },
        ),
    ];
    for (key, fields) in asks {
        let filter = EventFilter {
            include: vec![Matcher::new().fields(fields)],
            exclude: Vec::new(),
        };
        assert!(
            filter.admits(Source::Agentgraph, "turn-started", &stamped, None),
            "{key} did not match its own value"
        );
        assert!(
            !filter.admits(Source::Agentgraph, "turn-started", &different, None),
            "{key} matched a different value"
        );
        assert!(
            !filter.admits(Source::Agentgraph, "turn-started", &Labels::default(), None),
            "{key} matched an envelope that did not stamp it"
        );
    }
}

#[test]
fn a_matcher_naming_no_field_or_an_empty_field_is_refused_naming_list_index_and_matcher() {
    let empty = EventFilter::parse(r#"{"exclude": [{"kind": "x"}, {}]}"#)
        .expect_err("a field-less matcher is refused");
    assert!(empty.to_string().contains("exclude[1] {}"), "{empty}");
    assert!(empty.to_string().contains("matches every event"), "{empty}");

    let blank = EventFilter::parse(r#"{"include": [{"member": ""}]}"#)
        .expect_err("an empty field is refused");
    assert!(
        blank.to_string().contains(r#"include[0] {"member":""}"#),
        "{blank}"
    );
    assert!(blank.to_string().contains("`member` is empty"), "{blank}");

    let unknown = EventFilter::parse(r#"{"include": [{"stream": "s1"}]}"#)
        .expect_err("an unknown field is refused");
    assert!(unknown.to_string().contains("stream"), "{unknown}");

    let matcher = Matcher::parse(r#"{"round": "2"}"#).expect_err("round is not matchable");
    assert!(matcher.to_string().contains("round"), "{matcher}");
    let parsed = Matcher::parse(r#"{"kind": "change-*", "phase": "review"}"#).expect("parses");
    assert_eq!(parsed.kind.as_deref(), Some("change-*"));
    assert_eq!(parsed.fields.phase, Some(Phase::Review));
}

#[test]
fn the_documented_read_sets_are_the_profiles() {
    let documented: BTreeMap<String, Vec<u32>> =
        serde_json::from_str(&fixture("read-sets")).expect("the read sets are JSON");
    let registry = registry();
    assert_eq!(
        documented.keys().cloned().collect::<Vec<_>>(),
        vec![
            EVENT_ENVELOPE_FAMILY.to_owned(),
            REPLY_ENVELOPE_FAMILY.to_owned()
        ]
    );
    for (family, read_set) in documented {
        assert_eq!(registry.read_set(&family), read_set, "{family}");
    }
}

#[test]
fn the_documented_verbs_are_exactly_the_capabilities() {
    let documented: Vec<String> = serde_json::from_str(&fixture("verbs")).expect("JSON");
    let declared: Vec<String> = CAPABILITIES
        .iter()
        .map(|capability| capability.verb.join(" "))
        .collect();
    assert_eq!(documented, declared);
}

#[test]
fn a_dimension_converts_from_a_phase() {
    assert_eq!(
        Dimensions::from(Phase::Review),
        Dimensions::at(Phase::Review)
    );
    assert_eq!(Dimensions::from(None), Dimensions::none());
}

/// A document a spool holds, once something has written it.
fn document_at(path: &std::path::Path) -> Value {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return serde_json::from_str(&text).expect("a spool document is JSON");
        }
        assert!(
            std::time::Instant::now() < until,
            "{} was never written",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn offer_id(n: u32) -> String {
    format!("{:039}-1-{n:020}", 1)
}

#[test]
fn the_documented_spool_documents_are_what_a_bound_spool_reads_and_writes() {
    use onemessagebus::{Closed, Spool};
    use onemessagebus_agent::note::{Accepted, Addressee, Note, NoteInbox, Party};
    use std::time::Duration;

    let documents: Value =
        serde_json::from_str(&fixture("spool-documents")).expect("the documents are JSON");
    let dir = tempfile::tempdir().expect("a temp dir");
    let spool_dir = dir.path().join("notes");
    let inbox = NoteInbox::new();
    let _spool = Spool::bind(&spool_dir, &inbox).expect("binds");
    assert_eq!(
        document_at(&spool_dir.join("spool.json")),
        documents["spool.json"]
    );

    // An offer in the documented shape is taken, and its answer is written in the
    // documented shape.
    let offered = |n: u32, offer: &Value| {
        let id = offer_id(n);
        std::fs::write(
            spool_dir.join(format!("{id}.offer.json")),
            offer.to_string(),
        )
        .expect("offered");
        spool_dir.join(format!("{id}.answer.json"))
    };
    let answer = offered(1, &documents["offer"]);
    let taken = inbox
        .take_within(Duration::from_secs(30))
        .expect("the documented offer is taken");
    assert_eq!(
        taken.message(),
        &Note::to(Addressee::Worker, "look again at the migration")
    );
    taken.answer(Accepted::Interrupted {
        party: Party::Worker,
    });
    assert_eq!(document_at(&answer), documents["answer"]);

    let mut blank = documents["offer"].clone();
    blank["message"]["text"] = json!("   ");
    let refused = document_at(&offered(2, &blank));
    let mut expected = documents["answer-refused"].clone();
    assert_eq!(
        expected["answer"]["refused"]["reason"],
        json!("<why the receiver could not read the offer>"),
        "the refused answer's placeholder moved; update this substitution"
    );
    expected["answer"]["refused"]["reason"] = refused["answer"]["refused"]["reason"].clone();
    assert!(refused["answer"]["refused"]["reason"].is_string());
    assert_eq!(refused, expected);

    let closing = offered(3, &documents["offer"]);
    let held = inbox
        .take_within(Duration::from_secs(30))
        .expect("the third offer is taken");
    inbox.close(Closed::new("the conversation ended"));
    assert_eq!(document_at(&closing), documents["answer-closed"]);
    assert_eq!(
        document_at(&spool_dir.join("closed.json")),
        documents["closed.json"]
    );
    drop(held);

    // A sender writes the documented offer, and nothing else, while it waits.
    let unserviced = dir.path().join("unserviced");
    std::fs::create_dir(&unserviced).expect("made");
    let sending = {
        let unserviced = unserviced.clone();
        std::thread::spawn(move || {
            onemessagebus::Spool::connect_within::<Note, Accepted>(
                &unserviced,
                Duration::from_secs(5),
            )
            .send(Note::to(Addressee::Worker, "look again at the migration"))
        })
    };
    let offer = loop {
        let found = std::fs::read_dir(&unserviced)
            .expect("a directory")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().ends_with(".offer.json"));
        if let Some(path) = found {
            break path;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(document_at(&offer), documents["offer"]);
    std::fs::remove_file(&offer).expect("taken away from the sender");
    assert!(
        sending.join().expect("the sender finishes").is_err(),
        "a sender whose offer vanished was answered"
    );
}

#[test]
fn inbox_md_names_every_file_a_spool_holds() {
    let inbox_md = include_str!("../../../docs/inbox.md");
    let start = inbox_md
        .find("#### On disk")
        .expect("docs/inbox.md has an `On disk` section");
    let table: Vec<String> = inbox_md[start..]
        .lines()
        .skip_while(|line| !line.starts_with("| file"))
        .skip(2)
        .take_while(|line| line.starts_with("| `"))
        .map(|row| {
            row.split('`')
                .nth(1)
                .expect("a backticked file name")
                .to_owned()
        })
        .collect();
    assert_eq!(
        table,
        [
            "spool.json",
            "receiver.lock",
            "closed.json",
            "<id>.offer.json",
            "<id>.taken.json",
            "<id>.answer.json",
            "<id>.withdrawn",
        ],
        "docs/inbox.md's file table no longer names the files a spool holds"
    );
}

#[test]
fn the_documented_carry_store_is_what_the_carry_backend_writes() {
    use onemessagebus::Carry;
    use onemessagebus_agent::note::{Accepted, Addressee, Note, Notes};

    let documented: Value =
        serde_json::from_str(&fixture("carry-store")).expect("the store is JSON");
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = dir.path().join("carried.ndjson");
    let carrier: Notes = Carry::sender(&store);
    assert_eq!(
        carrier.send(Note::to(
            Addressee::Both,
            "the ruling applies to both of you"
        )),
        Ok(Accepted::Queued)
    );
    let written = std::fs::read_to_string(&store).expect("the store");
    let lines: Vec<Value> = written
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    assert_eq!(lines.len(), 2, "{written}");
    assert_eq!(lines[0], documented["header"]);
    assert_eq!(
        written.lines().next(),
        Some(documented["header"].to_string().as_str()),
        "the header is not written in the documented key order"
    );
    let mut record = documented["record"].clone();
    assert_eq!(
        record["ts"],
        json!("<RFC 3339, millisecond precision, UTC>"),
        "the record's placeholder moved; update this substitution"
    );
    record["ts"] = lines[1]["ts"].clone();
    assert_eq!(lines[1], record);
}

#[test]
fn the_documented_note_shapes_round_trip_through_the_note_types() {
    use onemessagebus::{Closed, Message};
    use onemessagebus_agent::note::{Accepted, Note, Undelivered};

    let compact = |value: &Value| serde_json::to_string(value).expect("serializes");

    let documented: Value = serde_json::from_str(&fixture("note")).expect("JSON");
    let note: Note = serde_json::from_value(documented.clone()).expect("the documented note");
    assert!(note.binds());
    assert_eq!(
        serde_json::to_string(&note).expect("serializes"),
        compact(&documented)
    );
    assert_eq!(Note::SCHEMA.to_string(), "agent.note@1");

    let accepted: Vec<Value> = serde_json::from_str(&fixture("accepted")).expect("JSON");
    let mut kinds = std::collections::HashSet::new();
    for wire in &accepted {
        let value: Accepted = serde_json::from_value(wire.clone()).expect("an Accepted");
        kinds.insert(std::mem::discriminant(&value));
        assert_eq!(
            serde_json::to_string(&value).expect("serializes"),
            compact(wire)
        );
    }
    assert_eq!(kinds.len(), 3, "the documented dispositions miss a variant");

    let refusals: Vec<Value> = serde_json::from_str(&fixture("note-undelivered")).expect("JSON");
    let mut kinds = std::collections::HashSet::new();
    for wire in &refusals {
        let refusal: Undelivered = serde_json::from_value(wire.clone()).expect("an Undelivered");
        kinds.insert(std::mem::discriminant(&refusal));
        assert_eq!(
            serde_json::to_string(&refusal).expect("serializes"),
            compact(wire)
        );
        // Carried in a close, it comes back as the refusal it was.
        let closed = Closed::from(&refusal);
        assert_eq!(closed.reason, compact(wire));
        assert_eq!(
            Undelivered::from(onemessagebus::Undelivered::Closed(closed)),
            refusal
        );
    }
    assert_eq!(kinds.len(), 3, "the documented refusals miss a variant");
}

#[test]
fn the_documented_planner_channel_is_the_layout_the_profile_declares() {
    let documented: Value =
        serde_json::from_str(&fixture("planner-channel")).expect("the layout is JSON");
    assert_eq!(
        serde_json::to_value(onemessagebus_agent::channel::queues()).expect("JSON"),
        documented,
        "the planner-channel queues differ from the contract"
    );
}

#[test]
fn the_documented_planner_channel_grants_and_refusals_are_the_allowlists() {
    use onemessagebus_agent::channel::{allowlist, allows, Op};
    let documented: Value = serde_json::from_str(&fixture("planner-channel-grants")).expect("JSON");
    let allowlist = allowlist();
    for author in ["planner", "monitor"] {
        let granted: Vec<&str> = allowlist
            .granted(&onemessagebus::Author::from(author))
            .iter()
            .map(|op| op.word())
            .collect();
        let stated: Vec<&str> = documented[author]
            .as_array()
            .expect("a list")
            .iter()
            .map(|word| word.as_str().expect("a word"))
            .collect();
        assert_eq!(
            granted, stated,
            "the {author}'s grants differ from the contract"
        );
    }
    let monitor = onemessagebus::Author::from("monitor");
    let refused = documented["refused-monitor"]
        .as_object()
        .expect("an object");
    let mut not_granted: Vec<&str> = Op::ALL
        .iter()
        .filter(|op| allows(&allowlist, &monitor, op.word()).is_err())
        .map(|op| op.word())
        .collect();
    not_granted.sort_unstable();
    let mut stated: Vec<&str> = refused.keys().map(String::as_str).collect();
    stated.sort_unstable();
    assert_eq!(not_granted, stated);
    for (op, reason) in refused {
        assert_eq!(
            allows(&allowlist, &monitor, op).expect_err("refused"),
            format!(
                "'{op}' is not an op the monitor may issue: {}. Surface it to the planner instead",
                reason.as_str().expect("a reason")
            )
        );
    }
}

#[test]
fn the_documented_configuration_resolves_against_the_planner_channel_and_a_widened_one_is_refused()
{
    use std::sync::Arc;

    use onemessagebus::{Author, Config, ConfigError, Layouts, OpWord, TransportKinds, NARROWED};
    use onemessagebus_agent::channel::PlannerChannel;

    let dir = tempfile::tempdir().expect("a scratch directory");
    let layouts = Layouts::new().with(Arc::new(PlannerChannel));
    let text = fixture("config");
    let bus = Config::parse(&text)
        .expect("the documented configuration loads")
        .with_transport_dir(dir.path())
        .resolve(&layouts, &TransportKinds::builtin())
        .expect("the documented configuration resolves");
    let names: Vec<String> = bus.queues().iter().map(ToString::to_string).collect();
    assert_eq!(
        names,
        [
            "command-outcomes",
            "commands",
            "findings",
            "replies",
            "surfaces"
        ]
    );
    let monitor = Author::from("monitor");
    assert!(bus
        .allowlist()
        .allows(&monitor, &OpWord("retry".to_owned()))
        .is_ok());
    let add = bus
        .allowlist()
        .allows(&monitor, &OpWord("add".to_owned()))
        .expect_err("add was narrowed away");
    assert_eq!(add.reason, NARROWED);

    let widened = text.replace(
        "capabilities: [retry, requeue, cancel, finding]",
        "capabilities: [retry, attest]",
    );
    assert_ne!(widened, text, "the widening did not apply to the fixture");
    let resolved = Config::parse(&widened)
        .expect("the file alone cannot know the profile's grants, so it loads")
        .with_transport_dir(dir.path())
        .resolve(&layouts, &TransportKinds::builtin());
    match resolved {
        Err(ConfigError::Narrowing(refusal)) => {
            assert_eq!(refusal.key, "authors.monitor.capabilities");
            assert!(refusal.why.contains("`attest`"), "{refusal}");
        }
        other => panic!("a widened grant was not refused by resolve: {other:?}"),
    }
}
