//! `relaxkit login github` (spec 6, 7, ruling K108). The flow is Task 18's `github::login`: it is skipped when
//! `gh auth status` passes, and otherwise runs the web login and `gh auth setup-git`. relaxkit never handles a token,
//! so there is no token delivery step; the `gh` route for Claude Code is fixed by setup (K42).

use std::io::IsTerminal;

use anyhow::Result;

use crate::ctx::Ctx;
use crate::github::{self, LoginOutcome};
use crate::state::with_run_lock;
use crate::ui::Mark;

/// Logs in with the terminal check made on the real stdin.
pub fn run(ctx: &Ctx) -> Result<i32> {
    run_with(ctx, std::io::stdin().is_terminal())
}

/// The login with the terminal question answered by the caller. Returns the exit code: 3 when the owner must finish.
pub fn run_with(ctx: &Ctx, interactive: bool) -> Result<i32> {
    let outcome = if ctx.dry_run {
        github::login_with(ctx, &ctx.run, interactive)?
    } else {
        with_run_lock(&ctx.dirs, || github::login_with(ctx, &ctx.run, interactive))?
    };
    match &outcome {
        LoginOutcome::AlreadyAuthenticated => {
            ctx.out.outcome(Mark::Ok, "already logged in to GitHub");
        }
        LoginOutcome::LoggedIn => ctx.out.outcome(Mark::Ok, "logged in to GitHub"),
        // The dry run's plan is printed by the GitHub step itself.
        LoginOutcome::Planned => {}
        LoginOutcome::ManualStep { command } => {
            ctx.out.outcome(
                Mark::Warn,
                &format!("finish the login yourself, then run it again: {command}"),
            );
        }
    }
    Ok(outcome.exit_code())
}
