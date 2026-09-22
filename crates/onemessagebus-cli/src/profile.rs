//! Which vocabulary a stream verb reads and writes through.
//!
//! `--profile` names one, and this build links one: `open`, the core's
//! vocabulary that reserves nothing, which is also the default. The flag stays
//! so a caller that names its profile keeps working, and so a name this build
//! does not link is refused rather than read as some other vocabulary.

use onemessagebus::{Open, Vocabulary};

/// The profiles this binary offers, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The vocabulary that reserves nothing: the default.
    Open,
}

impl Profile {
    /// The default profile: the open one.
    pub const DEFAULT: Profile = Profile::Open;

    /// Every profile, by the name `--profile` takes.
    pub const NAMES: &'static [&'static str] = &[Open::NAME];

    /// The profile `name` selects, or the default when none was named.
    pub fn select(name: Option<&str>) -> Result<Profile, String> {
        match name {
            None => Ok(Profile::DEFAULT),
            Some(name) if name == Open::NAME => Ok(Profile::Open),
            Some(other) => Err(format!(
                "`{other}` is not a profile this build links; choose one of: {}",
                Profile::NAMES.join(", ")
            )),
        }
    }
}
