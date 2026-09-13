//! The planner channel's `Router`: a reply is routed by the halves it carries,
//! so a verdict reaches the pending ask, edits reach the command path, and an
//! envelope carrying only edits leaves the ask standing.

use onemessagebus::{Layout, QueueName, Router};
use onemessagebus_agent::channel::{PlannerChannel, ReplyRouter, COMMANDS, REPLIES};
use serde_json::{json, Value};

fn replies() -> QueueName {
    REPLIES.parse().expect("a queue name")
}

fn routed(record: Value) -> Result<Vec<(String, Value)>, String> {
    let allowlist = PlannerChannel.allowlist();
    ReplyRouter
        .route(&replies(), record, &allowlist)
        .map(|records| {
            records
                .into_iter()
                .map(|(queue, record)| (queue.to_string(), record))
                .collect()
        })
}

#[test]
fn an_envelope_carrying_a_verdict_and_edits_reaches_both_the_command_path_and_the_reply_queue() {
    let both = routed(json!({
        "version": 3,
        "completion": false,
        "message": "go on",
        "commands": [{"op": "retry", "id": "build", "node": {"id": "build-2"}}]
    }))
    .expect("routed");
    let queues: Vec<&str> = both.iter().map(|(queue, _)| queue.as_str()).collect();
    assert_eq!(queues, [COMMANDS, REPLIES]);
    assert_eq!(both[0].1["commands"][0]["op"], json!("retry"));
    assert_eq!(both[1].1["reply"]["message"], json!("go on"));
}

#[test]
fn an_envelope_carrying_only_edits_reaches_the_command_path_alone() {
    let edits = routed(json!({"version": 3, "commands": [{"op": "cancel", "id": "build"}]}))
        .expect("routed");
    let queues: Vec<&str> = edits.iter().map(|(queue, _)| queue.as_str()).collect();
    assert_eq!(queues, [COMMANDS], "an edit reached the reply queue");
    let verdict =
        routed(json!({"version": 3, "completion": true, "reason": "done"})).expect("routed");
    let queues: Vec<&str> = verdict.iter().map(|(queue, _)| queue.as_str()).collect();
    assert_eq!(queues, [REPLIES]);
}

#[test]
fn a_framed_reply_is_kept_whole_and_a_monitor_is_refused_what_it_may_not_issue() {
    let framed = json!({"id": 0, "reply": {"completion": false, "message": "m"}, "at": 1});
    assert_eq!(
        routed(framed.clone()).expect("routed"),
        vec![(REPLIES.to_owned(), framed)]
    );
    let refused = routed(json!({
        "version": 3,
        "author": "monitor",
        "commands": [{"op": "drop", "id": "build"}]
    }))
    .expect_err("a monitor may not drop");
    assert!(
        refused.contains("'drop' is not an op the monitor may issue"),
        "{refused}"
    );
}
