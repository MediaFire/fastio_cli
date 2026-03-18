/// Field filtering and format selection utilities.
///
/// Provides the logic for `--fields` filtering across all output formats.
use serde_json::Value;

/// Filter a JSON value to include only the specified fields.
///
/// If `fields` is `None`, the value is returned unchanged.
/// For objects, only matching keys are kept.
/// For arrays, each element is filtered independently.
#[must_use]
pub fn filter_fields(value: &Value, fields: Option<&[String]>) -> Value {
    let Some(field_list) = fields else {
        return value.clone();
    };

    if field_list.is_empty() {
        return value.clone();
    }

    match value {
        Value::Array(items) => {
            let filtered: Vec<Value> = items
                .iter()
                .map(|item| filter_object(item, field_list))
                .collect();
            Value::Array(filtered)
        }
        Value::Object(_) => filter_object(value, field_list),
        other => other.clone(),
    }
}

/// Filter a single JSON object to include only the specified keys.
fn filter_object(value: &Value, fields: &[String]) -> Value {
    let Value::Object(map) = value else {
        return value.clone();
    };

    let mut filtered = serde_json::Map::new();
    for field in fields {
        if let Some(v) = map.get(field) {
            filtered.insert(field.clone(), v.clone());
        }
    }
    Value::Object(filtered)
}
