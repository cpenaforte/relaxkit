//! The context every command receives: directories, runner, network, output, and flags.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::exec::{FakeRunner, Runner, SystemRunner};
use crate::net::{FakeNet, HttpNet, Net};
use crate::platform::KitDirs;
use crate::state::RunId;
use crate::ui::{self, Mark, Palette, Stream, stderr_is_terminal, stdout_is_terminal};

/// Where user-facing lines go. `stdout()` prints; `capture()` keeps them for tests.
#[derive(Clone, Default)]
pub struct Reporter {
    capture: Option<Arc<Mutex<Vec<String>>>>,
    palette: Palette,
    progress: Palette,
}

impl Reporter {
    /// Prints to standard output, with color and glyphs decided by the terminals it and standard error are on.
    pub fn stdout() -> Reporter {
        Reporter {
            capture: None,
            palette: Palette::for_stream(stdout_is_terminal()),
            progress: Palette::for_stream(stderr_is_terminal()),
        }
    }

    /// Keeps the lines for a test. It has no color, no glyph set beyond the default, and no animation.
    pub fn capture() -> Reporter {
        Reporter {
            capture: Some(Arc::new(Mutex::new(Vec::new()))),
            palette: Palette::PLAIN,
            progress: Palette::PLAIN,
        }
    }

    /// Keeps the lines like `capture`, but painted as this reporter would print them, so the lines keep their colors
    /// when they are printed later.
    pub fn capture_styled(&self) -> Reporter {
        Reporter {
            capture: Some(Arc::new(Mutex::new(Vec::new()))),
            palette: self.palette,
            progress: self.progress,
        }
    }

    /// How a line printed to standard output is painted.
    pub fn palette(&self) -> Palette {
        self.palette
    }

    /// How a spinner on standard error is painted. A spinner runs only where this palette can animate.
    pub fn progress(&self) -> Palette {
        self.progress
    }

    /// Prints a line as it is. A line that reports a result goes through `outcome`, so it carries its mark.
    pub fn info(&self, msg: &str) {
        self.emit(Stream::Stdout, msg.to_string());
    }

    /// Prints a result on standard output: its mark, then the text.
    pub fn outcome(&self, mark: Mark, msg: &str) {
        self.emit(Stream::Stdout, ui::outcome(self.palette, mark, msg));
    }

    /// Prints an action that a dry run did not take: a skip mark, then the text.
    pub fn plan(&self, msg: &str) {
        self.outcome(Mark::Skip, msg);
    }

    /// Prints a warning on standard error, with the warning mark.
    pub fn warn(&self, msg: &str) {
        self.emit(Stream::Stderr, ui::outcome(self.progress, Mark::Warn, msg));
    }

    /// Lines recorded by a capture reporter, as they would print. Empty for `stdout()`.
    pub fn lines(&self) -> Vec<String> {
        match &self.capture {
            Some(lines) => lines.lock().expect("reporter lock").clone(),
            None => Vec::new(),
        }
    }

    fn emit(&self, stream: Stream, line: String) {
        match &self.capture {
            Some(lines) => lines.lock().expect("reporter lock").push(line),
            None => ui::print_line(stream, &line),
        }
    }
}

/// Everything a command may touch. Tests build one with `Ctx::for_test`.
pub struct Ctx {
    pub dirs: KitDirs,
    pub runner: Box<dyn Runner>,
    pub net: Box<dyn Net>,
    pub out: Reporter,
    pub dry_run: bool,
    pub project: Option<PathBuf>,
    /// Lets a step run its `sudo` plan (ruling K47). Without it, the plan is only printed.
    pub allow_sudo: bool,
    /// The run every journaled edit of this process belongs to, so `undo` reverts the whole process.
    pub run: RunId,
    /// The run group this process belongs to when a parent process started it (K105). `None` for a top-level run.
    pub group: Option<RunId>,
}

impl Ctx {
    /// A context rooted at `root`, with scripted runner and network and a capturing reporter.
    pub fn for_test(root: &Path, runner: FakeRunner, net: FakeNet) -> Ctx {
        Ctx {
            dirs: KitDirs::rooted(root),
            runner: Box::new(runner),
            net: Box::new(net),
            out: Reporter::capture(),
            dry_run: false,
            project: None,
            allow_sudo: false,
            run: RunId::new(),
            group: None,
        }
    }

    /// A context for the real machine: discovered directories, real processes and HTTP. The run group comes from
    /// the environment when a parent process started this one (K105).
    pub fn system(
        dry_run: bool,
        project: Option<PathBuf>,
        allow_sudo: bool,
    ) -> anyhow::Result<Ctx> {
        Ok(Ctx {
            dirs: KitDirs::discover()?,
            runner: Box::new(SystemRunner),
            net: Box::new(HttpNet::new()?),
            out: Reporter::stdout(),
            dry_run,
            project,
            allow_sudo,
            run: RunId::new(),
            group: group_from_env(std::env::var_os(RUN_GROUP_ENV).as_deref())?,
        })
    }
}

/// The environment variable a self step sets for the tool run it re-executes (K105).
pub const RUN_GROUP_ENV: &str = "RELAXKIT_RUN_GROUP";

/// The run group named by the environment: `None` when unset or empty, else the id it holds. A value that is not a
/// run id is an error, so a child never journals under a group it cannot name.
pub fn group_from_env(value: Option<&std::ffi::OsStr>) -> anyhow::Result<Option<RunId>> {
    match value.filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(text) => {
            let text = text.to_string_lossy();
            text.parse::<RunId>()
                .map(Some)
                .map_err(|err| anyhow::anyhow!("{RUN_GROUP_ENV}: {err}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_reporter_keeps_lines_as_they_print() {
        let ctx = Ctx::for_test(Path::new("/h"), FakeRunner::new(), FakeNet::new());
        ctx.out.info("hello");
        ctx.out.warn("careful");
        ctx.out.outcome(Mark::Ok, "done");
        ctx.out.plan("would run it");
        assert_eq!(
            ctx.out.lines(),
            vec!["hello", "!  careful", "✓  done", "·  would run it"]
        );
    }

    #[test]
    fn for_test_roots_directories() {
        let ctx = Ctx::for_test(Path::new("/h"), FakeRunner::new(), FakeNet::new());
        assert_eq!(ctx.dirs.config, PathBuf::from("/h/.config/relaxkit"));
        assert!(!ctx.dry_run);
        assert!(ctx.project.is_none());
    }

    #[test]
    fn the_run_group_comes_from_the_environment_when_it_is_a_run_id() {
        use std::ffi::OsStr;
        assert_eq!(group_from_env(None).unwrap(), None);
        assert_eq!(group_from_env(Some(OsStr::new(""))).unwrap(), None);
        let id = RunId::new();
        assert_eq!(
            group_from_env(Some(OsStr::new(id.as_str()))).unwrap(),
            Some(id)
        );
        assert!(group_from_env(Some(OsStr::new("../escape"))).is_err());
    }
}
