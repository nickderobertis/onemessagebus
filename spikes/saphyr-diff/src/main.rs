//! Differential harness: this repository's real YAML read (and written) by
//! serde_norway 0.9, which the core uses today, and by serde-saphyr 1.3, with
//! saphyr 0.2's node tree beside it for the schema-less walks.
//!
//! `cargo run -q -p saphyr-diff --release` prints, per document kind, every
//! case where the two crates differ in what they read, write, or say on error,
//! and which serde-saphyr option (if any) restores today's behaviour.
//! Throwaway: nothing here lands.

mod corpus;
mod tree;
mod vocab;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Instant;

use onemessagebus::{Config, Filter, Open, Predicate, Vocabulary};
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Options, SerializerOptions};

use corpus::{Case, Kind, Origin};
use vocab::{Commerce, Vcs};

/// A serde-saphyr `Options` variant tried against every difference.
struct Variant {
    name: &'static str,
    apply: fn(&mut Options),
    /// How the text is read: an option alone, or one of the code changes.
    shaped: Mode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `serde_saphyr::from_str_with_options`.
    Direct,
    /// [`shaped`]: serde_norway's error shape restored around a direct read.
    Shaped,
    /// [`via_value`]: read into `serde_json::Value`, then into the type.
    ViaValue,
}

const VARIANTS: &[Variant] = &[
    Variant { name: "default", apply: |_| {}, shaped: Mode::Direct },
    Variant { name: "with_snippet=false", apply: |o| o.with_snippet = false, shaped: Mode::Direct },
    Variant { name: "strict_booleans", apply: |o| o.strict_booleans = true, shaped: Mode::Direct },
    Variant {
        name: "strict_booleans+with_snippet=false",
        apply: |o| {
            o.strict_booleans = true;
            o.with_snippet = false;
        },
        shaped: Mode::Direct,
    },
    Variant { name: "no_schema", apply: |o| o.no_schema = true, shaped: Mode::Direct },
    Variant { name: "legacy_octal_numbers", apply: |o| o.legacy_octal_numbers = true, shaped: Mode::Direct },
    Variant { name: "duplicate_keys=FirstWins", apply: |o| o.duplicate_keys = DuplicateKeyPolicy::FirstWins, shaped: Mode::Direct },
    Variant { name: "duplicate_keys=LastWins", apply: |o| o.duplicate_keys = DuplicateKeyPolicy::LastWins, shaped: Mode::Direct },
    Variant { name: "merge_keys=AsOrdinary", apply: |o| o.merge_keys = MergeKeyPolicy::AsOrdinary, shaped: Mode::Direct },
    Variant {
        name: "reject_non_finite_typeless_float=false",
        apply: |o| o.reject_non_finite_typeless_float = false,
        shaped: Mode::Direct,
    },
    Variant { name: "CODE CHANGE: norway-shaped errors (key path, serde wording, ` at line L column C`)", apply: |o| o.with_snippet = false, shaped: Mode::Shaped },
    Variant {
        name: "CODE CHANGE: norway-shaped errors + strict_booleans",
        apply: |o| {
            o.with_snippet = false;
            o.strict_booleans = true;
        },
        shaped: Mode::Shaped,
    },
    Variant {
        name: "CODE CHANGE: via serde_json::Value + strict_booleans (serde wording and key path, no location)",
        apply: |o| {
            o.with_snippet = false;
            o.strict_booleans = true;
        },
        shaped: Mode::ViaValue,
    },
];

fn options(variant: &Variant) -> Options {
    let mut options = Options::default();
    (variant.apply)(&mut options);
    options
}

/// A serde-saphyr `SerializerOptions` variant tried against every written difference.
struct WriteVariant {
    name: &'static str,
    apply: fn(&mut SerializerOptions),
    /// The code change: drop the `%YAML 1.2` directive and `---` that `yaml_12` writes.
    strip_directive: bool,
}

