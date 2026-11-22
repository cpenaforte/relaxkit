//! Network access behind a trait. Used only by `update`, `status` and `setup`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};

use crate::brand;
use crate::fsx::write_atomic;

pub trait Net {
    /// Fetches a URL and returns its body as text. Non-success status is an error.
    fn get_text(&self, url: &str) -> Result<String>;
    /// Fetches a URL and writes its body to `dest`. The parent directory must already exist.
    fn download(&self, url: &str, dest: &Path) -> Result<()>;
}

/// Real HTTP through reqwest. The only identifying data sent is the User-Agent.
pub struct HttpNet {
    client: reqwest::blocking::Client,
}

impl HttpNet {
    pub fn new() -> Result<HttpNet> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(format!("{}/{}", brand::NAME, env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot build the HTTP client")?;
        Ok(HttpNet { client })
    }
}

impl Net for HttpNet {
    fn get_text(&self, url: &str) -> Result<String> {
        let response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        Ok(response.error_for_status()?.text()?)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let bytes = response.error_for_status()?.bytes()?;
        write_atomic(dest, &bytes).with_context(|| format!("cannot write {}", dest.display()))
    }
}

#[derive(Default)]
struct FakeState {
    bodies: BTreeMap<String, Vec<u8>>,
}

/// Scripted network. Clones share state.
#[derive(Clone, Default)]
pub struct FakeNet {
    state: Arc<Mutex<FakeState>>,
}

impl FakeNet {
    pub fn new() -> FakeNet {
        FakeNet::default()
    }

    /// Serves `body` for `url`.
    pub fn with(self, url: &str, body: &str) -> FakeNet {
        self.with_bytes(url, body.as_bytes().to_vec())
    }

    /// Serves raw `bytes` for `url`, such as a release archive. `get_text` refuses non-UTF-8 bytes.
    pub fn with_bytes(self, url: &str, bytes: Vec<u8>) -> FakeNet {
        self.state
            .lock()
            .expect("fake net lock")
            .bodies
            .insert(url.to_string(), bytes);
        self
    }

    fn bytes(&self, url: &str) -> Result<Vec<u8>> {
        self.state
            .lock()
            .expect("fake net lock")
            .bodies
            .get(url)
            .cloned()
            .ok_or_else(|| anyhow!("unexpected URL in test: {url}"))
    }
}

impl Net for FakeNet {
    fn get_text(&self, url: &str) -> Result<String> {
        String::from_utf8(self.bytes(url)?).with_context(|| format!("{url} is not UTF-8"))
    }

    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let body = self.bytes(url)?;
        std::fs::write(dest, body).with_context(|| format!("cannot write {}", dest.display()))
    }
}

/// Latest release tag of a GitHub repository, such as `v0.41.0`.
pub fn github_latest_tag(net: &dyn Net, repo: &str) -> Result<String> {
    let body = net.get_text(&format!(
        "https://api.github.com/repos/{repo}/releases/latest"
    ))?;
    let value: serde_json::Value =
        serde_json::from_str(&body).context("GitHub release response is not JSON")?;
    value
        .get("tag_name")
        .and_then(|tag| tag.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("GitHub release response has no tag_name"))
}

/// Latest published version of an npm package. Scoped names are encoded as `@scope%2fname`.
pub fn npm_latest(net: &dyn Net, package: &str) -> Result<String> {
    let encoded = package.replace('/', "%2f");
    let body = net.get_text(&format!("https://registry.npmjs.org/{encoded}/latest"))?;
    let value: serde_json::Value =
        serde_json::from_str(&body).context("npm registry response is not JSON")?;
    value
        .get("version")
        .and_then(|version| version.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("npm registry response has no version"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    /// Serves one HTTP 200 response with `body` on 127.0.0.1. The thread returns the raw request.
    fn serve_once(body: &str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let body = body.to_string();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let read = stream.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, handle)
    }

    #[test]
    fn http_requests_identify_only_by_relaxkit_user_agent() {
        let (url, server) = serve_once("{}");
        let net = HttpNet::new().unwrap();

        net.get_text(&url).unwrap();

        let request = server.join().unwrap();
        let headers: Vec<(String, String)> = request
            .lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        let user_agent = headers
            .iter()
            .find(|(name, _)| name == "user-agent")
            .map(|(_, value)| value.as_str());
        assert_eq!(user_agent, Some("relaxkit/0.1.0"), "request:\n{request}");
        let allowed = ["host", "user-agent", "accept", "accept-encoding"];
        for (name, _) in &headers {
            assert!(
                allowed.contains(&name.as_str()),
                "unexpected header {name} in request:\n{request}"
            );
        }
    }

    #[test]
    fn a_failed_download_write_keeps_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("tool.bin");
        fs::write(&dest, "previous").unwrap();
        // The atomic write needs this temp path. A directory there makes the write fail.
        fs::create_dir(dir.path().join("tool.bin.relaxkit-tmp")).unwrap();
        let (url, server) = serve_once("new body");
        let net = HttpNet::new().unwrap();

        let result = net.download(&url, &dest);

        server.join().unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "previous");
    }

    #[test]
    fn download_into_a_missing_directory_fails_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-dir");
        let dest = missing.join("tool.bin");
        let (url, server) = serve_once("new body");
        let net = HttpNet::new().unwrap();

        let result = net.download(&url, &dest);

        server.join().unwrap();
        assert!(result.is_err());
        assert!(!missing.exists(), "download created the missing directory");
    }

    #[cfg(unix)]
    #[test]
    fn download_over_a_symlink_writes_through_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("tool.bin");
        let link = dir.path().join("link.bin");
        fs::write(&real, "previous").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (url, server) = serve_once("new body");
        let net = HttpNet::new().unwrap();

        net.download(&url, &link).unwrap();

        server.join().unwrap();
        assert!(link.is_symlink(), "the link was replaced by a regular file");
        assert_eq!(fs::read_link(&link).unwrap(), real);
        assert_eq!(fs::read_to_string(&real).unwrap(), "new body");
    }

    #[test]
    fn github_latest_tag_parses_tag_name() {
        let net = FakeNet::new().with(
            "https://api.github.com/repos/nvm-sh/nvm/releases/latest",
            r#"{"tag_name":"v0.41.0"}"#,
        );
        assert_eq!(github_latest_tag(&net, "nvm-sh/nvm").unwrap(), "v0.41.0");
    }

    #[test]
    fn npm_latest_encodes_scoped_package() {
        let net = FakeNet::new().with(
            "https://registry.npmjs.org/@playwright%2fmcp/latest",
            r#"{"name":"@playwright/mcp","version":"0.0.86"}"#,
        );
        assert_eq!(npm_latest(&net, "@playwright/mcp").unwrap(), "0.0.86");
    }

    #[test]
    fn fake_net_errors_on_unknown_url() {
        let net = FakeNet::new();
        assert!(net.get_text("https://example.invalid/").is_err());
    }
}
