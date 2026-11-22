//! Recipes: the declarative description of each tool in the pack, embedded at build time.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use include_dir::{Dir, include_dir};

pub mod schema;

pub use schema::{
    KNOWN_HANDLERS, Kind, PLACEHOLDERS, Probe, Recipe, Step, Vars, VersionSource, parse_recipe,
};

static RECIPES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/recipes");

/// Parses every embedded recipe, sorted by id. Fails if any file is malformed or the set is inconsistent.
pub fn load_all() -> Result<Vec<Recipe>> {
    let mut recipes = Vec::new();
    for file in RECIPES.files() {
        let path = file.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let shown = path.display().to_string();
        let text = file
            .contents_utf8()
            .with_context(|| format!("recipe file {shown} is not UTF-8"))?;
        let recipe = parse_recipe(text).with_context(|| format!("recipe file {shown}"))?;
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if recipe.id != stem {
            bail!("recipe file {shown} declares id `{}`", recipe.id);
        }
        recipes.push(recipe);
    }
    recipes.sort_by(|a, b| a.id.cmp(&b.id));
    validate_set(&recipes)?;
    Ok(recipes)
}

/// Checks what only the whole set can show: ids are unique and every `requires` names a recipe in the set.
pub fn validate_set(recipes: &[Recipe]) -> Result<()> {
    let ids: BTreeSet<&str> = recipes.iter().map(|r| r.id.as_str()).collect();
    if ids.len() != recipes.len() {
        bail!("two recipes share an id");
    }
    for recipe in recipes {
        for dep in &recipe.requires {
            if !ids.contains(dep.as_str()) {
                bail!("recipe `{}` requires unknown recipe `{dep}`", recipe.id);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostKind;
    use std::collections::BTreeSet;

    const EXPECTED_IDS: [&str; 10] = [
        "superpowers",
        "rtk",
        "caveman",
        "playwright",
        "chrome-devtools",
        "serena",
        "ccusage",
        "claude-md-management",
        "frontend-design",
        "opencode-sandbox",
    ];

    const FIXTURE: &str = r#"
id = "fixture"
title = "Fixture"
kind = "binary"
hosts = ["claude"]
needs_login = false
telemetry_default_off = true

[probe.claude]
argv = ["fixture", "--version"]
"#;

    fn embedded() -> Vec<Recipe> {
        load_all().expect("embedded recipes load")
    }

    fn by_id(id: &str) -> Recipe {
        embedded()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("no recipe `{id}`"))
    }

    fn vars() -> Vars {
        Vars {
            node: "/n/node".into(),
            npx: "/n/npx".into(),
            marketplace: "/m".into(),
            chromium: "/c/chrome".into(),
        }
    }

    #[test]
    fn all_embedded_recipes_parse() {
        let mut ids: Vec<String> = embedded().into_iter().map(|r| r.id).collect();
        ids.sort();
        let mut expected: Vec<String> = EXPECTED_IDS.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[test]
    fn admission_rule_rejects_login_or_telemetry() {
        assert!(
            parse_recipe(FIXTURE).is_ok(),
            "the valid fixture must parse"
        );

        let login = FIXTURE.replace("needs_login = false", "needs_login = true");
        let err = parse_recipe(&login).unwrap_err().to_string();
        assert!(err.contains("admission rule"), "error was: {err}");
        assert!(err.contains("needs_login"), "error was: {err}");

        let telemetry = FIXTURE.replace(
            "telemetry_default_off = true",
            "telemetry_default_off = false",
        );
        let err = parse_recipe(&telemetry).unwrap_err().to_string();
        assert!(err.contains("admission rule"), "error was: {err}");
        assert!(err.contains("telemetry_default_off"), "error was: {err}");

        for r in embedded() {
            assert!(!r.needs_login, "{} needs login", r.id);
            assert!(
                r.telemetry_default_off,
                "{} telemetry not off by default",
                r.id
            );
        }
    }

    #[test]
    fn host_scope_matches_the_spec() {
        let both: BTreeSet<HostKind> = [HostKind::Claude, HostKind::Opencode].into();
        let claude_only: BTreeSet<HostKind> = [HostKind::Claude].into();
        let opencode_only: BTreeSet<HostKind> = [HostKind::Opencode].into();
        for id in ["claude-md-management", "frontend-design"] {
            let hosts: BTreeSet<HostKind> = by_id(id).hosts.into_iter().collect();
            assert_eq!(hosts, claude_only, "{id}");
        }
        let sandbox: BTreeSet<HostKind> = by_id("opencode-sandbox").hosts.into_iter().collect();
        assert_eq!(sandbox, opencode_only);
        for id in [
            "superpowers",
            "rtk",
            "caveman",
            "playwright",
            "chrome-devtools",
            "serena",
            "ccusage",
        ] {
            let hosts: BTreeSet<HostKind> = by_id(id).hosts.into_iter().collect();
            assert_eq!(hosts, both, "{id}");
        }
    }

    #[test]
    fn every_recipe_has_a_probe_for_each_host_it_supports() {
        for r in embedded() {
            assert!(!r.hosts.is_empty(), "{} has no hosts", r.id);
            for host in &r.hosts {
                assert!(
                    r.probe.contains_key(host),
                    "{} has no probe for {host}",
                    r.id
                );
            }
        }
    }

    fn pinned_fixture() -> Recipe {
        parse_recipe(&format!(
            "{FIXTURE}\n[version]\nnpm = \"fixture-pkg\"\npin = \"9.9.9\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn expand_substitutes_variables_and_rejects_unknown_ones() {
        let recipe = pinned_fixture();
        let argv: Vec<String> = [
            "{npx}",
            "-y",
            "@playwright/mcp@{version}",
            "--executablePath",
            "{chromium}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = recipe.expand(&argv, &vars()).unwrap();
        assert_eq!(
            out,
            [
                "/n/npx",
                "-y",
                "@playwright/mcp@9.9.9",
                "--executablePath",
                "/c/chrome"
            ]
        );

        let unknown = recipe
            .expand(&["{nope}".to_string()], &vars())
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("nope"), "error was: {unknown}");
        assert!(recipe.expand(&["{version".to_string()], &vars()).is_err());
        assert!(recipe.expand(&["version}".to_string()], &vars()).is_err());
        assert!(recipe.expand(&["{}".to_string()], &vars()).is_err());
    }

    #[test]
    fn version_placeholder_expands_to_the_pin_and_plugin_specs_follow_it() {
        let recipe = by_id("superpowers");
        assert_eq!(recipe.pin.as_deref(), Some("7.0.2"));
        let spec = recipe
            .install
            .get(&HostKind::Opencode)
            .unwrap()
            .iter()
            .find_map(|step| match step {
                Step::Plugin { spec } => Some(spec.clone()),
                _ => None,
            })
            .unwrap();
        let out = recipe.expand(&[spec], &vars()).unwrap();
        assert_eq!(
            out,
            ["superpowers@git+https://github.com/obra/superpowers.git#v7.0.2"]
        );
    }

    #[test]
    fn pin_is_required_with_a_source_and_rejected_without_one() {
        let no_pin = format!("{FIXTURE}\n[version]\nnpm = \"x\"\n");
        let err = parse_recipe(&no_pin).unwrap_err().to_string();
        assert!(err.contains("pin"), "error was: {err}");

        let pin_only = format!("{FIXTURE}\n[version]\npin = \"1.0.0\"\n");
        let err = parse_recipe(&pin_only).unwrap_err().to_string();
        assert!(err.contains("pin"), "error was: {err}");

        let not_semver = format!("{FIXTURE}\n[version]\nnpm = \"x\"\npin = \"latest\"\n");
        assert!(parse_recipe(&not_semver).is_err());
    }

    #[test]
    fn every_recipe_pin_matches_the_table() {
        let table: [(&str, Option<&str>); 10] = [
            ("superpowers", Some("7.0.2")),
            ("rtk", Some("1.2.1")),
            ("caveman", Some("4.1.0")),
            ("playwright", Some("0.0.86")),
            ("chrome-devtools", Some("1.13.0")),
            ("serena", Some("2.1.0")),
            ("ccusage", Some("20.0.51")),
            ("claude-md-management", None),
            ("frontend-design", None),
            ("opencode-sandbox", Some("0.7.0")),
        ];
        for (id, pin) in table {
            let recipe = by_id(id);
            assert_eq!(recipe.pin.as_deref(), pin, "pin of {id}");
            assert_eq!(
                recipe.version != VersionSource::None,
                pin.is_some(),
                "source and pin disagree for {id}"
            );
        }
    }

    /// True when the text holds a dotted three-part number that is not written through `{version}`.
    fn has_version_literal(text: &str) -> bool {
        let chars: Vec<char> = text.replace("{version}", "").chars().collect();
        let mut runs = 0;
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                    runs += 1;
                    i += 1;
                    continue;
                }
                if runs == 2 {
                    return true;
                }
                runs = 0;
            } else {
                i += 1;
                runs = 0;
            }
        }
        false
    }

    #[test]
    fn version_literals_only_appear_through_the_placeholder() {
        assert!(has_version_literal("pkg@1.13.0"));
        assert!(!has_version_literal("pkg@{version}"));
        assert!(!has_version_literal("--enable-web-dashboard"));

        for recipe in embedded() {
            let mut texts: Vec<String> = Vec::new();
            for step in recipe
                .install
                .values()
                .chain(recipe.update.values())
                .flatten()
            {
                match step {
                    Step::Exec { argv, .. } | Step::Mcp { argv, .. } => texts.extend(argv.clone()),
                    Step::Plugin { spec } => texts.push(spec.clone()),
                    Step::Handler { .. } => {}
                }
            }
            for probe in recipe.probe.values() {
                texts.extend(probe.argv.clone());
            }
            for text in texts {
                assert!(
                    !has_version_literal(&text),
                    "{}: literal version in `{text}`",
                    recipe.id
                );
            }
        }
    }

    #[test]
    fn caveman_cli_precedes_the_first_exec_on_opencode() {
        let steps = by_id("caveman")
            .install
            .remove(&HostKind::Opencode)
            .unwrap();
        let cli = steps
            .iter()
            .position(|s| matches!(s, Step::Handler { name } if name == "caveman_cli"))
            .expect("opencode install names caveman_cli");
        let first_exec = steps
            .iter()
            .position(|s| matches!(s, Step::Exec { .. }))
            .expect("opencode install has an exec");
        assert!(
            cli < first_exec,
            "caveman_cli must come before the first exec"
        );
    }

    #[test]
    fn version_placeholder_needs_a_version_source() {
        let text = FIXTURE.replace("\"--version\"", "\"{version}\"");
        let err = parse_recipe(&text).unwrap_err().to_string();
        assert!(err.contains("version"), "error was: {err}");
    }

    #[test]
    fn workdir_only_accepts_the_relaxkit_data_directory() {
        let bad = format!("{FIXTURE}\nworkdir = \"/Users/someone/project\"\n");
        assert!(parse_recipe(&bad).is_err());
        assert_eq!(by_id("serena").workdir.as_deref(), Some("data/serena"));
    }

    #[test]
    fn handler_names_must_be_known() {
        let bad = FIXTURE.replace(
            "[probe.claude]",
            "[[install.claude]]\nhandler = \"made_up\"\n\n[probe.claude]",
        );
        let err = parse_recipe(&bad).unwrap_err().to_string();
        assert!(err.contains("made_up"), "error was: {err}");
    }

    #[test]
    fn each_step_names_exactly_one_kind() {
        let two = FIXTURE.replace(
            "[probe.claude]",
            "[[install.claude]]\nexec = [\"a\"]\nplugin = \"b\"\n\n[probe.claude]",
        );
        assert!(parse_recipe(&two).is_err());
        let none = FIXTURE.replace("[probe.claude]", "[[install.claude]]\n\n[probe.claude]");
        assert!(parse_recipe(&none).is_err());
    }

    #[test]
    fn requires_must_name_a_known_recipe() {
        let text = FIXTURE.replace(
            "title = \"Fixture\"",
            "title = \"Fixture\"\nrequires = [\"nope\"]",
        );
        let r = parse_recipe(&text).unwrap();
        assert!(validate_set(&[r]).is_err());
        assert!(validate_set(&embedded()).is_ok());
    }

    #[test]
    fn serena_runs_in_its_data_directory_and_chrome_devtools_needs_playwright() {
        let serena = by_id("serena");
        assert_eq!(
            serena.env.get("SERENA_USAGE_REPORTING").map(String::as_str),
            Some("false")
        );
        let claude_mcp = serena.install.get(&HostKind::Claude).unwrap().iter().any(|s| {
            matches!(s, Step::Mcp { name, argv } if name == "serena" && argv.iter().any(|a| a == "claude-code"))
        });
        assert!(
            claude_mcp,
            "serena claude entry must pass --context claude-code"
        );
        let opencode_has_context =
            serena.install.get(&HostKind::Opencode).unwrap().iter().any(
                |s| matches!(s, Step::Mcp { argv, .. } if argv.iter().any(|a| a == "--context")),
            );
        assert!(
            !opencode_has_context,
            "serena opencode entry must use serena's default context"
        );
        assert_eq!(by_id("chrome-devtools").requires, ["playwright"]);
    }
}
