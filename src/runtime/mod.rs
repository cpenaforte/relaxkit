//! Runtimes relaxkit installs for itself: nvm and Node, uv, gh and Chromium.

pub mod caveman;
pub mod chromium;
pub mod gh;
pub mod nvm;
pub mod rtk;
pub mod uv;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::ctx::Ctx;
use crate::state::RunId;

/// Where one run's download for `name` is unpacked: `dirs.data/downloads/<run>/<name>`.
/// Each download gets its own directory, so nothing in it is trusted by name alone.
pub(crate) fn download_dir(ctx: &Ctx, run: &RunId, name: &str) -> PathBuf {
    ctx.dirs
        .data
        .join("downloads")
        .join(run.as_str())
        .join(name)
}

/// Removes whatever is at `dir`, then creates it as an empty directory.
pub(crate) fn fresh_dir(dir: &Path) -> Result<()> {
    match dir.symlink_metadata() {
        Ok(meta) if meta.is_dir() => {
            fs::remove_dir_all(dir).with_context(|| format!("cannot clear {}", dir.display()))?
        }
        Ok(_) => fs::remove_file(dir).with_context(|| format!("cannot clear {}", dir.display()))?,
        Err(_) => {}
    }
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))
}

/// Downloads `url` into a fresh `download_dir(name)/install.sh` and returns that path only when the
/// file's SHA-256 equals `expected`. A mismatch is an error that names the file, and nothing is run.
/// An empty `expected` matches nothing.
pub(crate) fn download_verified(
    ctx: &Ctx,
    run: &RunId,
    name: &str,
    url: &str,
    expected: &str,
) -> Result<PathBuf> {
    let work = download_dir(ctx, run, name);
    fresh_dir(&work)?;
    let file = work.join("install.sh");
    ctx.net.download(url, &file)?;
    let bytes = fs::read(&file).with_context(|| format!("cannot read {}", file.display()))?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        bail!(
            "{} does not match its pinned SHA-256 (expected {expected}, got {actual}); nothing was run",
            file.display()
        );
    }
    Ok(file)
}

/// SHA-256 of `bytes` as lowercase hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The SHA-256 that a checksums file lists for `name`. Each line is `<hex>  <name>`, with an optional `*` before
/// the name.
pub(crate) fn checksum_for(checksums: &str, name: &str) -> Result<String> {
    checksums
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, file)| file.trim().trim_start_matches('*') == name)
        .map(|(hash, _)| hash.trim().to_ascii_lowercase())
        .ok_or_else(|| anyhow!("the checksums file has no entry for {name}"))
}

/// A path as text for a command line. A non-UTF-8 path is an error, not a lossy string.
pub(crate) fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use anyhow::Result;

    use crate::ctx::{Ctx, Reporter};
    use crate::exec::{Cmd, FakeRunner, Output, Runner, SystemRunner};
    use crate::net::FakeNet;
    use crate::platform::KitDirs;
    use crate::state::RunId;

    /// Keeps each command as it was run, environment included, and answers through `inner`.
    /// `FakeRunner::calls` shows only the display string, so the environment needs this.
    /// `real` prefixes run through the system (such as `tar`, for an archive built in the test), and
    /// `effect` writes a file after a matching command succeeds, as the real tool would.
    #[derive(Clone)]
    pub struct Recorder {
        inner: FakeRunner,
        cmds: Arc<Mutex<Vec<Cmd>>>,
        real: Vec<String>,
        effects: Vec<(String, PathBuf, String)>,
    }

    impl Recorder {
        pub fn new(inner: FakeRunner) -> Recorder {
            Recorder {
                inner,
                cmds: Arc::new(Mutex::new(Vec::new())),
                real: Vec::new(),
                effects: Vec::new(),
            }
        }

        /// Commands whose display starts with `prefix` run for real instead of being answered.
        pub fn real(mut self, prefix: &str) -> Recorder {
            self.real.push(prefix.to_string());
            self
        }

        /// After a command whose display starts with `prefix` succeeds, `path` is written with `text`.
        pub fn effect(mut self, prefix: &str, path: PathBuf, text: &str) -> Recorder {
            self.effects
                .push((prefix.to_string(), path, text.to_string()));
            self
        }

        pub fn cmds(&self) -> Vec<Cmd> {
            self.cmds.lock().expect("recorder lock").clone()
        }

        /// The display string of every command asked for, answered or not, in order.
        pub fn lines(&self) -> Vec<String> {
            self.cmds().iter().map(|cmd| cmd.to_string()).collect()
        }
    }

    impl Runner for Recorder {
        fn run(&self, cmd: &Cmd) -> Result<Output> {
            self.cmds.lock().expect("recorder lock").push(cmd.clone());
            let display = cmd.to_string();
            if self
                .real
                .iter()
                .any(|prefix| display.starts_with(prefix.as_str()))
            {
                return SystemRunner.run(cmd);
            }
            let out = self.inner.run(cmd)?;
            if out.code == 0 {
                for (prefix, path, text) in &self.effects {
                    if display.starts_with(prefix.as_str()) {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(path, text)?;
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
                        }
                    }
                }
            }
            Ok(out)
        }

        fn which(&self, program: &str) -> Option<PathBuf> {
            self.inner.which(program)
        }
    }

    /// A context rooted at `root` whose runner records every command.
    pub fn ctx_with(root: &Path, runner: &Recorder, net: FakeNet) -> Ctx {
        Ctx {
            dirs: KitDirs::rooted(root),
            runner: Box::new(runner.clone()),
            net: Box::new(net),
            out: Reporter::capture(),
            dry_run: false,
            project: None,
            allow_sudo: false,
            run: RunId::new(),
            group: None,
        }
    }
}
