//! Sandbox configuration for Claude Code: settings, the self-test and the Linux prerequisites.

pub mod claude;
pub mod health;
pub mod selftest;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::edit::{jsonc_append, jsonc_get, jsonc_set};
use crate::exec::Cmd;
use crate::platform::{Os, PrereqPlan, current_os, prerequisite_plan};

/// The domains both hosts' sandboxes allow by default. The user's own domains are kept beside these.
pub const ALLOWED_DOMAINS: [&str; 7] = [
    "github.com",
    "api.github.com",
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
];

/// How `gh` reaches GitHub from the Claude Code sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhPath {
    /// `gh` runs inside the sandbox. On macOS that needs `enableWeakerNetworkIsolation`.
    Inside,
    /// `gh` runs outside the sandbox, allowed through `sandbox.excludedCommands`.
    ExcludedFallback,
}

/// The Linux prerequisites of the sandbox, as a plan only. Nothing is run here.
pub fn linux_prerequisites(_ctx: &Ctx) -> Result<PrereqPlan> {
    let os = current_os()?;
    let release = match os {
        Os::Linux => std::fs::read_to_string("/etc/os-release").ok(),
        Os::MacOs => None,
    };
    Ok(prerequisite_plan(os, release.as_deref()))
}

/// Runs the plan's one sudo invocation. It runs only when the calling flow passes `confirmed`, which it does after
/// the user's explicit yes. A dry run prints the invocation and runs nothing.
pub fn run_prerequisites(ctx: &Ctx, plan: &PrereqPlan, confirmed: bool) -> Result<()> {
    let Some((program, args)) = plan.sudo.split_first() else {
        return Ok(());
    };
    if !confirmed {
        bail!(
            "the sandbox prerequisites need sudo, and the user has not said yes; nothing was run"
        );
    }
    if ctx.dry_run {
        ctx.out.plan(&format!("would run {}", plan.sudo.join(" ")));
        return Ok(());
    }
    let cmd = args.iter().fold(Cmd::new(program), |cmd, arg| cmd.arg(arg));
    let out = ctx.runner.run(&cmd)?;
    if out.code != 0 {
        bail!(
            "the sandbox prerequisites failed with exit {}: {}",
            out.code,
            out.stderr.trim()
        );
    }
    Ok(())
}

