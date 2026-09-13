//! The core's vocabulary journey, driven over the agent vocabulary: the same
//! table the core runs over a vocabulary with no agent word in it, differing
//! only in the fixture handed in.

use onemessagebus::conformance::{drive, Fixture, Sample};
use onemessagebus_agent::{Agent, Dimensions, Labels, MatchFields, Phase, Source};
use serde_json::json;

#[test]
fn the_agent_vocabulary_holds_the_conformance_table() {
    let fixture = Fixture::<Agent> {
        namespace: "agent",
        sources: [Source::Agentgraph, Source::Pipeline],
        labels: Labels {
            run_id: Some("R".to_owned()),
            member: Some("worker".to_owned()),
            persona: Some("engineer".to_owned()),
            ..Labels::default()
        },
        other_labels: Labels {
            run_id: Some("R".to_owned()),
            member: Some("supervisor".to_owned()),
            persona: Some("reviewer".to_owned()),
            ..Labels::default()
        },
        matching: MatchFields {
            member: Some("worker".to_owned()),
            ..MatchFields::default()
        },
        dimensions: Dimensions::at(Phase::Development),
        unknown_key: "stage",
    };
    let sample = Sample::<Labels> {
        conforming: Labels {
            run_id: Some("R".to_owned()),
            round: Some(2),
            ..Labels::default()
        },
        violating: (json!({ "round": "two" }), "/round"),
    };
    drive(&fixture, &sample);
}
