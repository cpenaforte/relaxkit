//! Runtimes relaxkit installs for itself. Task 10 adds nvm and Node; Task 11 adds uv, gh and Chromium.

pub mod nvm;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

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
    use crate::exec::{Cmd, FakeRunner, Output, Runner};
    use crate::net::FakeNet;
    use crate::platform::KitDirs;
    use crate::state::RunId;

    /// Keeps each command as it was run, environment included, and answers through `inner`.
    /// `FakeRunner::calls` shows only the display string, so the environment needs this.
    #[derive(Clone)]
    pub struct Recorder {
        inner: FakeRunner,
        cmds: Arc<Mutex<Vec<Cmd>>>,
    }

    impl Recorder {
        pub fn new(inner: FakeRunner) -> Recorder {
            Recorder {
                inner,
                cmds: Arc::new(Mutex::new(Vec::new())),
            }
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
            self.inner.run(cmd)
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
            run: RunId::new(),
        }
    }
}
