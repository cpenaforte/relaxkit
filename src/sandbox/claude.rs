//! Claude Code's sandbox settings (spec 7): the three switches, the allowlist, the relaxkit commands that run outside
//! the sandbox, and `gh` when it is selected for that route. Each key is set or merged; other settings stay as they are.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{ALLOWED_DOMAINS, GhPath, union_strings};
use crate::brand;
use crate::ctx::Ctx;
use crate::edit::{jsonc_remove, jsonc_set};
use crate::host::{Host, HostKind};
use crate::platform::Os;
use crate::state::{RunId, safe_write};

/// The pack commands that change the machine, which run outside the sandbox (spec 7). `status` is not among them,
/// so it stays sandboxed.
pub const PACK_COMMANDS: [&str; 9] = [
    "setup",
    "update",
    "config",
    "preset",
    "privacy",
    "login",
    "undo",
    "uninstall",
    "doctor",
];

/// The sandbox exclusions: each pack command under the binary name and under the `rlx` alias (spec 4).
pub fn excluded_commands() -> Vec<String> {
    [brand::BIN, brand::ALIAS]
        .iter()
        .flat_map(|bin| {
            PACK_COMMANDS
                .iter()
                .map(move |command| format!("{bin} {command}"))
        })
        .collect()
}

/// Writes the sandbox settings into Claude Code's settings file. A second run changes nothing.
pub fn apply(ctx: &Ctx, run: &RunId, host: &dyn Host, gh: GhPath) -> Result<()> {
    if host.info().kind != HostKind::Claude {
        bail!(
            "the Claude sandbox settings only apply to Claude Code, not {}",
            host.info().kind
        );
    }
    let path = host.config_file(ctx);
    let old = read_text(&path)?;
    let text = settings_text(&old, crate::platform::current_os()?, gh)?;
    if text == old {
        return Ok(());
    }
    safe_write(ctx, run, &path, &text)
}

/// The settings text with the sandbox keys applied for `os` and the `gh` route.
pub fn settings_text(old: &str, os: Os, gh: GhPath) -> Result<String> {
    let mut text = jsonc_set(old, &["sandbox", "enabled"], &json!(true))?;
    text = jsonc_set(
        &text,
        &["sandbox", "autoAllowBashIfSandboxed"],
        &json!(true),
    )?;
    text = jsonc_set(&text, &["sandbox", "failIfUnavailable"], &json!(true))?;
    text = union_strings(
        &text,
        &["sandbox", "network", "allowedDomains"],
        &ALLOWED_DOMAINS,
    )?;
    let mut excluded = excluded_commands();
    if gh == GhPath::ExcludedFallback {
        excluded.push("gh *".to_string());
    }
    let excluded: Vec<&str> = excluded.iter().map(String::as_str).collect();
    text = union_strings(&text, &["sandbox", "excludedCommands"], &excluded)?;
    if weaker_isolation(os, gh) {
        jsonc_set(
            &text,
            &["sandbox", "enableWeakerNetworkIsolation"],
            &json!(true),
        )
    } else {
        jsonc_remove(&text, &["sandbox", "enableWeakerNetworkIsolation"])
    }
}

/// Whether `enableWeakerNetworkIsolation` is set: on macOS only, and only when `gh` runs inside the sandbox, which
/// needs it there (spike B1). Linux never sets it.
pub fn weaker_isolation(os: Os, gh: GhPath) -> bool {
    os == Os::MacOs && gh == GhPath::Inside
}

