//! Sandbox configuration for both hosts (spec section 7): the OpenCode sandbox config, and later the Claude Code
//! settings, the self-test, the Linux prerequisites and the plugin health check.

pub mod claude;
pub mod opencode;
pub mod selftest;

use selftest::RUNTIME_PACKAGE;

use anyhow::{Context, Result, anyhow, bail};
use semver::{Version, VersionReq};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::edit::{jsonc_get, jsonc_set};
use crate::exec::Cmd;
use crate::net::Net;
use crate::platform::{Os, PrereqPlan, current_os, prerequisite_plan};

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

/// How `gh` reaches GitHub from the Claude Code sandbox (spec 7, owner decision O5, spike B1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhPath {
    /// `gh` runs inside the sandbox. On macOS that needs `enableWeakerNetworkIsolation` (spike B1).
    Inside,
    /// `gh` runs outside the sandbox, allowed through `sandbox.excludedCommands` (owner decision O5).
    ExcludedFallback,
}

/// The Linux prerequisites of the sandbox, as a plan only (spec 7, ruling K36). Nothing is run here.
pub fn linux_prerequisites(_ctx: &Ctx) -> Result<PrereqPlan> {
    let os = current_os()?;
    let release = match os {
        Os::Linux => std::fs::read_to_string("/etc/os-release").ok(),
        Os::MacOs => None,
    };
    Ok(prerequisite_plan(os, release.as_deref()))
}

/// Runs the plan's one sudo invocation. It runs only when the calling flow passes `confirmed`, which it does after
/// the user's explicit yes (spec 4 and 7). A dry run prints the invocation and runs nothing.
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
        ctx.out.info(&format!("would run {}", plan.sudo.join(" ")));
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

/// What the plugin health check reports about the pinned OpenCode sandbox plugin (spec 7, item 25).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHealth {
    /// Days since the plugin's latest release.
    pub release_age_days: u32,
    /// The runtime version the pinned plugin bundles: the highest published version its dependency range allows.
    pub bundled_runtime: String,
    /// The highest published runtime version.
    pub newest_runtime: String,
    /// How many published runtime versions are newer than the bundled one.
    pub runtime_lag: u32,
}

/// The plugin health of `plugin_version`, read from the npm registry through `ctx.net`. It fails soft: the caller
/// shows a failure as an `unknown` line (see `health_line`) and never stops setup or update.
pub fn plugin_health(ctx: &Ctx, plugin_version: &str) -> Result<PluginHealth> {
    plugin_health_at(ctx, plugin_version, now_seconds())
}

/// `plugin_health` with the clock given, so the release age is computed against a known instant in tests.
fn plugin_health_at(ctx: &Ctx, plugin_version: &str, now: i64) -> Result<PluginHealth> {
    let net: &dyn Net = ctx.net.as_ref();
    let plugin = npm_document(net, PLUGIN_PACKAGE)?;
    let latest = plugin["dist-tags"]["latest"]
        .as_str()
        .ok_or_else(|| anyhow!("{PLUGIN_PACKAGE} has no latest tag"))?;
    let released = plugin["time"][latest]
        .as_str()
        .ok_or_else(|| anyhow!("{PLUGIN_PACKAGE} has no release time for {latest}"))?;
    let days = (now - parse_utc(released)?).max(0) / 86_400;
    let release_age_days = u32::try_from(days).unwrap_or(u32::MAX);

    let range = plugin["versions"][plugin_version]["dependencies"][RUNTIME_PACKAGE]
        .as_str()
        .ok_or_else(|| {
            anyhow!("{PLUGIN_PACKAGE}@{plugin_version} has no {RUNTIME_PACKAGE} dependency")
        })?;
    let range = VersionReq::parse(range).with_context(|| {
        format!("{PLUGIN_PACKAGE}@{plugin_version} has a bad {RUNTIME_PACKAGE} range")
    })?;

    let runtime = npm_document(net, RUNTIME_PACKAGE)?;
    let published: Vec<Version> = runtime["versions"]
        .as_object()
        .ok_or_else(|| anyhow!("{RUNTIME_PACKAGE} lists no versions"))?
        .keys()
        .filter_map(|text| Version::parse(text).ok())
        .filter(|version| version.pre.is_empty())
        .collect();
    let bundled = published
        .iter()
        .filter(|version| range.matches(version))
        .max()
        .ok_or_else(|| anyhow!("no published {RUNTIME_PACKAGE} matches {range}"))?;
    let newest = published
        .iter()
        .max()
        .ok_or_else(|| anyhow!("{RUNTIME_PACKAGE} has no published versions"))?;
    let behind = published
        .iter()
        .filter(|version| *version > bundled)
        .count();

    Ok(PluginHealth {
        release_age_days,
        bundled_runtime: bundled.to_string(),
        newest_runtime: newest.to_string(),
        runtime_lag: u32::try_from(behind).unwrap_or(u32::MAX),
    })
}

