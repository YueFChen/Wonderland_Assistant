//! Small, fail-closed JSON Schema evaluator for the v1 contract vocabulary.
//!
//! Unsupported assertion keywords are rejected while loading a contract instead of being
//! silently ignored. This keeps the host's validation behavior predictable without adding a
//! runtime schema compiler dependency to the Core.

use regex::Regex;
use serde_json::{Map, Number, Value};
use std::collections::HashSet;

const MAX_SCHEMA_DEPTH: usize = 64;
const SUPPORTED_KEYWORDS: &[&str] = &[
    "$comment",
    "$defs",
    "$schema",
    "$ref",
    "additionalProperties",
    "allOf",
    "anyOf",
    "const",
    "default",
    "deprecated",
    "description",
    "enum",
    "examples",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "items",
    "maxItems",
    "maxLength",
    "maximum",
    "minItems",
    "minLength",
    "minimum",
    "oneOf",
    "pattern",
    "properties",
    "readOnly",
    "required",
    "title",
    "type",
    "uniqueItems",
    "writeOnly",
];

pub fn ensure_supported_in(schema: &Value, root: &Value) -> Result<(), String> {
    walk_schema(schema, root, 0, &mut HashSet::new())
}

fn walk_schema(
    schema: &Value,
    root: &Value,
    depth: usize,
    visited_refs: &mut HashSet<String>,
) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err("Schema nesting exceeds the supported limit.".to_owned());
    }
    let Some(object) = schema.as_object() else {
        // JSON Schema Draft 2020-12 permits boolean schemas.
        return if schema.is_boolean() {
            Ok(())
        } else {
            Err("A JSON Schema must be an object or boolean.".to_owned())
        };
    };

    if let Some(keyword) = object
        .iter()
        .find(|(keyword, _)| !SUPPORTED_KEYWORDS.contains(&keyword.as_str()))
    {
        return Err(format!(
            "Contract uses unsupported JSON Schema keyword '{}'.",
            keyword.0
        ));
    }

    if let Some(reference) = object.get("$ref") {
        let reference = reference
            .as_str()
            .ok_or_else(|| "Schema keyword '$ref' must be a string.".to_owned())?;
        if visited_refs.insert(reference.to_owned()) {
            let target = resolve_ref(root, reference)?;
            walk_schema(target, root, depth + 1, visited_refs)?;
            visited_refs.remove(reference);
        }
    }
    if let Some(dialect) = object.get("$schema") {
        if dialect.as_str() != Some("https://json-schema.org/draft/2020-12/schema") {
            return Err("Only JSON Schema Draft 2020-12 is supported.".to_owned());
        }
    }
    if let Some(pattern) = object.get("pattern") {
        let pattern = pattern
            .as_str()
            .ok_or_else(|| "Schema keyword 'pattern' must be a string.".to_owned())?;
        if pattern.len() > 2_048 {
            return Err("Schema pattern exceeds 2048 bytes.".to_owned());
        }
        Regex::new(pattern).map_err(|error| format!("Invalid schema pattern: {error}"))?;
    }
    if let Some(kind) = object.get("type") {
        let valid_kind = |kind: &Value| {
            kind.as_str().is_some_and(|kind| {
                matches!(
                    kind,
                    "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
                )
            })
        };
        if !valid_kind(kind)
            && !kind
                .as_array()
                .is_some_and(|kinds| !kinds.is_empty() && kinds.iter().all(valid_kind))
        {
            return Err("Schema keyword 'type' contains an unsupported type.".to_owned());
        }
    }
    if let Some(required) = object.get("required") {
        if !required
            .as_array()
            .is_some_and(|keys| keys.iter().all(Value::is_string))
        {
            return Err("Schema keyword 'required' must be an array of strings.".to_owned());
        }
    }
    if let Some(enum_values) = object.get("enum") {
        if !enum_values
            .as_array()
            .is_some_and(|values| !values.is_empty())
        {
            return Err("Schema keyword 'enum' must be a non-empty array.".to_owned());
        }
    }
    for keyword in ["minLength", "maxLength", "minItems", "maxItems"] {
        if object
            .get(keyword)
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(format!(
                "Schema keyword '{keyword}' must be a non-negative integer."
            ));
        }
    }
    for keyword in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
        if object
            .get(keyword)
            .is_some_and(|value| value.as_f64().is_none())
        {
            return Err(format!("Schema keyword '{keyword}' must be a number."));
        }
    }
    if object
        .get("properties")
        .is_some_and(|properties| !properties.is_object())
    {
        return Err("Schema keyword 'properties' must be an object.".to_owned());
    }
    for keyword in ["items", "additionalProperties"] {
        if object
            .get(keyword)
            .is_some_and(|child| !child.is_object() && !child.is_boolean())
        {
            return Err(format!("Schema keyword '{keyword}' must be a schema."));
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if object
            .get(keyword)
            .is_some_and(|children| !children.as_array().is_some())
        {
            return Err(format!("Schema keyword '{keyword}' must be an array."));
        }
    }

    for keyword in ["properties", "$defs"] {
        if let Some(children) = object.get(keyword).and_then(Value::as_object) {
            for child in children.values() {
                walk_schema(child, root, depth + 1, visited_refs)?;
            }
        }
    }
    for keyword in ["items", "additionalProperties"] {
        if let Some(child) = object.get(keyword) {
            if child.is_object() || child.is_boolean() {
                walk_schema(child, root, depth + 1, visited_refs)?;
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(children) = object.get(keyword).and_then(Value::as_array) {
            for child in children {
                walk_schema(child, root, depth + 1, visited_refs)?;
            }
        }
    }
    Ok(())
}

pub fn validate(instance: &Value, schema: &Value, root: &Value) -> Result<(), String> {
    validate_at(instance, schema, root, 0)
}

fn validate_at(instance: &Value, schema: &Value, root: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err("Schema evaluation exceeds the supported nesting limit.".to_owned());
    }
    if schema == &Value::Bool(true) {
        return Ok(());
    }
    if schema == &Value::Bool(false) {
        return Err("Value is rejected by a false schema.".to_owned());
    }
    let object = schema
        .as_object()
        .ok_or_else(|| "A JSON Schema must be an object or boolean.".to_owned())?;

    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        let target = resolve_ref(root, reference)?;
        validate_at(instance, target, root, depth + 1)?;
    }

    if let Some(expected) = object.get("type") {
        let matches = match expected {
            Value::String(kind) => matches_type(instance, kind),
            Value::Array(kinds) => kinds.iter().any(|kind| {
                kind.as_str()
                    .is_some_and(|kind| matches_type(instance, kind))
            }),
            _ => return Err("Schema keyword 'type' must be a string or array.".to_owned()),
        };
        if !matches {
            return Err(format!(
                "Value does not match expected type '{}'.",
                expected
            ));
        }
    }

    if let Some(expected) = object.get("const") {
        if instance != expected {
            return Err("Value does not match the required constant.".to_owned());
        }
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        if !values.iter().any(|value| value == instance) {
            return Err("Value is not in the allowed enum.".to_owned());
        }
    }

    for (keyword, expected_count) in [("minLength", true), ("maxLength", false)] {
        if let (Some(text), Some(limit)) = (
            instance.as_str(),
            object.get(keyword).and_then(Value::as_u64),
        ) {
            let length = text.chars().count() as u64;
            if (expected_count && length < limit) || (!expected_count && length > limit) {
                return Err(format!("String violates '{keyword}'."));
            }
        }
    }
    if let (Some(text), Some(pattern)) = (
        instance.as_str(),
        object.get("pattern").and_then(Value::as_str),
    ) {
        let regex =
            Regex::new(pattern).map_err(|error| format!("Invalid schema pattern: {error}"))?;
        if !regex.is_match(text) {
            return Err("String does not match the required pattern.".to_owned());
        }
    }

    if let Some(properties) = instance.as_object() {
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !properties.contains_key(key) {
                    return Err(format!("Required property '{key}' is missing."));
                }
            }
        }
        let declared = object.get("properties").and_then(Value::as_object);
        for (key, value) in properties {
            if let Some(property_schema) = declared.and_then(|declared| declared.get(key)) {
                validate_at(value, property_schema, root, depth + 1)
                    .map_err(|error| format!("Property '{key}': {error}"))?;
            } else if let Some(additional) = object.get("additionalProperties") {
                match additional {
                    Value::Bool(false) => {
                        return Err(format!("Unexpected property '{key}'."));
                    }
                    Value::Bool(true) => {}
                    schema => validate_at(value, schema, root, depth + 1)
                        .map_err(|error| format!("Property '{key}': {error}"))?,
                }
            }
        }
    }

    if let Some(items) = instance.as_array() {
        for (keyword, minimum) in [("minItems", true), ("maxItems", false)] {
            if let Some(limit) = object.get(keyword).and_then(Value::as_u64) {
                let length = items.len() as u64;
                if (minimum && length < limit) || (!minimum && length > limit) {
                    return Err(format!("Array violates '{keyword}'."));
                }
            }
        }
        if object.get("uniqueItems") == Some(&Value::Bool(true)) {
            for index in 0..items.len() {
                if items[index + 1..]
                    .iter()
                    .any(|value| value == &items[index])
                {
                    return Err("Array items must be unique.".to_owned());
                }
            }
        }
        if let Some(item_schema) = object.get("items") {
            for (index, item) in items.iter().enumerate() {
                validate_at(item, item_schema, root, depth + 1)
                    .map_err(|error| format!("Array item {index}: {error}"))?;
            }
        }
    }

    if let Some(number) = instance.as_number() {
        validate_number(number, object)?;
    }

    for (keyword, mode) in [("allOf", 0), ("anyOf", 1), ("oneOf", 2)] {
        if let Some(schemas) = object.get(keyword).and_then(Value::as_array) {
            let matches = schemas
                .iter()
                .filter(|child| validate_at(instance, child, root, depth + 1).is_ok())
                .count();
            let valid = match mode {
                0 => matches == schemas.len(),
                1 => matches > 0,
                _ => matches == 1,
            };
            if !valid {
                return Err(format!("Value does not satisfy '{keyword}'."));
            }
        }
    }
    Ok(())
}

