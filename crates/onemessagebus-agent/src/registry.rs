//! The registry the agent profile constructs: every schema the stack registers,
//! at every version this build reads.
//!
//! Two families carry versions. `agent.event-envelope` reads `[2, 1]` and
//! writes 2 for `pipeline` and 1 for `agentgraph` and `vcs` — the per-source
//! write version is a profile fact, [`Source::write_version`](crate::Source::write_version).
//! `agent.reply-envelope`
//! reads `[3, 2]` and writes 3; its shape is `onepipeline`'s reply, registered
//! here as JSON Schema so the profile owns the wire shape while `onepipeline`
//! keeps owning each command's meaning. `agent.note@1` is the note contract's
//! message ([`Note`](crate::note::Note)). The planner channel's four record
//! types are `agent.planner-surface@1`, `agent.queued-reply@1`,
//! `agent.queued-commands@1` and `agent.command-outcome@1`
//! ([`channel`](crate::channel)), and the transport plugin protocol's three
//! shapes are registered under the core's `onemessagebus` namespace so a client
//! in another language validates against them.

use onemessagebus::{Registry, SchemaId};
use serde_json::Value;

use crate::event::{Envelope, EventFilter, Labels};

/// The event envelope's family: `agent.event-envelope`.
pub const EVENT_ENVELOPE_FAMILY: &str = "agent.event-envelope";

/// The reply envelope's family: `agent.reply-envelope`.
pub const REPLY_ENVELOPE_FAMILY: &str = "agent.reply-envelope";

/// The event envelope versions this build reads, newest first.
pub const EVENT_ENVELOPE_READS: &[u32] = &[2, 1];

/// The reply envelope versions this build reads, newest first.
pub const REPLY_ENVELOPE_READS: &[u32] = &[3, 2];

/// `agent.artifact-ref@1`: the core's [`ArtifactRef`](crate::ArtifactRef).
pub const ARTIFACT_REF: SchemaId = SchemaId::literal("agent", "artifact-ref", 1);

/// `agent.event-filter@1`: the agent [`EventFilter`].
pub const EVENT_FILTER: SchemaId = SchemaId::literal("agent", "event-filter", 1);

const REPLY_ENVELOPE_V2: &str = include_str!("../schemas/reply-envelope-v2.schema.json");
const REPLY_ENVELOPE_V3: &str = include_str!("../schemas/reply-envelope-v3.schema.json");

/// The event envelope's schema at `version`: the generated document with `v`
/// pinned to that version, so a payload checks against the version it declares.
#[must_use]
pub fn event_envelope_schema(version: u32) -> Value {
    let mut schema = schemars::schema_for!(Envelope).to_value();
    if let Some(v) = schema.pointer_mut("/properties/v") {
        if let Some(object) = v.as_object_mut() {
            object.insert("const".to_owned(), Value::from(version));
        }
    }
    schema
}

/// Every schema the agent profile registers.
///
/// # Panics
///
/// Never for this build's own documents; each is a committed JSON Schema.
#[must_use]
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    for version in EVENT_ENVELOPE_READS {
        registry
            .register_schema(
                SchemaId::literal("agent", "event-envelope", *version),
                event_envelope_schema(*version),
            )
            .expect("the event envelope schema registers");
    }
    for (version, document) in [(2, REPLY_ENVELOPE_V2), (3, REPLY_ENVELOPE_V3)] {
        let schema: Value = serde_json::from_str(document).expect("a committed schema is JSON");
        registry
            .register_schema(
                SchemaId::literal("agent", "reply-envelope", version),
                schema,
            )
            .expect("the reply envelope schema registers");
    }
    registry
        .register_schema(
            ARTIFACT_REF,
            schemars::schema_for!(crate::ArtifactRef).to_value(),
        )
        .expect("the artifact reference schema registers");
    registry
        .register_schema(EVENT_FILTER, schemars::schema_for!(EventFilter).to_value())
        .expect("the filter schema registers");
    registry
        .register::<Labels>()
        .expect("the labels schema registers");
    registry
        .register::<crate::note::Note>()
        .expect("the note schema registers");
    registry
        .register::<crate::channel::Surface>()
        .expect("the planner surface schema registers");
    registry
        .register::<crate::channel::QueuedReply>()
        .expect("the queued reply schema registers");
    registry
        .register::<crate::channel::QueuedCommands>()
        .expect("the queued commands schema registers");
    registry
        .register::<crate::channel::CommandOutcome>()
        .expect("the command outcome schema registers");
    onemessagebus::transport::register_protocol(&mut registry)
        .expect("the transport plugin protocol's schemas register");
    registry
}
