//! The codecs the agent stack serves over the bus: each a
//! [`Codec`](onemessagebus::Codec) that `onemessagebus serve --codec <name>`
//! runs, with every fixed string its protocol reads or writes declared once in
//! its module.

use std::sync::LazyLock;

use onemessagebus::CodecName;

pub mod onejudge;

/// Every codec this profile declares, by the name `serve --codec` gives it.
pub static CODECS: LazyLock<Vec<CodecName>> = LazyLock::new(|| {
    vec![onejudge::CODEC
        .parse()
        .expect("the linked onejudge codec has a valid name")]
});
