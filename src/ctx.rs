//! The context every command receives: directories, runner, network, output, and flags.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::exec::{FakeRunner, Runner, SystemRunner};
use crate::net::{FakeNet, HttpNet, Net};
use crate::platform::KitDirs;

/// Where user-facing lines go. `stdout()` prints; `capture()` keeps them for tests.
#[derive(Clone, Default)]
pub struct Reporter {
    capture: Option<Arc<Mutex<Vec<String>>>>,
}

impl Reporter {
    pub fn stdout() -> Reporter {
        Reporter { capture: None }
    }

    pub fn capture() -> Reporter {
        Reporter {
            capture: Some(Arc::new(Mutex::new(Vec::new()))),
        }
    }

    pub fn info(&self, msg: &str) {
        self.emit(msg, "");
    }

    pub fn warn(&self, msg: &str) {
        self.emit(msg, "warning: ");
    }

    pub fn step(&self, msg: &str) {
        self.emit(msg, "==> ");
    }

    /// Messages recorded by a `capture()` reporter, without any prefix. Empty for `stdout()`.
    pub fn lines(&self) -> Vec<String> {
        match &self.capture {
            Some(lines) => lines.lock().expect("reporter lock").clone(),
            None => Vec::new(),
        }
    }

    fn emit(&self, msg: &str, prefix: &str) {
        match &self.capture {
            Some(lines) => lines.lock().expect("reporter lock").push(msg.to_string()),
            None if prefix == "warning: " => {
                write_line(&mut std::io::stderr().lock(), &format!("{prefix}{msg}"));
            }
            None => write_line(&mut std::io::stdout().lock(), &format!("{prefix}{msg}")),
        }
    }
}

/// Writes one line. A closed pipe or a full disk must not panic the process, so the error is ignored.
fn write_line(out: &mut dyn Write, line: &str) {
    let _ = writeln!(out, "{line}");
}

/// Everything a command may touch. Tests build one with `Ctx::for_test`.
pub struct Ctx {
    pub dirs: KitDirs,
    pub runner: Box<dyn Runner>,
    pub net: Box<dyn Net>,
    pub out: Reporter,
    pub dry_run: bool,
    pub project: Option<PathBuf>,
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
        }
    }

    /// A context for the real machine: discovered directories, real processes and HTTP.
    pub fn system(dry_run: bool, project: Option<PathBuf>) -> anyhow::Result<Ctx> {
        Ok(Ctx {
            dirs: KitDirs::discover()?,
            runner: Box::new(SystemRunner),
            net: Box::new(HttpNet::new()?),
            out: Reporter::stdout(),
            dry_run,
            project,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, ErrorKind};

    use super::*;

    /// A writer whose far end has closed, as after `relaxkit ... | head`.
    struct ClosedPipe;

    impl io::Write for ClosedPipe {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn write_line_ignores_a_closed_pipe() {
        write_line(&mut ClosedPipe, "hello");
    }

    #[test]
    fn capture_reporter_keeps_plain_messages() {
        let ctx = Ctx::for_test(Path::new("/h"), FakeRunner::new(), FakeNet::new());
        ctx.out.info("hello");
        ctx.out.warn("careful");
        ctx.out.step("doing");
        assert_eq!(ctx.out.lines(), vec!["hello", "careful", "doing"]);
    }

    #[test]
    fn for_test_roots_directories() {
        let ctx = Ctx::for_test(Path::new("/h"), FakeRunner::new(), FakeNet::new());
        assert_eq!(ctx.dirs.config, PathBuf::from("/h/.config/relaxkit"));
        assert!(!ctx.dry_run);
        assert!(ctx.project.is_none());
    }
}
