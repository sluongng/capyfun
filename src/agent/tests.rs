use super::*;
use std::collections::HashMap;

#[test]
fn harness_kind_round_trips() {
    for kind in ["claude_code", "codex", "antigravity", "pi"] {
        assert_eq!(HarnessKind::parse(kind).unwrap().as_str(), kind);
    }
    assert!(HarnessKind::parse("gemini").is_err());
}

#[test]
fn provider_defaults_to_conventional_env_var() {
    assert_eq!(default_env_var("anthropic"), Some("ANTHROPIC_API_KEY"));
    assert_eq!(default_env_var("openai"), Some("OPENAI_API_KEY"));
    assert_eq!(default_env_var("nebius"), Some("NEBIUS_API_KEY"));
    assert_eq!(default_env_var("acme"), None);
}

#[test]
fn credential_var_uses_default_when_unset() {
    assert_eq!(credential_var("anthropic", None).unwrap(), "ANTHROPIC_API_KEY");
}

#[test]
fn credential_var_unknown_provider_without_override_errors() {
    let err = credential_var("acme", None).unwrap_err().to_string();
    assert!(err.contains("no default credential env var"), "{err}");
}

#[test]
fn credential_var_parses_env_scheme_override() {
    assert_eq!(
        credential_var("anthropic", Some("env:WORK_ANTHROPIC_KEY")).unwrap(),
        "WORK_ANTHROPIC_KEY"
    );
}

#[test]
fn credential_var_rejects_unknown_scheme() {
    let err = credential_var("anthropic", Some("vault:secret/key"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("only the `env:NAME` scheme"), "{err}");
}

#[test]
fn credential_var_rejects_empty_name() {
    assert!(credential_var("anthropic", Some("env:")).is_err());
}

#[test]
fn resolve_credential_resolved_when_var_set() {
    let env = HashMap::from([("ANTHROPIC_API_KEY".to_string(), "sk-test".to_string())]);
    let got = resolve_credential("anthropic", HarnessKind::ClaudeCode, None, |v| {
        env.get(v).cloned()
    })
    .unwrap();
    assert_eq!(
        got,
        Credential::Resolved {
            var: "ANTHROPIC_API_KEY".to_string()
        }
    );
}

#[test]
fn harness_provider_matrix() {
    assert!(HarnessKind::ClaudeCode.can_drive("anthropic"));
    assert!(!HarnessKind::ClaudeCode.can_drive("openai"));
    assert!(HarnessKind::Antigravity.can_drive("google"));
    assert!(!HarnessKind::Antigravity.can_drive("anthropic"));
    assert!(HarnessKind::Codex.can_drive("openai"));
    assert!(HarnessKind::Codex.can_drive("nebius"));
    assert!(!HarnessKind::Codex.can_drive("anthropic"));
    assert!(HarnessKind::Pi.can_drive("openai"));
    assert!(HarnessKind::Pi.can_drive("nebius"));
    assert!(!HarnessKind::Pi.can_drive("google"));
}

#[test]
fn known_providers_are_recognized() {
    for p in KNOWN_PROVIDERS {
        assert!(is_known_provider(p), "{p}");
    }
    assert!(!is_known_provider("cohere"));
}

#[test]
fn cli_harnesses_support_ambient_login() {
    assert!(HarnessKind::ClaudeCode.supports_ambient_login());
    assert!(HarnessKind::Codex.supports_ambient_login());
    assert!(HarnessKind::Antigravity.supports_ambient_login());
    assert!(!HarnessKind::Pi.supports_ambient_login());
}

#[test]
fn resolve_credential_cli_harnesses_fall_through_to_ambient() {
    // claude_code, codex, and antigravity authenticate via their own CLI login.
    for (provider, harness) in [
        ("openai", HarnessKind::ClaudeCode),
        ("openai", HarnessKind::Codex),
        ("google", HarnessKind::Antigravity),
    ] {
        let got = resolve_credential(provider, harness, None, |_| None).unwrap();
        assert_eq!(got, Credential::Ambient, "{harness:?}");
    }
}

#[test]
fn resolve_credential_pi_harness_requires_a_key() {
    let err = resolve_credential("nebius", HarnessKind::Pi, None, |_| None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("NEBIUS_API_KEY"), "{err}");
    assert!(err.contains("pi"), "{err}");
}

#[test]
fn resolve_credential_honors_override_var_name() {
    let env = HashMap::from([("WORK_KEY".to_string(), "x".to_string())]);
    let got = resolve_credential(
        "openai",
        HarnessKind::Codex,
        Some("env:WORK_KEY"),
        |v| env.get(v).cloned(),
    )
    .unwrap();
    assert_eq!(
        got,
        Credential::Resolved {
            var: "WORK_KEY".to_string()
        }
    );
}

#[test]
fn harness_command_claude_code_uses_print_flag() {
    let (program, args) = harness_command(HarnessKind::ClaudeCode, None, "hello").unwrap();
    assert_eq!(program, "claude");
    assert_eq!(args, vec!["-p", "--permission-mode", "acceptEdits", "hello"]);
}

#[test]
fn harness_command_claude_code_includes_model() {
    let (_, args) =
        harness_command(HarnessKind::ClaudeCode, Some("claude-opus-4-8"), "hi").unwrap();
    assert_eq!(
        args,
        vec!["-p", "--permission-mode", "acceptEdits", "--model", "claude-opus-4-8", "hi"]
    );
}

#[test]
fn harness_command_codex_uses_exec_subcommand() {
    let (program, args) = harness_command(HarnessKind::Codex, Some("gpt-5.5"), "hi").unwrap();
    assert_eq!(program, "codex");
    assert_eq!(args, vec!["exec", "--model", "gpt-5.5", "hi"]);
}

#[test]
fn harness_command_antigravity_prompt_follows_print_flag() {
    let (program, args) =
        harness_command(HarnessKind::Antigravity, Some("Gemini 3.5 Flash (High)"), "hi").unwrap();
    assert_eq!(program, "agy");
    // Prompt is the value of -p; --model follows.
    assert_eq!(args, vec!["-p", "hi", "--model", "Gemini 3.5 Flash (High)"]);
}

#[test]
fn provider_google_defaults_to_gemini_key() {
    assert_eq!(default_env_var("google"), Some("GEMINI_API_KEY"));
}

#[test]
fn harness_command_pi_has_no_cli_invocation() {
    // The pi harness is HTTP-based; it has no CLI command to build.
    assert!(harness_command(HarnessKind::Pi, None, "hi").is_err());
}

#[test]
fn provider_default_base_urls() {
    assert_eq!(
        default_base_url("nebius"),
        Some("https://api.tokenfactory.us-central1.nebius.com/v1")
    );
    assert_eq!(default_base_url("openai"), Some("https://api.openai.com/v1"));
    assert_eq!(default_base_url("anthropic"), None);
    assert_eq!(default_base_url("acme"), None);
}