/// Adds each wanted string to the JSON array at `path`, after the items already there, and skips any that are
/// present. A missing array is created. Other keys stay as they are.
pub fn union_strings(text: &str, path: &[&str], wanted: &[&str]) -> Result<String> {
    match jsonc_get(text, path)? {
        None | Some(Value::Null) => {
            // No list yet: write the whole list once, without repeats.
            let mut items: Vec<Value> = Vec::new();
            for word in wanted {
                if !items.iter().any(|item| item.as_str() == Some(word)) {
                    items.push(Value::String((*word).to_string()));
                }
            }
            jsonc_set(text, path, &Value::Array(items))
        }
        Some(Value::Array(items)) => {
            // An existing list gets each missing item appended, so its comments and layout stay as they are, and
            // nothing is written when every item is present.
            let mut present: Vec<Value> = items;
            let mut out = text.to_string();
            for word in wanted {
                if !present.iter().any(|item| item.as_str() == Some(word)) {
                    out = jsonc_append(&out, path, &json!(word))?;
                    present.push(Value::String((*word).to_string()));
                }
            }
            Ok(out)
        }
        Some(_) => bail!("`{}` is not a list", path.join(".")),
    }
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
    fn union_keeps_the_layout_and_escapes() {
        let text = "{\n    \"label\": \"caf\\u00e9\",\n    \"network\": {\"allowedDomains\": [\"a\"]}\n}\n";

        let out = union_strings(text, &["network", "allowedDomains"], &["b"]).unwrap();

        assert!(out.contains("\n    \"label\": \"caf\\u00e9\",\n"), "{out}");
    }

    #[test]
    fn union_appends_to_a_multi_line_array_and_keeps_its_comments_and_layout() {
        // An edit that adds one item must not re-render the array around it.
        let text = "{\n  // keep me\n  \"network\": {\n    \"allowedDomains\": [\n      \"a.example\", // first\n      \"b.example\"\n    ]\n  }\n}\n";

        let out = union_strings(text, &["network", "allowedDomains"], &["c.example"]).unwrap();

        assert!(out.contains("// keep me"), "{out}");
        assert!(out.contains("      \"a.example\", // first\n"), "{out}");
        assert!(out.contains("\"b.example\""), "{out}");
        assert!(out.contains("\"c.example\""), "{out}");
        let value: Value = serde_json::from_str(&strip_line_comments(&out)).unwrap();
        assert_eq!(
            value["network"]["allowedDomains"],
            json!(["a.example", "b.example", "c.example"])
        );
    }

    #[test]
    fn union_with_nothing_missing_returns_the_input_byte_identical() {
        let text = "{\n  // keep me\n  \"network\": {\n    \"allowedDomains\": [\n      \"a.example\", // first\n      \"b.example\"\n    ]\n  }\n}\n";

        let out = union_strings(
            text,
            &["network", "allowedDomains"],
            &["b.example", "a.example"],
        )
        .unwrap();

        assert_eq!(out, text);
    }

    /// Drops `//` comments, so a test can read the values of a JSONC text with serde_json.
    fn strip_line_comments(text: &str) -> String {
        text.lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn union_refuses_a_value_that_is_not_a_list() {
        let text = r#"{"network": {"allowedDomains": "github.com"}}"#;

        assert!(union_strings(text, &["network", "allowedDomains"], &["pypi.org"]).is_err());
    }
}

#[cfg(test)]
mod prerequisite_tests {
    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;

    #[test]
    fn linux_prerequisites_plan() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(root.path(), FakeRunner::new(), FakeNet::new());

        let plan = linux_prerequisites(&ctx).unwrap();

        match current_os().unwrap() {
            Os::MacOs => assert_eq!(plan, PrereqPlan::default()),
            Os::Linux => {
                let script = plan.sudo.join(" ");
                assert!(
                    script.contains("bubblewrap") && script.contains("socat"),
                    "{script}"
                );
                assert_eq!(plan.sudo.iter().filter(|word| *word == "sudo").count(), 1);
            }
        }
    }

    fn plan() -> PrereqPlan {
        PrereqPlan {
            lines: vec!["install bubblewrap and socat with apt-get".to_string()],
            sudo: vec![
                "sudo".into(),
                "sh".into(),
                "-c".into(),
                "apt-get install -y bubblewrap socat".into(),
            ],
        }
    }

    #[test]
    fn prerequisites_run_only_after_a_confirmed_yes() {
        let root = tempfile::tempdir().unwrap();
        let runner = FakeRunner::new().reply("sudo sh -c", Output::ok(""));
        let ctx = Ctx::for_test(root.path(), runner.clone(), FakeNet::new());

        assert!(run_prerequisites(&ctx, &plan(), false).is_err());
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());

        run_prerequisites(&ctx, &plan(), true).unwrap();
        assert_eq!(runner.calls().len(), 1, "{:?}", runner.calls());
    }

    #[test]
    fn a_failed_sudo_run_is_an_error_with_its_stderr() {
        let root = tempfile::tempdir().unwrap();
        let runner = FakeRunner::new().reply("sudo sh -c", Output::fail(1, "Sorry, try again."));
        let ctx = Ctx::for_test(root.path(), runner, FakeNet::new());

        let err = run_prerequisites(&ctx, &plan(), true).unwrap_err();

        assert!(format!("{err:#}").contains("Sorry, try again."), "{err:#}");
    }

    #[test]
    fn an_empty_plan_runs_nothing_even_when_confirmed() {
        let root = tempfile::tempdir().unwrap();
        let runner = FakeRunner::new();
        let ctx = Ctx::for_test(root.path(), runner.clone(), FakeNet::new());

        run_prerequisites(&ctx, &PrereqPlan::default(), true).unwrap();

        assert!(runner.calls().is_empty());
    }
}
