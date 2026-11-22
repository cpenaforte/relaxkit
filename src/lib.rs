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
pub mod selfupdate;
pub mod state;
pub mod tools;

use anyhow::{Result, anyhow};

use cli::{Cli, Command};
use ctx::{Ctx, Reporter};
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
            let mut ctx = Ctx::system(cli.dry_run, None, false)?;
            // The self step runs first for a plain update and for `--self` (K92). Its text is held apart, so that
            // `--json` keeps stdout a JSON document.
            let self_step = self_only || (tool.is_none() && !tools_only);
            let mut self_lines = Vec::new();
            let mut self_failed = false;
            // True when a swapped binary already ran the tool phase (K96), so this process must not run it again.
            let mut tools_done_by_new_binary = false;
            if self_step {
                let tools_out = std::mem::replace(&mut ctx.out, Reporter::capture());
                let outcome = selfupdate::run_with(&ctx, check, !self_only);
                let captured = ctx.out.lines();
                ctx.out = tools_out;
                match outcome {
                    Ok(result) => {
                        self_lines.push(selfupdate::row(&result));
                        // A swapped binary that ran its tool phase is done here (K96). A failed tool in that phase
                        // is named and the exit is 1 (K102).
                        match result {
                            selfupdate::SelfUpdate::Updated { .. } => {
                                tools_done_by_new_binary = true;
                            }
                            selfupdate::SelfUpdate::UpdatedToolsFailed { .. } => {
                                tools_done_by_new_binary = true;
                                self_failed = true;
                            }
                            _ => {}
                        }
                    }
                    Err(err) => {
                        self_failed = true;
                        self_lines.push(format!("relaxkit itself: failed: {err:#}"));
                    }
                }
                self_lines.extend(captured);
            }
            for line in &self_lines {
                if cli.json {
                    eprintln!("{line}");
                } else {
                    ctx.out.info(line);
                }
            }
            if self_only {
                // K101: `--self` runs no tool phase, and the report says so.
                ctx.out.info("tools: not run (--self)");
                return Ok(if self_failed { 1 } else { 0 });
            }
            if tools_done_by_new_binary {
                return Ok(if self_failed { 1 } else { 0 });
            }
            // A failed self step does not stop the tool phase, which runs with the current binary (K92).
            let args = commands::update::UpdateArgs { check, tool };
            let report = commands::update::run(&ctx, &args, &NvmSearch::from_env())?;
            if cli.json {
                println!("{}", commands::update::to_json(&report));
            } else {
                for line in commands::update::lines(&report) {
                    ctx.out.info(&line);
                }
            }
            Ok(if self_failed || !report.failed.is_empty() {
                1
            } else {
                0
            })
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
