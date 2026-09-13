//! The payload bounds at their edges, each through the public function
//! Contract W names.

use onemessagebus::{
    bound_detail, bound_payload, bound_text, MAX_ACTIVITY_DETAIL_CHARS, MAX_PAYLOAD_TEXT_BYTES,
    TRUNCATED_KEY,
};
use serde_json::{json, Map, Value};

/// `n` bytes of ASCII.
fn ascii(n: usize) -> String {
    "abcdefghij".chars().cycle().take(n).collect()
}

#[test]
fn bound_text_leaves_a_text_at_the_bound_whole_and_unflagged() {
    let text = ascii(MAX_PAYLOAD_TEXT_BYTES);
    assert_eq!(bound_text(&text), (text.clone(), false));
    assert_eq!(bound_text("brief"), ("brief".to_owned(), false));
}

#[test]
fn bound_text_keeps_the_last_bytes_of_a_text_past_the_bound_and_reports_the_cut() {
    let text = format!("X{}", ascii(MAX_PAYLOAD_TEXT_BYTES));
    let (kept, cut) = bound_text(&text);
    assert!(cut);
    assert_eq!(kept.len(), MAX_PAYLOAD_TEXT_BYTES);
    assert_eq!(
        kept,
        ascii(MAX_PAYLOAD_TEXT_BYTES),
        "the tail is what is kept"
    );
}

#[test]
fn bound_text_moves_forward_to_a_character_boundary() {
    // 4095 ASCII bytes after a three-byte character whose middle byte the cut
    // would land on: 1 + 3 + 4095 = 4099 bytes, cut at byte 3.
    let text = format!("a€{}", ascii(MAX_PAYLOAD_TEXT_BYTES - 1));
    assert_eq!(text.len(), MAX_PAYLOAD_TEXT_BYTES + 3);
    let (kept, cut) = bound_text(&text);
    assert!(cut);
    assert!(
        kept.len() < MAX_PAYLOAD_TEXT_BYTES,
        "moved forward, so inside the bound"
    );
    assert_eq!(kept, ascii(MAX_PAYLOAD_TEXT_BYTES - 1));
    assert!(std::str::from_utf8(kept.as_bytes()).is_ok());
}

fn payload(entries: &[(&str, Value)]) -> Map<String, Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

#[test]
fn bound_payload_keeps_the_first_bytes_of_each_over_long_text_and_stamps_truncated() {
    let long = format!("{}Z", ascii(MAX_PAYLOAD_TEXT_BYTES));
    let bounded = bound_payload(payload(&[
        ("output", json!(long)),
        ("note", json!("short")),
        ("count", json!(7)),
        (
            "nested",
            json!({ "inner": format!("{}Z", ascii(MAX_PAYLOAD_TEXT_BYTES)) }),
        ),
    ]));
    assert_eq!(bounded["output"], json!(ascii(MAX_PAYLOAD_TEXT_BYTES)));
    assert_eq!(bounded["note"], json!("short"));
    assert_eq!(bounded["count"], json!(7));
    assert_eq!(
        bounded["nested"]["inner"].as_str().map(str::len),
        Some(MAX_PAYLOAD_TEXT_BYTES + 1),
        "a nested object is carried untouched"
    );
    assert_eq!(bounded[TRUNCATED_KEY], json!(true));
    assert_eq!(
        bounded.keys().next_back().map(String::as_str),
        Some(TRUNCATED_KEY)
    );
}

#[test]
fn bound_payload_leaves_a_payload_inside_the_bound_unchanged_with_no_stamp() {
    let exact = ascii(MAX_PAYLOAD_TEXT_BYTES);
    let original = payload(&[("output", json!(exact)), ("count", json!(1))]);
    let bounded = bound_payload(original.clone());
    assert_eq!(bounded, original);
    assert!(!bounded.contains_key(TRUNCATED_KEY));
}

#[test]
fn bound_payload_moves_back_to_a_character_boundary() {
    // 4094 ASCII bytes, then a three-byte character straddling byte 4096.
    let text = format!("{}€{}", ascii(MAX_PAYLOAD_TEXT_BYTES - 2), ascii(10));
    let bounded = bound_payload(payload(&[("output", json!(text))]));
    let kept = bounded["output"].as_str().expect("text");
    assert_eq!(kept, ascii(MAX_PAYLOAD_TEXT_BYTES - 2));
    assert!(kept.len() <= MAX_PAYLOAD_TEXT_BYTES);
    assert_eq!(bounded[TRUNCATED_KEY], json!(true));
}

/// Two over-long top-level texts in one payload are each cut on their own —
/// one plainly, one moved back off a straddling character — under one stamp.
#[test]
fn bound_payload_cuts_every_over_long_text_and_stamps_truncated_once() {
    let plain = format!("{}tail", ascii(MAX_PAYLOAD_TEXT_BYTES + 200));
    // 4095 ASCII bytes, then a two-byte character straddling byte 4096.
    let straddling = format!("{}é{}", ascii(MAX_PAYLOAD_TEXT_BYTES - 1), ascii(50));
    let nested = json!({ "inner": ascii(MAX_PAYLOAD_TEXT_BYTES + 9), "n": 1 });
    let bounded = bound_payload(payload(&[
        ("stdout", json!(plain)),
        ("note", json!("short")),
        ("stderr", json!(straddling)),
        ("nested", nested.clone()),
    ]));
    assert_eq!(bounded["stdout"], json!(ascii(MAX_PAYLOAD_TEXT_BYTES)));
    assert_eq!(
        bounded["stderr"],
        json!(ascii(MAX_PAYLOAD_TEXT_BYTES - 1)),
        "the straddling character is dropped whole"
    );
    assert_eq!(bounded["note"], json!("short"));
    assert_eq!(
        bounded["nested"], nested,
        "a nested object is carried untouched"
    );
    assert_eq!(bounded[TRUNCATED_KEY], json!(true));
    let keys: Vec<&str> = bounded.keys().map(String::as_str).collect();
    assert_eq!(
        keys.iter().filter(|key| **key == TRUNCATED_KEY).count(),
        1,
        "{keys:?}"
    );
    assert_eq!(keys.len(), 5, "four values and one stamp: {keys:?}");
    assert_eq!(keys.last(), Some(&TRUNCATED_KEY));
    let written = serde_json::to_string(&bounded).expect("serializes");
    assert_eq!(
        written.matches(&format!("\"{TRUNCATED_KEY}\"")).count(),
        1,
        "{written}"
    );
}

#[test]
fn bound_detail_collapses_whitespace_and_counts_characters() {
    let (collapsed, cut) = bound_detail("  ran   the\tgate\n\n twice ");
    assert_eq!(collapsed, "ran the gate twice");
    assert!(!cut);

    let exact: String = "é".repeat(MAX_ACTIVITY_DETAIL_CHARS);
    assert_eq!(
        exact.len(),
        MAX_ACTIVITY_DETAIL_CHARS * 2,
        "two bytes per character"
    );
    assert_eq!(bound_detail(&exact), (exact.clone(), false));

    let longer = format!("{exact}x");
    let (kept, cut) = bound_detail(&longer);
    assert!(cut);
    assert_eq!(kept, exact, "counted in characters, not bytes");
    assert_eq!(kept.chars().count(), MAX_ACTIVITY_DETAIL_CHARS);
}
