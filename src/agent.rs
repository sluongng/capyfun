//! Generative-transform execution path: resolve a model's credential and run an
//! agent harness (a coding-agent CLI) over a prompt.
//!
//! This is the minimal proof of the `agent_transform` execution model from
//! `docs/design/transformations.md`: render a prompt, resolve the model's
//! credential *reference* to a value at execution time, inject it into the
//! harness subprocess, and capture the agent's output. It is intentionally
//! standalone — it is **not** yet wired to the `agent`/`harness`/`model` config
//! rules (those are the T4/T5 milestones). The credential-resolution rules here
//! are the load-bearing piece and are unit-tested in isolation.

use std::process::Command;

use anyhow::{bail, Context, Result};

/// Agent harness runtimes CapyFun knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessKind {
    /// Anthropic's Claude Code CLI (`claude`). Drives Anthropic models.
    ClaudeCode,
    /// OpenAI's Codex CLI (`codex`). Drives OpenAI/open models.
    Codex,
    /// Google's Antigravity CLI (`agy`). Drives Google Gemini models.
    Antigravity,
    /// The Pi CLI. Drives OpenAI/open models.
    Pi,
}

impl HarnessKind {
    /// Parse a harness `kind` string (matches the `harness(kind=...)` config field).
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "claude_code" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            "antigravity" => Ok(Self::Antigravity),
            "pi" => Ok(Self::Pi),
            other => bail!(
                "unknown harness kind `{other}` (known: claude_code, codex, antigravity, pi)"
            ),
        }
    }

    /// The config `kind` string for this harness.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::Pi => "pi",
        }
    }

    /// Whether this harness can authenticate via its own CLI login, so a missing
    /// API key is not fatal (the run inherits the login). The CLI harnesses
    /// (`claude_code`, `codex`, `antigravity`) can; the HTTP-based `pi` harness
    /// cannot.
    pub fn supports_ambient_login(&self) -> bool {
        matches!(self, Self::ClaudeCode | Self::Codex | Self::Antigravity)
    }

    /// Whether this harness can drive a model from `provider`. Encodes the
    /// harness→provider matrix from `docs/design/transformations.md`:
    /// `claude_code`→anthropic; `antigravity`→google; `codex`→openai (and other
    /// open models); `pi`→openai/nebius (and other open models). An `agent`
    /// whose harness cannot drive its model is a validation error.
    pub fn can_drive(&self, provider: &str) -> bool {
        match self {
            Self::ClaudeCode => provider == "anthropic",
            Self::Antigravity => provider == "google",
            Self::Codex => matches!(provider, "openai" | "nebius"),
            Self::Pi => matches!(provider, "openai" | "nebius"),
        }
    }
}

/// The model providers CapyFun recognizes (matches the `model(provider=...)`
/// config field). Membership is the validation source of truth.
pub const KNOWN_PROVIDERS: [&str; 4] = ["anthropic", "openai", "google", "nebius"];

/// Whether `provider` is one CapyFun recognizes.
pub fn is_known_provider(provider: &str) -> bool {
    KNOWN_PROVIDERS.contains(&provider)
}

/// The conventional environment variable a provider's key is read from when a
/// model declares no explicit `credential` reference. Mirrors the table in
/// `docs/design/transformations.md` (Credentials).
pub fn default_env_var(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "google" => Some("GEMINI_API_KEY"),
        "nebius" => Some("NEBIUS_API_KEY"),
        _ => None,
    }
}

/// The default OpenAI-compatible API base URL for a provider, used by the `pi`
/// HTTP harness when no explicit base URL is given. Providers driven by a CLI
/// harness (anthropic via claude_code) have none.
pub fn default_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "nebius" => Some("https://api.tokenfactory.us-central1.nebius.com/v1"),
        "openai" => Some("https://api.openai.com/v1"),
        _ => None,
    }
}

/// Resolve which environment variable holds the credential for a model: parse an
/// explicit `env:NAME` reference, else fall back to the provider's conventional
/// variable. This validates the reference *shape* only; whether the variable is
/// actually set is an execution-time concern handled by [`resolve_credential`].
pub fn credential_var(provider: &str, credential: Option<&str>) -> Result<String> {
    match credential {
        Some(reference) => {
            let name = reference.strip_prefix("env:").ok_or_else(|| {
                anyhow::anyhow!(
                    "unrecognized credential reference `{reference}` \
                     (only the `env:NAME` scheme is supported)"
                )
            })?;
            if name.is_empty() {
                bail!("credential reference `env:` has an empty variable name");
            }
            Ok(name.to_string())
        }
        None => default_env_var(provider).map(str::to_owned).ok_or_else(|| {
            anyhow::anyhow!(
                "no default credential env var for provider `{provider}`; \
                 set an explicit `credential = \"env:NAME\"`"
            )
        }),
    }
}

/// The result of resolving a credential against a concrete environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// The key is present; the named variable is passed through to the harness.
    Resolved { var: String },
    /// No key was found, but `claude_code` can use its own ambient CLI login
    /// (keyring/session), so the run proceeds with the inherited environment.
    Ambient,
}