const WRITE_VARIANTS: &[WriteVariant] = &[
    WriteVariant { name: "default", apply: |_| {}, strip_directive: false },
    WriteVariant { name: "compact_list_indent=false", apply: |o| o.compact_list_indent = false, strip_directive: false },
    WriteVariant { name: "empty_as_braces=false", apply: |o| o.empty_as_braces = false, strip_directive: false },
    WriteVariant { name: "yaml_12", apply: |o| o.yaml_12 = true, strip_directive: false },
    WriteVariant {
        name: "compact_list_indent=false+empty_as_braces=false",
        apply: |o| {
            o.compact_list_indent = false;
            o.empty_as_braces = false;
        },
        strip_directive: false,
    },
    WriteVariant {
        name: "compact_list_indent=false+empty_as_braces=false+prefer_block_scalars=false",
        apply: |o| {
            o.compact_list_indent = false;
            o.empty_as_braces = false;
            o.prefer_block_scalars = false;
        },
        strip_directive: false,
    },
    WriteVariant { name: "CODE CHANGE: yaml_12, `%YAML 1.2` and `---` stripped", apply: |o| o.yaml_12 = true, strip_directive: true },
];

/// What one crate made of one text at one stage: a rendering (order-sensitive)
/// of the value read, or the error's text.
type Outcome = Result<String, String>;

fn norway<T: DeserializeOwned + std::fmt::Debug>(text: &str) -> Result<T, String> {
    serde_norway::from_str(text).map_err(|failure| failure.to_string())
}

fn saphyr<T: DeserializeOwned + std::fmt::Debug>(text: &str, variant: &Variant) -> Result<T, String> {
    match variant.shaped {
        Mode::Direct => {}
        Mode::Shaped => return shaped(text, options(variant)),
        Mode::ViaValue => return via_value(text, options(variant)),
    }
    serde_saphyr::from_str_with_options(text, options(variant)).map_err(|failure| failure.to_string())
}

/// The code change: read through `serde_path_to_error` for the key path, word
/// serde's own errors the way serde's defaults (and so serde_norway) word them,
/// and suffix the location as serde_norway does — `<path>: <message> at line L column C`.
fn shaped<T: DeserializeOwned>(text: &str, options: Options) -> Result<T, String> {
    use serde_path_to_error::Segment;
    let mut segments: Vec<Segment> = Vec::new();
    let result = serde_saphyr::with_deserializer_from_str_with_options(text, options, |deserializer| {
        serde_path_to_error::deserialize(deserializer).map_err(|failure| {
            segments = failure.path().iter().cloned().collect();
            failure.into_inner()
        })
    });
    result.map_err(|failure| {
        let bare = failure.without_snippet();
        // serde_norway's path ends at the node being read when the error is
        // raised: the value itself for an error its visitor raises (an invalid
        // value or type, an unknown variant), the enclosing mapping for an
        // unknown key or for an error a `Deserialize` impl raises once the
        // value is read (a custom message). The tracked path always ends at
        // the key, except for a missing field, raised at the mapping's end.
        let missing = matches!(bare, serde_saphyr::Error::SerdeMissingField { .. });
        let at_value = matches!(
            bare,
            serde_saphyr::Error::SerdeInvalidValue { .. }
                | serde_saphyr::Error::SerdeInvalidType { .. }
                | serde_saphyr::Error::SerdeUnknownVariant { .. }
                | serde_saphyr::Error::Unexpected { .. }
        );
        if !missing && !at_value {
            segments.pop();
        }
        let mut path = String::new();
        for segment in &segments {
            match segment {
                Segment::Seq { index } => {
                    let _ = write!(path, "[{index}]");
                }
                Segment::Map { key } | Segment::Enum { variant: key } => {
                    if !path.is_empty() {
                        path.push('.');
                    }
                    path.push_str(key);
                }
                Segment::Unknown => path.push_str(".?"),
            }
        }
        let mut out = String::new();
        if !path.is_empty() {
            let _ = write!(out, "{path}: ");
        }
        out.push_str(&serde_wording(bare));
        // serde_norway gives no location for a field missing from the root, or
        // for an error a `Deserialize` impl raises in a value at the root.
        let custom = matches!(bare, serde_saphyr::Error::Message { .. });
        let unplaced = (missing || custom) && path.is_empty();
        if let Some(at) = bare.location().filter(|_| !unplaced) {
            let _ = write!(out, " at line {} column {}", at.line(), at.column());
        }
        out
    })
}

