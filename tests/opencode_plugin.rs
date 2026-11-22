//! Drift guards between the OpenCode plugin under `adapters/opencode-plugin` and the crate (Task 25b). Each test reads
//! a value from a plugin file and compares it with the value the crate defines, so a change on either side fails here.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use clap::CommandFactory;
use relaxkit::cli::Cli;
use relaxkit::sandbox::ALLOWED_DOMAINS;
use relaxkit::sandbox::claude::PACK_COMMANDS;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn package_json() -> serde_json::Value {
    serde_json::from_str(&read("adapters/opencode-plugin/package.json"))
        .expect("package.json parses")
}

/// The double-quoted strings of `export const <name> = [ ... ];` in a plugin source file, in order.
fn js_string_array(source: &str, name: &str) -> Vec<String> {
    let declaration = format!("export const {name} = [");
    let start = source
        .find(&declaration)
        .unwrap_or_else(|| panic!("`{declaration}` is missing"))
        + declaration.len();
    let end = start
        + source[start..]
            .find(']')
            .unwrap_or_else(|| panic!("{name} is not closed"));
    source[start..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(String::from)
        .collect()
}

#[test]
fn plugin_allowed_domains_match_the_claude_sandbox_list() {
    let config = read("adapters/opencode-plugin/src/config.js");
    let plugin = js_string_array(&config, "ALLOWED_DOMAINS");
    let crate_list: Vec<String> = ALLOWED_DOMAINS
        .iter()
        .map(|domain| domain.to_string())
        .collect();
    assert_eq!(plugin, crate_list);
}

#[test]
fn plugin_registers_one_command_per_cli_subcommand() {
    let commands = read("adapters/opencode-plugin/src/commands.js");
    let mut plugin = js_string_array(&commands, "COMMAND_NAMES");
    plugin.sort();
    let mut subcommands: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_string())
        .filter(|name| name != "help")
        .collect();
    subcommands.sort();
    assert_eq!(plugin, subcommands);
}

#[test]
fn plugin_mutating_commands_are_the_pack_commands() {
    let commands = read("adapters/opencode-plugin/src/commands.js");
    let plugin: BTreeSet<String> = js_string_array(&commands, "MUTATING_COMMANDS")
        .into_iter()
        .collect();
    let pack: BTreeSet<String> = PACK_COMMANDS.iter().map(|name| name.to_string()).collect();
    assert_eq!(plugin, pack);
}

#[test]
fn plugin_package_matches_the_crate() {
    let package = package_json();
    assert_eq!(package["name"], "relaxkit-opencode", "{package}");
    assert_eq!(package["version"], env!("CARGO_PKG_VERSION"), "{package}");
    assert_eq!(
        package["dependencies"]["@anthropic-ai/sandbox-runtime"], "0.0.83",
        "{package}"
    );
}

#[test]
fn install_spec_tag_is_the_crate_version() {
    // The install spec pins `v<crate version>`, so the tag must name the plugin's own version.
    let package = package_json();
    let plugin_version = package["version"]
        .as_str()
        .expect("package.json has a version");
    assert_eq!(
        format!("v{}", env!("CARGO_PKG_VERSION")),
        format!("v{plugin_version}")
    );
}
