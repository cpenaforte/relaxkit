//! Edits TOML (Codex-style `config.toml`) with `toml_edit`, which keeps comments and layout.

use anyhow::{Context, Result, anyhow};
use toml_edit::{DocumentMut, Item, Table, TableLike};

/// Sets the value at `path` to `value`, creating missing tables. Comments are kept.
pub fn toml_set(text: &str, path: &[&str], mut value: Item) -> Result<String> {
    let (key, parents) = path.split_last().ok_or_else(|| anyhow!("empty path"))?;
    let mut doc: DocumentMut = text.parse().context("invalid TOML")?;
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for parent in parents {
        if !table.contains_key(parent) {
            let mut implicit = Table::new();
            implicit.set_implicit(true);
            table.insert(parent, Item::Table(implicit));
        }
        table = table
            .get_mut(parent)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| anyhow!("`{parent}` exists and is not a table"))?;
    }
    match table.get_mut(key) {
        Some(existing) => {
            // Keep the old value's spacing and trailing comment; the key's own decor is untouched.
            if let (Item::Value(old), Item::Value(new)) = (&*existing, &mut value) {
                *new.decor_mut() = old.decor().clone();
            }
            *existing = value;
        }
        None => {
            table.insert(key, value);
        }
    }
    Ok(doc.to_string())
}

/// Reads the item at `path`, or `None` when it is absent.
pub fn toml_get(text: &str, path: &[&str]) -> Result<Option<Item>> {
    let doc: DocumentMut = text.parse().context("invalid TOML")?;
    let Some((key, parents)) = path.split_last() else {
        return Ok(None);
    };
    let mut table: &dyn TableLike = doc.as_table();
    for parent in parents {
        match table.get(parent).and_then(Item::as_table_like) {
            Some(child) => table = child,
            None => return Ok(None),
        }
    }
    Ok(table.get(key).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_set_preserves_comments() {
        let text = "[display]\n# note\npro_hint = true\n";
        let out = toml_set(text, &["display", "pro_hint"], toml_edit::value(false)).unwrap();
        assert_eq!(out, "[display]\n# note\npro_hint = false\n");
    }

    #[test]
    fn toml_set_creates_missing_tables() {
        let out = toml_set(
            "",
            &["mcp_servers", "playwright", "command"],
            toml_edit::value("npx"),
        )
        .unwrap();
        assert_eq!(
            toml_get(&out, &["mcp_servers", "playwright", "command"])
                .unwrap()
                .unwrap()
                .as_str(),
            Some("npx")
        );
        assert!(
            toml_get(&out, &["mcp_servers", "absent"])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn toml_set_refuses_to_overwrite_a_scalar_parent() {
        assert!(toml_set("a = 1\n", &["a", "b"], toml_edit::value(2)).is_err());
    }
}
