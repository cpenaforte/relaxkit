//! Self-update tests (Task 22, rulings K91 to K100).
//!
//! The release signature is checked with a throwaway test key. Its public key, the fixture archive, the checksums file
//! listing the archive under each release target name, and the minisign signature over the checksums are checked in
//! under `tests/fixtures/selfupdate`. The private half was generated once for the fixture and discarded; it is not in
//! the repo. Nothing here reaches the network: `FakeNet` serves the release, and `Recorder` runs `tar` for real and
//! answers every other command with success.
#![cfg(unix)]

use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use relaxkit::ctx::{Ctx, Reporter};
use relaxkit::exec::{Cmd, Output, Runner, SystemRunner};
use relaxkit::net::FakeNet;
use relaxkit::platform::KitDirs;
use relaxkit::selfupdate::{self, EMBEDDED_KEY, RELEASE_REPO, SelfUpdate};
use relaxkit::state::{RunId, journal};
use tempfile::TempDir;

const VERSION: &str = "9.9.9";
const PAYLOAD: &str = "#!/bin/sh\necho relaxkit fixture 9.9.9\n";
const INSTALLED: &str = env!("CARGO_PKG_VERSION");

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/selfupdate")
        .join(name);
    fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The test public key, as the binary would read it from the embedded file.
fn test_key() -> String {
    String::from_utf8(fixture("test.minisign.pub")).unwrap()
}

fn base_url() -> String {
    format!("https://github.com/{RELEASE_REPO}/releases/download/v{VERSION}")
}

fn tag_url() -> String {
    format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest")
}

fn checksums_name() -> String {
    format!("relaxkit-{VERSION}-checksums.txt")
}

fn asset() -> String {
    selfupdate::asset_name(VERSION, selfupdate::target().unwrap())
}

