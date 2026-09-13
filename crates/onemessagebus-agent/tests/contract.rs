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
