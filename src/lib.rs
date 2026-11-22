pub mod brand;
pub mod cli;
pub mod commands;
pub mod config;
pub mod config_apply;
pub mod ctx;
pub mod edit;
pub mod exec;
pub mod fsx;
pub mod github;
pub mod host;
pub mod net;
pub mod platform;
pub mod presets;
pub mod recipe;
pub mod roles;
pub mod runtime;
pub mod sandbox;
pub mod state;
pub mod tools;

use anyhow::{Result, anyhow};

use cli::{Cli, Command};
use ctx::Ctx;
use runtime::nvm::NvmSearch;

/// Runs a parsed command and returns the process exit code. Each handler is a stub until its task lands.
pub fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Setup { allow_sudo } => {
            let ctx = Ctx::system(cli.dry_run, None, allow_sudo)?;
            Ok(commands::setup::run(&ctx)?.exit_code)
        }
        Command::Status => {
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            for line in commands::status::run(&ctx)?.lines {
                ctx.out.info(&line);
            }
            Ok(0)
        }
        Command::Doctor => {
            let project = cli.project.then(std::env::current_dir).transpose()?;
            let ctx = Ctx::system(cli.dry_run, project, false)?;
            let checks = commands::doctor::run(&ctx)?;
            if cli.json {
                println!("{}", commands::doctor::to_json(&checks));
            } else {
                for check in &checks {
                    let fix = check
                        .fix
                        .as_deref()
                        .map(|fix| format!("\n      fix: {fix}"))
                        .unwrap_or_default();
                    ctx.out.info(&format!(
                        "{:<5} {}: {}{fix}",
                        check.status.as_str(),
                        check.id,
                        check.message
                    ));
                }
            }
            let failed = checks
                .iter()
                .any(|check| check.status == commands::doctor::CheckStatus::Fail);
            Ok(if failed { 1 } else { 0 })
        }
        Command::Update {
            self_only,
            tools_only,
            check,
            tool,
        } => {
            if self_only {
                return Err(anyhow!("relaxkit's own update is built in a later step"));
            }
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            // A plain update says that the self step comes first and is not built yet (ruling K63).
            if tool.is_none() && !tools_only {
                ctx.out.info("relaxkit itself: updated in a later step");
            }
            let args = commands::update::UpdateArgs { check, tool };
            let report = commands::update::run(&ctx, &args, &NvmSearch::from_env())?;
            if cli.json {
                println!("{}", commands::update::to_json(&report));
            } else {
                for line in commands::update::lines(&report) {
                    ctx.out.info(&line);
                }
            }
            Ok(if report.failed.is_empty() { 0 } else { 1 })
        }
        Command::Config(_)
        | Command::Preset { .. }
        | Command::Privacy { .. }
        | Command::Login { .. }
        | Command::Undo { .. }
        | Command::Uninstall
        | Command::Usage { .. } => Err(anyhow!("not implemented")),
    }
}
