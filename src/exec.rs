//! Command execution behind a trait, so tests can script every external call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};

use crate::platform;

/// A command to run: program, arguments, extra environment, and working directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cmd {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
}

impl Cmd {
    pub fn new(program: &str) -> Cmd {
        Cmd {
            program: program.to_string(),
            ..Cmd::default()
        }
    }

    pub fn arg(mut self, arg: &str) -> Cmd {
        self.args.push(arg.to_string());
        self
    }

    pub fn args(mut self, args: &[&str]) -> Cmd {
        self.args.extend(args.iter().map(|a| a.to_string()));
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Cmd {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    pub fn cwd(mut self, dir: &Path) -> Cmd {
        self.cwd = Some(dir.to_path_buf());
        self
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn argv(&self) -> &[String] {
        &self.args
    }

    pub fn environment(&self) -> &[(String, String)] {
        &self.env
    }

    pub fn working_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }
}

/// Shell-quotes one word for display. Plain words pass through unchanged.
fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

impl std::fmt::Display for Cmd {
    /// Program and arguments, shell-joined. Environment and working directory are not shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut words = vec![shell_quote(&self.program)];
        words.extend(self.args.iter().map(|a| shell_quote(a)));
        write!(f, "{}", words.join(" "))
    }
}

/// The result of a finished command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// A successful run that printed `stdout`.
    pub fn ok(stdout: &str) -> Output {
        Output {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    /// A failed run with exit `code` and `stderr`.
    pub fn fail(code: i32, stderr: &str) -> Output {
        Output {
            code,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }
}

pub trait Runner {
    /// Runs a command and returns its output. An exit code other than zero is still `Ok`.
    fn run(&self, cmd: &Cmd) -> Result<Output>;
    /// Finds an executable on the PATH.
    fn which(&self, program: &str) -> Option<PathBuf>;
}

/// Runs real processes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let mut process = std::process::Command::new(cmd.program());
        process.args(cmd.argv());
        for (key, value) in cmd.environment() {
            process.env(key, value);
        }
        if let Some(dir) = cmd.working_dir() {
            process.current_dir(dir);
        }
        let finished = process
            .output()
            .with_context(|| format!("cannot run `{cmd}`"))?;
        Ok(Output {
            code: finished.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&finished.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&finished.stderr).into_owned(),
        })
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        if program.contains('/') {
            let path = PathBuf::from(program);
            return platform::is_executable(&path).then_some(path);
        }
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(program))
            .find(|candidate| platform::is_executable(candidate))
    }
}

#[derive(Default)]
struct FakeState {
    replies: Vec<(String, Output)>,
    programs: BTreeMap<String, PathBuf>,
    calls: Vec<String>,
}

/// A scripted runner. Clones share state, so a test can keep one clone to read `calls()`.
#[derive(Clone, Default)]
pub struct FakeRunner {
    state: Arc<Mutex<FakeState>>,
}

impl FakeRunner {
    pub fn new() -> FakeRunner {
        FakeRunner::default()
    }

    /// Answers any command whose display string starts with `prefix`. The first match wins.
    pub fn reply(self, prefix: &str, output: Output) -> FakeRunner {
        self.state
            .lock()
            .expect("fake runner lock")
            .replies
            .push((prefix.to_string(), output));
        self
    }

    /// Makes `which(name)` return `path`.
    pub fn with_program(self, name: &str, path: &Path) -> FakeRunner {
        self.state
            .lock()
            .expect("fake runner lock")
            .programs
            .insert(name.to_string(), path.to_path_buf());
        self
    }

    /// Display strings of every answered command, in order.
    pub fn calls(&self) -> Vec<String> {
        self.state.lock().expect("fake runner lock").calls.clone()
    }
}

impl Runner for FakeRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let display = cmd.to_string();
        let mut state = self.state.lock().expect("fake runner lock");
        let reply = state
            .replies
            .iter()
            .find(|(prefix, _)| display.starts_with(prefix.as_str()))
            .map(|(_, output)| output.clone());
        match reply {
            Some(output) => {
                state.calls.push(display);
                Ok(output)
            }
            None => Err(anyhow!("unexpected command in test: {display}")),
        }
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.state
            .lock()
            .expect("fake runner lock")
            .programs
            .get(program)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_runner_records_and_replies() {
        let fake = FakeRunner::new().reply("claude plugin list", Output::ok("a\n"));
        let out = fake
            .run(&Cmd::new("claude").args(&["plugin", "list"]))
            .unwrap();
        assert_eq!(out.stdout, "a\n");
        assert_eq!(fake.calls(), vec!["claude plugin list".to_string()]);
        assert!(fake.run(&Cmd::new("nope")).is_err());
    }

    #[test]
    fn fake_runner_clones_share_calls() {
        let fake = FakeRunner::new().reply("echo", Output::ok(""));
        let keeper = fake.clone();
        fake.run(&Cmd::new("echo").arg("hi")).unwrap();
        assert_eq!(keeper.calls(), vec!["echo hi".to_string()]);
    }

    #[test]
    fn cmd_display_quotes_only_when_needed() {
        let cmd = Cmd::new("sh").args(&["-c", "echo hi there"]);
        assert_eq!(cmd.to_string(), "sh -c 'echo hi there'");
        assert_eq!(Cmd::new("a").arg("it's").to_string(), "a 'it'\\''s'");
    }

    #[test]
    fn fake_runner_resolves_programs() {
        let fake = FakeRunner::new().with_program("claude", Path::new("/usr/bin/claude"));
        assert_eq!(fake.which("claude"), Some(PathBuf::from("/usr/bin/claude")));
        assert_eq!(fake.which("missing"), None);
    }

    #[test]
    fn system_runner_captures_output() {
        let out = SystemRunner
            .run(&Cmd::new("sh").args(&["-c", "printf hi; printf err >&2; exit 3"]))
            .unwrap();
        assert_eq!(out.code, 3);
        assert_eq!(out.stdout, "hi");
        assert_eq!(out.stderr, "err");
    }

    #[test]
    fn system_runner_finds_sh_on_path() {
        assert!(SystemRunner.which("sh").is_some());
        assert!(
            SystemRunner
                .which("definitely-not-a-real-program-xyz")
                .is_none()
        );
    }
}
