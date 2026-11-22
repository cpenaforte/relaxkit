//! The OpenCode sandbox config (spec 7, owner decision O6, spike B2). It writes `~/.config/opencode/sandbox.json` in
//! enforce mode, with the allowlist and a deny on writing the project's `.opencode/sandbox.json`, so a project cannot
//! override the global policy. The plugin entry itself is written by the recipe's `plugin` step.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{ALLOWED_DOMAINS, PLUGIN_PACKAGE, union_strings};
use crate::ctx::Ctx;
use crate::edit::json_set;
use crate::host::{Host, HostKind};
use crate::state::{RunId, safe_write};

/// The project file a sandboxed command must not write (spike B2): the global policy stays the one that applies.
pub const PROJECT_OVERRIDE: &str = ".opencode/sandbox.json";

/// Writes the global sandbox config for the pinned plugin. The plugin must already be listed at `version` in the
/// OpenCode config, so the policy is never written for a plugin that does not load. A dry run changes nothing.
pub fn apply(ctx: &Ctx, run: &RunId, host: &dyn Host, version: &str) -> Result<()> {
    if host.info().kind != HostKind::Opencode {
        bail!(
            "opencode_sandbox_config only applies to OpenCode, not {}",
            host.info().kind
        );
    }
    let path = sandbox_config_path(ctx);
    if ctx.dry_run {
        ctx.out
            .info(&format!("would write {} in enforce mode", path.display()));
        return Ok(());
    }
    require_pinned(ctx, host, version)?;
    let old = read_text(&path)?;
    let text = sandbox_text(&old)?;
    if text == old {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    safe_write(ctx, run, &path, &text)
}

/// Fails unless the OpenCode config lists the pinned plugin at `version`. Without it the plugin does not load, so a
/// sandbox config for it would be a policy that nothing enforces.
fn require_pinned(ctx: &Ctx, host: &dyn Host, version: &str) -> Result<()> {
    let listed = host
        .plugin_list(ctx)?
        .iter()
        .any(|plugin| plugin.name == PLUGIN_PACKAGE && plugin.version.as_deref() == Some(version));
    if listed {
        return Ok(());
    }
    bail!(
        "{PLUGIN_PACKAGE}@{version} is not in the OpenCode plugins list, so its sandbox config was not written; nothing was changed"
    )
}

/// The sandbox config with the global switch cleared, enforce mode, the allowlist and the project-override deny
/// applied. Other keys are kept.
pub fn sandbox_text(old: &str) -> Result<String> {
    let enabled = json_set(old, &["disabled"], &json!(false))?;
    let mode = json_set(&enabled, &["mode"], &json!("enforce"))?;
    let domains = union_strings(&mode, &["network", "allowedDomains"], &ALLOWED_DOMAINS)?;
    union_strings(&domains, &["filesystem", "denyWrite"], &[PROJECT_OVERRIDE])
}

/// The file's text, or an empty string when it does not exist yet.
fn read_text(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// The global sandbox config path, `~/.config/opencode/sandbox.json` (spec 7).
pub fn sandbox_config_path(ctx: &Ctx) -> PathBuf {
    ctx.dirs
        .home
        .join(".config")
        .join("opencode")
        .join("sandbox.json")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::*;
    use crate::exec::FakeRunner;
    use crate::host::HostInfo;
    use crate::host::opencode::OpencodeHost;
    use crate::net::FakeNet;

    fn host(root: &Path) -> OpencodeHost {
        OpencodeHost::new(
            HostInfo {
                kind: HostKind::Opencode,
                binary: PathBuf::from("/usr/bin/opencode"),
                version: "2.2.6".into(),
            },
            root.join(".config/opencode"),
        )
    }

    fn ctx(root: &Path) -> Ctx {
        Ctx::for_test(root, FakeRunner::new(), FakeNet::new())
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// An OpenCode config whose `plugins` list holds the given specs.
    fn config_with_plugins(root: &Path, specs: &[&str]) {
        let list = specs
            .iter()
            .map(|spec| format!("\"{spec}\""))
            .collect::<Vec<_>>()
            .join(", ");
        write(
            &root.join(".config/opencode/opencode.jsonc"),
            &format!("{{\n  \"plugins\": [{list}]\n}}\n"),
        );
    }

    fn sandbox_file(root: &Path) -> PathBuf {
        root.join(".config/opencode/sandbox.json")
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn opencode_sandbox_is_pinned_and_enforced() {
        let root = tempfile::tempdir().unwrap();
        config_with_plugins(root.path(), &["opencode-sandbox@0.7.0"]);
        write(
            &sandbox_file(root.path()),
            r#"{"mode": "permissive", "network": {"allowedDomains": ["internal.example"]}}"#,
        );
        let ctx = ctx(root.path());

        apply(&ctx, &ctx.run, &host(root.path()), "0.7.0").unwrap();

        let value = read_json(&sandbox_file(root.path()));
        assert_eq!(value["mode"], json!("enforce"));
        let domains = value["network"]["allowedDomains"].as_array().unwrap();
        assert!(domains.contains(&json!("internal.example")), "{domains:?}");
        for domain in ALLOWED_DOMAINS {
            assert!(domains.contains(&json!(domain)), "{domain} missing");
            assert_eq!(
                domains
                    .iter()
                    .filter(|item| **item == json!(domain))
                    .count(),
                1,
                "{domain} duplicated"
            );
        }
    }

    #[test]
    fn opencode_global_config_blocks_project_overrides() {
        let root = tempfile::tempdir().unwrap();
        config_with_plugins(root.path(), &["opencode-sandbox@0.7.0"]);
        let ctx = ctx(root.path());

        apply(&ctx, &ctx.run, &host(root.path()), "0.7.0").unwrap();

        let value = read_json(&sandbox_file(root.path()));
        let deny = value["filesystem"]["denyWrite"].as_array().unwrap();
        assert!(deny.contains(&json!(".opencode/sandbox.json")), "{deny:?}");
    }

    #[test]
    fn opencode_sandbox_refuses_when_the_pinned_plugin_is_not_listed() {
        let root = tempfile::tempdir().unwrap();
        config_with_plugins(root.path(), &["other@1.0.0"]);
        let ctx = ctx(root.path());

        let err = apply(&ctx, &ctx.run, &host(root.path()), "0.7.0").unwrap_err();

        assert!(
            format!("{err:#}").contains("opencode-sandbox@0.7.0"),
            "{err:#}"
        );
        assert!(!sandbox_file(root.path()).exists());
    }

    #[test]
    fn opencode_sandbox_dry_run_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let mut ctx = ctx(root.path());
        ctx.dry_run = true;

        apply(&ctx, &ctx.run, &host(root.path()), "0.7.0").unwrap();

        assert!(!sandbox_file(root.path()).exists());
        assert!(!root.path().join(".config/opencode").exists());
    }

    #[test]
    fn opencode_sandbox_clears_a_pre_existing_disabled_switch() {
        // Spec 7: `disabled` is the global off switch of the plugin, so the enforce write must clear it.
        let root = tempfile::tempdir().unwrap();
        config_with_plugins(root.path(), &["opencode-sandbox@0.7.0"]);
        write(
            &sandbox_file(root.path()),
            r#"{"disabled": true, "mode": "permissive"}"#,
        );
        let ctx = ctx(root.path());

        apply(&ctx, &ctx.run, &host(root.path()), "0.7.0").unwrap();

        let value = read_json(&sandbox_file(root.path()));
        assert_eq!(value["disabled"], json!(false));
        assert_eq!(value["mode"], json!("enforce"));
    }

    #[test]
    fn opencode_sandbox_is_written_once() {
        let root = tempfile::tempdir().unwrap();
        config_with_plugins(root.path(), &["opencode-sandbox@0.7.0"]);
        let ctx = ctx(root.path());
        apply(&ctx, &ctx.run, &host(root.path()), "0.7.0").unwrap();
        let once = fs::read_to_string(sandbox_file(root.path())).unwrap();

        apply(&ctx, &ctx.run, &host(root.path()), "0.7.0").unwrap();

        assert_eq!(fs::read_to_string(sandbox_file(root.path())).unwrap(), once);
    }
}
