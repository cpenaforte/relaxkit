//! Format-preserving editors. Every function is pure: text in, text out.

pub mod block;
pub mod json;
pub mod jsonc;
pub mod toml;

pub use block::{block_get, block_remove, block_upsert};
pub use json::{json_get, json_remove, json_set};
pub use jsonc::{jsonc_get, jsonc_remove, jsonc_set};
pub use toml::{toml_get, toml_set};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_setters_are_idempotent() {
        let value = serde_json::json!({"type": "remote"});

        let once = jsonc_set("{}", &["a", "b"], &value).unwrap();
        assert_eq!(jsonc_set(&once, &["a", "b"], &value).unwrap(), once);

        let once = json_set("{}\n", &["a", "b"], &value).unwrap();
        assert_eq!(json_set(&once, &["a", "b"], &value).unwrap(), once);

        let once = toml_set("", &["a", "b"], toml_edit::value(false)).unwrap();
        assert_eq!(
            toml_set(&once, &["a", "b"], toml_edit::value(false)).unwrap(),
            once
        );

        let once = block_upsert("notes\n", "core", "body");
        assert_eq!(block_upsert(&once, "core", "body"), once);
    }
}
