pub mod adapters;
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
pub mod ui;

use anyhow::Result;

use cli::{Cli, Command, ConfigCmd, HostArg, LoginTarget, PresetArg};
use ctx::Ctx;
use host::HostKind;
use presets::Preset;
use runtime::nvm::NvmSearch;
use ui::{Mark, Spinner};

/// Runs a parsed command and returns the process exit code. Each handler is a stub until its task lands.
pub fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Setup { allow_sudo } => {
            let ctx = Ctx::system(cli.dry_run, None, allow_sudo)?;
            Ok(commands::setup::run(&ctx)?.exit_code)
        }
        Command::Status => {
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            let report = commands::status::run(&ctx)?;
            for line in report.render(ctx.out.palette()) {
                ctx.out.info(&line);
            }
            Ok(0)
        }
        Command::Doctor { all } => {
            let project = cli.project.then(std::env::current_dir).transpose()?;
            let ctx = Ctx::system(cli.dry_run, project, false)?;
            // A spinner runs only for the text report: `--json` keeps standard output a JSON document.
            let spinner =
                (!cli.json).then(|| Spinner::start("checking the pack", ctx.out.progress()));
            let checks = commands::doctor::run(&ctx);
            if let Some(spinner) = spinner {
                spinner.finish();
            }
            let checks = checks?;
            if cli.json {
                println!("{}", commands::doctor::to_json(&checks));
            } else {
                for line in commands::doctor::lines(&checks, all, ctx.out.palette()) {
                    ctx.out.info(&line);
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
                let spinner = (!cli.json).then(|| {
                    let label = if check {
                        "checking relaxkit"
                    } else {
                        "updating relaxkit"
                    };
                    Spinner::start(label, ctx.out.progress())
                });
                let styled = ctx.out.capture_styled();
                let tools_out = std::mem::replace(&mut ctx.out, styled);
                let outcome = selfupdate::run_with(&ctx, check, !self_only);
                let captured = ctx.out.lines();
                ctx.out = tools_out;
                if let Some(spinner) = spinner {
                    spinner.finish();
                }
                match outcome {
                    Ok(result) => {
                        let mark = selfupdate::mark(&result);
                        self_lines.push(ui::outcome(
                            ctx.out.palette(),
                            mark,
                            &selfupdate::row(&result),
                        ));
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
                            // Brew's new binary ran the tool phase through the link (K103).
                            selfupdate::SelfUpdate::DelegatedToBrew { .. } => {
                                tools_done_by_new_binary = true;
                            }
                            _ => {}
                        }
                    }
                    Err(err) => {
                        self_failed = true;
                        self_lines.push(ui::outcome(
                            ctx.out.palette(),
                            Mark::Fail,
                            &format!("relaxkit itself: failed: {err:#}"),
                        ));
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
                ctx.out.plan("tools: not run (--self)");
                return Ok(if self_failed { 1 } else { 0 });
            }
            if tools_done_by_new_binary {
                return Ok(if self_failed { 1 } else { 0 });
            }
            // A failed self step does not stop the tool phase, which runs with the current binary (K92).
            let args = commands::update::UpdateArgs { check, tool };
            let spinner = (!cli.json).then(|| {
                let label = if check {
                    "checking the tools"
                } else {
                    "updating the tools"
                };
                Spinner::start(label, ctx.out.progress())
            });
            let report = commands::update::run(&ctx, &args, &NvmSearch::from_env());
            if let Some(spinner) = spinner {
                spinner.finish();
            }
            let report = report?;
            if cli.json {
                println!("{}", commands::update::to_json(&report));
            } else {
                for line in commands::update::lines(&report, ctx.out.palette()) {
                    ctx.out.info(&line);
                }
            }
            Ok(if self_failed || !report.failed.is_empty() {
                1
            } else {
                0
            })
        }
        Command::Config(config) => {
            let project = cli.project.then(std::env::current_dir).transpose()?;
            let ctx = Ctx::system(cli.dry_run, project, false)?;
            match config {
                ConfigCmd::Set { key, value, host } => {
                    commands::config::set(&ctx, &key, &value, host.map(host_kind))?;
                }
                ConfigCmd::Unset { key, host } => {
                    commands::config::unset(&ctx, &key, host.map(host_kind))?;
                }
                ConfigCmd::Show { key } => {
                    if cli.json {
                        println!("{}", commands::config::show_json(&ctx, key.as_deref())?);
                    } else {
                        for line in commands::config::show(&ctx, key.as_deref())? {
                            ctx.out.info(&line);
                        }
                    }
                }
            }
            Ok(0)
        }
        Command::Preset { name } => {
            refuse_project(&cli, "preset")?;
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            commands::preset::run(&ctx, preset_of(name))?;
            Ok(0)
        }
        Command::Privacy { claude_code } => {
            refuse_project(&cli, "privacy")?;
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            commands::privacy::run(&ctx, claude_code)?;
            Ok(0)
        }
        Command::Login { target } => {
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            match target {
                LoginTarget::Github => commands::login::run(&ctx),
            }
        }
        Command::Undo { run } => {
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            for line in commands::undo::run(&ctx, run.as_deref())? {
                ctx.out.info(&line);
            }
            Ok(0)
        }
        Command::Uninstall => {
            refuse_project(&cli, "uninstall")?;
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            let spinner = Spinner::start("removing what relaxkit wrote", ctx.out.progress());
            let lines = commands::uninstall::run(&ctx);
            spinner.finish();
            for line in lines? {
                ctx.out.info(&line);
            }
            Ok(0)
        }
        Command::Usage { args } => {
            let ctx = Ctx::system(cli.dry_run, None, false)?;
            commands::usage::run(&ctx, &args)
        }
    }
}

/// `preset`, `privacy` and `uninstall` are global by spec (K113, K115), so `--project` is refused rather than ignored.
fn refuse_project(cli: &Cli, command: &str) -> Result<()> {
    if cli.project {
        anyhow::bail!("--project does not apply to {command}; it is global");
    }
    Ok(())
}

/// The host a `--host` flag names.
fn host_kind(arg: HostArg) -> HostKind {
    match arg {
        HostArg::Claude => HostKind::Claude,
        HostArg::OpenCode => HostKind::Opencode,
    }
}

/// The preset a `preset` argument names.
fn preset_of(arg: PresetArg) -> Preset {
    match arg {
        PresetArg::Lean => Preset::Lean,
        PresetArg::Balanced => Preset::Balanced,
        PresetArg::Careful => Preset::Careful,
    }
}
