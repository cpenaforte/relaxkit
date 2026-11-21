//! Edits strict JSON (`settings.json`). Key order is kept; output is two-space pretty JSON.

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value};

/// Sets `path` to `value`, creating missing parent objects. Key order is kept.
pub fn json_set(text: &str, path: &[&str], value: &Value) -> Result<String> {
    let (key, parents) = path.split_last().ok_or_else(|| anyhow!("empty path"))?;
    let mut root = parse(text)?;
    let mut object = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("the document root is not an object"))?;
    for parent in parents {
        object = object
            .entry(*parent)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| anyhow!("`{parent}` exists and is not an object"))?;
    }
    object.insert((*key).to_string(), value.clone());
    render(&root)
}

/// Removes the value at `path`. Missing paths leave the text unchanged.
pub fn json_remove(text: &str, path: &[&str]) -> Result<String> {
    let Some((key, parents)) = path.split_last() else {
        return Ok(text.to_string());
    };
    let mut root = parse(text)?;
    let root_object = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("the document root is not an object"))?;
    if remove_at(root_object, parents, key) {
        render(&root)
    } else {
        Ok(text.to_string())
    }
}

/// Removes `key` from the object reached through `parents`. False when the path is absent.
fn remove_at(object: &mut Map<String, Value>, parents: &[&str], key: &str) -> bool {
    match parents.split_first() {
        None => object.shift_remove(key).is_some(),
        Some((first, rest)) => match object.get_mut(*first).and_then(Value::as_object_mut) {
            Some(child) => remove_at(child, rest, key),
            None => false,
        },
    }
}

pub fn json_get(text: &str, path: &[&str]) -> Result<Option<Value>> {
    let mut current = parse(text)?;
    for key in path {
        current = match current.get(*key) {
            Some(next) => next.clone(),
            None => return Ok(None),
        };
    }
    Ok(Some(current))
}

/// An empty document is treated as an empty object.
fn parse(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(text).context("invalid JSON")
}

/// Pretty JSON with two-space indent and a trailing newline.
fn render(value: &Value) -> Result<String> {
    let mut out = serde_json::to_string_pretty(value).context("cannot render JSON")?;
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SETTINGS: &str = "{\n  \"permissions\": {\n    \"allow\": []\n  },\n  \"hooks\": {},\n  \"modelSettings\": {\n    \"model\": \"sonnet\"\n  }\n}\n";

    #[test]
    fn json_set_preserves_key_order_and_indent() {
        let out = json_set(SETTINGS, &["autoCompactWindow"], &json!(200000)).unwrap();
        let expected = "{\n  \"permissions\": {\n    \"allow\": []\n  },\n  \"hooks\": {},\n  \"modelSettings\": {\n    \"model\": \"sonnet\"\n  },\n  \"autoCompactWindow\": 200000\n}\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn json_set_creates_missing_parents() {
        let out = json_set("", &["a", "b"], &json!("x")).unwrap();
        assert_eq!(json_get(&out, &["a", "b"]).unwrap(), Some(json!("x")));
        assert!(out.ends_with("}\n"));
    }

    #[test]
    fn json_set_refuses_to_overwrite_a_scalar_parent() {
        assert!(json_set("{\"a\": 1}\n", &["a", "b"], &json!(2)).is_err());
    }

    #[test]
    fn json_remove_keeps_order_of_the_rest() {
        let out = json_remove(SETTINGS, &["hooks"]).unwrap();
        let permissions = out.find("\"permissions\"").unwrap();
        let model = out.find("\"modelSettings\"").unwrap();
        assert!(permissions < model);
        assert_eq!(json_get(&out, &["hooks"]).unwrap(), None);
        assert_eq!(json_remove(SETTINGS, &["nope"]).unwrap(), SETTINGS);
    }

    #[test]
    fn json_remove_first_key_keeps_the_order_of_the_rest() {
        let out = json_remove("{\"a\":1,\"b\":2,\"c\":3}", &["a"]).unwrap();
        let parsed: Map<String, Value> = serde_json::from_str(&out).unwrap();
        let keys: Vec<&str> = parsed.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["b", "c"]);
    }

    #[test]
    fn json_get_reads_nested_values() {
        assert_eq!(
            json_get(SETTINGS, &["modelSettings", "model"]).unwrap(),
            Some(json!("sonnet"))
        );
        assert_eq!(
            json_get(SETTINGS, &["modelSettings", "absent"]).unwrap(),
            None
        );
    }
}
