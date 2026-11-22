//! Sandbox configuration for both hosts (spec section 7): the OpenCode sandbox config, and later the Claude Code
//! settings, the self-test, the Linux prerequisites and the plugin health check.

pub mod opencode;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::edit::{json_get, json_set};

/// The package name of the OpenCode sandbox plugin (spec 7, pinned in the recipe).
pub const PLUGIN_PACKAGE: &str = "opencode-sandbox";

/// The domains both hosts' sandboxes allow by default (spec 7). The user's own domains are kept beside these.
pub const ALLOWED_DOMAINS: [&str; 7] = [
    "github.com",
    "api.github.com",
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
];

/// Adds each wanted string to the JSON array at `path`, after the items already there, and skips any that are
/// present. A missing array is created. Other keys stay as they are.
pub fn union_strings(text: &str, path: &[&str], wanted: &[&str]) -> Result<String> {
    let mut items = match json_get(text, path)? {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => bail!("`{}` is not a list", path.join(".")),
    };
    for word in wanted {
        if !items.iter().any(|item| item.as_str() == Some(word)) {
            items.push(Value::String((*word).to_string()));
        }
    }
    json_set(text, path, &Value::Array(items))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn union_keeps_the_items_already_there_and_adds_each_missing_one_once() {
        let text = r#"{"network": {"allowedDomains": ["internal.example", "github.com"]}}"#;

        let out = union_strings(
            text,
            &["network", "allowedDomains"],
            &["github.com", "pypi.org", "pypi.org"],
        )
        .unwrap();

        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            value["network"]["allowedDomains"],
            json!(["internal.example", "github.com", "pypi.org"])
        );
    }

    #[test]
    fn union_creates_a_missing_list_and_keeps_other_keys() {
        let text = r#"{"mode": "enforce"}"#;

        let out = union_strings(
            text,
            &["filesystem", "denyWrite"],
            &[".opencode/sandbox.json"],
        )
        .unwrap();

        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["mode"], json!("enforce"));
        assert_eq!(
            value["filesystem"]["denyWrite"],
            json!([".opencode/sandbox.json"])
        );
    }

    #[test]
    fn union_refuses_a_value_that_is_not_a_list() {
        let text = r#"{"network": {"allowedDomains": "github.com"}}"#;

        assert!(union_strings(text, &["network", "allowedDomains"], &["pypi.org"]).is_err());
    }
}
