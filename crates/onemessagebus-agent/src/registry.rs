//! The registry the agent profile constructs: every schema the stack registers,
//! at every version this build reads.
//!
//! One family carries versions: `agent.event-envelope` reads `[2, 1]` and
//! writes 2 for `pipeline` and 1 for `agentgraph` and `vcs` — the per-source
//! write version is a profile fact, [`Source::write_version`](crate::Source::write_version).
//! `agent.note@1` is the note contract's message ([`Note`](crate::note::Note));
//! and the transport plugin protocol's three shapes are registered under the
//! core's `onemessagebus` namespace so a client in another language validates
//! against them. A protocol a program other than the agent stack owns — its
//! records, its queues — is not registered here: the program publishes it as a
//! schema bundle a configuration links (`docs/schema-links.md`).

use onemessagebus::{Registry, SchemaId};
use serde_json::Value;

use crate::event::{Envelope, EventFilter, Labels};

/// The event envelope's family: `agent.event-envelope`.
pub const EVENT_ENVELOPE_FAMILY: &str = "agent.event-envelope";

/// The event envelope versions this build reads, newest first.
pub const EVENT_ENVELOPE_READS: &[u32] = &[2, 1];

/// `agent.artifact-ref@1`: the core's [`ArtifactRef`](crate::ArtifactRef).
pub const ARTIFACT_REF: SchemaId = SchemaId::literal("agent", "artifact-ref", 1);

/// `agent.event-filter@1`: the agent [`EventFilter`].
pub const EVENT_FILTER: SchemaId = SchemaId::literal("agent", "event-filter", 1);

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
/// Never for this build's own documents; each is generated from its type.
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
    onemessagebus::transport::register_protocol(&mut registry)
        .expect("the transport plugin protocol's schemas register");
    registry
}