fn validate_number(number: &Number, schema: &Map<String, Value>) -> Result<(), String> {
    let Some(value) = number.as_f64() else {
        return Ok(());
    };
    for (keyword, lower_bound) in [
        ("minimum", true),
        ("exclusiveMinimum", true),
        ("maximum", false),
        ("exclusiveMaximum", false),
    ] {
        let Some(bound) = schema.get(keyword).and_then(Value::as_f64) else {
            continue;
        };
        let is_exclusive = keyword.starts_with("exclusive");
        let valid = if lower_bound {
            if is_exclusive {
                value > bound
            } else {
                value >= bound
            }
        } else if is_exclusive {
            value < bound
        } else {
            value <= bound
        };
        if !valid {
            return Err(format!("Number violates '{keyword}'."));
        }
    }
    Ok(())
}

fn matches_type(value: &Value, kind: &str) -> bool {
    match kind {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        _ => false,
    }
}

fn resolve_ref<'a>(root: &'a Value, reference: &str) -> Result<&'a Value, String> {
    if reference == "#" {
        return Ok(root);
    }
    let Some(pointer) = reference.strip_prefix('#') else {
        return Err("Only local JSON Schema references are supported.".to_owned());
    };
    root.pointer(pointer)
        .ok_or_else(|| format!("Unresolved schema reference '{reference}'."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_nested_contract_data_and_local_refs() {
        let schema = json!({
            "$defs": { "nonEmpty": { "type": "string", "minLength": 1 } },
            "type": "object",
            "required": ["name", "count"],
            "properties": {
                "name": { "$ref": "#/$defs/nonEmpty" },
                "count": { "type": "integer", "minimum": 0 }
            },
            "additionalProperties": false
        });
        ensure_supported_in(&schema, &schema).unwrap();
        validate(&json!({ "name": "echo", "count": 1 }), &schema, &schema).unwrap();
        assert!(validate(&json!({ "name": "", "count": -1 }), &schema, &schema).is_err());
        assert!(
            validate(
                &json!({ "name": "ok", "count": 1, "extra": true }),
                &schema,
                &schema
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_schema_features_it_cannot_enforce() {
        let schema = json!({ "type": "object", "unevaluatedProperties": false });
        assert!(ensure_supported_in(&schema, &schema).is_err());
    }

    #[test]
    fn validates_referenced_schemas_and_rejects_remote_refs() {
        let contract = json!({
            "$defs": { "value": { "type": "string", "multipleOf": 2 } },
            "methods": { "echo": { "params": { "$ref": "#/$defs/value" } } }
        });
        assert!(ensure_supported_in(&contract["methods"]["echo"]["params"], &contract).is_err());
        let remote_ref = json!({ "$ref": "https://example.invalid/schema" });
        assert!(ensure_supported_in(&remote_ref, &remote_ref).is_err());
    }
}
