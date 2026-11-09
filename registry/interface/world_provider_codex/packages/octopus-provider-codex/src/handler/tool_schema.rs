use serde_json::Value;
use std::collections::BTreeSet;

// A const/enum already implies these types in JSON Schema. Spell them out for
// native tool renderers that otherwise default an untyped literal to a string.
// Only schema positions are visited; defaults, examples and literal objects are data.
pub(super) fn make_literal_types_explicit(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else { return; };
    if !object.contains_key("type") {
        let literals = object.get("const").map(std::slice::from_ref)
            .or_else(|| object.get("enum").and_then(Value::as_array).map(Vec::as_slice));
        if let Some(literals) = literals.filter(|values| !values.is_empty()) {
            let mut types = literals.iter().map(|value| match value {
                Value::Null => "null", Value::Bool(_) => "boolean", Value::String(_) => "string",
                Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
                Value::Number(_) => "number", Value::Array(_) => "array", Value::Object(_) => "object",
            }).collect::<BTreeSet<_>>();
            if types.contains("number") { types.remove("integer"); }
            let value = if types.len() == 1 { Value::String(types.first().unwrap().to_string()) }
                else { Value::Array(types.into_iter().map(|kind| Value::String(kind.into())).collect()) };
            object.insert("type".into(), value);
        }
    }
    for keyword in ["properties", "patternProperties", "$defs", "definitions", "dependentSchemas", "dependencies"] {
        if let Some(children) = object.get_mut(keyword).and_then(Value::as_object_mut) {
            for child in children.values_mut() { make_literal_types_explicit(child); }
        }
    }
    for keyword in ["items", "additionalItems", "additionalProperties", "unevaluatedItems", "unevaluatedProperties", "contains", "propertyNames", "not", "if", "then", "else", "contentSchema"] {
        if let Some(child) = object.get_mut(keyword) {
            if let Some(items) = child.as_array_mut() {
                for item in items { make_literal_types_explicit(item); }
            } else { make_literal_types_explicit(child); }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for child in children { make_literal_types_explicit(child); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_literals_keep_their_json_types_without_rewriting_data() {
        let mut schema = json!({"type":"object", "properties": {
            "version":{"const":1}, "enabled":{"enum":[true,false]},
            "number":{"enum":[1,2.5]}, "optional":{"enum":[null,"yes"]},
            "nested":{"items":{"oneOf":[{"const":false},{"const":2}]}},
            "literal":{"const":{"version":{"const":1}}},
            "explicit":{"type":"number","const":1}
        }, "default":{"version":{"const":1}}, "examples":[{"version":{"const":1}}]});
        make_literal_types_explicit(&mut schema);
        let properties = &schema["properties"];
        assert_eq!(properties["version"], json!({"type":"integer","const":1}));
        assert_eq!(properties["enabled"]["type"], "boolean");
        assert_eq!(properties["number"]["type"], "number");
        assert_eq!(properties["optional"]["type"], json!(["null","string"]));
        assert_eq!(properties["nested"]["items"]["oneOf"][1]["type"], "integer");
        assert_eq!(properties["literal"]["const"], json!({"version":{"const":1}}));
        assert_eq!(properties["explicit"]["type"], "number");
        assert_eq!(schema["default"], json!({"version":{"const":1}}));
        assert_eq!(schema["examples"], json!([{"version":{"const":1}}]));
        let once = schema.clone();
        make_literal_types_explicit(&mut schema);
        assert_eq!(schema, once);
    }
}
