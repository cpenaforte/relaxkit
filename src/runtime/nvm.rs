//! nvm and the Node it installs. relaxkit runs nvm in a subshell, never `nvm use` or `nvm alias default`,
//! so the user's default Node and shell are not changed by an update.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};

use crate::ctx::Ctx;
use crate::exec::Cmd;
use crate::host::run_edit;
use crate::runtime::{download_verified, utf8};
use crate::state::RunId;

/// The profile files nvm's installer may append to. Each is backed up and journaled before the install.
const PROFILE_FILES: [&str; 4] = [".zshrc", ".bashrc", ".bash_profile", ".profile"];

/// Whether the installer may edit the shell profile. Only a first install may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileMode {
    Edit,
    Untouched,
}

/// The nvm release whose installer relaxkit runs. Pinned by owner decision O8.
pub const NVM_VERSION: &str = "v0.41.0";

/// SHA-256 of nvm's `install.sh` at `NVM_VERSION`. The download must match before anything runs.
pub const NVM_INSTALL_SHA256: &str =
    "045932fe05f0d4247c946f2ec0ec4026dbfd8ce88a5f67ea28d8ea833227f48b";

/// The Node major that relaxkit installs through nvm.
pub const NODE_MAJOR: &str = "24";

/// Homebrew's prefixes for Apple silicon, Intel macOS and Linuxbrew.
pub const BREW_PREFIXES: [&str; 3] = ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"];

/// A usable nvm. `dir` is the NVM_DIR that holds `versions/`. `script` is the `nvm.sh` that is sourced:
/// `dir/nvm.sh` for a relaxkit or user install, and Homebrew's copy when nvm came from Homebrew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvmInstall {
    pub dir: PathBuf,
    pub script: PathBuf,
}

/// A Node installed by nvm. `root` is the version directory, such as `versions/node/v24.22.3`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInstall {
    pub version: String,
    pub root: PathBuf,
}

/// Where `find_nvm` looks, in order. `from_env` reads the real environment; tests build it directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NvmSearch {
    pub nvm_dir: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub brew_prefixes: Vec<PathBuf>,
    /// The SHA-256 the nvm installer must match. Empty matches nothing, so a search built by hand fails closed.
    pub installer_sha256: String,
}