/// The second code change: serde-saphyr reads the text into `serde_json::Value`
/// (an error there is shaped as [`shaped`] shapes it), and serde_json reads the
/// type from that value, so serde's own wording comes back — `Matcher` already
/// reads its fields this way — at the cost of the line and column.
fn via_value<T: DeserializeOwned>(text: &str, options: Options) -> Result<T, String> {
    let value: Value = shaped(text, options)?;
    serde_path_to_error::deserialize(value).map_err(|failure| {
        let path = failure.path().to_string();
        let inner = failure.into_inner();
        if path == "." { inner.to_string() } else { format!("{path}: {inner}") }
    })
}

/// serde's default wording for the errors serde itself raises (`serde::de::Error`'s
/// provided methods), which serde_norway passes through and serde-saphyr rewords.
fn serde_wording(failure: &serde_saphyr::Error) -> String {
    use serde_saphyr::{DefaultMessageFormatter, Error, MessageFormatter};
    fn one_of(names: &[&str]) -> String {
        match names {
            [] => "there are no fields".to_owned(),
            [one] => format!("expected `{one}`"),
            [a, b] => format!("expected `{a}` or `{b}`"),
            many => format!("expected one of {}", many.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")),
        }
    }
    match failure {
        Error::SerdeUnknownField { field, expected, .. } => format!("unknown field `{field}`, {}", one_of(expected)),
        Error::SerdeUnknownVariant { variant, expected, .. } => {
            let tail = if expected.is_empty() { "there are no variants".to_owned() } else { one_of(expected) };
            format!("unknown variant `{variant}`, {tail}")
        }
        other => DefaultMessageFormatter.format_message(other).into_owned(),
    }
}

/// Render a read value so that two reads compare equal exactly when they hold
/// the same data in the same order (`Value` objects keep document order under
/// `preserve_order`, and `Debug` walks them in it).
fn render<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

fn value_render(value: &Value) -> String {
    serde_json::to_string(value).expect("a Value serializes")
}

#[derive(Default)]
struct Report {
    /// Per document kind, the difference lines.
    sections: BTreeMap<&'static str, Vec<String>>,
    /// Per document kind, the count of cases run and cases differing.
    tallies: BTreeMap<&'static str, (usize, usize)>,
}

impl Report {
    fn tally(&mut self, kind: &'static str, differed: bool) {
        let entry = self.tallies.entry(kind).or_default();
        entry.0 += 1;
        if differed {
            entry.1 += 1;
        }
    }

    fn push(&mut self, kind: &'static str, text: String) {
        self.sections.entry(kind).or_default().push(text);
    }
}

fn indent(text: &str) -> String {
    text.lines().map(|line| format!("      | {line}")).collect::<Vec<_>>().join("\n")
}

fn show(outcome: &Outcome) -> String {
    match outcome {
        Ok(value) => format!("    read:\n{}", indent(value)),
        Err(error) => format!("    error:\n{}", indent(error)),
    }
}

/// Compare one stage of one case under every variant; answer a difference
/// block when the default variant differs from serde_norway.
fn stage(
    label: &str,
    stage: &str,
    expect: Option<&str>,
    before: &Outcome,
    after: &dyn Fn(&Variant) -> Outcome,
) -> Option<String> {
    let default = after(&VARIANTS[0]);
    if &default == before {
        return None;
    }
    let mut same = Vec::new();
    let mut class_only = Vec::new();
    for variant in VARIANTS {
        let outcome = after(variant);
        if &outcome == before {
            same.push(variant.name);
        } else if matches!((&outcome, before), (Err(_), Err(_))) {
            let keeps = expect.is_none_or(|needle| outcome.as_ref().err().is_some_and(|e| e.contains(needle)));
            if keeps {
                class_only.push(variant.name);
            }
        }
    }
    let class = match (before, &default) {
        (Ok(_), Ok(_)) => "READ DIFFERS",
        (Err(_), Err(_)) => "ERROR TEXT DIFFERS",
        (Ok(_), Err(_)) => "SAPHYR REFUSES WHAT NORWAY READS",
        (Err(_), Ok(_)) => "SAPHYR READS WHAT NORWAY REFUSES",
    };
    let mut block = format!("  [{stage}] {label}: {class}\n");
    if let Some(needle) = expect {
        let held = default.as_ref().err().is_some_and(|e| e.contains(needle));
        let _ = writeln!(block, "    the test asserts the error contains {needle:?}: saphyr default {}", if held { "still does" } else { "DOES NOT" });
    }
    let _ = writeln!(block, "    -- serde_norway 0.9 --\n{}", show(before));
    let _ = writeln!(block, "    -- serde-saphyr 1.3 (default Options) --\n{}", show(&default));
    if matches!(default, Err(_)) {
        for variant in VARIANTS.iter().filter(|v| v.name == "with_snippet=false" || v.shaped != Mode::Direct) {
            let other = after(variant);
            if other != default && &other != before {
                let _ = writeln!(block, "    -- serde-saphyr 1.3 ({}) --\n{}", variant.name, show(&other));
            }
        }
    }
    let _ = writeln!(
        block,
        "    reproduced exactly by: {}",
        if same.is_empty() { "NONE of the variants tried".to_owned() } else { same.join(", ") }
    );
    if !class_only.is_empty() {
        let _ = writeln!(
            block,
            "    refuses as norway does{} under: {}",
            if expect.is_some() { ", naming what the test asserts," } else { "" },
            class_only.join(", ")
        );
    }
    Some(block)
}

fn outcome<T: std::fmt::Debug>(result: Result<T, String>) -> Outcome {
    result.map(|value| render(&value))
}

fn value_outcome(result: Result<Value, String>) -> Outcome {
    result.map(|value| value_render(&value))
}

fn run_config(report: &mut Report, case: &Case) {
    let text = case.text.as_str();
    let mut blocks = Vec::new();
    // Stage 1: the schema-less pass `Config::parse` makes for codec keys and author names.
    let before = value_outcome(norway::<Value>(text));
    blocks.extend(stage(&case.label, "value pass", None, &before, &|variant| {
        value_outcome(saphyr::<Value>(text, variant))
    }));
    // Stage 2: the typed pass.
    let typed_before = outcome(norway::<Config>(text));
    blocks.extend(stage(&case.label, "typed pass", None, &typed_before, &|variant| {
        outcome(saphyr::<Config>(text, variant))
    }));
    // The whole `Config::parse` as a user meets it: what the asserted error
    // cases check. serde-saphyr's would-be result is norway's own where both
    // YAML stages agree (the rest of `parse` is deterministic over them), and
    // the stage it first fails at otherwise.
    if let Some(needle) = case.expect.as_deref() {
        let parsed = Config::parse(text);
        let real_why = match &parsed {
            Err(onemessagebus::ConfigError::Parse { why, .. }) => Some(why.clone()),
            _ => None,
        };
        // The `why` of a parse refusal (its display is `<path>: <why>`, the path
        // empty here), and the whole display of any other.
        let real = parsed.map(|config| render(&config)).map_err(|e| real_why.clone().unwrap_or_else(|| e.to_string()));
        let would_be = |variant: &Variant| -> Outcome {
            let value = value_outcome(saphyr::<Value>(text, variant));
            if let Err(error) = value {
                return Err(error);
            }
            let typed = outcome(saphyr::<Config>(text, variant));
            if value == before && typed == typed_before {
                return real.clone();
            }
            match (&before, &typed_before, &typed) {
                // Norway failed a check between the two passes; saphyr's value pass agrees.
                (Ok(_), Err(norway_typed), _) if real_why.as_ref() != Some(norway_typed) && value == before => real.clone(),
                (_, _, Err(error)) => Err(error.clone()),
                _ => Err(format!("(would-be undetermined: the YAML stages differ) {value:?} / {typed:?}")),
            }
        };
        blocks.extend(stage(&case.label, "Config::parse", Some(needle), &real, &would_be));
    }
    report.tally("config", !blocks.is_empty());
    for block in blocks {
        report.push("config", block);
    }
}

fn run_filter<V: Vocabulary>(report: &mut Report, case: &Case, vocab: &'static str) {
    let text = case.text.as_str();
    // An asserted case is read under the vocabulary its test reads it under; any
    // other filter under `Open` (the command line's) and under each vocabulary
    // that accepts it today — a vocabulary refusing a filter is not its reader.
    match case.vocab {
        Some(only) if only != vocab => return,
        None if vocab != "Open" && Filter::<V>::parse(text).is_err() => return,
        _ => {}
    }
    let label = format!("{} [vocabulary {vocab}]", case.label);
    let parse = |result: Result<Filter<V>, String>| -> Outcome {
        result
            .map_err(|failure| format!("the event filter is unusable: {failure}"))
            .and_then(|filter| filter.validate().map(|()| filter))
            .map(|filter| render(&filter))
    };
    let before = parse(norway::<Filter<V>>(text));
    let mut blocks: Vec<String> = Vec::new();
    blocks.extend(stage(&label, "Filter::parse", case.expect.as_deref(), &before, &|variant| {
        parse(saphyr::<Filter<V>>(text, variant))
    }));
    if let Some(needle) = case.expect.as_deref() {
        // The asserted substring must be in what Filter::parse says today, or
        // the harness's restatement of it is wrong.
        let real = Filter::<V>::parse(text).map(|f| render(&f)).map_err(|e| e.to_string());
        assert_eq!(real, before, "the harness's Filter::parse restatement drifted for {label}");
        assert!(
            before.as_ref().err().is_some_and(|e| e.contains(needle)),
            "serde_norway itself does not say {needle:?} for {label}: {before:?}"
        );
    }
    report.tally("filter", !blocks.is_empty());
    for block in blocks {
        report.push("filter", block);
    }
}

fn run_predicate(report: &mut Report, case: &Case) {
    let text = case.text.as_str();
    let parse = |result: Result<Predicate, String>| -> Outcome {
        result.map_err(|failure| format!("not a predicate: {failure}")).map(|p| render(&p))
    };
    let before = parse(norway::<Predicate>(text));
    if text.trim_start().starts_with('{') {
        let real = Predicate::read(text).map(|p| render(&p));
        assert_eq!(real, before, "the harness's Predicate::read restatement drifted for {}", case.label);
    }
    let block = stage(&case.label, "Predicate::read", case.expect.as_deref(), &before, &|variant| {
        parse(saphyr::<Predicate>(text, variant))
    });
    report.tally("predicate", block.is_some());
    report.push_opt("predicate", block);
}

impl Report {
    fn push_opt(&mut self, kind: &'static str, block: Option<String>) {
        if let Some(block) = block {
            self.push(kind, block);
        }
    }
}

/// What each crate writes for a configuration that reads, and whether each
/// reads the other's bytes back to the same configuration.
fn run_write(report: &mut Report, label: &str, config: &Config) {
    let before = serde_norway::to_string(config).expect("norway writes");
    let after = |variant: &WriteVariant| -> String {
        let mut options = SerializerOptions::default();
        (variant.apply)(&mut options);
        let written = serde_saphyr::to_string_with_options(config, options).expect("saphyr writes");
        if variant.strip_directive {
            written.strip_prefix("%YAML 1.2\n---\n").map(str::to_owned).unwrap_or(written)
        } else {
            written
        }
    };
    let default = after(&WRITE_VARIANTS[0]);
    let mut block = String::new();
    if default != before {
        let same: Vec<_> = WRITE_VARIANTS.iter().filter(|v| after(v) == before).map(|v| v.name).collect();
        let _ = writeln!(block, "  [write] {label}: BYTES DIFFER");
        let _ = writeln!(block, "    -- serde_norway::to_string --\n{}", indent(&before));
        let _ = writeln!(block, "    -- serde_saphyr::to_string (default SerializerOptions) --\n{}", indent(&default));
        for variant in &WRITE_VARIANTS[1..] {
            let written = after(variant);
            if written != default && written != before {
                let _ = writeln!(block, "    -- serde_saphyr with {} --\n{}", variant.name, indent(&written));
            }
        }
        let _ = writeln!(
            block,
            "    reproduced byte for byte by: {}",
            if same.is_empty() { "NONE of the variants tried".to_owned() } else { same.join(", ") }
        );
    }
    // Cross reads: each crate's bytes read back by both, to the same configuration.
    for (writer, bytes) in [("norway", &before), ("saphyr", &default)] {
        let by_norway = Config::parse(bytes).map_err(|e| e.to_string());
        let by_saphyr = saphyr::<Config>(bytes, &VARIANTS[0]);
        for (reader, read) in [("norway (Config::parse)", by_norway), ("saphyr", by_saphyr)] {
            match read {
                Ok(read) if &read == config => {}
                Ok(read) => {
                    let _ = writeln!(block, "  [write] {label}: {writer}'s bytes read by {reader} to a DIFFERENT configuration: {read:?}");
                }
                Err(error) => {
                    let _ = writeln!(block, "  [write] {label}: {writer}'s bytes REFUSED by {reader}:\n{}", indent(&error));
                }
            }
        }
    }
    report.tally("config writes", !block.is_empty());
    if !block.is_empty() {
        report.push("config writes", block);
    }
}

/// The schema-less walks need a value per key in document order with its YAML
/// type; compare saphyr's node tree, made into that shape, with norway's Value.
fn run_tree(report: &mut Report, kind: &'static str, case: &Case) {
    let before = value_outcome(norway::<Value>(&case.text));
    let after = tree::to_json(&case.text);
    let section = match kind {
        "config" => "schema-less: config",
        "filter" => "schema-less: filter",
        _ => "schema-less: predicate",
    };
    let differs = before != after;
    report.tally(section, differs);
    if differs {
        report.push(
            section,
            format!(
                "  [saphyr tree vs norway Value] {}\n    -- serde_norway Value --\n{}\n    -- saphyr::Yaml as JSON --\n{}\n",
                case.label,
                show(&before),
                show(&after)
            ),
        );
    }
}

/// Per-run microseconds over `ROUNDS` rounds of `ITERATIONS` runs each (after a
/// warm-up): the fastest round's mean and the median round's mean. The host is
/// shared, so the fastest round is the least disturbed estimate.
const ROUNDS: usize = 9;
const ITERATIONS: u32 = 1000;

fn time<F: FnMut()>(mut f: F) -> (f64, f64) {
    for _ in 0..ITERATIONS / 4 {
        f();
    }
    let mut means: Vec<f64> = (0..ROUNDS)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..ITERATIONS {
                f();
            }
            start.elapsed().as_secs_f64() * 1e6 / f64::from(ITERATIONS)
        })
        .collect();
    means.sort_by(f64::total_cmp);
    (means[0], means[ROUNDS / 2])
}