/// A release as GitHub serves it: the tag, the checksums, their signature, and the archive under this target's name.
fn release(archive: Vec<u8>, checksums: Vec<u8>, signature: Vec<u8>) -> FakeNet {
    FakeNet::new()
        .with(&tag_url(), &format!(r#"{{"tag_name":"v{VERSION}"}}"#))
        .with_bytes(&format!("{}/{}", base_url(), checksums_name()), checksums)
        .with_bytes(
            &format!("{}/{}.minisig", base_url(), checksums_name()),
            signature,
        )
        .with_bytes(&format!("{}/{}", base_url(), asset()), archive)
}

/// The fixture release, unmodified.
fn good_release() -> FakeNet {
    release(
        fixture("archive.tar.gz"),
        fixture("checksums.txt"),
        fixture("checksums.txt.minisig"),
    )
}

type Call = (String, Vec<String>);

/// Runs `tar` for real, so the archive is unpacked as it is in production. Every other command is recorded and answered
/// with success. Clones share the record.
#[derive(Clone, Default)]
struct Recorder {
    calls: Arc<Mutex<Vec<Call>>>,
    /// The bytes of the program at the moment each command ran, so a test can see what was on disk then.
    seen: Arc<Mutex<Vec<Option<Vec<u8>>>>>,
    /// The exit code of `update --tools` (K102). Zero by default.
    rerun_exit: i32,
    /// Makes `update --tools` fail to start, which is a spawn error (K102).
    rerun_spawn_error: bool,
    /// The exit code of `brew upgrade relaxkit` (K103). Zero by default.
    brew_exit: i32,
    /// What `<bin> --version` reports, in order, for the brew-managed link (K103). `0.1.0` once the queue is empty.
    versions: Arc<Mutex<VecDeque<String>>>,
}

impl Recorder {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn program_bytes_at(&self, index: usize) -> Option<Vec<u8>> {
        self.seen.lock().unwrap()[index].clone()
    }
}

impl Runner for Recorder {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        self.calls
            .lock()
            .unwrap()
            .push((cmd.program().to_string(), cmd.argv().to_vec()));
        self.seen.lock().unwrap().push(fs::read(cmd.program()).ok());
        if cmd.program() == "tar" {
            return SystemRunner.run(cmd);
        }
        let words: Vec<&str> = cmd.argv().iter().map(String::as_str).collect();
        if cmd.program() == "brew" && self.brew_exit != 0 {
            return Ok(Output::fail(self.brew_exit, "brew refused"));
        }
        if words == ["--version"] {
            let next = self.versions.lock().unwrap().pop_front();
            return Ok(Output::ok(&format!(
                "relaxkit {}\n",
                next.unwrap_or_else(|| INSTALLED.to_string())
            )));
        }
        if words == ["update", "--tools"] {
            if self.rerun_spawn_error {
                anyhow::bail!("cannot run {}", cmd.program());
            }
            if self.rerun_exit != 0 {
                return Ok(Output::fail(self.rerun_exit, "a tool failed"));
            }
        }
        Ok(Output::ok(""))
    }

    fn which(&self, _program: &str) -> Option<PathBuf> {
        None
    }
}

fn ctx_for(root: &Path, runner: &Recorder, net: FakeNet) -> Ctx {
    Ctx {
        dirs: KitDirs::rooted(root),
        runner: Box::new(runner.clone()),
        net: Box::new(net),
        out: Reporter::capture(),
        dry_run: false,
        project: None,
        allow_sudo: false,
        run: RunId::new(),
    }
}

/// An installed binary at `dir/relaxkit` holding `text`, with mode 0755.
fn installed(dir: &Path, text: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let exe = dir.join("relaxkit");
    fs::write(&exe, text).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    exe
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn temp() -> TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn selects_the_asset_for_the_platform() {
    assert_eq!(
        selfupdate::triple_for("macos", "aarch64"),
        Some("aarch64-apple-darwin")
    );
    assert_eq!(
        selfupdate::triple_for("macos", "x86_64"),
        Some("x86_64-apple-darwin")
    );
    assert_eq!(
        selfupdate::triple_for("linux", "x86_64"),
        Some("x86_64-unknown-linux-musl")
    );
    assert_eq!(
        selfupdate::triple_for("linux", "aarch64"),
        Some("aarch64-unknown-linux-musl")
    );
    assert_eq!(selfupdate::triple_for("windows", "x86_64"), None);
    assert_eq!(selfupdate::triple_for("linux", "riscv64"), None);
    let target = selfupdate::target().unwrap();
    assert_eq!(
        selfupdate::asset_name("9.9.9", target),
        format!("relaxkit-9.9.9-{target}.tar.gz")
    );
}

#[test]
fn rejects_a_bad_checksum() {
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let mut archive = fixture("archive.tar.gz");
    archive.push(b'x');
    let runner = Recorder::default();
    let ctx = ctx_for(
        root.path(),
        &runner,
        release(
            archive,
            fixture("checksums.txt"),
            fixture("checksums.txt.minisig"),
        ),
    );

    let err = selfupdate::apply(&ctx, &exe, &test_key(), false)
        .expect_err("an archive with the wrong checksum was accepted");

    assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old binary\n");
    assert_eq!(mode(&exe), 0o755);
    assert!(!ctx.dirs.bin.join("relaxkit.prev").exists());
    assert!(!ctx.dirs.bin.join("relaxkit.new").exists());
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn rejects_a_bad_signature() {
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    // One byte added to the checksums file breaks its signature, so nothing after the check may run.
    let mut checksums = fixture("checksums.txt");
    checksums.push(b'\n');
    let runner = Recorder::default();
    let net = release(
        fixture("archive.tar.gz"),
        checksums,
        fixture("checksums.txt.minisig"),
    );
    let requests = net.clone();
    let ctx = ctx_for(root.path(), &runner, net);

    let err = selfupdate::apply(&ctx, &exe, &test_key(), false)
        .expect_err("a checksums file with a bad signature was accepted");

    assert!(format!("{err:#}").contains("does not verify"), "{err:#}");
    // The order (K94): the archive is not requested until the checksums file has verified.
    let archive_url = format!("{}/{}", base_url(), asset());
    assert!(
        !requests.requested().contains(&archive_url),
        "the archive was requested before the signature was checked: {:?}",
        requests.requested()
    );
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old binary\n");
    assert_eq!(mode(&exe), 0o755);
    assert!(!ctx.dirs.bin.join("relaxkit.prev").exists());
    assert!(!ctx.dirs.bin.join("relaxkit.new").exists());
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn swaps_the_binary_atomically_and_keeps_the_previous_one() {
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    let ctx = ctx_for(root.path(), &runner, good_release());

    let outcome = selfupdate::apply(&ctx, &exe, &test_key(), false).unwrap();

    assert_eq!(
        outcome,
        SelfUpdate::Updated {
            from: INSTALLED.to_string(),
            to: VERSION.to_string(),
        }
    );
    assert_eq!(fs::read_to_string(&exe).unwrap(), PAYLOAD);
    assert_eq!(mode(&exe), 0o755);
    let prev = ctx.dirs.bin.join("relaxkit.prev");
    assert_eq!(fs::read_to_string(&prev).unwrap(), "old binary\n");
    assert_eq!(mode(&prev), 0o755);
    assert!(
        !ctx.dirs.bin.join("relaxkit.new").exists(),
        "the staged file was left behind"
    );
    let journaled: Vec<PathBuf> = journal::read(&ctx.dirs)
        .unwrap()
        .into_iter()
        .map(|record| record.file)
        .collect();
    assert!(journaled.contains(&prev), "{journaled:?}");
}

#[test]
fn homebrew_owned_binary_delegates_to_brew_upgrade() {
    for path in [
        "/opt/homebrew/Cellar/relaxkit/0.1.0/bin/relaxkit",
        "/usr/local/Cellar/relaxkit/0.1.0/bin/relaxkit",
        "/home/linuxbrew/.linuxbrew/Cellar/relaxkit/0.1.0/bin/relaxkit",
    ] {
        assert!(selfupdate::is_homebrew_path(Path::new(path)), "{path}");
    }
    assert!(!selfupdate::is_homebrew_path(Path::new(
        "/usr/local/bin/relaxkit"
    )));
    let root = temp();
    let runner = Recorder::default();
    let ctx = ctx_for(root.path(), &runner, FakeNet::new());

    let outcome = selfupdate::apply(
        &ctx,
        Path::new("/opt/homebrew/Cellar/relaxkit/0.1.0/bin/relaxkit"),
        &test_key(),
        false,
    )
    .unwrap();

    assert_eq!(
        outcome,
        SelfUpdate::DelegatedToBrew {
            from: INSTALLED.to_string(),
            to: INSTALLED.to_string(),
        }
    );
    let calls = runner.calls();
    assert!(
        calls.contains(&(
            "brew".to_string(),
            vec!["upgrade".into(), "relaxkit".into()]
        )),
        "{calls:?}"
    );
}

/// The brew-managed link for the Cellar binary used below.
const BREW_LINK: &str = "/opt/homebrew/bin/relaxkit";
const CELLAR_EXE: &str = "/opt/homebrew/Cellar/relaxkit/0.1.0/bin/relaxkit";

/// A runner whose link reports `before`, then `after`, around the brew upgrade (K103).
fn brew_runner(before: &str, after: &str) -> Recorder {
    let runner = Recorder::default();
    runner
        .versions
        .lock()
        .unwrap()
        .extend([before.to_string(), after.to_string()]);
    runner
}

#[test]
fn brew_upgrade_is_followed_by_a_re_run_through_the_link_and_is_updated_when_the_version_moves() {
    // K103 (F22-3): after `brew upgrade relaxkit`, the new binary runs `update --tools` through the brew link. The row
    // says "updated" because the link now reports a different version.
    let root = temp();
    let runner = brew_runner(INSTALLED, VERSION);
    let ctx = ctx_for(root.path(), &runner, FakeNet::new());

    let outcome = selfupdate::apply(&ctx, Path::new(CELLAR_EXE), &test_key(), false).unwrap();

    assert_eq!(
        outcome,
        SelfUpdate::DelegatedToBrew {
            from: INSTALLED.to_string(),
            to: VERSION.to_string(),
        }
    );
    assert!(selfupdate::row(&outcome).contains("updated"), "{outcome:?}");
    let calls = runner.calls();
    assert!(
        calls.contains(&(
            BREW_LINK.to_string(),
            vec!["update".into(), "--tools".into()]
        )),
        "{calls:?}"
    );
    assert!(
        calls.iter().all(|(program, _)| program != CELLAR_EXE),
        "the Cellar path was run after the upgrade: {calls:?}"
    );
}

#[test]
fn brew_upgrade_to_the_same_version_is_current() {
    let root = temp();
    let runner = brew_runner(INSTALLED, INSTALLED);
    let ctx = ctx_for(root.path(), &runner, FakeNet::new());

    let outcome = selfupdate::apply(&ctx, Path::new(CELLAR_EXE), &test_key(), false).unwrap();

    let row = selfupdate::row(&outcome);
    assert!(row.contains("current") && !row.contains("updated"), "{row}");
}

#[test]
fn a_failing_brew_upgrade_is_named_and_runs_no_re_run() {
    let root = temp();
    let runner = Recorder {
        brew_exit: 1,
        ..brew_runner(INSTALLED, VERSION)
    };
    let ctx = ctx_for(root.path(), &runner, FakeNet::new());

    let err = selfupdate::apply(&ctx, Path::new(CELLAR_EXE), &test_key(), false)
        .expect_err("a failing brew upgrade was accepted");

    assert!(
        format!("{err:#}").contains("brew upgrade relaxkit exited 1"),
        "{err:#}"
    );
    assert!(
        runner
            .calls()
            .iter()
            .all(|(_, argv)| argv != &["update", "--tools"]),
        "{:?}",
        runner.calls()
    );
}

#[test]
fn self_only_on_brew_runs_no_re_run() {
    // K101 applies to brew too: `--self` upgrades through brew and runs no tool.
    let root = temp();
    let runner = brew_runner(INSTALLED, VERSION);
    let ctx = ctx_for(root.path(), &runner, FakeNet::new());

    selfupdate::apply_with(&ctx, Path::new(CELLAR_EXE), &test_key(), false, false).unwrap();

    assert!(
        runner
            .calls()
            .iter()
            .all(|(_, argv)| argv != &["update", "--tools"]),
        "{:?}",
        runner.calls()
    );
}

#[test]
fn reruns_the_new_binary_for_the_tool_phase() {
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    let ctx = ctx_for(root.path(), &runner, good_release());

    selfupdate::apply(&ctx, &exe, &test_key(), false).unwrap();

    let calls = runner.calls();
    let rerun = calls
        .iter()
        .position(|(program, argv)| {
            program == exe.to_str().unwrap() && argv == &["update", "--tools"]
        })
        .unwrap_or_else(|| panic!("no `<new relaxkit> update --tools` call: {calls:?}"));
    // The re-run starts after the swap, so the program it runs is the new binary.
    assert_eq!(
        runner.program_bytes_at(rerun).as_deref(),
        Some(PAYLOAD.as_bytes())
    );
}

#[test]
fn update_through_the_rlx_symlink_replaces_the_real_binary() {
    let root = temp();
    let dir = root.path().join("usr/bin");
    let real = installed(&dir, "old binary\n");
    let link = dir.join("rlx");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let runner = Recorder::default();
    let ctx = ctx_for(root.path(), &runner, good_release());

    let resolved = selfupdate::resolve_exe(&link).unwrap();
    assert_eq!(resolved, fs::canonicalize(&real).unwrap());
    let outcome = selfupdate::apply(&ctx, &resolved, &test_key(), false).unwrap();

    assert!(matches!(outcome, SelfUpdate::Updated { .. }), "{outcome:?}");
    assert!(
        link.is_symlink(),
        "the rlx link was replaced by a regular file"
    );
    assert_eq!(fs::read_link(&link).unwrap(), real);
    assert_eq!(fs::read_to_string(&real).unwrap(), PAYLOAD);
    assert_eq!(mode(&real), 0o755);
}

#[test]
fn current_version_means_no_download() {
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    // Only the tag is served. Any download would be an unexpected URL and fail the run.
    let net = FakeNet::new().with(&tag_url(), &format!(r#"{{"tag_name":"v{INSTALLED}"}}"#));
    let ctx = ctx_for(root.path(), &runner, net);

    assert_eq!(
        selfupdate::apply(&ctx, &exe, &test_key(), false).unwrap(),
        SelfUpdate::Current {
            version: INSTALLED.to_string()
        }
    );
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old binary\n");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn the_placeholder_key_is_not_configured_and_nothing_is_fetched() {
    assert!(
        EMBEDDED_KEY.contains(selfupdate::PLACEHOLDER_MARKER),
        "the embedded key is no longer the placeholder"
    );
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    // Nothing is served: any network call would fail the run.
    let ctx = ctx_for(root.path(), &runner, FakeNet::new());

    assert_eq!(
        selfupdate::apply(&ctx, &exe, EMBEDDED_KEY, false).unwrap(),
        SelfUpdate::NotConfigured
    );
    assert_eq!(
        selfupdate::run(&ctx, false).unwrap(),
        SelfUpdate::NotConfigured
    );
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old binary\n");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn check_reports_the_newest_without_downloading() {
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    let net = FakeNet::new().with(&tag_url(), &format!(r#"{{"tag_name":"v{VERSION}"}}"#));
    let ctx = ctx_for(root.path(), &runner, net);

    assert_eq!(
        selfupdate::apply(&ctx, &exe, &test_key(), true).unwrap(),
        SelfUpdate::Available {
            version: VERSION.to_string()
        }
    );
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old binary\n");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn check_without_a_network_reports_newest_unknown_and_a_real_update_fails() {
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    let ctx = ctx_for(root.path(), &runner, FakeNet::new());

    let outcome = selfupdate::apply(&ctx, &exe, &test_key(), true).unwrap();
    assert!(
        matches!(outcome, SelfUpdate::NewestUnknown { .. }),
        "{outcome:?}"
    );
    assert!(
        selfupdate::apply(&ctx, &exe, &test_key(), false).is_err(),
        "a real update without the release lookup did not fail"
    );
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old binary\n");
}

#[test]
fn verify_accepts_the_signed_checksums_and_rejects_a_changed_file() {
    let checksums = fixture("checksums.txt");
    let signature = String::from_utf8(fixture("checksums.txt.minisig")).unwrap();

    selfupdate::verify(&checksums, &signature, &test_key()).unwrap();

    let mut changed = checksums.clone();
    changed.push(b'\n');
    assert!(selfupdate::verify(&changed, &signature, &test_key()).is_err());
}

#[test]
fn a_link_where_the_previous_binary_goes_is_refused_and_never_written_through() {
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    let ctx = ctx_for(root.path(), &runner, good_release());
    // dirs.bin/relaxkit.prev is a link to a file elsewhere. A write through it would change that file.
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let target = elsewhere.join("victim");
    fs::write(&target, "not relaxkit's\n").unwrap();
    fs::create_dir_all(&ctx.dirs.bin).unwrap();
    std::os::unix::fs::symlink(&target, ctx.dirs.bin.join("relaxkit.prev")).unwrap();

    let err = selfupdate::apply(&ctx, &exe, &test_key(), false)
        .expect_err("a link at the previous-binary path was written through");

    assert!(format!("{err:#}").contains("is a link"), "{err:#}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "not relaxkit's\n");
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old binary\n");
    // Only the unpack runs before the swap; the new binary is never run.
    assert!(
        runner.calls().iter().all(|(program, _)| program == "tar"),
        "{:?}",
        runner.calls()
    );
}

#[test]
fn a_link_at_the_temp_name_of_the_previous_binary_is_refused_too() {
    // write_atomic writes `<name>.relaxkit-tmp` beside the target, and creating a file follows a link there.
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    let ctx = ctx_for(root.path(), &runner, good_release());
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let target = elsewhere.join("victim");
    fs::write(&target, "not relaxkit's\n").unwrap();
    fs::create_dir_all(&ctx.dirs.bin).unwrap();
    std::os::unix::fs::symlink(&target, ctx.dirs.bin.join("relaxkit.prev.relaxkit-tmp")).unwrap();

    let result = selfupdate::apply(&ctx, &exe, &test_key(), false);

    assert!(result.is_err(), "a link at the temp name was accepted");
    assert_eq!(fs::read_to_string(&target).unwrap(), "not relaxkit's\n");
    assert_eq!(fs::read_to_string(&exe).unwrap(), "old binary\n");
}

#[test]
fn an_equal_version_names_the_newest_in_its_row_and_downloads_nothing() {
    // K98 (F22-4): the current row names the newest version it compared against, and only the tag is requested.
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    let net = FakeNet::new().with(&tag_url(), &format!(r#"{{"tag_name":"v{INSTALLED}"}}"#));
    let requests = net.clone();
    let ctx = ctx_for(root.path(), &runner, net);

    let outcome = selfupdate::apply(&ctx, &exe, &test_key(), true).unwrap();

    let row = selfupdate::row(&outcome);
    assert!(row.contains(&format!("-> {INSTALLED}")), "{row}");
    assert_eq!(requests.requested(), vec![tag_url()]);
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn self_only_swaps_but_runs_no_tool_phase() {
    // K101 (F22-2): `update --self` updates relaxkit itself only. After a successful swap, the new binary is not run
    // and no tool is touched.
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder::default();
    let ctx = ctx_for(root.path(), &runner, good_release());

    let outcome = selfupdate::apply_with(&ctx, &exe, &test_key(), false, false).unwrap();

    assert_eq!(
        outcome,
        SelfUpdate::Updated {
            from: INSTALLED.to_string(),
            to: VERSION.to_string(),
        }
    );
    assert_eq!(fs::read_to_string(&exe).unwrap(), PAYLOAD);
    let calls = runner.calls();
    assert!(
        calls.iter().all(|(program, _)| program == "tar"),
        "--self ran a command other than the unpack: {calls:?}"
    );
}

#[test]
fn a_child_that_exits_non_zero_is_a_result_and_the_tool_phase_is_not_repeated() {
    // K102 (F22-1): the new binary ran its tool phase and a tool failed. That is a result, named, with exactly one
    // `update --tools` call and no second tool pass from this process.
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder {
        rerun_exit: 1,
        ..Recorder::default()
    };
    let ctx = ctx_for(root.path(), &runner, good_release());

    let outcome = selfupdate::apply(&ctx, &exe, &test_key(), false).unwrap();

    match outcome {
        SelfUpdate::UpdatedToolsFailed { from, to, reason } => {
            assert_eq!(from, INSTALLED);
            assert_eq!(to, VERSION);
            assert!(reason.contains("a tool failed"), "{reason}");
        }
        other => panic!("expected UpdatedToolsFailed, got {other:?}"),
    }
    let reruns = runner
        .calls()
        .iter()
        .filter(|(_, argv)| argv == &["update", "--tools"])
        .count();
    assert_eq!(reruns, 1, "{:?}", runner.calls());
}

#[test]
fn a_child_that_cannot_start_is_an_error_so_the_tool_phase_runs_here() {
    // K102 (F22-1): only a spawn error is an Err, which lets this process run the tool phase (K92).
    let root = temp();
    let exe = installed(&root.path().join("usr/bin"), "old binary\n");
    let runner = Recorder {
        rerun_spawn_error: true,
        ..Recorder::default()
    };
    let ctx = ctx_for(root.path(), &runner, good_release());

    assert!(selfupdate::apply(&ctx, &exe, &test_key(), false).is_err());
}
