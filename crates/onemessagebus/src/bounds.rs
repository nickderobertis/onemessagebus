//! The payload bounds, and the three cutting rules over them.
//!
//! Two constants and three rules. The constants are shared; which rule applies
//! is a property of the field, and each producer knows its own:
//!
//! * [`bound_payload`] is the **emitter's** rule, applied to the untyped payload
//!   map before an envelope is stamped: the first [`MAX_PAYLOAD_TEXT_BYTES`] of
//!   every top-level text value, with `"truncated": true` stamped on the payload
//!   when anything was cut.
//! * [`bound_text`] is a **producer's** rule over a field of its own typed
//!   payload: the last [`MAX_PAYLOAD_TEXT_BYTES`], reported back for the
//!   producer to record. What names a failure is the tail of a process's output.
//! * [`bound_detail`] is the rule over a one-line summary: whitespace collapsed,
//!   the first [`MAX_ACTIVITY_DETAIL_CHARS`] characters kept.
//!
//! Every cut leaves valid UTF-8 inside the bound. A payload already inside its
//! bound passes [`bound_payload`] unchanged, with no `truncated` stamp.

use serde_json::{Map, Value};

/// The byte bound on a payload text field.
pub const MAX_PAYLOAD_TEXT_BYTES: usize = 4096;

/// The character bound on a one-line activity summary.
pub const MAX_ACTIVITY_DETAIL_CHARS: usize = 160;

/// The key a payload carries, set to `true`, once [`bound_payload`] has cut
/// one of its values.
pub const TRUNCATED_KEY: &str = "truncated";

/// Bound one text field to [`MAX_PAYLOAD_TEXT_BYTES`], keeping its **tail**,
/// and report whether it had to be cut.
///
/// The cut moves *forward* to the next character boundary, so the result is
/// valid UTF-8 within the bound.
#[must_use]
pub fn bound_text(text: &str) -> (String, bool) {
    if text.len() <= MAX_PAYLOAD_TEXT_BYTES {
        return (text.to_owned(), false);
    }
    let mut start = text.len() - MAX_PAYLOAD_TEXT_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), true)
}

/// Bound every top-level text value of a payload to its **head** —
/// the first [`MAX_PAYLOAD_TEXT_BYTES`], the cut moved *back* to a character
/// boundary — and stamp [`TRUNCATED_KEY`] `true` when any value was cut.
///
/// Non-text values and nested objects are carried untouched: the bound is on
/// the text fields the wire contract names, not a walk of the document.
#[must_use]
pub fn bound_payload(payload: Map<String, Value>) -> Map<String, Value> {
    let mut bounded = Map::new();
    let mut truncated = false;
    for (key, value) in payload {
        match value {
            Value::String(text) if text.len() > MAX_PAYLOAD_TEXT_BYTES => {
                truncated = true;
                let cut = floor_char_boundary(&text, MAX_PAYLOAD_TEXT_BYTES);
                bounded.insert(key, Value::String(text[..cut].to_owned()));
            }
            other => {
                bounded.insert(key, other);
            }
        }
    }
    if truncated {
        bounded.insert(TRUNCATED_KEY.to_owned(), Value::Bool(true));
    }
    bounded
}

/// Bound a one-line summary to [`MAX_ACTIVITY_DETAIL_CHARS`], and report
/// whether it had to be cut.
///
/// Runs of whitespace collapse to one space first, and the bound counts
/// **characters**, not bytes: a summary is prose a person reads.
#[must_use]
pub fn bound_detail(detail: &str) -> (String, bool) {
    let collapsed: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_ACTIVITY_DETAIL_CHARS {
        return (collapsed, false);
    }
    (
        collapsed.chars().take(MAX_ACTIVITY_DETAIL_CHARS).collect(),
        true,
    )
}

/// The largest character boundary at or before `at`.
fn floor_char_boundary(value: &str, at: usize) -> usize {
    let mut index = at.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}
