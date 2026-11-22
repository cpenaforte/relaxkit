//! Test support for the binary-level tests (ruling K54). Each `Scratch` is a temporary HOME with stub host
//! commands and a fake nvm, all on a PATH that holds nothing else of the owner's. The relaxkit process gets a
//! cleared environment, stdin from null, and proxy variables that point at a closed local port, so no test
//! reaches the real network, the real home, or a real `sudo`: `sudo` is a stub too.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tempfile::TempDir;

/// Host commands relaxkit may call. Each is a stub that logs its argv. `bwrap` and `socat` are the Linux sandbox packages:
/// on the PATH they count as installed, so no test depends on what the machine has.
pub const STUBS: [&str; 13] = [
    "claude", "opencode", "gh", "brew", "uv", "rtk", "npm", "npx", "serena", "caveman", "sudo",
    "bwrap", "socat",
];

/// The Chromium executable inside a `chromium-<revision>` directory, as Playwright lays it out on each OS.
#[cfg(target_os = "macos")]
const CHROMIUM_BINARY: &str =
    "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing";
#[cfg(not(target_os = "macos"))]
const CHROMIUM_BINARY: &str = "chrome-linux64/chrome";

/// The Node version the fake nvm reports.
pub const NODE_VERSION: &str = "v24.13.0";

/// A proxy that refuses every connection at once, so a request that escapes the stubs fails fast.
const CLOSED_PROXY: &str = "http://127.0.0.1:9";

const STUB: &str = r#"#!/bin/sh
# Stub for a host command in relaxkit's tests. It logs its argv, answers the few queries relaxkit makes,
# and fails on a command line that contains RELAXKIT_STUB_FAIL. It writes nothing outside the scratch area.
name="${0##*/}"
line="$name $*"
echo "$line" >> "$RELAXKIT_STUB_LOG"
if [ -n "$RELAXKIT_STUB_FAIL" ]; then
  case "$line" in
    *"$RELAXKIT_STUB_FAIL"*) echo "stub refused: $line" >&2; exit 1 ;;
  esac
fi
# RELAXKIT_STUB_FAIL_ONCE fails the first call that matches, then the fail-once marker is spent (shell builtins only).
# A fail-skip file holds how many matching calls pass before that one (shell builtins only).
if [ -n "$RELAXKIT_STUB_FAIL_ONCE" ]; then
  case "$line" in
    *"$RELAXKIT_STUB_FAIL_ONCE"*)
      skipping=0
      if [ -f "$RELAXKIT_STUB_STATE/fail-skip" ]; then
        read -r left < "$RELAXKIT_STUB_STATE/fail-skip"
        if [ "$left" -gt 0 ]; then echo "$((left - 1))" > "$RELAXKIT_STUB_STATE/fail-skip"; skipping=1; fi
      fi
      if [ "$skipping" = 0 ] && read -r armed < "$RELAXKIT_STUB_STATE/fail-once" 2>/dev/null && [ "$armed" = armed ]; then
        echo "spent" > "$RELAXKIT_STUB_STATE/fail-once"
        echo "stub refused once: $line" >&2
        exit 1
      fi ;;
  esac
fi
# `claude mcp add` and `claude mcp remove` keep the user-scope entries in ~/.claude.json, as Claude Code does.
mcp_names="$RELAXKIT_STUB_STATE/mcp-names"
mcp_forget() {
  : >> "$mcp_names"
  tmp="$mcp_names.tmp"; : > "$tmp"
  while IFS= read -r n; do [ "$n" = "$1" ] || echo "$n" >> "$tmp"; done < "$mcp_names"
  mv "$tmp" "$mcp_names"
}
mcp_write() {
  body=""; sep=""
  while IFS= read -r n; do body="$body$sep\"$n\": {}"; sep=", "; done < "$mcp_names"
  printf '{"mcpServers": {%s}}\n' "$body" > "$HOME/.claude.json"
}
case "$line" in
  *"@anthropic-ai/sandbox-runtime"*) printf 'status=1\n403\n'; exit 0 ;;
  "claude --version"|"claude -v") echo "2.1.330 (Claude Code)" ;;
  "opencode --version") echo "2.2.6" ;;
  "rtk --version") echo "rtk 1.2.1" ;;
  "uv --version") echo "uv 0.13.9" ;;
  "gh --version") echo "gh version 2.80.0 (2026-10-01)" ;;
  "claude plugin list")
    if [ -f "$RELAXKIT_STUB_STATE/plugins" ]; then cat "$RELAXKIT_STUB_STATE/plugins"; fi ;;
  "claude plugin install "*)
    mkdir -p "$RELAXKIT_STUB_STATE"
    echo "${line#claude plugin install }  Version: 1.0.0" >> "$RELAXKIT_STUB_STATE/plugins" ;;
  "claude plugin marketplace list") echo "❯ anthropic-plugin-directory  (fixture)" ;;
  "claude mcp add "*) mcp_forget "$5"; echo "$5" >> "$mcp_names"; mcp_write ;;
  "claude mcp remove "*) mcp_forget "$5"; mcp_write ;;
  "gh auth status") exit "${RELAXKIT_STUB_GH_AUTH:-0}" ;;
  # `opencode models --standalone` lists the ids in $RELAXKIT_STUB_STATE/opencode-models (one `id [window]` per line).
  # With no such file the listing cannot be read, and the stub fails, as an unreadable list does.
  "opencode models --standalone")
    if [ -f "$RELAXKIT_STUB_STATE/opencode-models" ]; then cut -d' ' -f1 "$RELAXKIT_STUB_STATE/opencode-models"; else exit 1; fi ;;
  "opencode models --verbose")
    if [ -f "$RELAXKIT_STUB_STATE/opencode-models" ]; then cat "$RELAXKIT_STUB_STATE/opencode-models"; else exit 1; fi ;;
