//! Network access behind a trait. Used only by `update`, `status` and `setup`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};

use crate::brand;

pub trait Net {
    /// Fetches a URL and returns its body as text. Non-success status is an error.
    fn get_text(&self, url: &str) -> Result<String>;
    /// Fetches a URL and writes its body to `dest`.
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
        std::fs::write(dest, &bytes).with_context(|| format!("cannot write {}", dest.display()))
    }
}

#[derive(Default)]
struct FakeState {
    bodies: BTreeMap<String, String>,
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
        self.state
            .lock()
            .expect("fake net lock")
            .bodies
            .insert(url.to_string(), body.to_string());
        self
    }
}

impl Net for FakeNet {
    fn get_text(&self, url: &str) -> Result<String> {
        self.state
            .lock()
            .expect("fake net lock")
            .bodies
            .get(url)
            .cloned()
            .ok_or_else(|| anyhow!("unexpected URL in test: {url}"))
    }

    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let body = self.get_text(url)?;
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
    use super::*;

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
