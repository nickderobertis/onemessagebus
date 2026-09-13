//! Which vocabulary a stream verb reads and writes through.
//!
//! `--profile` names one: `agent` — the default, the vocabulary this binary
//! links — or `open`, the core's vocabulary that reserves nothing. Choosing it
//! the same way on `emit` and on `merge` is what makes the source words and
//! each source's write version one decision rather than two.

use onemessagebus::{Open, Vocabulary};
use onemessagebus_agent::Agent;

/// The profiles this binary offers, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The agent stack's vocabulary: the default.
    Agent,
    /// The vocabulary that reserves nothing.
    Open,
}

impl Profile {
    /// The default profile: the agent one this binary links.
    pub const DEFAULT: Profile = Profile::Agent;

    /// Every profile, by the name `--profile` takes.
    pub const NAMES: &'static [&'static str] = &[Agent::NAME, Open::NAME];

    /// The profile `name` selects, or the default when none was named.
    pub fn select(name: Option<&str>) -> Result<Profile, String> {
        match name {
            None => Ok(Profile::DEFAULT),
            Some(name) if name == Agent::NAME => Ok(Profile::Agent),
            Some(name) if name == Open::NAME => Ok(Profile::Open),
            Some(other) => Err(format!(
                "`{other}` is not a profile this build links; choose one of: {}",
                Profile::NAMES.join(", ")
            )),
        }
    }
}
