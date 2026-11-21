//! Edits JSON with comments and trailing commas (`settings.jsonc`, MCP config).

use anyhow::{Context, Result, anyhow};
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use jsonc_parser::{JsonValue, ParseOptions};
use serde_json::Value;

/// Sets `path` to `value`, creating missing parent objects. Comments and layout are kept.
pub fn jsonc_set(text: &str, path: &[&str], value: &Value) -> Result<String> {
    let (key, parents) = path.split_last().ok_or_else(|| anyhow!("empty path"))?;
    let root = parse_root(text)?;
    let object = object_at(root_object(&root)?, parents)?;
    let input = to_input(value);
    match object.get(key) {
        Some(prop) => prop.set_value(input),
        None => {
            object.append(key, input);
        }
    }
    Ok(root.to_string())
}

/// Removes the value at `path`. Missing paths leave the text unchanged.
pub fn jsonc_remove(text: &str, path: &[&str]) -> Result<String> {
    let Some((key, parents)) = path.split_last() else {
        return Ok(text.to_string());
    };
    let root = parse_root(text)?;
    let mut object = root_object(&root)?;
    for parent in parents {
        match object.object_value(parent) {
            Some(child) => object = child,
            None => return Ok(text.to_string()),
        }
    }
    match object.get(key) {
        Some(prop) => {
            prop.remove();
            Ok(root.to_string())
        }
        None => Ok(text.to_string()),
    }
}

/// Reads the value at `path`, or `None` when it is absent.
pub fn jsonc_get(text: &str, path: &[&str]) -> Result<Option<Value>> {
    let parsed = jsonc_parser::parse_to_value(text, &ParseOptions::default())
        .map_err(|err| anyhow!("invalid JSONC: {err}"))?;
    let Some(mut current) = parsed else {
        return Ok(None);
    };
    for key in path {
        current = match current {
            JsonValue::Object(obj) => match obj.get(key).cloned() {
                Some(next) => next,
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
    }
    to_serde(current).map(Some)
}

/// An empty document is treated as an empty object.
fn normalized(text: &str) -> &str {
    if text.trim().is_empty() { "{}" } else { text }
}

fn parse_root(text: &str) -> Result<CstRootNode> {
    CstRootNode::parse(normalized(text), &ParseOptions::default())
        .map_err(|err| anyhow!("invalid JSONC: {err}"))
}

fn root_object(root: &CstRootNode) -> Result<CstObject> {
    root.object_value()
        .ok_or_else(|| anyhow!("the document root is not an object"))
}

/// Walks `keys` down from `start`, creating missing objects on the way.
fn object_at(start: CstObject, keys: &[&str]) -> Result<CstObject> {
    let mut current = start;
    for key in keys {
        current = current
            .object_value_or_create(key)
            .ok_or_else(|| anyhow!("`{key}` exists and is not an object"))?;
    }
    Ok(current)
}

fn to_input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(items) => CstInputValue::Array(items.iter().map(to_input).collect()),
        Value::Object(map) => CstInputValue::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), to_input(value)))
                .collect(),
        ),
    }
}

/// Converts a parsed JSONC value into serde_json, keeping object key order.
fn to_serde(value: JsonValue<'_>) -> Result<Value> {
    Ok(match value {
        JsonValue::Null => Value::Null,
        JsonValue::Boolean(b) => Value::Bool(b),
        JsonValue::Number(n) => {
            serde_json::from_str(n).with_context(|| format!("invalid number `{n}`"))?
        }
        JsonValue::String(s) => Value::String(s.into_owned()),
        JsonValue::Array(items) => Value::Array(
            items
                .into_iter()
                .map(to_serde)
                .collect::<Result<Vec<_>>>()?,
        ),
        JsonValue::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (key, value) in obj {
                map.insert(key.into_owned(), to_serde(value)?);
            }
            Value::Object(map)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WITH_COMMENT: &str =
        "{\n  // keep me\n  \"mcp\": { \"legacy\": { \"type\": \"local\" }, },\n}";

    #[test]
    fn jsonc_set_preserves_comments_and_trailing_commas() {
        let out = jsonc_set(
            WITH_COMMENT,
            &["mcp", "servers", "x"],
            &json!({"type": "remote"}),
        )
        .unwrap();
        assert!(out.contains("// keep me"));
        assert!(out.contains("\"legacy\""));
        assert_eq!(
            jsonc_get(&out, &["mcp", "legacy", "type"]).unwrap(),
            Some(json!("local"))
        );
        assert_eq!(
            jsonc_get(&out, &["mcp", "servers", "x", "type"]).unwrap(),
            Some(json!("remote"))
        );
    }

    #[test]
    fn jsonc_set_creates_missing_parents() {
        let out = jsonc_set("{}", &["a", "b", "c"], &json!(1)).unwrap();
        assert_eq!(jsonc_get(&out, &["a", "b", "c"]).unwrap(), Some(json!(1)));
        let out = jsonc_set("", &["x"], &json!(true)).unwrap();
        assert_eq!(jsonc_get(&out, &["x"]).unwrap(), Some(json!(true)));
    }

    #[test]
    fn jsonc_set_refuses_to_overwrite_a_scalar_parent() {
        assert!(jsonc_set("{\"a\": 1}", &["a", "b"], &json!(2)).is_err());
    }

    #[test]
    fn jsonc_remove_deletes_only_the_target() {
        let out = jsonc_remove("{\"a\": 1, \"b\": {\"c\": 2}}", &["b", "c"]).unwrap();
        assert_eq!(jsonc_get(&out, &["b", "c"]).unwrap(), None);
        assert_eq!(jsonc_get(&out, &["a"]).unwrap(), Some(json!(1)));
        assert_eq!(jsonc_get(&out, &["b"]).unwrap(), Some(json!({})));
    }

    #[test]
    fn jsonc_remove_missing_path_is_a_no_op() {
        let text = "{\n  // c\n  \"a\": 1,\n}\n";
        assert_eq!(jsonc_remove(text, &["zzz", "q"]).unwrap(), text);
    }

    #[test]
    fn jsonc_get_reads_nested_values() {
        let text = "{\"a\": {\"b\": [1, \"two\", null]}, \"flag\": false}";
        assert_eq!(
            jsonc_get(text, &["a", "b"]).unwrap(),
            Some(json!([1, "two", null]))
        );
        assert_eq!(jsonc_get(text, &["flag"]).unwrap(), Some(json!(false)));
        assert_eq!(jsonc_get(text, &["a", "missing"]).unwrap(), None);
    }
}