fn timing(cases: &[Case]) -> String {
    let largest = cases
        .iter()
        .filter(|case| case.origin != Origin::Probe && case.kind == Kind::Config && case.expect.is_none())
        .filter(|case| Config::parse(&case.text).is_ok())
        .max_by_key(|case| case.text.len())
        .expect("a configuration reads");
    let text = largest.text.as_str();
    let mut strict = Options::default();
    strict.strict_booleans = true;
    let rows: Vec<(&str, (f64, f64))> = vec![
        ("serde_norway  Value+Config (the two YAML passes Config::parse makes)", time(|| {
            let _: Value = serde_norway::from_str(text).expect("reads");
            let _: Config = serde_norway::from_str(text).expect("reads");
        })),
        ("serde-saphyr  Value+Config, default Options", time(|| {
            let _: Value = serde_saphyr::from_str(text).expect("reads");
            let _: Config = serde_saphyr::from_str(text).expect("reads");
        })),
        ("serde-saphyr  Value+Config, strict_booleans", time(|| {
            let _: Value = serde_saphyr::from_str_with_options(text, strict.clone()).expect("reads");
            let _: Config = serde_saphyr::from_str_with_options(text, strict.clone()).expect("reads");
        })),
        ("serde_norway  Value only", time(|| {
            let _: Value = serde_norway::from_str(text).expect("reads");
        })),
        ("serde-saphyr  Value only", time(|| {
            let _: Value = serde_saphyr::from_str(text).expect("reads");
        })),
        ("serde_norway  Config only", time(|| {
            let _: Config = serde_norway::from_str(text).expect("reads");
        })),
        ("serde-saphyr  Config only", time(|| {
            let _: Config = serde_saphyr::from_str(text).expect("reads");
        })),
        ("saphyr 0.2    node tree only (Yaml::load_from_str)", time(|| {
            let _ = tree::load(text);
        })),
        ("Config::parse today, end to end (YAML + validation)", time(|| {
            let _ = Config::parse(text).expect("reads");
        })),
    ];
    let mut out = format!(
        "largest real document: {} ({} bytes, {} lines)\n  {} build; per run, microseconds: fastest round's mean / median round's mean, {ROUNDS} rounds of {ITERATIONS} runs\n",
        largest.label,
        text.len(),
        text.lines().count(),
        if cfg!(debug_assertions) { "DEBUG" } else { "release" }
    );
    for (name, (fastest, median)) in rows {
        let _ = writeln!(out, "  {name:<72} {fastest:>8.1} / {median:>8.1}");
    }
    out
}

