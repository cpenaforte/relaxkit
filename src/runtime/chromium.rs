//! The Chromium that Playwright MCP drives. It comes from the same `@playwright/mcp` version that the MCP
//! entry pins, so the browser build always matches the server.

use anyhow::{Context, Result, bail};
use semver::Version;

use crate::ctx::Ctx;
use crate::exec::Cmd;
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
        ctx.out.info(&format!("would install Chromium for {spec}"));
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