/// A package's registry document, read through the net abstraction. Scoped names are encoded as `@scope%2fname`.
fn npm_document(net: &dyn Net, package: &str) -> Result<Value> {
    let encoded = package.replace('/', "%2f");
    let body = net
        .get_text(&format!("https://registry.npmjs.org/{encoded}"))
        .with_context(|| format!("cannot read the npm registry entry for {package}"))?;
    serde_json::from_str(&body)
        .with_context(|| format!("the npm registry entry for {package} is not JSON"))
}

/// One line for `status` or `doctor`: the health of the plugin, or `unknown` with the reason when it cannot be read.
pub fn health_line(result: &Result<PluginHealth>) -> String {
    match result {
        Ok(health) => format!(
            "{PLUGIN_PACKAGE}: latest release {} days ago; bundles {RUNTIME_PACKAGE} {}, newest {}, {} releases behind",
            health.release_age_days,
            health.bundled_runtime,
            health.newest_runtime,
            health.runtime_lag
        ),
        Err(err) => format!("{PLUGIN_PACKAGE}: unknown ({err:#})"),
    }
}

/// Seconds since the epoch of an RFC 3339 UTC time as the npm registry writes it (`2026-11-18T12:00:00.000Z`).
fn parse_utc(text: &str) -> Result<i64> {
    let number = |range: std::ops::Range<usize>| -> Result<i64> {
        text.get(range)
            .and_then(|digits| digits.parse().ok())
            .ok_or_else(|| anyhow!("not an RFC 3339 UTC time: {text}"))
    };
    let days = days_from_civil(number(0..4)?, number(5..7)?, number(8..10)?);
    Ok(days * 86_400 + number(11..13)? * 3_600 + number(14..16)? * 60 + number(17..19)?)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (the standard civil-calendar algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Adds each wanted string to the JSON array at `path`, after the items already there, and skips any that are
/// present. A missing array is created. Other keys stay as they are.
pub fn union_strings(text: &str, path: &[&str], wanted: &[&str]) -> Result<String> {
    let mut items = match jsonc_get(text, path)? {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => bail!("`{}` is not a list", path.join(".")),
    };
    for word in wanted {
        if !items.iter().any(|item| item.as_str() == Some(word)) {
            items.push(Value::String((*word).to_string()));
        }
    }
    jsonc_set(text, path, &Value::Array(items))
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
    fn union_refuses_a_value_that_is_not_a_list() {
        let text = r#"{"network": {"allowedDomains": "github.com"}}"#;

        assert!(union_strings(text, &["network", "allowedDomains"], &["pypi.org"]).is_err());
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use crate::exec::FakeRunner;
    use crate::net::FakeNet;
    use crate::sandbox::selftest::RUNTIME_PACKAGE;

    /// 2026-11-21T12:00:00Z.
    const NOW: i64 = 1_795_262_400;

    fn ctx(net: FakeNet) -> Ctx {
        Ctx::for_test(
            std::path::Path::new("/nonexistent-home"),
            FakeRunner::new(),
            net,
        )
    }

    fn registry(plugin_latest_at: &str) -> FakeNet {
        let plugin = format!(
            r#"{{"dist-tags": {{"latest": "0.7.0"}}, "time": {{"0.7.0": "{plugin_latest_at}"}},
                "versions": {{"0.7.0": {{"dependencies": {{"{RUNTIME_PACKAGE}": "^0.0.76"}}}}}}}}"#
        );
        let runtime_versions: String = (76..=83)
            .map(|patch| format!("\"0.0.{patch}\": {{}}"))
            .collect::<Vec<_>>()
            .join(", ");
        let runtime = format!(r#"{{"versions": {{{runtime_versions}}}}}"#);
        FakeNet::new()
            .with("https://registry.npmjs.org/opencode-sandbox", &plugin)
            .with(
                "https://registry.npmjs.org/@anthropic-ai%2fsandbox-runtime",
                &runtime,
            )
    }

    #[test]
    fn plugin_health_reports_age_and_runtime_lag() {
        let ctx = ctx(registry("2026-11-18T12:00:00.000Z"));

        let health = plugin_health_at(&ctx, "0.7.0", NOW).unwrap();

        assert_eq!(
            health,
            PluginHealth {
                release_age_days: 3,
                bundled_runtime: "0.0.76".to_string(),
                newest_runtime: "0.0.83".to_string(),
                runtime_lag: 7,
            }
        );
        assert!(health_line(&Ok(health)).contains("0.0.83"));
    }

    #[test]
    fn plugin_health_fails_soft_when_the_registry_is_unreachable() {
        let ctx = ctx(FakeNet::new());

        let result = plugin_health_at(&ctx, "0.7.0", NOW);

        assert!(result.is_err());
        let line = health_line(&result);
        assert!(line.contains("unknown"), "{line}");
        assert!(line.contains("opencode-sandbox"), "{line}");
    }

    #[test]
    fn utc_times_from_the_registry_are_read_as_seconds() {
        assert_eq!(parse_utc("1970-01-01T00:00:00.000Z").unwrap(), 0);
        assert_eq!(parse_utc("2000-03-01T00:00:00.000Z").unwrap(), 951_868_800);
        assert!(parse_utc("not a time").is_err());
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