fn main() {
    let cases = corpus::load();
    let mut report = Report::default();
    for case in &cases {
        match case.kind {
            Kind::Config => {
                run_config(&mut report, case);
                run_tree(&mut report, "config", case);
                if case.expect.is_none() {
                    if let Ok(config) = Config::parse(&case.text) {
                        run_write(&mut report, &case.label, &config);
                    }
                }
            }
            Kind::Filter => {
                run_filter::<Open>(&mut report, case, "Open");
                run_filter::<Commerce>(&mut report, case, "Commerce");
                run_filter::<Vcs>(&mut report, case, "onevcs");
                run_tree(&mut report, "filter", case);
            }
            Kind::Predicate => {
                run_predicate(&mut report, case);
                run_tree(&mut report, "predicate", case);
            }
        }
    }
    for (label, config) in corpus::round_trips() {
        run_write(&mut report, &label, &config);
    }

    println!("saphyr-diff: serde_norway 0.9.42 (today) vs serde-saphyr 1.3.0, saphyr 0.2.1\n");
    println!("{}", corpus::summary(&cases));
    for (kind, lines) in &report.sections {
        println!("==== {kind}: {} difference block(s) ====", lines.len());
        for line in lines {
            println!("{line}");
        }
    }
    println!("==== tallies (cases run, cases differing) ====");
    for (kind, (run, differed)) in &report.tallies {
        println!("  {kind}: {run} run, {differed} differ");
    }
    println!("\n==== parse time ====\n{}", timing(&cases));
}