esac
exit 0
"#;

const NVM_SH: &str = r#"# Stub nvm: `nvm install 24` reports the Node 24 that the scratch home already holds.
nvm() {
  if [ "$1" = "install" ] && [ "$2" = "24" ]; then
    echo "nvm install 24" >> "$RELAXKIT_STUB_LOG"
    echo "Now using node v24.13.0 (npm v11.6.0)"
    return 0
  fi
  return 1
}
"#;

/// A temporary HOME, its stub bin directory, and the call log.
pub struct Scratch {
    _dir: TempDir,
    home: PathBuf,
    bin: PathBuf,
    state: PathBuf,
    log: PathBuf,
    extra_env: BTreeMap<String, String>,
}

impl Scratch {
    /// A scratch with a Claude Code stub only. Setup with an OpenCode stub present reaches GitHub for caveman's
    /// OpenCode files (ruling K54 forbids that here), so the success-path tests use this fixture.
    pub fn claude_only() -> Scratch {
        let scratch = Scratch::new();
        fs::remove_file(scratch.bin.join("opencode")).expect("remove the opencode stub");
        scratch
    }

    /// A scratch with every stub on the PATH and a fake Node 24 under `.nvm`.
    pub fn new() -> Scratch {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        let bin = root.join("bin");
        let state = root.join("state");
        let log = root.join("calls.log");
        for path in [&home, &bin, &state] {
            fs::create_dir_all(path).expect("scratch dir");
        }
        fs::write(&log, "").expect("call log");
        // The AppArmor profile that Ubuntu 24.04 needs for the sandbox, stood in by a file so no test reads /etc.
        fs::write(root.join("relaxkit-bwrap"), "").expect("profile stand-in");
        for name in STUBS {
            write_executable(&bin.join(name), STUB);
        }
        let nvm = home.join(".nvm");
        let node_bin = nvm
            .join("versions")
            .join("node")
            .join(NODE_VERSION)
            .join("bin");
        fs::create_dir_all(&node_bin).expect("node bin");
        for name in ["node", "npm", "npx"] {
            write_executable(&node_bin.join(name), STUB);
        }
        fs::write(nvm.join("nvm.sh"), NVM_SH).expect("nvm.sh");
        // uv installs serena into ~/.local/bin, which is on the MCP PATH, so the locked serena entry resolves.
        let local_bin = home.join(".local/bin");
        fs::create_dir_all(&local_bin).expect("local bin");
        write_executable(&local_bin.join("serena"), STUB);
        // A Chromium build already in Playwright's cache, so the `{chromium}` path resolves (a stub npx installs nothing).
        let cache = if cfg!(target_os = "macos") {
            home.join("Library/Caches/ms-playwright")
        } else {
            home.join(".cache/ms-playwright")
        };
        let browser = cache.join("chromium-1200").join(CHROMIUM_BINARY);
        fs::create_dir_all(browser.parent().unwrap()).expect("chromium dir");
        write_executable(&browser, "#!/bin/sh\nexit 0\n");
        // The Claude marketplace that lists the two plugins the pack resolves from disk (spec 5.1).
        let catalog =
            home.join(".claude/plugins/marketplaces/anthropic-plugin-directory/.claude-plugin");
        fs::create_dir_all(&catalog).expect("marketplace catalog");
        fs::write(
            catalog.join("marketplace.json"),
            r#"{"plugins":[{"name":"claude-md-management"},{"name":"frontend-design"}]}"#,
        )
        .expect("marketplace.json");
        Scratch {
            _dir: dir,
            home,
            bin,
            state,
            log,
            extra_env: BTreeMap::new(),
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The stub state directory: the files the stubs read (for example `opencode-models`).
    pub fn state(&self) -> &Path {
        &self.state
    }

    /// Every call the stubs logged, oldest first, as `program args`.
    pub fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .expect("call log")
            .lines()
            .map(String::from)
            .collect()
    }

    /// Forgets the logged calls, so a later run can be checked on its own.
    pub fn clear_calls(&self) {
        fs::write(&self.log, "").expect("clear call log");
    }

    /// Removes a stub from the PATH, so the host it belongs to looks absent.
    pub fn without(&self, name: &str) {
        fs::remove_file(self.bin.join(name)).expect("remove stub");
    }

    /// Makes every stub fail on a command line containing `needle`.
    pub fn fail_calls_matching(&mut self, needle: &str) {
        self.extra_env
            .insert("RELAXKIT_STUB_FAIL".into(), needle.into());
    }

    /// Makes the first call whose command line contains `needle` fail; every later call succeeds.
    pub fn fail_once(&mut self, needle: &str) {
        self.fail_once_after(needle, 0);
    }

    /// Like `fail_once`, but the first `passes` calls that contain `needle` succeed before the failing one.
    pub fn fail_once_after(&mut self, needle: &str, passes: u32) {
        fs::write(self.state.join("fail-once"), "armed\n").expect("arm fail-once");
        fs::write(self.state.join("fail-skip"), format!("{passes}\n")).expect("arm fail-skip");
        self.extra_env
            .insert("RELAXKIT_STUB_FAIL_ONCE".into(), needle.into());
    }

    /// Makes `gh auth status` exit with `code` (0 is logged in).
    pub fn gh_auth_status(&mut self, code: i32) {
        self.extra_env
            .insert("RELAXKIT_STUB_GH_AUTH".into(), code.to_string());
    }

    /// The PATH the relaxkit process sees: the stubs, then the system directories for `sh`, `basename` and `cat`.
    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.bin.display())
    }

    /// The relaxkit binary with a cleared environment, pointed at this scratch. Stdin is null, so a prompt would
    /// see end of file rather than a terminal.
    pub fn relaxkit(&self) -> assert_cmd::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_relaxkit"));
        command
            .env_clear()
            .env("LC_ALL", "C.UTF-8")
            .env("HOME", &self.home)
            .env("PATH", self.path())
            .env("NVM_DIR", self.home.join(".nvm"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env(
                "RELAXKIT_APPARMOR_PROFILE",
                self.home
                    .parent()
                    .expect("scratch root")
                    .join("relaxkit-bwrap"),
            )
            .env("RELAXKIT_STUB_LOG", &self.log)
            .env("RELAXKIT_STUB_STATE", &self.state)
            .env("HTTPS_PROXY", CLOSED_PROXY)
            .env("HTTP_PROXY", CLOSED_PROXY)
            .env("https_proxy", CLOSED_PROXY)
            .env("http_proxy", CLOSED_PROXY)
            .stdin(Stdio::null());
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        assert_cmd::Command::from_std(command)
    }
}

