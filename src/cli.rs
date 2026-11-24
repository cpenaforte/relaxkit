use clap::{Parser, Subcommand, ValueEnum};

use crate::brand;

#[derive(Debug, Parser)]
#[command(name = brand::BIN, bin_name = brand::BIN, version, about = env!("CARGO_PKG_DESCRIPTION"), after_help = root_after_help())]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Show what would change without writing anything
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Apply to the current project where the tool or host supports project scope
    #[arg(long, global = true)]
    pub project: bool,

    /// Print machine-readable output
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install prerequisites, the pack, config, sandbox and the GitHub login
    #[command(after_help = "Examples:\n  relaxkit setup --dry-run\n  relaxkit setup --allow-sudo")]
    Setup {
        /// Run the sandbox's sudo plan (Linux packages). Without it, setup prints the command instead
        #[arg(long)]
        allow_sudo: bool,
    },
    /// Show versions, drift and enforced-vs-advisory settings
    Status,
    /// Check hooks, schemas, the sandbox self-test and MCP reachability
    Doctor {
        /// List the checks that passed too, not only the failures and warnings
        #[arg(long)]
        all: bool,
    },
    /// Update relaxkit itself, then each tool, and update the lockfile
    #[command(
        after_help = "Examples:\n  relaxkit update --check\n  relaxkit update --self\n  relaxkit update rtk"
    )]
    Update {
        /// Update only relaxkit itself
        #[arg(long = "self", conflicts_with_all = ["tools_only", "tool"])]
        self_only: bool,
        /// Update only the tools
        #[arg(long = "tools", conflicts_with = "tool")]
        tools_only: bool,
        /// Report available updates without installing them
        #[arg(long)]
        check: bool,
        /// Update one named tool
        tool: Option<String>,
    },
    /// Show, set or unset roles, tiers and other preferences
    #[command(
        subcommand,
        after_help = "Examples:\n  relaxkit config show\n  relaxkit config set implementer cheap\n  relaxkit config set planner anthropic/claude-sonnet-5-5#high\n  relaxkit config unset planner"
    )]
    Config(ConfigCmd),
    /// Apply a preset
    Preset {
        /// Preset name
        name: PresetArg,
    },
    /// Show or change telemetry
    Privacy {
        /// Change Claude Code's telemetry setting
        #[arg(long)]
        claude_code: bool,
    },
    /// Log in to a service
    Login {
        /// Service to log in to
        target: LoginTarget,
    },
    /// Revert the last change, or a named run group
    Undo {
        /// Revert this run group (a self step and the tool run it re-executes) instead of the newest one
        #[arg(long)]
        run: Option<String>,
    },
    /// Remove the pack's MCP entries, config keys, roles blocks and binary copies
    Uninstall,
    /// Run the usage tool. Tool flags must come after `--` or after the first argument.
    Usage {
        /// Arguments for the usage tool. Relaxkit's global flags (--dry-run, --project, --json) are read before the first argument, and -h and --help are relaxkit's own only before the first argument; other flags pass through.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Set a role to a tier or an exact model
    Set {
        /// Role, such as planner, implementer, reviewer, voice or compact-at
        key: String,
        /// Tier (cheap, standard, smarter) or exact model (provider/model[#variant])
        value: String,
        /// Limit the change to one host
        #[arg(long)]
        host: Option<HostArg>,
    },
    /// Remove a setting so the preset value applies again
    Unset {
        /// Role to unset
        key: String,
        /// Limit the change to one host
        #[arg(long)]
        host: Option<HostArg>,
    },
    /// Show settings, all or one key
    Show {
        /// Role to show
        key: Option<String>,
    },
}

/// Root help: the examples, then the link to the documentation.
fn root_after_help() -> String {
    format!(
        "Examples:\n  relaxkit setup --dry-run\n  relaxkit status\n  relaxkit config set implementer cheap\n  relaxkit doctor --all\n  relaxkit undo\nDocumentation: https://github.com/{}/tree/main/docs",
        brand::REPO
    )
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PresetArg {
    Lean,
    Balanced,
    Careful,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HostArg {
    Claude,
    #[value(name = "opencode")]
    OpenCode,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LoginTarget {
    Github,
}