fn read_text(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn settings(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn claude_sandbox_keys() {
        let out = settings_text("{}", Os::MacOs, GhPath::Inside).unwrap();
        let value = settings(&out);

        let sandbox = &value["sandbox"];
        assert_eq!(sandbox["enabled"], json!(true));
        assert_eq!(sandbox["autoAllowBashIfSandboxed"], json!(true));
        assert_eq!(sandbox["failIfUnavailable"], json!(true));
        let domains = sandbox["network"]["allowedDomains"].as_array().unwrap();
        for domain in ALLOWED_DOMAINS {
            assert!(domains.contains(&json!(domain)), "{domain} missing");
        }
        assert!(domains.contains(&json!("github.com")));
        assert!(domains.contains(&json!("static.crates.io")));
    }

    #[test]
    fn claude_sandbox_keeps_the_users_domains_once() {
        let old =
            r#"{"sandbox": {"network": {"allowedDomains": ["internal.example", "github.com"]}}}"#;

        let out = settings_text(old, Os::Linux, GhPath::Inside).unwrap();

        let value = settings(&out);
        let domains = value["sandbox"]["network"]["allowedDomains"]
            .as_array()
            .unwrap();
        assert!(domains.contains(&json!("internal.example")), "{domains:?}");
        assert_eq!(
            domains
                .iter()
                .filter(|item| **item == json!("github.com"))
                .count(),
            1,
            "{domains:?}"
        );
    }

    #[test]
    fn weaker_isolation_follows_spike_b1_on_macos_and_is_absent_on_linux() {
        // B1: gh inside the sandbox on macOS needs the flag. Linux never sets it.
        assert!(weaker_isolation(Os::MacOs, GhPath::Inside));
        assert!(!weaker_isolation(Os::MacOs, GhPath::ExcludedFallback));
        assert!(!weaker_isolation(Os::Linux, GhPath::Inside));
        assert!(!weaker_isolation(Os::Linux, GhPath::ExcludedFallback));

        let on_mac = settings(&settings_text("{}", Os::MacOs, GhPath::Inside).unwrap());
        assert_eq!(
            on_mac["sandbox"]["enableWeakerNetworkIsolation"],
            json!(true)
        );
        let on_linux = settings(&settings_text("{}", Os::Linux, GhPath::Inside).unwrap());
        assert!(
            on_linux["sandbox"]
                .get("enableWeakerNetworkIsolation")
                .is_none()
        );
        let fallback = settings(&settings_text("{}", Os::MacOs, GhPath::ExcludedFallback).unwrap());
        assert!(
            fallback["sandbox"]
                .get("enableWeakerNetworkIsolation")
                .is_none()
        );
    }

    #[test]
    fn claude_excludes_mutating_pack_commands() {
        let out = settings_text("{}", Os::Linux, GhPath::Inside).unwrap();

        let value = settings(&out);
        let excluded = value["sandbox"]["excludedCommands"].as_array().unwrap();
        for command in [
            "relaxkit setup",
            "relaxkit update",
            "relaxkit config",
            "relaxkit preset",
            "relaxkit privacy",
            "relaxkit login",
            "relaxkit undo",
            "relaxkit uninstall",
            "relaxkit doctor",
        ] {
            assert!(excluded.contains(&json!(command)), "{command} missing");
        }
        assert!(!excluded.contains(&json!("relaxkit status")));
        // The rlx alias is how the owner types the same commands, so it is excluded the same way.
        for command in [
            "rlx setup",
            "rlx update",
            "rlx config",
            "rlx preset",
            "rlx privacy",
            "rlx login",
            "rlx undo",
            "rlx uninstall",
            "rlx doctor",
        ] {
            assert!(excluded.contains(&json!(command)), "{command} missing");
        }
        assert!(!excluded.contains(&json!("rlx status")));
    }

    #[test]
    fn claude_gh_fallback_adds_gh_only_when_selected() {
        let fallback = settings(&settings_text("{}", Os::Linux, GhPath::ExcludedFallback).unwrap());
        let inside = settings(&settings_text("{}", Os::Linux, GhPath::Inside).unwrap());

        assert!(
            fallback["sandbox"]["excludedCommands"]
                .as_array()
                .unwrap()
                .contains(&json!("gh *"))
        );
        assert!(
            !inside["sandbox"]["excludedCommands"]
                .as_array()
                .unwrap()
                .contains(&json!("gh *"))
        );
    }

    #[test]
    fn claude_settings_keep_the_layout_and_escapes() {
        // Spec 5: strict JSON edits keep the file's layout and spelling, not only its values.
        let old = "{\n    \"effortLevel\": \"caf\\u00e9\",\n    \"hooks\": {\"Stop\": []}\n}\n";

        let out = settings_text(old, Os::Linux, GhPath::Inside).unwrap();

        assert!(
            out.contains("\n    \"effortLevel\": \"caf\\u00e9\",\n"),
            "{out}"
        );
        assert!(out.contains("\"hooks\": {\"Stop\": []}"), "{out}");
    }

    #[test]
    fn claude_settings_keep_other_keys_and_a_second_run_changes_nothing() {
        let old = r#"{"effortLevel": "high", "hooks": {"Stop": []}}"#;

        let once = settings_text(old, Os::Linux, GhPath::Inside).unwrap();
        let twice = settings_text(&once, Os::Linux, GhPath::Inside).unwrap();

        let value = settings(&once);
        assert_eq!(value["effortLevel"], json!("high"));
        assert_eq!(value["hooks"], json!({"Stop": []}));
        assert_eq!(once, twice);
    }
}
