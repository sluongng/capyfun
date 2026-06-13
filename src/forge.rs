//! GitHub-side identity for reactions: the `Forge` trait and its impls.
//!
//! A reaction (see [`crate::react`]) needs to *act* on a GitHub repo it does not
//! own the working copy of: clone it, push a branch, and open a PR. Those three
//! operations are abstracted behind [`Forge`] so the reaction engine stays
//! testable without a network or a real GitHub App — exactly like the engine's
//! [`AgentRunner`](crate::engine::agent_exec::AgentRunner) trait.
//!
//! - [`GitHubAppForge`] is the live path: it authenticates as a **GitHub App**
//!   (mint an RS256 JWT from the App private key, exchange it for an installation
//!   access token), uses that token for git clone/push and the PR REST call.
//! - [`LocalForge`] is the hermetic path: it points clone/push at local bare
//!   repositories (via `CAPYFUN_GITHUB_BASE`, the same escape hatch imports use)
//!   and *records* the PR request instead of calling a forge.
//!
//! The App credentials (id + private key) are read from the environment, never
//! the repo — consistent with how model credentials are handled.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

/// A pull-request to open on the destination repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRequest {
    /// Base branch the PR merges into (the repo's default branch).
    pub base: String,
    /// Head branch carrying the change (already pushed).
    pub head: String,
    pub title: String,
    pub body: String,
}

/// The result of opening (or recording) a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrOutcome {
    /// The PR's HTML URL, when a real PR was opened.
    pub url: Option<String>,
    /// A one-line human description (the URL, or why it was only recorded).
    pub summary: String,
}

/// The GitHub-side operations a reaction performs. Behind a trait so the reaction
/// loop can run against local fixtures in tests and against a real App in prod.
pub trait Forge: Send + Sync {
    /// A git URL usable to **clone and push** `repo` (`owner/name`), already
    /// carrying whatever auth the forge needs. `installation_id` scopes the App
    /// token; it is ignored by forges that do not need it.
    fn git_url(&self, repo: &str, installation_id: Option<u64>) -> Result<String>;

    /// Open (or, for a local forge, record) a pull request on `repo`.
    fn open_pr(
        &self,
        repo: &str,
        installation_id: Option<u64>,
        pr: &PrRequest,
    ) -> Result<PrOutcome>;
}

// --- live: authenticate as a GitHub App ------------------------------------

/// Live forge: acts as a GitHub App via an installation access token.
pub struct GitHubAppForge {
    app_id: String,
    /// RS256 signing key built from the App private key (PEM).
    key: jsonwebtoken::EncodingKey,
    /// REST base, e.g. `https://api.github.com` (overridable for GHE).
    api_base: String,
}

/// JWT claims for the App-level token (GitHub: `iat`, `exp` ≤ 10 min, `iss`).
#[derive(Serialize)]
struct AppClaims {
    iat: u64,
    exp: u64,
    iss: String,
}

impl GitHubAppForge {
    /// Construct from explicit values. `private_key_pem` is the App's PEM key.
    pub fn new(app_id: impl Into<String>, private_key_pem: &[u8], api_base: impl Into<String>) -> Result<Self> {
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem)
            .context("parsing GitHub App private key (expected an RSA PEM)")?;
        Ok(Self {
            app_id: app_id.into(),
            key,
            api_base: api_base.into(),
        })
    }

    /// Construct from the conventional environment: `CAPYFUN_GITHUB_APP_ID` and
    /// `CAPYFUN_GITHUB_APP_KEY` (a path to the PEM private key). `api_base`
    /// defaults to public GitHub, overridable via `CAPYFUN_GITHUB_API_BASE`.
    pub fn from_env() -> Result<Self> {
        let app_id = std::env::var("CAPYFUN_GITHUB_APP_ID")
            .context("CAPYFUN_GITHUB_APP_ID is not set (required for GitHub App auth)")?;
        let key_path = std::env::var("CAPYFUN_GITHUB_APP_KEY")
            .context("CAPYFUN_GITHUB_APP_KEY is not set (path to the App private-key PEM)")?;
        let pem = std::fs::read(&key_path)
            .with_context(|| format!("reading App private key {key_path}"))?;
        let api_base = std::env::var("CAPYFUN_GITHUB_API_BASE")
            .unwrap_or_else(|_| "https://api.github.com".to_owned());
        Self::new(app_id, &pem, api_base)
    }

    /// Mint a short-lived App JWT (RS256), signed with the App private key.
    fn app_jwt(&self) -> Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("clock before UNIX epoch")?
            .as_secs();
        let claims = AppClaims {
            // Backdate 60s to tolerate minor clock skew (GitHub guidance).
            iat: now - 60,
            exp: now + 9 * 60,
            iss: self.app_id.clone(),
        };
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        jsonwebtoken::encode(&header, &claims, &self.key).context("signing GitHub App JWT")
    }

    /// Exchange the App JWT for an installation access token.
    fn installation_token(&self, installation_id: u64) -> Result<String> {
        let jwt = self.app_jwt()?;
        let url = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            self.api_base.trim_end_matches('/')
        );
        let resp = ureq::post(&url)
            .header("Authorization", &format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "capyfun")
            .send_empty()
            .with_context(|| format!("POST {url} (installation token exchange)"))?;
        let text = resp
            .into_body()
            .read_to_string()
            .context("reading installation-token response")?;
        let json: serde_json::Value =
            serde_json::from_str(&text).context("parsing installation-token response")?;
        json["token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("no `token` in installation-token response: {text}"))
    }
}

