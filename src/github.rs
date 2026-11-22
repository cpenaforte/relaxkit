//! GitHub login (spec 7, ruling K43) and the `gh` route from Claude Code (spec 7, ruling K42).
//!
//! relaxkit never reads or writes a GitHub token. `gh` keeps the owner's keyring login, and nothing this module
//! writes names a token, an environment variable or a session hook.

use anyhow::{Result, bail};

use crate::ctx::Ctx;
use crate::exec::{Cmd, CommandFailed};
use crate::host::Host;
use crate::runtime::gh::ensure_gh;
use crate::runtime::utf8;
use crate::sandbox::GhPath;
use crate::state::RunId;

/// The exit code of `relaxkit login github` when the owner must finish the login by hand (ruling K43).
pub const MANUAL_STEP_EXIT_CODE: i32 = 3;

/// What `login` did, or would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// `gh auth status` already passes, so no login command ran.
    AlreadyAuthenticated,
    /// The web login and `gh auth setup-git` both ran and passed.
    LoggedIn,
    /// A dry run: nothing ran.
    Planned,
    /// No terminal, and gh is not logged in. The owner runs `command` (a single shell line) to finish.
    ManualStep { command: String },
}

impl LoginOutcome {
    /// The process exit code for this outcome: 3 for a manual step, 0 otherwise.
    pub fn exit_code(&self) -> i32 {
        match self {
            LoginOutcome::ManualStep { .. } => MANUAL_STEP_EXIT_CODE,
            _ => 0,
        }
    }
}

/// `relaxkit login github`. The terminal check reads the real stdin; see `login_with` for the logic.
pub fn login(ctx: &Ctx, run: &RunId) -> Result<LoginOutcome> {
    login_with(
        ctx,
        run,
        std::io::IsTerminal::is_terminal(&std::io::stdin()),
    )
}

/// The login with the terminal question answered by the caller, so tests do not depend on how they were started.
pub fn login_with(ctx: &Ctx, run: &RunId, interactive: bool) -> Result<LoginOutcome> {
    // Installs gh first when it is missing (Task 11); a dry run only reports the path it would use.
    let gh = ensure_gh(ctx, run)?;
    let program = utf8(&gh.path)?;
    let login_cmd = Cmd::new(program).args(&["auth", "login", "--web", "--clipboard"]);
    let setup_cmd = Cmd::new(program).args(&["auth", "setup-git"]);

    if ctx.dry_run {
        ctx.out.info(&format!(
            "would run {login_cmd} and then {setup_cmd}, unless gh auth status passes"
        ));
        return Ok(LoginOutcome::Planned);
    }

    let status = ctx
        .runner
        .run(&Cmd::new(program).args(&["auth", "status"]))?;
    if status.code == 0 {
        return Ok(LoginOutcome::AlreadyAuthenticated);
    }

    if !interactive {
        // The web flow waits for the owner (a prompt and a one-time code), so it cannot run here.
        let command = format!("{login_cmd} && {setup_cmd}");
        ctx.out.warn(&format!(
            "GitHub login needs a terminal. Run this in one to finish: {command}"
        ));
        return Ok(LoginOutcome::ManualStep { command });
    }

    ctx.out.step(
        "logging in to GitHub: a browser opens and a one-time code is copied for you to paste",
    );
    let login = ctx.runner.run(&login_cmd.inherit_terminal())?;
    if login.code != 0 {
        bail!(
            "gh auth login failed with exit {}; the messages above say why",
            login.code
        );
    }
    let setup = ctx.runner.run(&setup_cmd)?;
    if setup.code != 0 {
        return Err(CommandFailed {
            command: setup_cmd.to_string(),
            code: setup.code,
            output: setup.stderr.trim().to_string(),
        }
        .into());
    }
    Ok(LoginOutcome::LoggedIn)
}

