//! saphyr 0.2's node tree, made into the shape the schema-less walks read:
//! a JSON value with keys in document order. Anything JSON cannot hold is
//! rendered as a marker string, so a comparison shows it rather than hiding it.

use saphyr::{LoadableYamlNode, Scalar, Yaml};
use serde_json::{Map, Number, Value};

pub fn load(text: &str) -> Result<Vec<Yaml<'_>>, String> {
    Yaml::load_from_str(text).map_err(|failure| failure.to_string())
}

pub fn to_json(text: &str) -> Result<String, String> {
    let documents = load(text)?;
    let value = match documents.as_slice() {
        [] => Value::Null,
        [document] => convert(document),
        many => return Err(format!("{} documents", many.len())),
    };
    Ok(serde_json::to_string(&value).expect("a Value serializes"))
}

fn scalar(value: &Scalar<'_>) -> Value {
    match value {
        Scalar::Null => Value::Null,
        Scalar::Boolean(b) => Value::Bool(*b),
        Scalar::Integer(i) => Value::Number((*i).into()),
        Scalar::FloatingPoint(f) => Number::from_f64(f.0).map_or_else(|| Value::String(format!("<non-finite {}>", f.0)), Value::Number),
        Scalar::String(s) => Value::String(s.to_string()),
    }
}

fn convert(node: &Yaml<'_>) -> Value {
    match node {
        Yaml::Value(value) => scalar(value),
        Yaml::Representation(text, style, tag) => Value::String(format!("<unresolved {text:?} {style:?} {tag:?}>")),
        Yaml::Sequence(items) => Value::Array(items.iter().map(convert).collect()),
        Yaml::Mapping(entries) => {
            let mut map = Map::new();
            for (key, value) in entries {
                let key = match key {
                    Yaml::Value(Scalar::String(s)) => s.to_string(),
                    other => format!("<non-string key {other:?}>"),
                };
                map.insert(key, convert(value));
            }
            Value::Object(map)
        }
        Yaml::Tagged(tag, inner) => Value::String(format!("<tagged {tag:?} {}>", convert(inner))),
        Yaml::Alias(id) => Value::String(format!("<alias {id}>")),
        Yaml::BadValue => Value::String("<bad value>".to_owned()),
    }
}