/// Resolve the credential for a `(provider, harness)` pair against an environment
/// lookup. `lookup` returns a variable's value if set — a closure so this stays
/// pure and unit-testable without touching the real process environment.
///
/// Rules (see `docs/design/transformations.md`):
/// - the variable is set → [`Credential::Resolved`];
/// - not set, harness has a CLI login (`claude_code`, `codex`) →
///   [`Credential::Ambient`];
/// - not set, HTTP harness (`pi`) → error.
pub fn resolve_credential(
    provider: &str,
    harness: HarnessKind,
    credential: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Credential> {
    let var = credential_var(provider, credential)?;
    match lookup(&var) {
        Some(_) => Ok(Credential::Resolved { var }),
        None if harness.supports_ambient_login() => Ok(Credential::Ambient),
        None => bail!(
            "credential env var `{var}` is not set (required for the `{}` harness)",
            harness.as_str()
        ),
    }
}

/// Build the harness CLI invocation (program + args) that runs `prompt`
/// non-interactively and prints the agent's response to stdout.
pub fn harness_command(
    harness: HarnessKind,
    model_id: Option<&str>,
    prompt: &str,
) -> Result<(String, Vec<String>)> {
    let mut args = Vec::new();
    let program = match harness {
        HarnessKind::ClaudeCode => {
            args.push("-p".to_string());
            // Auto-accept file edits so an agent_transform can actually modify
            // files in its working dir non-interactively. Without this, `claude
            // -p` leaves edits "pending permission" and writes nothing (a silent
            // no-op). Harmless for text-only `agent-run` prompts.
            args.push("--permission-mode".to_string());
            args.push("acceptEdits".to_string());
            if let Some(m) = model_id {
                args.push("--model".to_string());
                args.push(m.to_string());
            }
            args.push(prompt.to_string());
            "claude"
        }
        HarnessKind::Codex => {
            args.push("exec".to_string());
            if let Some(m) = model_id {
                args.push("--model".to_string());
                args.push(m.to_string());
            }
            args.push(prompt.to_string());
            "codex"
        }
        HarnessKind::Antigravity => {
            // `agy --print <prompt>`: the prompt is the flag's value, so it must
            // immediately follow `-p` (unlike claude, where it is positional).
            args.push("-p".to_string());
            args.push(prompt.to_string());
            if let Some(m) = model_id {
                args.push("--model".to_string());
                args.push(m.to_string());
            }
            "agy"
        }
        HarnessKind::Pi => {
            bail!("the `pi` harness is HTTP-based and has no CLI command")
        }
    };
    Ok((program.to_string(), args))
}

/// A single agent invocation: the harness to run, the model it drives, the
/// credential reference to resolve, and the prompt.
#[derive(Debug)]
pub struct AgentRun {
    pub harness: HarnessKind,
    pub provider: String,
    pub model_id: Option<String>,
    pub credential: Option<String>,
    /// Override for the OpenAI-compatible base URL (the `pi` HTTP harness).
    /// `None` falls back to [`default_base_url`] for the provider.
    pub base_url: Option<String>,
    pub prompt: String,
}

/// What an agent run produced.
#[derive(Debug)]
pub struct AgentOutput {
    /// How the credential resolved (for reporting/logging).
    pub credential: Credential,
    /// The harness's stdout — the agent's response (the future "patch" source).
    pub stdout: String,
}

/// Resolve credentials and execute the agent over the prompt. CLI harnesses
/// (`claude_code`, `codex`) shell out to their binary; the `pi` harness calls an
/// OpenAI-compatible HTTP endpoint directly.
pub fn run_agent(run: &AgentRun) -> Result<AgentOutput> {
    let credential = resolve_credential(
        &run.provider,
        run.harness,
        run.credential.as_deref(),
        |v| std::env::var(v).ok(),
    )?;

    match run.harness {
        HarnessKind::ClaudeCode | HarnessKind::Codex | HarnessKind::Antigravity => {
            run_cli_harness(run, credential)
        }
        HarnessKind::Pi => run_http_harness(run, credential),
    }
}

/// Shell out to a CLI harness (`claude` / `codex`). The resolved credential is
/// supplied via the inherited environment (the variable is, by construction,
/// present in our own env when [`Credential::Resolved`]); `Ambient` runs pass
/// the environment through so the harness can use its own login. A non-zero exit
/// aborts with the captured stderr.
fn run_cli_harness(run: &AgentRun, credential: Credential) -> Result<AgentOutput> {
    let (program, args) = harness_command(run.harness, run.model_id.as_deref(), &run.prompt)?;

    let output = Command::new(&program)
        .args(&args)
        .output()
        .with_context(|| format!("spawning harness `{program}`"))?;

    if !output.status.success() {
        bail!(
            "harness `{program}` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(AgentOutput {
        credential,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// Drive the `pi` harness against an OpenAI-compatible chat-completions endpoint
/// (e.g. Nebius). Resolves the base URL from the run override or the provider
/// default, sends the prompt as a single user message, and returns the reply.
fn run_http_harness(run: &AgentRun, credential: Credential) -> Result<AgentOutput> {
    let var = match &credential {
        Credential::Resolved { var } => var,
        // resolve_credential only yields Ambient for CLI harnesses, not pi.
        Credential::Ambient => bail!("the `pi` harness requires an API key"),
    };
    let key = std::env::var(var).with_context(|| format!("reading credential ${var}"))?;

    let base = run
        .base_url
        .clone()
        .or_else(|| default_base_url(&run.provider).map(str::to_owned))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no base URL for provider `{}`; pass --base-url",
                run.provider
            )
        })?;
    let model = run.model_id.as_deref().ok_or_else(|| {
        anyhow::anyhow!("the `pi` harness needs a model id (e.g. --model nvidia/nemotron-3-ultra)")
    })?;

    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": run.prompt }],
    });

    let resp = ureq::post(&url)
        .header("Authorization", &format!("Bearer {key}"))
        .send_json(&body)
        .with_context(|| format!("POST {url}"))?;
    let text = resp
        .into_body()
        .read_to_string()
        .with_context(|| format!("reading response from {url}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing response from {url}"))?;

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("unexpected response shape from {url}: {text}"))?;

    Ok(AgentOutput {
        credential,
        stdout: content.to_owned(),
    })
}

#[cfg(test)]
mod tests;
