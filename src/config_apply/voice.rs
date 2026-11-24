//! caveman's default voice (`defaultMode`): global in `~/.config/caveman/config.json`, or per project in
//! `.caveman.json` at the project root. Other keys in the file are kept.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde_json::json;

use crate::ctx::Ctx;
use crate::edit::jsonc_set;
use crate::presets::Voice;
use crate::state::{RunId, safe_write};

/// Sets caveman's `defaultMode` to `voice`, globally or for the current project. An unchanged file is not written.
pub fn apply(ctx: &Ctx, run: &RunId, voice: Voice, project: bool) -> Result<()> {
    let path = target(ctx, project)?;
    let old = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    let text = jsonc_set(&old, &["defaultMode"], &json!(voice.as_str()))?;
    if text == old {
        return Ok(());
    }
    safe_write(ctx, run, &path, &text)
}

fn target(ctx: &Ctx, project: bool) -> Result<PathBuf> {
    if project {
        let root = ctx.project.as_ref().ok_or_else(|| {
            anyhow!("no project is set, so there is no project .caveman.json to write")
        })?;
        Ok(root.join(".caveman.json"))
    } else {
        Ok(ctx
            .dirs
            .home
            .join(".config")
            .join("caveman")
            .join("config.json"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};

    use super::*;
    use crate::exec::FakeRunner;
    use crate::net::FakeNet;

    fn ctx(root: &std::path::Path) -> Ctx {
        Ctx::for_test(root, FakeRunner::new(), FakeNet::new())
    }

    fn global(root: &std::path::Path) -> PathBuf {
        root.join(".config/caveman/config.json")
    }

    #[test]
    fn voice_keeps_the_layout_and_escapes() {
        // Strict JSON edits keep the file's layout and spelling, not only its values.
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(global(root.path()).parent().unwrap()).unwrap();
        fs::write(
            global(root.path()),
            "{\n    \"reinforce\": true,\n    \"label\": \"caf\\u00e9\",\n    \"defaultMode\": \"caveman\"\n}\n",
        )
        .unwrap();
        let ctx = ctx(root.path());

        apply(&ctx, &ctx.run, Voice::Ultracave, false).unwrap();

        let text = fs::read_to_string(global(root.path())).unwrap();
        assert!(text.contains("\n    \"reinforce\": true,\n"), "{text}");
        assert!(text.contains("\"label\": \"caf\\u00e9\""), "{text}");
    }

    #[test]
    fn voice_writes_default_mode_and_keeps_other_keys() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(global(root.path()).parent().unwrap()).unwrap();
        fs::write(
            global(root.path()),
            "{\n  \"reinforce\": true,\n  \"defaultMode\": \"caveman\"\n}\n",
        )
        .unwrap();
        let ctx = ctx(root.path());

        apply(&ctx, &ctx.run, Voice::Ultracave, false).unwrap();

        let value: Value =
            serde_json::from_str(&fs::read_to_string(global(root.path())).unwrap()).unwrap();
        assert_eq!(
            value,
            json!({"reinforce": true, "defaultMode": "ultracave"})
        );
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["reinforce", "defaultMode"],
            "key order must survive"
        );
    }

    #[test]
    fn voice_accepts_off_and_megacave_and_rejects_loud() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        apply(&ctx, &ctx.run, Voice::Off, false).unwrap();
        assert_eq!(read_mode(root.path()), "off");
        apply(&ctx, &ctx.run, Voice::Megacave, false).unwrap();
        assert_eq!(read_mode(root.path()), "megacave");

        let err = Voice::parse("loud").unwrap_err();
        assert!(
            format!("{err:#}").contains("caveman, ultracave, megacave or off"),
            "{err:#}"
        );
    }

    #[test]
    fn project_voice_writes_caveman_json_in_the_project_root() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("work/app");
        fs::create_dir_all(&project).unwrap();
        let mut ctx = ctx(root.path());
        ctx.project = Some(project.clone());

        apply(&ctx, &ctx.run, Voice::Megacave, true).unwrap();

        let text = fs::read_to_string(project.join(".caveman.json")).unwrap();
        assert!(text.contains("\"defaultMode\": \"megacave\""), "{text}");
        assert!(
            !global(root.path()).exists(),
            "the global file must not be touched"
        );
    }

    #[test]
    fn project_voice_without_a_project_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());

        let err = apply(&ctx, &ctx.run, Voice::Caveman, true).unwrap_err();

        assert!(format!("{err:#}").contains("no project"), "{err:#}");
    }

    fn read_mode(root: &std::path::Path) -> String {
        let value: Value =
            serde_json::from_str(&fs::read_to_string(global(root)).unwrap()).unwrap();
        value["defaultMode"].as_str().unwrap().to_string()
    }
}