impl NvmSearch {
    pub fn from_env() -> NvmSearch {
        NvmSearch {
            nvm_dir: std::env::var_os("NVM_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            brew_prefixes: BREW_PREFIXES.iter().map(PathBuf::from).collect(),
            installer_sha256: NVM_INSTALL_SHA256.to_string(),
        }
    }
}

pub fn find_nvm(ctx: &Ctx) -> Option<NvmInstall> {
    find_nvm_in(ctx, &NvmSearch::from_env())
}

/// The first nvm found, in the order NVM_DIR, `~/.nvm`, `$XDG_CONFIG_HOME/nvm`, then Homebrew.
/// A candidate counts only when its `nvm.sh` exists.
pub fn find_nvm_in(ctx: &Ctx, search: &NvmSearch) -> Option<NvmInstall> {
    let home = ctx.dirs.home.join(".nvm");
    let mut candidates = Vec::new();
    if let Some(dir) = &search.nvm_dir {
        candidates.push(install_in(dir.clone()));
    }
    candidates.push(install_in(home.clone()));
    if let Some(xdg) = &search.xdg_config_home {
        candidates.push(install_in(xdg.join("nvm")));
    }
    // Homebrew's nvm keeps its script in the formula, and its versions in ~/.nvm, as its caveats say.
    for prefix in &search.brew_prefixes {
        candidates.push(NvmInstall {
            dir: home.clone(),
            script: prefix.join("opt").join("nvm").join("nvm.sh"),
        });
    }
    candidates.into_iter().find(|nvm| nvm.script.is_file())
}

/// An nvm whose script sits in its own directory.
fn install_in(dir: PathBuf) -> NvmInstall {
    let script = dir.join("nvm.sh");
    NvmInstall { dir, script }
}

pub fn ensure_nvm(ctx: &Ctx, run: &RunId) -> Result<NvmInstall> {
    ensure_nvm_in(ctx, run, &NvmSearch::from_env())
}

/// The nvm in use, installing the latest release into `~/.nvm` when none is found.
pub fn ensure_nvm_in(ctx: &Ctx, run: &RunId, search: &NvmSearch) -> Result<NvmInstall> {
    if let Some(found) = find_nvm_in(ctx, search) {
        return Ok(found);
    }
    let target = install_in(ctx.dirs.home.join(".nvm"));
    if ctx.dry_run {
        ctx.out.info("would install nvm from its latest release");
        return Ok(target);
    }
    run_installer(
        ctx,
        run,
        &target,
        ProfileMode::Edit,
        &search.installer_sha256,
    )?;
    let out = ctx.runner.run(&nvm_cmd(&target, "command -v nvm")?)?;
    if out.code != 0 || out.stdout.trim() != "nvm" {
        bail!(
            "nvm did not load from {} after its install",
            target.script.display()
        );
    }
    Ok(target)
}

/// Runs the latest installer over an existing nvm without touching the shell profile.
/// A Homebrew nvm is left to Homebrew, since its script is not one relaxkit can replace.
pub fn update_nvm_in(ctx: &Ctx, run: &RunId, search: &NvmSearch) -> Result<NvmInstall> {
    let Some(found) = find_nvm_in(ctx, search) else {
        bail!("nvm is not installed; run `relaxkit setup` first");
    };
    if found.script != found.dir.join("nvm.sh") {
        ctx.out
            .info("nvm comes from Homebrew, so it is not updated here");
        return Ok(found);
    }
    if ctx.dry_run {
        ctx.out.info("would update nvm from its latest release");
        return Ok(found);
    }
    run_installer(
        ctx,
        run,
        &found,
        ProfileMode::Untouched,
        &search.installer_sha256,
    )?;
    Ok(found)
}

/// Downloads the installer of the newest nvm release into a fresh directory and runs it with bash.
/// The first install lets it edit the profile, after relaxkit has backed the profile up.
fn run_installer(
    ctx: &Ctx,
    run: &RunId,
    nvm: &NvmInstall,
    mode: ProfileMode,
    expected_sha256: &str,
) -> Result<()> {
    let url = format!("https://raw.githubusercontent.com/nvm-sh/nvm/{NVM_VERSION}/install.sh");
    let installer = download_verified(ctx, run, "nvm", &url, expected_sha256)?;
    // nvm 0.41 refuses an NVM_DIR that is set, missing and not its default, and NVM_DIR is always set here.
    fs::create_dir_all(&nvm.dir).with_context(|| format!("cannot create {}", nvm.dir.display()))?;
    let cmd = Cmd::new("bash")
        .arg(utf8(&installer)?)
        .env("NVM_DIR", utf8(&nvm.dir)?);
    match mode {
        ProfileMode::Edit => {
            let profiles: Vec<PathBuf> = PROFILE_FILES
                .iter()
                .map(|name| ctx.dirs.home.join(name))
                .collect();
            run_edit(ctx, &profiles, &cmd)
        }
        ProfileMode::Untouched => {
            let cmd = cmd.env("PROFILE", "/dev/null");
            let out = ctx.runner.run(&cmd)?;
            if out.code != 0 {
                bail!(
                    "nvm update failed (exit {}): {}",
                    out.code,
                    out.stderr.trim()
                );
            }
            Ok(())
        }
    }
}

/// `bash -c '. "<script>" && <action>'` with NVM_DIR set, so nvm loads the way its docs say.
fn nvm_cmd(nvm: &NvmInstall, action: &str) -> Result<Cmd> {
    let script = utf8(&nvm.script)?;
    Ok(Cmd::new("bash")
        .args(&["-c", &format!(". \"{script}\" && {action}")])
        .env("NVM_DIR", utf8(&nvm.dir)?))
}

pub fn update_nvm(ctx: &Ctx, run: &RunId) -> Result<NvmInstall> {
    update_nvm_in(ctx, run, &NvmSearch::from_env())
}

pub fn ensure_node(ctx: &Ctx, run: &RunId) -> Result<NodeInstall> {
    ensure_node_in(ctx, run, &NvmSearch::from_env())
}

/// The Node 24 that nvm installs for relaxkit. `nvm install 24` is the only Node command run,
/// so a Node already installed is reused by nvm itself and the default alias is left alone.
pub fn ensure_node_in(ctx: &Ctx, run: &RunId, search: &NvmSearch) -> Result<NodeInstall> {
    let nvm = ensure_nvm_in(ctx, run, search)?;
    let action = format!("nvm install {NODE_MAJOR}");
    if ctx.dry_run {
        ctx.out.info(&format!("would run {action}"));
        let version = format!("v{NODE_MAJOR}");
        return Ok(NodeInstall {
            root: nvm.dir.join("versions").join("node").join(&version),
            version,
        });
    }
    let out = ctx.runner.run(&nvm_cmd(&nvm, &action)?)?;
    if out.code != 0 {
        bail!("{action} failed (exit {}): {}", out.code, out.stderr.trim());
    }
    let text = format!("{}\n{}", out.stdout, out.stderr);
    let version = parse_node_version(&text)
        .ok_or_else(|| anyhow!("{action} did not report a Node {NODE_MAJOR} version"))?;
    let root = nvm.dir.join("versions").join("node").join(&version);
    if !root.join("bin").join("node").is_file() {
        bail!(
            "nvm reported {version}, but {} is missing",
            root.join("bin/node").display()
        );
    }
    Ok(NodeInstall { version, root })
}

/// Points the fixed link `dirs.data/node` at `node.root`. The new link is made under a temp name and
/// renamed over the old one, so the link always names a Node: the rename replaces it in one step.
/// A version without `bin/node` is refused, which leaves the old link in place.
pub fn link_node(ctx: &Ctx, node: &NodeInstall) -> Result<PathBuf> {
    let link = ctx.dirs.data.join("node");
    if !node.root.is_absolute() {
        bail!(
            "cannot link to {}: the path is not absolute",
            node.root.display()
        );
    }
    if !node.root.join("bin").join("node").is_file() {
        bail!("cannot link to {}: it has no bin/node", node.root.display());
    }
    if ctx.dry_run {
        ctx.out.info(&format!(
            "would link {} to {}",
            link.display(),
            node.root.display()
        ));
        return Ok(link);
    }
    if link
        .symlink_metadata()
        .is_ok_and(|meta| !meta.file_type().is_symlink())
    {
        bail!("{} exists and is not a link; move it aside", link.display());
    }
    fs::create_dir_all(&ctx.dirs.data)
        .with_context(|| format!("cannot create {}", ctx.dirs.data.display()))?;
    let temp = ctx.dirs.data.join("node.relaxkit-tmp");
    let _ = fs::remove_file(&temp);
    std::os::unix::fs::symlink(&node.root, &temp)
        .with_context(|| format!("cannot create {}", temp.display()))?;
    if let Err(err) = fs::rename(&temp, &link) {
        let _ = fs::remove_file(&temp);
        return Err(err).with_context(|| format!("cannot replace {}", link.display()));
    }
    Ok(link)
}

pub fn node_bin(ctx: &Ctx) -> PathBuf {
    ctx.dirs.data.join("node").join("bin").join("node")
}

pub fn npx_bin(ctx: &Ctx) -> PathBuf {
    ctx.dirs.data.join("node").join("bin").join("npx")
}

/// Node and version managers found on the PATH. They are listed for `status` and never used.
/// relaxkit's own link is excluded.
pub fn other_nodes(ctx: &Ctx) -> Vec<(String, PathBuf)> {
    ["node", "fnm", "volta", "mise", "asdf"]
        .iter()
        .filter_map(|name| ctx.runner.which(name).map(|path| (name.to_string(), path)))
        .filter(|(_, path)| !path.starts_with(&ctx.dirs.data))
        .collect()
}

/// The first `v24.x.y` token in nvm's output.
fn parse_node_version(text: &str) -> Option<String> {
    let prefix = format!("v{NODE_MAJOR}.");
    text.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '(' | ')' | ','))
        .map(|token| token.trim_end_matches('.'))
        .find(|token| {
            token.strip_prefix(prefix.as_str()).is_some_and(|rest| {
                let parts: Vec<&str> = rest.split('.').collect();
                parts.len() == 2
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            })
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::exec::{FakeRunner, Output};
    use crate::net::FakeNet;
    use crate::runtime::testing::{Recorder, ctx_with};
    use crate::state::journal;

    const INSTALL_URL: &str = "https://raw.githubusercontent.com/nvm-sh/nvm/v0.41.0/install.sh";
    const INSTALLER: &str = "echo installer\n";

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A fake nvm: only `nvm.sh` has to exist for detection.
    fn nvm_at(dir: &Path) {
        write(&dir.join("nvm.sh"), "# nvm\n");
    }

    /// A Node binary as nvm would leave it, so the version directory is complete.
    fn node_at(home: &Path, version: &str) -> PathBuf {
        let root = home.join(".nvm/versions/node").join(version);
        write(&root.join("bin/node"), "#!/bin/sh\n");
        root
    }

    fn none() -> NvmSearch {
        NvmSearch::default()
    }

    /// The display prefix of the `nvm install 24` command for an nvm at `home/.nvm`.
    fn install_prefix(home: &Path) -> String {
        format!(
            "bash -c '. \"{}/.nvm/nvm.sh\" && nvm install 24",
            home.display()
        )
    }

    fn net_with_installer(body: &str) -> FakeNet {
        FakeNet::new().with(INSTALL_URL, body)
    }

    /// A search whose pin is the hash of the fake installer, so the fake download passes verification.
    fn pinned() -> NvmSearch {
        NvmSearch {
            installer_sha256: crate::runtime::sha256_hex(INSTALLER.as_bytes()),
            ..NvmSearch::default()
        }
    }

    #[test]
    fn find_nvm_checks_nvm_dir_then_home_then_xdg_then_homebrew() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx_with(
            root.path(),
            &Recorder::new(FakeRunner::new()),
            FakeNet::new(),
        );
        let custom = root.path().join("custom");
        let home = ctx.dirs.home.join(".nvm");
        let xdg = root.path().join("xdg");
        let brew = root.path().join("brew");
        let search = NvmSearch {
            nvm_dir: Some(custom.clone()),
            xdg_config_home: Some(xdg.clone()),
            brew_prefixes: vec![brew.clone()],
            installer_sha256: String::new(),
        };
        nvm_at(&custom);
        nvm_at(&home);
        nvm_at(&xdg.join("nvm"));
        nvm_at(&brew.join("opt/nvm"));

        assert_eq!(find_nvm_in(&ctx, &search).unwrap().dir, custom);

        fs::remove_file(custom.join("nvm.sh")).unwrap();
        assert_eq!(find_nvm_in(&ctx, &search).unwrap().dir, home);

        fs::remove_file(home.join("nvm.sh")).unwrap();
        assert_eq!(find_nvm_in(&ctx, &search).unwrap().dir, xdg.join("nvm"));

        fs::remove_file(xdg.join("nvm/nvm.sh")).unwrap();
        let brew_found = find_nvm_in(&ctx, &search).unwrap();
        assert_eq!(brew_found.dir, home, "Homebrew nvm keeps NVM_DIR at ~/.nvm");
        assert_eq!(brew_found.script, brew.join("opt/nvm/nvm.sh"));

        fs::remove_file(brew.join("opt/nvm/nvm.sh")).unwrap();
        assert!(find_nvm_in(&ctx, &search).is_none());
    }

    #[test]
    fn existing_nvm_is_reused_and_the_default_is_untouched() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        nvm_at(&home.join(".nvm"));
        let version = node_at(home, "v24.22.3");
        let runner = Recorder::new(FakeRunner::new().reply(
            &install_prefix(home),
            Output::ok("Downloading and installing node v24.22.3...\n"),
        ));
        let ctx = ctx_with(home, &runner, FakeNet::new());

        let node = ensure_node_in(&ctx, &RunId::new(), &none()).unwrap();

        assert_eq!(node.version, "v24.22.3");
        assert_eq!(node.root, version);
        let lines = runner.lines();
        assert_eq!(lines.len(), 1, "commands: {lines:?}");
        assert!(lines[0].contains("nvm install 24"), "{lines:?}");
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("nvm use") && !line.contains("alias default")),
            "{lines:?}"
        );
    }

    #[test]
    fn missing_nvm_installs_the_pinned_release() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        write(&home.join(".zshrc"), "export ZSH=1\n");
        write(&home.join(".bashrc"), "alias ll=ls\n");
        node_at(home, "v24.22.3");
        let run = RunId::new();
        let runner = Recorder::new(
            FakeRunner::new()
                .reply("bash /", Output::ok(""))
                .reply(
                    &format!(
                        "bash -c '. \"{}/.nvm/nvm.sh\" && command -v nvm'",
                        home.display()
                    ),
                    Output::ok("nvm\n"),
                )
                .reply(
                    &install_prefix(home),
                    Output::ok("Downloading and installing node v24.22.3...\n"),
                ),
        );
        let ctx = ctx_with(home, &runner, net_with_installer(INSTALLER));

        let node = ensure_node_in(&ctx, &run, &pinned()).unwrap();

        assert_eq!(node.version, "v24.22.3");
        let installer_run = runner
            .cmds()
            .into_iter()
            .find(|cmd| cmd.to_string().starts_with("bash /"))
            .expect("the installer ran");
        assert!(
            installer_run
                .environment()
                .iter()
                .all(|(key, _)| key != "PROFILE"),
            "PROFILE must not be set on a first install"
        );
        let records = journal::read(&ctx.dirs).unwrap();
        let zshrc = records
            .iter()
            .find(|record| record.file == home.join(".zshrc"))
            .expect(".zshrc is journaled before the installer runs");
        let backup = zshrc
            .backup
            .as_ref()
            .expect("an existing profile has a backup");
        assert_eq!(fs::read_to_string(backup).unwrap(), "export ZSH=1\n");
        let bashrc = records
            .iter()
            .find(|record| record.file == home.join(".bashrc"))
            .expect(".bashrc is journaled");
        assert_eq!(
            fs::read_to_string(bashrc.backup.as_ref().unwrap()).unwrap(),
            "alias ll=ls\n"
        );
    }

    #[test]
    fn nvm_update_never_touches_the_profile() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        nvm_at(&home.join(".nvm"));
        write(&home.join(".zshrc"), "export ZSH=1\n");
        let run = RunId::new();
        let runner = Recorder::new(FakeRunner::new().reply("bash /", Output::ok("")));
        let ctx = ctx_with(home, &runner, net_with_installer(INSTALLER));

        let nvm = update_nvm_in(&ctx, &run, &pinned()).unwrap();

        assert_eq!(nvm.dir, home.join(".nvm"));
        let update = runner.cmds().into_iter().last().expect("the update ran");
        assert!(
            update
                .environment()
                .contains(&("PROFILE".to_string(), "/dev/null".to_string())),
            "{:?}",
            update.environment()
        );
        assert!(
            journal::read(&ctx.dirs).unwrap().is_empty(),
            "an update must not journal or back up any profile"
        );
        assert_eq!(
            fs::read_to_string(home.join(".zshrc")).unwrap(),
            "export ZSH=1\n"
        );
    }

    /// Notes whether `dir` exists when an installer script runs. A script is recognised by its first
    /// argument, an absolute path.
    struct Probe {
        inner: Recorder,
        dir: PathBuf,
        seen: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
    }

    impl crate::exec::Runner for Probe {
        fn run(&self, cmd: &crate::exec::Cmd) -> Result<crate::exec::Output> {
            if cmd.argv().first().is_some_and(|arg| arg.starts_with('/')) {
                self.seen.lock().unwrap().push(self.dir.is_dir());
            }
            self.inner.run(cmd)
        }

        fn which(&self, program: &str) -> Option<PathBuf> {
            self.inner.which(program)
        }
    }

    #[test]
    fn the_nvm_directory_exists_before_the_installer_runs() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let nvm_dir = home.join(".nvm");
        let node_bin = nvm_dir.join("versions/node/v24.22.3/bin/node");
        let inner = Recorder::new(
            FakeRunner::new()
                .reply("bash /", Output::ok(""))
                .reply(
                    &format!(
                        "bash -c '. \"{}/.nvm/nvm.sh\" && command -v nvm'",
                        home.display()
                    ),
                    Output::ok("nvm\n"),
                )
                .reply(
                    &install_prefix(home),
                    Output::ok("Downloading and installing node v24.22.3...\n"),
                ),
        )
        .effect(&install_prefix(home), node_bin, "#!/bin/sh\n");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let probe = Probe {
            inner,
            dir: nvm_dir.clone(),
            seen: seen.clone(),
        };
        let ctx = Ctx {
            dirs: crate::platform::KitDirs::rooted(home),
            runner: Box::new(probe),
            net: Box::new(net_with_installer(INSTALLER)),
            out: crate::ctx::Reporter::capture(),
            dry_run: false,
            project: None,
            run: RunId::new(),
        };
        let search = NvmSearch {
            xdg_config_home: Some(home.join("xdg")),
            ..pinned()
        };

        ensure_node_in(&ctx, &RunId::new(), &search).unwrap();

        assert_eq!(*seen.lock().unwrap(), vec![true]);
    }

    #[test]
    fn an_installer_that_does_not_match_its_pin_is_not_run() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let run = RunId::new();
        let runner = Recorder::new(FakeRunner::new().reply("bash /", Output::ok("")));
        let ctx = ctx_with(home, &runner, net_with_installer("tampered\n"));

        let err = ensure_node_in(&ctx, &run, &pinned()).unwrap_err();

        let message = format!("{err:#}");
        assert!(message.contains("install.sh"), "{message}");
        assert!(message.contains("pinned"), "{message}");
        assert!(
            runner
                .lines()
                .iter()
                .all(|line| !line.starts_with("bash /")),
            "the installer ran: {:?}",
            runner.lines()
        );
    }

    #[test]
    fn link_node_swaps_atomically() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let old = NodeInstall {
            version: "v24.1.0".into(),
            root: node_at(home, "v24.1.0"),
        };
        let new = NodeInstall {
            version: "v24.2.0".into(),
            root: node_at(home, "v24.2.0"),
        };
        let ctx = ctx_with(home, &Recorder::new(FakeRunner::new()), FakeNet::new());

        let link = link_node(&ctx, &old).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), old.root);

        let link = link_node(&ctx, &new).unwrap();
        assert_eq!(link, ctx.dirs.data.join("node"));
        assert_eq!(fs::read_link(&link).unwrap(), new.root);
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "the link is still a link"
        );
        assert!(
            ctx.dirs
                .data
                .join("node.relaxkit-tmp")
                .symlink_metadata()
                .is_err(),
            "the temp link was renamed away"
        );
    }

    #[test]
    fn link_to_a_missing_version_fails_and_keeps_the_old_link() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let old = NodeInstall {
            version: "v24.1.0".into(),
            root: node_at(home, "v24.1.0"),
        };
        let missing = NodeInstall {
            version: "v24.9.9".into(),
            root: home.join(".nvm/versions/node/v24.9.9"),
        };
        let ctx = ctx_with(home, &Recorder::new(FakeRunner::new()), FakeNet::new());
        let link = link_node(&ctx, &old).unwrap();

        assert!(link_node(&ctx, &missing).is_err());

        assert_eq!(fs::read_link(&link).unwrap(), old.root);
    }

    #[test]
    fn other_nodes_are_listed_but_never_used() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        nvm_at(&home.join(".nvm"));
        let nvm_root = node_at(home, "v24.22.3");
        let brew_node = Path::new("/opt/homebrew/bin/node");
        let runner = Recorder::new(FakeRunner::new().with_program("node", brew_node).reply(
            &install_prefix(home),
            Output::ok("Downloading and installing node v24.22.3...\n"),
        ));
        let ctx = ctx_with(home, &runner, FakeNet::new());

        let others = other_nodes(&ctx);
        let node = ensure_node_in(&ctx, &RunId::new(), &none()).unwrap();

        assert!(
            others.contains(&("node".to_string(), brew_node.to_path_buf())),
            "{others:?}"
        );
        assert_eq!(node.root, nvm_root);
        assert!(
            runner
                .lines()
                .iter()
                .all(|line| !line.contains("/opt/homebrew")),
            "the PATH node was run: {:?}",
            runner.lines()
        );
    }

    #[test]
    fn node_and_npx_paths_are_absolute_under_the_link() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx_with(
            root.path(),
            &Recorder::new(FakeRunner::new()),
            FakeNet::new(),
        );
        let link = ctx.dirs.data.join("node");

        let node = node_bin(&ctx);
        let npx = npx_bin(&ctx);

        assert!(node.is_absolute() && npx.is_absolute());
        assert_eq!(node, link.join("bin/node"));
        assert_eq!(npx, link.join("bin/npx"));
    }

    #[test]
    fn node_version_is_read_from_nvm_output() {
        assert_eq!(
            parse_node_version("Downloading and installing node v24.22.3...\n"),
            Some("v24.22.3".to_string())
        );
        assert_eq!(
            parse_node_version("v24.22.3 is already installed.\n"),
            Some("v24.22.3".to_string())
        );
        assert_eq!(parse_node_version("Now using node v22.1.0\n"), None);
    }
}