/// The `gh` route from Claude Code (ruling K42): always outside the sandbox, through `sandbox.excludedCommands`.
/// No probe runs, so this never returns `GhPath::Inside`.
pub fn choose_gh_path(_ctx: &Ctx, _host: &dyn Host) -> GhPath {
    GhPath::ExcludedFallback
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::host::claude::{ClaudeHost, claude_paths};
    use crate::host::{HostInfo, HostKind};
    use crate::net::FakeNet;
    use crate::platform::KitDirs;
    use crate::runtime::testing::{Recorder, ctx_with};

    const GH: &str = "/usr/local/bin/gh";

    fn status_line() -> String {
        format!("{GH} auth status")
    }

    fn login_line() -> String {
        format!("{GH} auth login --web --clipboard")
    }

    fn setup_line() -> String {
        format!("{GH} auth setup-git")
    }

    /// A scripted runner where `gh` is on the PATH and `gh auth status` returns `status`.
    fn gh_on_path(status: Output) -> FakeRunner {
        FakeRunner::new()
            .with_program("gh", Path::new(GH))
            .reply(&status_line(), status)
    }

    /// The context for a test rooted at `root`, recording every command.
    fn ctx(root: &Path, runner: &Recorder) -> Ctx {
        ctx_with(root, runner, FakeNet::new())
    }

    #[test]
    fn login_skips_when_already_authenticated() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            gh_on_path(Output::ok("Logged in"))
                .reply(&login_line(), Output::ok(""))
                .reply(&setup_line(), Output::ok("")),
        );
        let ctx = ctx(root.path(), &runner);

        let outcome = login_with(&ctx, &RunId::new(), true).unwrap();

        assert_eq!(outcome, LoginOutcome::AlreadyAuthenticated);
        assert_eq!(runner.lines(), vec![status_line()]);
    }

    #[test]
    fn login_runs_the_web_flow_then_git_setup() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            gh_on_path(Output::fail(1, "You are not logged in"))
                .reply(&login_line(), Output::ok(""))
                .reply(&setup_line(), Output::ok("")),
        );
        let ctx = ctx(root.path(), &runner);

        let outcome = login_with(&ctx, &RunId::new(), true).unwrap();

        assert_eq!(outcome, LoginOutcome::LoggedIn);
        assert_eq!(
            runner.lines(),
            vec![status_line(), login_line(), setup_line()]
        );
        let cmds = runner.cmds();
        assert!(
            cmds[1].inherits_terminal(),
            "the web login needs the owner's terminal for its prompt and one-time code"
        );
        assert!(!cmds[2].inherits_terminal());
    }

    #[test]
    fn login_installs_gh_first_when_missing() {
        // No gh on the PATH and no private copy: the install runs first, and its download fails (FakeNet has no
        // release). Nothing reaches `gh auth`.
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(FakeRunner::new());
        let ctx = ctx(root.path(), &runner);

        let err = login_with(&ctx, &RunId::new(), true).unwrap_err();

        assert!(format!("{err:#}").contains("cli/cli"), "{err:#}");
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn login_uses_the_private_copy_when_gh_is_not_on_the_path() {
        let root = tempfile::tempdir().unwrap();
        let bin = KitDirs::rooted(root.path()).bin.join("gh");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let runner = Recorder::new(
            FakeRunner::new().reply(&format!("{} auth status", bin.display()), Output::ok("")),
        );
        let ctx = ctx(root.path(), &runner);

        let outcome = login_with(&ctx, &RunId::new(), true).unwrap();

        assert_eq!(outcome, LoginOutcome::AlreadyAuthenticated);
        assert_eq!(
            runner.lines(),
            vec![format!("{} auth status", bin.display())]
        );
    }

    #[test]
    fn without_a_terminal_it_prints_the_command_and_exits_with_code_3() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            gh_on_path(Output::fail(1, "You are not logged in"))
                .reply(&login_line(), Output::ok(""))
                .reply(&setup_line(), Output::ok("")),
        );
        let ctx = ctx(root.path(), &runner);

        let outcome = login_with(&ctx, &RunId::new(), false).unwrap();

        let command = format!("{} && {}", login_line(), setup_line());
        assert_eq!(
            outcome,
            LoginOutcome::ManualStep {
                command: command.clone()
            }
        );
        assert_eq!(outcome.exit_code(), 3);
        assert_eq!(runner.lines(), vec![status_line()], "no login ran");
        assert!(
            ctx.out.lines().iter().any(|line| line.contains(&command)),
            "{:?}",
            ctx.out.lines()
        );
    }

    #[test]
    fn without_a_terminal_an_existing_login_still_passes_quietly() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(gh_on_path(Output::ok("Logged in")));
        let ctx = ctx(root.path(), &runner);

        let outcome = login_with(&ctx, &RunId::new(), false).unwrap();

        assert_eq!(outcome, LoginOutcome::AlreadyAuthenticated);
        assert_eq!(outcome.exit_code(), 0);
    }

    #[test]
    fn a_dry_run_runs_no_login_command() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            gh_on_path(Output::fail(1, "not logged in")).reply(&login_line(), Output::ok("")),
        );
        let mut ctx = ctx(root.path(), &runner);
        ctx.dry_run = true;

        let outcome = login_with(&ctx, &RunId::new(), true).unwrap();

        assert_eq!(outcome, LoginOutcome::Planned);
        assert!(runner.lines().is_empty(), "{:?}", runner.lines());
    }

    #[test]
    fn a_failed_web_login_is_an_error_and_git_setup_does_not_run() {
        let root = tempfile::tempdir().unwrap();
        let runner = Recorder::new(
            gh_on_path(Output::fail(1, "not logged in"))
                .reply(&login_line(), Output::fail(1, "login cancelled"))
                .reply(&setup_line(), Output::ok("")),
        );
        let ctx = ctx(root.path(), &runner);

        assert!(login_with(&ctx, &RunId::new(), true).is_err());
        assert_eq!(runner.lines(), vec![status_line(), login_line()]);
    }

    /// Every file under `dir`, recursively.
    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    found.extend(files_under(&path));
                } else {
                    found.push(path);
                }
            }
        }
        found
    }

    #[test]
    fn login_leaves_no_token_hook_or_env_entry_on_disk() {
        // Replaces the token-delivery test (ruling K42): after a login, no file under the home holds a token,
        // a token hook, or an envVars entry, the settings file is unchanged, and no command carried an environment.
        let root = tempfile::tempdir().unwrap();
        let settings = root.path().join(".claude").join("settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let before = r#"{"theme": "dark"}"#;
        fs::write(&settings, before).unwrap();
        let runner = Recorder::new(
            gh_on_path(Output::fail(1, "not logged in"))
                .reply(&login_line(), Output::ok(""))
                .reply(&setup_line(), Output::ok("")),
        );
        let ctx = ctx(root.path(), &runner);

        let outcome = login_with(&ctx, &RunId::new(), true).unwrap();

        assert_eq!(outcome, LoginOutcome::LoggedIn);
        assert!(
            runner
                .lines()
                .iter()
                .all(|line| !line.contains("auth token")),
            "{:?}",
            runner.lines()
        );
        assert!(runner.cmds().iter().all(|cmd| cmd.environment().is_empty()));
        assert_eq!(fs::read_to_string(&settings).unwrap(), before);
        let forbidden = [
            "gho_",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "envVars",
            "CLAUDE_ENV_FILE",
            "SessionStart",
            "gh auth token",
            "enableWeakerNetworkIsolation",
        ];
        for file in files_under(root.path()) {
            let text = fs::read_to_string(&file).unwrap_or_default();
            for word in forbidden {
                assert!(!text.contains(word), "{} contains {word}", file.display());
            }
        }
    }

    #[test]
    fn the_claude_route_is_the_excluded_fallback_whether_or_not_gh_works() {
        let root = tempfile::tempdir().unwrap();
        let (dir, state) = claude_paths(root.path(), None);
        let host = ClaudeHost::new(
            HostInfo {
                kind: HostKind::Claude,
                binary: PathBuf::from("/usr/local/bin/claude"),
                version: "2.1.330".to_string(),
            },
            dir,
            state,
        );
        let working = Recorder::new(gh_on_path(Output::ok("Logged in")));
        let broken = Recorder::new(gh_on_path(Output::fail(1, "x509: certificate")));

        assert_eq!(
            choose_gh_path(&ctx(root.path(), &working), &host),
            GhPath::ExcludedFallback
        );
        assert_eq!(
            choose_gh_path(&ctx(root.path(), &broken), &host),
            GhPath::ExcludedFallback
        );
        assert!(
            working.lines().is_empty(),
            "no probe runs: {:?}",
            working.lines()
        );
        assert!(
            broken.lines().is_empty(),
            "no probe runs: {:?}",
            broken.lines()
        );
    }
}
