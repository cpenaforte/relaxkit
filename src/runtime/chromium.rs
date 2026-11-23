//! The Chromium that Playwright MCP drives. It comes from the same `@playwright/mcp` version that the MCP
//! entry pins, so the browser build always matches the server.

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;

use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::platform;
use crate::runtime::{nvm, utf8};
use crate::state::RunId;

/// The npm package whose exact version pins the Chromium build.
pub const PLAYWRIGHT_MCP: &str = "@playwright/mcp";

/// Installs the Chromium build that `@playwright/mcp` at exactly `playwright_mcp_version` uses. The
/// version must be an exact semver, so `latest` and ranges are refused before anything runs. npx comes from
/// the Node link, and the link's bin directory is first on PATH so npx finds the same Node.
pub fn ensure_chromium(ctx: &Ctx, _run: &RunId, playwright_mcp_version: &str) -> Result<()> {
    let version = Version::parse(playwright_mcp_version).with_context(|| {
        format!("`{playwright_mcp_version}` is not an exact version of {PLAYWRIGHT_MCP}")
    })?;
    let spec = format!("{PLAYWRIGHT_MCP}@{version}");
    if ctx.dry_run {
        ctx.out.plan(&format!("would install Chromium for {spec}"));
        return Ok(());
    }
    let npx = nvm::npx_bin(ctx);
    let node_dir = ctx.dirs.data.join("node").join("bin");
    let path = match std::env::var_os("PATH") {
        Some(current) => format!("{}:{}", utf8(&node_dir)?, current.to_string_lossy()),
        None => utf8(&node_dir)?.to_string(),
    };
    let cmd = Cmd::new(utf8(&npx)?)
        .args(&["-y", &spec, "install-browser", "chromium"])
        .env("PATH", &path);
    let out = ctx.runner.run(&cmd)?;
    if out.code != 0 {
        bail!(
            "installing Chromium for {spec} failed (exit {}): {}",
            out.code,
            out.stderr.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;
    use crate::runtime::testing::{Recorder, ctx_with};
    use crate::state::RunId;

    #[test]
    fn chromium_install_uses_the_exact_playwright_version() {
        let root = tempfile::tempdir().unwrap();
        let npx = root.path().join(".local/share/relaxkit/node/bin/npx");
        let runner = Recorder::new(FakeRunner::new().reply(
            &format!(
                "{} -y @playwright/mcp@0.0.86 install-browser chromium",
                npx.display()
            ),
            Output::ok(""),
        ));
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());

        ensure_chromium(&ctx, &RunId::new(), "0.0.86").unwrap();

        let cmds = runner.cmds();
        assert_eq!(cmds.len(), 1, "{:?}", runner.lines());
        let argv = cmds[0].argv();
        assert!(
            argv.contains(&"@playwright/mcp@0.0.86".to_string()),
            "{argv:?}"
        );
        assert!(argv.contains(&"install-browser".to_string()), "{argv:?}");
        assert!(argv.contains(&"chromium".to_string()), "{argv:?}");
        assert!(
            runner.lines().iter().all(|line| !line.contains("latest")),
            "{:?}",
            runner.lines()
        );
    }

    #[test]
    fn chromium_refuses_a_version_that_is_not_exact() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx_with(root.path(), &runner, FakeNet::new());

        assert!(ensure_chromium(&ctx, &RunId::new(), "latest").is_err());
        assert!(ensure_chromium(&ctx, &RunId::new(), "^0.0.86").is_err());
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }
}

/// The Chromium that Playwright installed: `PLAYWRIGHT_BROWSERS_PATH` when set, else Playwright's cache.
pub fn installed_chromium(ctx: &Ctx) -> Result<PathBuf> {
    let root = match std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").filter(|value| !value.is_empty())
    {
        Some(path) => PathBuf::from(path),
        None => platform::playwright_cache_dir(&ctx.dirs.home)?,
    };
    chromium_binary_in(&root)
}

/// The Chromium executable in the newest `chromium-<revision>` directory under `root`, for this OS and CPU.
pub fn chromium_binary_in(root: &Path) -> Result<PathBuf> {
    find_chromium(root, &platform::chromium_candidates()?)
}

/// The Chromium executable in the newest `chromium-<revision>` directory under `root`, at the first of `candidates`
/// that exists inside that directory.
fn find_chromium(root: &Path, candidates: &[&str]) -> Result<PathBuf> {
    let mut newest: Option<(u64, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(revision) = name
                .to_str()
                .and_then(|name| name.strip_prefix("chromium-"))
                .and_then(|rest| rest.parse::<u64>().ok())
            else {
                continue;
            };
            if newest.as_ref().is_none_or(|(best, _)| revision > *best) {
                newest = Some((revision, entry.path()));
            }
        }
    }
    let (_, dir) = newest.ok_or_else(|| {
        anyhow!(
            "no Chromium under {}; the playwright recipe installs it first",
            root.display()
        )
    })?;
    for relative in candidates {
        let candidate = dir.join(relative);
        if platform::is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    bail!(
        "the Chromium under {} has no known executable path for this OS",
        dir.display()
    )
}

#[cfg(test)]
mod location_tests {
    use super::*;
    use crate::platform::{Arch, Os, chromium_candidates, chromium_candidates_for};

    #[test]
    fn an_arm64_linux_install_is_found_with_the_arm64_layout() {
        // The layout Playwright 0.0.86 writes on Linux arm64, which relaxkit did not look for before.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("chromium-1255");
        let binary = dir.join("chrome-linux-arm64").join("chrome");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "chrome").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let found = find_chromium(
            root.path(),
            &chromium_candidates_for(Os::Linux, Arch::Aarch64),
        )
        .unwrap();

        assert_eq!(found, binary);
    }

    #[test]
    fn the_newest_revision_wins_and_its_binary_is_found() {
        let root = tempfile::tempdir().unwrap();
        let older = root.path().join("chromium-1100");
        let newer = root.path().join("chromium-1255");
        let binary = chromium_candidates().unwrap()[0];
        std::fs::create_dir_all(older.join(binary).parent().unwrap()).unwrap();
        std::fs::write(older.join(binary), "old").unwrap();
        std::fs::create_dir_all(newer.join(binary).parent().unwrap()).unwrap();
        std::fs::write(newer.join(binary), "new").unwrap();
        for path in [older.join(binary), newer.join(binary)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir_all(root.path().join("chromium_headless_shell-1300")).unwrap();

        assert_eq!(chromium_binary_in(root.path()).unwrap(), newer.join(binary));
    }

    #[test]
    fn a_missing_chromium_is_an_error_that_says_what_to_run() {
        let root = tempfile::tempdir().unwrap();

        let err = chromium_binary_in(root.path()).unwrap_err();

        assert!(format!("{err:#}").contains("playwright"), "{err:#}");
    }
}