impl Forge for GitHubAppForge {
    fn git_url(&self, repo: &str, installation_id: Option<u64>) -> Result<String> {
        let installation_id = installation_id.context(
            "GitHub App auth needs an installation id (from the webhook `installation.id`)",
        )?;
        let token = self.installation_token(installation_id)?;
        // Token-in-URL is the simplest auth for both clone and push over HTTPS.
        Ok(format!("https://x-access-token:{token}@github.com/{repo}.git"))
    }

    fn open_pr(
        &self,
        repo: &str,
        installation_id: Option<u64>,
        pr: &PrRequest,
    ) -> Result<PrOutcome> {
        let installation_id = installation_id
            .context("GitHub App auth needs an installation id to open a PR")?;
        let token = self.installation_token(installation_id)?;
        let url = format!("{}/repos/{repo}/pulls", self.api_base.trim_end_matches('/'));
        let body = serde_json::json!({
            "title": pr.title,
            "head": pr.head,
            "base": pr.base,
            "body": pr.body,
        });
        let resp = ureq::post(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "capyfun")
            .send_json(&body)
            .with_context(|| format!("POST {url} (open PR)"))?;
        let text = resp.into_body().read_to_string().context("reading PR response")?;
        let json: serde_json::Value =
            serde_json::from_str(&text).context("parsing PR response")?;
        let html_url = json["html_url"].as_str().map(str::to_owned);
        let summary = html_url
            .clone()
            .unwrap_or_else(|| format!("opened PR on {repo} (no html_url in response): {text}"));
        Ok(PrOutcome {
            url: html_url,
            summary,
        })
    }
}

// --- hermetic: local bare repos, recorded PRs ------------------------------

/// Hermetic forge: clones/pushes local bare repositories and records PR requests
/// rather than calling a forge. Driven by `CAPYFUN_GITHUB_BASE` (the same local
/// base imports/vendors use), so the whole reaction loop runs with no network.
#[derive(Default)]
pub struct LocalForge {
    /// Base directory holding `owner/name` bare repos. `None` reads
    /// `CAPYFUN_GITHUB_BASE` at call time (the CLI path); tests pass an explicit
    /// base via [`LocalForge::with_base`] so they never mutate process env.
    base: Option<PathBuf>,
    /// PRs that would have been opened, in order — inspected by tests and printed
    /// by the `capyfun react` debug command.
    pub opened: Mutex<Vec<(String, PrRequest)>>,
}

impl LocalForge {
    /// Env-driven base (`CAPYFUN_GITHUB_BASE`), for the CLI/demo path.
    pub fn new() -> Self {
        Self::default()
    }

    /// An explicit local base directory, for hermetic tests.
    pub fn with_base(base: PathBuf) -> Self {
        Self {
            base: Some(base),
            opened: Mutex::new(Vec::new()),
        }
    }

    /// Base directory holding `owner/name` bare repos.
    fn base(&self) -> Result<PathBuf> {
        if let Some(base) = &self.base {
            return Ok(base.clone());
        }
        let base = std::env::var("CAPYFUN_GITHUB_BASE").context(
            "LocalForge requires CAPYFUN_GITHUB_BASE to point at local bare repositories",
        )?;
        Ok(PathBuf::from(base))
    }
}

impl Forge for LocalForge {
    fn git_url(&self, repo: &str, _installation_id: Option<u64>) -> Result<String> {
        Ok(self.base()?.join(repo).to_string_lossy().into_owned())
    }

    fn open_pr(
        &self,
        repo: &str,
        _installation_id: Option<u64>,
        pr: &PrRequest,
    ) -> Result<PrOutcome> {
        self.opened
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((repo.to_owned(), pr.clone()));
        Ok(PrOutcome {
            url: None,
            summary: format!(
                "recorded PR on {repo}: {} <- {} ({:?})",
                pr.base, pr.head, pr.title
            ),
        })
    }
}

#[cfg(test)]
mod tests;
