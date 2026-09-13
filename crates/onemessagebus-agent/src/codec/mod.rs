//! The codecs the agent stack serves over the bus: each a
//! [`Codec`](onemessagebus::Codec) that `onemessagebus serve --codec <name>`
//! runs, with every fixed string its protocol reads or writes declared once in
//! its module.

pub mod onejudge;

/// Every codec this profile declares, by the name `serve --codec` gives it.
pub const CODECS: &[&str] = &[onejudge::CODEC];