fn write_executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, text).expect("write stub");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod stub");
}

/// Every regular file under `root` with its bytes, keyed by path. Used to show that a run changed nothing.
pub fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                out.insert(path, bytes);
            }
        }
    }
    out
}

pub fn write_lock_except(scratch: &Scratch, skipped: Option<&str>) {
    let mut lock = String::from("relaxkit = \"0.1.0\"\n");
    for (id, version, route, hosts) in LOCKED_RECIPES {
        if Some(*id) == skipped {
            continue;
        }
        lock.push_str(&format!(
            "\n[tools.{id}]\nversion = \"{version}\"\nroute = \"{route}\"\nhosts = [{hosts}]\n"
        ));
    }
    let lock_path = scratch.home().join(".config/relaxkit/lock.toml");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    fs::write(&lock_path, lock).unwrap();
}

pub const LOCKED_RECIPES: &[(&str, &str, &str, &str)] = &[
    ("caveman", "4.1.0", "npm", "\"claude\", \"opencode\""),
    ("ccusage", "1.0.0", "npm", "\"claude\", \"opencode\""),
    (
        "chrome-devtools",
        "1.0.0",
        "npm",
        "\"claude\", \"opencode\"",
    ),
    (
        "claude-md-management",
        "1.0.0",
        "claude-plugin",
        "\"claude\"",
    ),
    ("frontend-design", "1.0.0", "claude-plugin", "\"claude\""),
    ("playwright", "1.0.0", "npm", "\"claude\", \"opencode\""),
    ("rtk", "1.2.1", "script", "\"claude\", \"opencode\""),
    ("serena", "2.1.0", "uv", "\"claude\", \"opencode\""),
    (
        "superpowers",
        "5.0.1",
        "opencode-plugin",
        "\"claude\", \"opencode\"",
    ),
];
