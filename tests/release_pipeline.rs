//! Release pipeline tests (Task 27 part A): the Homebrew formula renderer, its template and the release workflow.
//!
//! Nothing here reaches the network or runs a release. The renderer runs on fake checksums, and the workflow is
//! parsed as YAML, never run. The repository name is the constant `OWNER/relaxkit` (ruling P-2).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

const VERSION: &str = "0.1.0";
const REPO: &str = "OWNER/relaxkit";
const TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn has_ruby() -> bool {
    Command::new("ruby")
        .arg("-v")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The fake sha256 for the target at `index`: that index's letter repeated 64 times.
fn fake_hash(index: usize) -> String {
    char::from(b'a' + index as u8).to_string().repeat(64)
}

/// A checksums file in the format the release workflow writes: `<sha256>  <asset name>` per line.
fn fake_checksums(dir: &Path, targets: &[&str]) -> PathBuf {
    let mut text = String::new();
    for (index, target) in targets.iter().enumerate() {
        text.push_str(&format!(
            "{}  relaxkit-{VERSION}-{target}.tar.gz\n",
            fake_hash(index)
        ));
    }
    let path = dir.join(format!("relaxkit-{VERSION}-checksums.txt"));
    fs::write(&path, text).unwrap();
    path
}

fn render(checksums: &Path) -> Output {
    Command::new("sh")
        .arg(repo_root().join("scripts/render-formula.sh"))
        .arg(VERSION)
        .arg(checksums)
        .output()
        .expect("cannot run scripts/render-formula.sh")
}

fn rendered_formula() -> String {
    let dir = TempDir::new().unwrap();
    let checksums = fake_checksums(dir.path(), &TARGETS);
    let out = render(&checksums);
    assert!(
        out.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn formula_installs_the_rlx_alias() {
    let formula = rendered_formula();
    assert!(
        formula.contains(r#"bin.install_symlink "relaxkit" => "rlx""#),
        "the formula must install the rlx alias as a symlink:\n{formula}"
    );
}

#[test]
fn formula_renders_four_urls_and_checksums() {
    let dir = TempDir::new().unwrap();
    let checksums = fake_checksums(dir.path(), &TARGETS);
    let out = render(&checksums);
    assert!(
        out.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let formula = String::from_utf8(out.stdout).unwrap();

    let lines: Vec<&str> = formula.lines().map(str::trim).collect();
    for (index, target) in TARGETS.iter().enumerate() {
        let url = format!(
            "https://github.com/{REPO}/releases/download/v{VERSION}/relaxkit-{VERSION}-{target}.tar.gz"
        );
        let at = lines
            .iter()
            .position(|line| *line == format!(r#"url "{url}""#))
            .unwrap_or_else(|| panic!("no url line for {target}:\n{formula}"));
        let expected = format!(r#"sha256 "{}""#, fake_hash(index));
        assert_eq!(
            lines.get(at + 1).copied(),
            Some(expected.as_str()),
            "the checksum for {target} must follow its url"
        );
    }
    assert_eq!(
        formula.matches("/releases/download/").count(),
        4,
        "the formula must carry exactly four asset URLs"
    );

    if has_ruby() {
        let rb = dir.path().join("relaxkit.rb");
        fs::write(&rb, &formula).unwrap();
        let check = Command::new("ruby").arg("-c").arg(&rb).output().unwrap();
        assert!(
            check.status.success(),
            "ruby -c rejected the formula: {}",
            String::from_utf8_lossy(&check.stderr)
        );
    } else {
        eprintln!("ruby is not installed: the ruby -c check is skipped");
    }
}

#[test]
fn formula_render_refuses_a_missing_checksum() {
    let dir = TempDir::new().unwrap();
    let full = render(&fake_checksums(dir.path(), &TARGETS));
    assert!(
        full.status.success(),
        "the full checksum set must render first"
    );
    let partial_dir = TempDir::new().unwrap();
    let checksums = fake_checksums(partial_dir.path(), &TARGETS[..3]);
    let out = render(&checksums);
    assert!(
        !out.status.success(),
        "a missing target checksum must fail the render"
    );
    assert!(
        out.stdout.is_empty(),
        "a failed render must print no formula"
    );
}

#[test]
fn release_workflow_is_yaml_and_publishes_only_after_signing() {
    assert!(has_ruby(), "ruby is required for the release.yml check");
    let path = repo_root().join(".github/workflows/release.yml");
    // Psych (Ruby's YAML) reads the bare key `on` as the boolean true, so the trigger may come back under "true".
    let out = Command::new("ruby")
        .args(["-ryaml", "-rjson", "-e"])
        .arg("print JSON.generate(YAML.load_file(ARGV[0]))")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "release.yml is not valid YAML: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();

    let triggers = doc
        .get("on")
        .or_else(|| doc.get("true"))
        .expect("no trigger");
    assert_eq!(triggers["push"]["tags"], json!(["v*"]));

    let jobs = &doc["jobs"];
    let include = jobs["build"]["strategy"]["matrix"]["include"]
        .as_array()
        .expect("the build job has no matrix include");
    let mut built: Vec<&str> = include
        .iter()
        .map(|entry| entry["target"].as_str().unwrap())
        .collect();
    built.sort();
    let mut expected = TARGETS.to_vec();
    expected.sort();
    assert_eq!(built, expected, "the build matrix must be the four targets");
    assert_eq!(jobs["publish"]["needs"], json!(["build"]));

    for job in jobs.as_object().unwrap().values() {
        for step in job["steps"].as_array().unwrap() {
            let run = step["run"].as_str().unwrap_or("");
            assert!(
                !run.contains("secrets.MINISIGN_KEY"),
                "the signing secret must reach a step through env, never inside a run script"
            );
        }
    }

    let steps = jobs["publish"]["steps"].as_array().unwrap();
    let position = |needle: &str| {
        steps
            .iter()
            .position(|step| step["run"].as_str().is_some_and(|run| run.contains(needle)))
            .unwrap_or_else(|| panic!("no publish step runs `{needle}`"))
    };
    let sign = position("minisign -S");
    let verify = position("minisign -V");
    let publish = position("gh release create");
    assert!(
        sign < verify && verify < publish,
        "the release must be signed, then verified, then published"
    );
}
