use clap::{Parser, Subcommand, ValueEnum};

use crate::brand;

#[derive(Debug, Parser)]
#[command(name = brand::BIN, bin_name = brand::BIN, version, about = "Set up and manage a curated set of agent tools")]
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
    Setup,
    /// Show versions, drift and enforced-vs-advisory settings
    Status,
    /// Check hooks, schemas, the sandbox self-test and MCP reachability
    Doctor,
    /// Update relaxkit itself, then each tool, and update the lockfile
    Update {
        /// Update only relaxkit itself
        #[arg(long = "self", conflicts_with = "tools_only")]
        self_only: bool,
        /// Update only the tools
        #[arg(long = "tools")]
        tools_only: bool,
        /// Report available updates without installing them
        #[arg(long)]
        check: bool,
        /// Update one named tool
        tool: Option<String>,
    },
    /// Show, set or unset roles, tiers and other preferences
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
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
    /// Revert the last change, or a named run
    Undo {
        /// Revert this run instead of the last one
        #[arg(long)]
        run: Option<String>,
    },
    /// Remove relaxkit and what it installed
    Uninstall,
    /// Run the usage tool with the given arguments
    Usage {
        /// Arguments passed through unchanged
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
