//! The vocabularies the real filters are written against, beside the core's `Open`:
//! the core test's commerce vocabulary (tests/vocabulary.rs), and a mirror of
//! onevcs's `VcsEvents` (its `MatchFields`), whose filter errors onevcs's tests assert.

use onemessagebus::{Labels, NoDimensions, Reserved, Source, Vocabulary};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commerce;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Department {
    Billing,
    Shipping,
}

impl std::fmt::Display for Department {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Department::Billing => "billing",
            Department::Shipping => "shipping",
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommerceMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<String>,
}

impl Vocabulary for Commerce {
    type Source = Department;
    type Dimensions = NoDimensions;
    type Labels = Labels;
    type Fields = CommerceMatch;

    const NAME: &'static str = "commerce";
    const RESERVED: &'static [Reserved] = &[Reserved::text("tenant"), Reserved::text("order")];
    const DIMENSIONS: &'static [Reserved] = &[];
    const DEFAULT_SOURCE: &'static str = "billing";

    fn write_version(_: &Self::Source) -> u32 {
        1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vcs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Development,
    Integrate,
    Review,
    Release,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatchFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<Phase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persona: Option<String>,
}

impl Vocabulary for Vcs {
    type Source = Source;
    type Dimensions = NoDimensions;
    type Labels = Labels;
    type Fields = MatchFields;

    const NAME: &'static str = "vcs";
    const RESERVED: &'static [Reserved] = &[];
    const DIMENSIONS: &'static [Reserved] = &[];
    const DEFAULT_SOURCE: &'static str = "onevcs";

    fn write_version(_: &Self::Source) -> u32 {
        1
    }
}
