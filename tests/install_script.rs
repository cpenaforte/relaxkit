//! Install script tests (Task 26).
//!
//! Each test lays out a fake release under a temporary directory, laid out as GitHub serves it under `download/`:
//! `v<version>/` holds the archive for this host, the checksums file, and nothing else. The archive holds a stub
//! `relaxkit` that appends its argv to a log and exits zero. `RELAXKIT_RELEASE_BASE` is a `file://` URL, so nothing
//! reaches the network, and `RELAXKIT_BIN_DIR` is a temporary directory, so nothing reaches the real home. The
//! script runs under `sh`. No test runs the real relaxkit or the real `relaxkit setup`.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const VERSION: &str = "9.9.9";

const TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
];

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh")
}

/// The release target this test host installs, as the script maps `uname` to it.
fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        other => panic!("no relaxkit release for {other:?}"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake release: the archive for this host, and a checksums file listing that archive under all four targets.
struct Release {
    root: TempDir,
}

impl Release {
    fn new() -> Release {
        let root = TempDir::new().unwrap();
        let staging = root.path().join("staging");
        fs::create_dir_all(&staging).unwrap();
        let log = root.path().join("calls.log");
        let stub = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display());
        write_executable(&staging.join("relaxkit"), &stub);

        let dir = root.path().join("download").join(format!("v{VERSION}"));
        fs::create_dir_all(&dir).unwrap();
        let archive = dir.join(asset_name(host_target()));
        let status = Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .arg("relaxkit")
            .status()
            .unwrap();
        assert!(status.success(), "cannot build the fake archive");

        let hash = sha256_hex(&fs::read(&archive).unwrap());
        let sums: String = TARGETS
            .iter()
            .map(|target| format!("{hash}  {}\n", asset_name(target)))
            .collect();
        fs::write(dir.join(checksums_name()), sums).unwrap();
        Release { root }
    }

    fn dir(&self) -> PathBuf {
        self.root
            .path()
            .join("download")
            .join(format!("v{VERSION}"))
    }

    fn archive(&self) -> PathBuf {
        self.dir().join(asset_name(host_target()))
    }

    fn log(&self) -> PathBuf {
        self.root.path().join("calls.log")
    }

    /// The stub relaxkit's recorded calls, or `None` when it never ran.
    fn calls(&self) -> Option<String> {
        fs::read_to_string(self.log()).ok()
    }

    fn run(&self, bin: &Path, flags: &[&str]) -> Output {
        self.run_with("sh", bin, flags)
    }

    /// `run` under `shell`, with no controlling terminal, as a CI runner has none: `/dev/tty` cannot open there.
    fn run_with(&self, shell: &str, bin: &Path, flags: &[&str]) -> Output {
        let mut command = Command::new(shell);
        command
            .arg(script())
            .args(flags)
            .env("RELAXKIT_VERSION", VERSION)
            .env(
                "RELAXKIT_RELEASE_BASE",
                format!("file://{}/download", self.root.path().display()),
            )
            .env("RELAXKIT_BIN_DIR", bin)
            .stdin(Stdio::null());
        // SAFETY: the closure runs in the forked child before exec and only calls setsid.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.output().unwrap()
    }
}

fn asset_name(target: &str) -> String {
    format!("relaxkit-{VERSION}-{target}.tar.gz")
}

fn checksums_name() -> String {
    format!("relaxkit-{VERSION}-checksums.txt")
}

/// The names in a directory, or none when it does not exist.
fn entries(dir: &Path) -> Vec<String> {
    match fs::read_dir(dir) {
        Ok(read) => read
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn installs_the_binary_for_the_platform_and_verifies_the_checksum() {
    let release = Release::new();
    let bin = release.root.path().join("bin");

    let out = release.run(&bin, &[]);

    assert!(
        out.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let installed = bin.join("relaxkit");
    let mode = fs::metadata(&installed).unwrap().permissions().mode();
    assert!(mode & 0o111 != 0, "relaxkit is not executable");
    let body = fs::read_to_string(&installed).unwrap();
    assert!(
        body.starts_with("#!/bin/sh"),
        "the installed file is not the release binary"
    );
    assert!(
        release
            .calls()
            .as_deref()
            .is_some_and(|calls| calls.contains("setup")),
        "setup did not run by default"
    );
}

#[test]
fn installs_under_dash_without_a_terminal() {
    // Ubuntu's `sh` is dash. A failed redirect on the `:` builtin makes dash exit silently, which is what the terminal
    // probe did on a CI runner: /dev/tty cannot open there, and the install stopped with no message.
    let release = Release::new();
    let bin = release.root.path().join("bin");

    let out = release.run_with("/bin/dash", &bin, &[]);

    assert!(
        out.status.success(),
        "install failed under dash (status {:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(release.calls().is_some(), "setup did not run under dash");
}

#[test]
fn tampered_archive_fails_and_installs_nothing() {
    let release = Release::new();
    let bin = release.root.path().join("bin");
    fs::write(release.archive(), b"not the archive the checksums list").unwrap();

    let out = release.run(&bin, &[]);

    assert!(!out.status.success(), "a tampered archive was installed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("checksum"),
        "the failure does not name the checksum: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        entries(&bin).is_empty(),
        "files were left in the bin directory: {:?}",
        entries(&bin)
    );
    assert!(release.calls().is_none(), "setup ran after a failed check");
}

#[test]
fn runs_setup_unless_no_setup_is_given() {
    let release = Release::new();

    let skipped = release.root.path().join("bin-skipped");
    let out = release.run(&skipped, &["--no-setup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(release.calls().is_none(), "--no-setup still ran setup");

    let ran = release.root.path().join("bin-ran");
    let out = release.run(&ran, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = release
        .calls()
        .expect("setup did not run without --no-setup");
    assert!(
        calls.lines().any(|line| line == "setup"),
        "unexpected calls: {calls}"
    );
}

#[test]
fn creates_the_rlx_alias_without_clobbering_another_rlx() {
    let release = Release::new();

    // No rlx yet: a symlink to the relative name relaxkit is created.
    let fresh = release.root.path().join("bin-fresh");
    let out = release.run(&fresh, &["--no-setup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let link = fresh.join("rlx");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("relaxkit"));

    // A regular file named rlx is left alone, and a note is printed.
    let file_bin = release.root.path().join("bin-file");
    fs::create_dir_all(&file_bin).unwrap();
    fs::write(file_bin.join("rlx"), "another tool").unwrap();
    let out = release.run(&file_bin, &["--no-setup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(file_bin.join("rlx")).unwrap(),
        "another tool"
    );
    assert!(
        !fs::symlink_metadata(file_bin.join("rlx"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("rlx"),
        "no note about rlx"
    );

    // A symlink rlx that points elsewhere is left alone, and a note is printed.
    let link_bin = release.root.path().join("bin-link");
    fs::create_dir_all(&link_bin).unwrap();
    let elsewhere = release.root.path().join("other-tool");
    fs::write(&elsewhere, "another tool").unwrap();
    std::os::unix::fs::symlink(&elsewhere, link_bin.join("rlx")).unwrap();
    let out = release.run(&link_bin, &["--no-setup"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_link(link_bin.join("rlx")).unwrap(), elsewhere);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("rlx"),
        "no note about rlx"
    );
}

#[test]
fn shellcheck_passes() {
    let present = Command::new("shellcheck").arg("--version").output().is_ok();
    if !present {
        eprintln!("skipped: shellcheck is not installed");
        return;
    }
    let out = Command::new("shellcheck")
        .args(["-s", "sh"])
        .arg(script())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "shellcheck: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
