//! When a configured hook runs on a message: the `when` key of a `hooks:`
//! entry (`docs/hooks.md`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::queue::{FieldPath, Predicate};

/// When a configured hook runs on a message at all.
///
/// On the wire `{"carries": P}` — the field at `P` is there and holds something
/// (not `null`, `""`, `[]` or `{}`) — or any queue predicate (`docs/queues.md`).
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    /// The message carries something at this field.
    Carries(FieldPath),
    /// The message satisfies this predicate.
    Matches(Predicate),
}

impl When {
    /// Whether a hook runs on `message`.
    #[must_use]
    pub fn admits(&self, message: &Value) -> bool {
        match self {
            Self::Carries(field) => Predicate::NonEmpty {
                field: field.clone(),
                non_empty: true,
            }
            .matches(message),
            Self::Matches(predicate) => predicate.matches(message),
        }
    }
}

/// The key naming the `carries` form.
const CARRIES: &str = "carries";

impl Serialize for When {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Carries(field) => {
                let mut form = Map::new();
                form.insert(CARRIES.to_owned(), Value::String(field.to_string()));
                form.serialize(serializer)
            }
            Self::Matches(predicate) => predicate.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for When {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if let Some(form) = value.as_object().filter(|form| form.contains_key(CARRIES)) {
            if form.len() != 1 {
                let others: Vec<&str> = form
                    .keys()
                    .map(String::as_str)
                    .filter(|key| *key != CARRIES)
                    .collect();
                return Err(serde::de::Error::custom(format!(
                    "`carries` stands alone in a `when`, and this also names {}",
                    others.join(" and ")
                )));
            }
            let field = form
                .get(CARRIES)
                .and_then(Value::as_str)
                .ok_or_else(|| serde::de::Error::custom("`carries` names a field path"))?
                .parse::<FieldPath>()
                .map_err(serde::de::Error::custom)?;
            return Ok(Self::Carries(field));
        }
        Predicate::deserialize(value)
            .map(Self::Matches)
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for When {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("When")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let field = generator.subschema_for::<FieldPath>();
        let predicate = generator.subschema_for::<Predicate>();
        schemars::json_schema!({
            "description": "When a hook runs on a message: `{carries: <field path>}`, or any queue predicate.",
            "anyOf": [
                {
                    "type": "object",
                    "properties": { "carries": field },
                    "required": ["carries"],
                    "additionalProperties": false
                },
                predicate
            ]
        })
    }
}
