//! The profile's key tables against the types they describe.
//!
//! `RESERVED_LABELS` and `DIMENSIONS` are data because the command line and the
//! SDK manifest cannot see a Rust type, so they restate what `Labels`,
//! `Dimensions` and `MatchFields` already say in fields. Each struct's field set
//! is read here from its generated JSON Schema — the same document the registry
//! records — so a field added to a type without its table entry, or a table
//! entry whose kind the field does not carry, fails this suite by name.

use onemessagebus::{Admits, Reserved};
use onemessagebus_agent::{Dimensions, Labels, MatchFields, DIMENSIONS, RESERVED_LABELS};
use schemars::JsonSchema;
use serde_json::Value;

/// A type's JSON Schema, as JSON.
fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("a schema serializes")
}

/// The schema's properties, in declaration order (schemars preserves it).
fn properties(schema: &Value) -> Vec<(String, Value)> {
    schema["properties"]
        .as_object()
        .expect("an object schema has properties")
        .iter()
        .map(|(key, property)| (key.clone(), property.clone()))
        .collect()
}

/// What a property admits, read from its schema: a `$ref` to closed string
/// constants is a word, and otherwise the JSON type beside `null` decides.
fn admits(root: &Value, property: &Value) -> Option<Admits> {
    let arms: Vec<&Value> = match property.get("anyOf") {
        Some(Value::Array(arms)) => arms.iter().collect(),
        _ => vec![property],
    };
    let mut found = None;
    for arm in arms {
        let kind = if let Some(Value::String(pointer)) = arm.get("$ref") {
            let target = root.pointer(pointer.trim_start_matches('#'))?;
            is_closed_words(target).then_some(Admits::Word)
        } else {
            match arm.get("type") {
                Some(Value::String(ty)) => json_type(ty),
                Some(Value::Array(types)) => {
                    let mut named = types
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|t| *t != "null");
                    match (named.next(), named.next()) {
                        (Some(ty), None) => json_type(ty),
                        _ => return None,
                    }
                }
                _ => return None,
            }
        };
        match (found, kind) {
            (_, None) if arm.get("type") == Some(&Value::String("null".into())) => {}
            (None, Some(kind)) => found = Some(kind),
            _ => return None,
        }
    }
    found
}

fn json_type(ty: &str) -> Option<Admits> {
    match ty {
        "string" => Some(Admits::Text),
        "integer" => Some(Admits::Integer),
        _ => None,
    }
}

/// Whether a definition is a closed set of string words.
fn is_closed_words(definition: &Value) -> bool {
    if let Some(Value::Array(words)) = definition.get("enum") {
        return !words.is_empty() && words.iter().all(Value::is_string);
    }
    match definition.get("oneOf") {
        Some(Value::Array(arms)) => {
            !arms.is_empty()
                && arms
                    .iter()
                    .all(|arm| arm.get("const").is_some_and(Value::is_string))
        }
        _ => false,
    }
}

/// Holds `T`'s schema to `table`: the same keys in the same order, each
/// admitting what the table says. Every disagreement is named at once.
fn assert_agrees<T: JsonSchema>(type_name: &str, table_name: &str, table: &[Reserved]) {
    let schema = schema_of::<T>();
    let fields = properties(&schema);
    let field_keys: Vec<&str> = fields.iter().map(|(key, _)| key.as_str()).collect();
    let table_keys: Vec<&str> = table.iter().map(|reserved| reserved.key).collect();

    let missing: Vec<&str> = table_keys
        .iter()
        .copied()
        .filter(|key| !field_keys.contains(key))
        .collect();
    let extra: Vec<&str> = field_keys
        .iter()
        .copied()
        .filter(|key| !table_keys.contains(key))
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{type_name} and {table_name} disagree: {table_name} names {missing:?} that {type_name} has no field for, \
         and {type_name} has fields {extra:?} that {table_name} does not name"
    );
    assert_eq!(
        field_keys, table_keys,
        "{type_name}'s fields are not in {table_name}'s order"
    );

    let wrong: Vec<String> = table
        .iter()
        .zip(&fields)
        .filter_map(|(reserved, (key, property))| {
            let found = admits(&schema, property);
            (found != Some(reserved.admits)).then(|| {
                format!(
                    "{key}: {table_name} says {:?}, {type_name}'s schema admits {found:?}",
                    reserved.admits
                )
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{type_name} and {table_name} disagree on what a key admits: {wrong:?}"
    );
}

#[test]
fn labels_declares_exactly_the_reserved_labels_in_wire_order() {
    assert_agrees::<Labels>("Labels", "RESERVED_LABELS", RESERVED_LABELS);
}

#[test]
fn dimensions_declares_exactly_the_dimensions() {
    assert_agrees::<Dimensions>("Dimensions", "DIMENSIONS", DIMENSIONS);
}

/// The grammar matches a dimension and a text label by exact equality; an
/// integer label (`round`) is deliberately not matchable. The set is derived
/// from the tables by kind, so a new text label is expected in `MatchFields`
/// without anyone listing it here.
#[test]
fn match_fields_names_the_dimensions_and_every_text_label() {
    let matchable: Vec<Reserved> = DIMENSIONS
        .iter()
        .chain(
            RESERVED_LABELS
                .iter()
                .filter(|reserved| reserved.admits == Admits::Text),
        )
        .copied()
        .collect();
    assert_agrees::<MatchFields>(
        "MatchFields",
        "DIMENSIONS plus RESERVED_LABELS' text keys",
        &matchable,
    );
}

/// The reader above has to tell the kinds apart, or every assertion it backs
/// would pass by reading nothing.
#[test]
fn the_schema_reader_tells_each_kind_apart() {
    let labels = schema_of::<Labels>();
    let dimensions = schema_of::<Dimensions>();
    let property = |schema: &Value, key: &str| schema["properties"][key].clone();
    assert_eq!(
        admits(&labels, &property(&labels, "run_id")),
        Some(Admits::Text)
    );
    assert_eq!(
        admits(&labels, &property(&labels, "round")),
        Some(Admits::Integer)
    );
    assert_eq!(
        admits(&dimensions, &property(&dimensions, "phase")),
        Some(Admits::Word)
    );
    assert_eq!(
        admits(&labels, &serde_json::json!({"type": "boolean"})),
        None
    );
}
