//! Where a tool came from, and the helpers that read a recipe's plugin specs and probes.

use crate::host::HostKind;
use crate::recipe::{Kind, Recipe, Step};

/// Where a tool came from. Recorded in the lock so `status` can say how it got there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Brew,
    Script,
    Npm,
    Uv,
    ClaudePlugin(String),
    OpencodePlugin,
    Unknown,
}

impl Route {
    /// The text stored in `lock.toml`.
    pub fn as_str(&self) -> String {
        match self {
            Route::Brew => "brew".to_string(),
            Route::Script => "script".to_string(),
            Route::Npm => "npm".to_string(),
            Route::Uv => "uv".to_string(),
            Route::ClaudePlugin(marketplace) => format!("claude-plugin:{marketplace}"),
            Route::OpencodePlugin => "opencode-plugin".to_string(),
            Route::Unknown => "unknown".to_string(),
        }
    }
}

/// The name of a plugin spec: the text before the `@` that starts the marketplace or source. A leading `@`
/// belongs to a scoped name and is kept.
pub fn plugin_name(spec: &str) -> &str {
    let start = usize::from(spec.starts_with('@'));
    match spec[start..].find('@') {
        Some(at) => &spec[..start + at],
        None => spec,
    }
}

/// The first plugin spec a recipe installs on `kind`.
pub fn plugin_spec(recipe: &Recipe, kind: HostKind) -> Option<&str> {
    recipe
        .install
        .get(&kind)?
        .iter()
        .find_map(|step| match step {
            Step::Plugin { spec } => Some(spec.as_str()),
            _ => None,
        })
}

/// Whether a list of steps runs `uv`.
pub fn uses_uv(steps: &[Step]) -> bool {
    steps.iter().any(|step| match step {
        Step::Exec { argv, .. } => argv.first().map(String::as_str) == Some("uv"),
        _ => false,
    })
}

/// The route of a tool relaxkit installed on `kind`. `marketplace` is the expanded marketplace of a plugin.
pub fn install_route(recipe: &Recipe, kind: HostKind, marketplace: &str) -> Route {
    if plugin_spec(recipe, kind).is_some() {
        return match kind {
            HostKind::Claude => Route::ClaudePlugin(marketplace.to_string()),
            HostKind::Opencode => Route::OpencodePlugin,
        };
    }
    let steps = recipe.install.get(&kind).map(Vec::as_slice).unwrap_or(&[]);
    if uses_uv(steps) {
        return Route::Uv;
    }
    let runs_npx = recipe
        .install
        .get(&kind)
        .into_iter()
        .flatten()
        .any(|step| match step {
            Step::Exec { argv, .. } | Step::Mcp { argv, .. } => {
                argv.iter().any(|a| a.contains("{npx}"))
            }
            _ => false,
        });
    match recipe.kind {
        Kind::Npm => Route::Npm,
        Kind::Mcp if runs_npx => Route::Npm,
        Kind::Binary => Route::Script,
        _ => Route::Unknown,
    }
}
