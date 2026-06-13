use super::*;
use crate::ir::{Import, Ir, Monorepo, Vendor};

fn test_ir() -> Ir {
    Ir {
        monorepo: Monorepo {
            name: "m".into(),
            default_branch: "main".into(),
        },
        imports: vec![Import {
            label: "//third_party/go/github.com/google/uuid:uuid".into(),
            name: "uuid".into(),
            package: "//third_party/go/github.com/google/uuid".into(),
            repo: "google/uuid".into(),
            git_ref: "refs/heads/master".into(),
            dest: "third_party/go/github.com/google/uuid".into(),
            patches: vec![],
            transforms: vec![],
        }],
        vendors: vec![Vendor {
            label: "//third_party/rust/anyhow:anyhow".into(),
            name: "anyhow".into(),
            package: "//third_party/rust/anyhow".into(),
            repo: "dtolnay/anyhow".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            dest: "third_party/rust/anyhow".into(),
        }],
        exports: vec![],
        harnesses: vec![],
        models: vec![],
        agents: vec![],
        prompt_templates: vec![],
        reactions: vec![],
    }
}

#[test]
fn index_from_ir_maps_repos() {
    let idx = Index::from_ir(&test_ir());
    assert_eq!(idx.repos(), 2);
    assert!(idx.0.contains_key("google/uuid"));
    assert!(idx.0.contains_key("dtolnay/anyhow"));
}

#[test]
fn import_matches_push_on_tracked_ref_only() {
    let idx = Index::from_ir(&test_ir());

    let on_ref = Event {
        kind: "PushEvent".into(),
        repo: "google/uuid".into(),
        git_ref: Some("refs/heads/master".into()),
        sha: Some("abc".into()),
    };
    let t = match_event(&idx, &on_ref);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].label, "//third_party/go/github.com/google/uuid:uuid");

    // Push to a different ref does not match.
    let other_ref = Event {
        git_ref: Some("refs/heads/dev".into()),
        ..on_ref.clone()
    };
    assert!(match_event(&idx, &other_ref).is_empty());

    // A non-push event for an import does not match.
    let create = Event {
        kind: "CreateEvent".into(),
        ..on_ref
    };
    assert!(match_event(&idx, &create).is_empty());
}

#[test]
fn vendor_matches_release_and_tag_events() {
    let idx = Index::from_ir(&test_ir());
    for kind in ["CreateEvent", "ReleaseEvent"] {
        let ev = Event {
            kind: kind.into(),
            repo: "dtolnay/anyhow".into(),
            git_ref: Some("1.0.99".into()),
            sha: None,
        };
        let t = match_event(&idx, &ev);
        assert_eq!(t.len(), 1, "{kind}");
        assert_eq!(t[0].label, "//third_party/rust/anyhow:anyhow");
    }
    // A plain push to a pinned vendor is not a trigger.
    let push = Event {
        kind: "PushEvent".into(),
        repo: "dtolnay/anyhow".into(),
        git_ref: Some("refs/heads/master".into()),
        sha: Some("x".into()),
    };
    assert!(match_event(&idx, &push).is_empty());
}

#[test]
fn unsubscribed_repo_yields_nothing() {
    let idx = Index::from_ir(&test_ir());
    let ev = Event {
        kind: "PushEvent".into(),
        repo: "someone/else".into(),
        git_ref: Some("refs/heads/main".into()),
        sha: Some("x".into()),
    };
    assert!(match_event(&idx, &ev).is_empty());
}

#[test]
fn parses_gharchive_push_line() {
    let line = r#"{"type":"PushEvent","repo":{"name":"google/uuid"},"payload":{"ref":"refs/heads/master","head":"deadbeef"}}"#;
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    let ev = parse_archive_event(&v).unwrap();
    assert_eq!(ev.kind, "PushEvent");
    assert_eq!(ev.repo, "google/uuid");
    assert_eq!(ev.git_ref.as_deref(), Some("refs/heads/master"));
    assert_eq!(ev.sha.as_deref(), Some("deadbeef"));
}

#[test]
fn parses_webhook_push_payload() {
    let line =
        r#"{"ref":"refs/heads/main","after":"cafe","repository":{"full_name":"acme/backend"}}"#;
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    let ev = parse_webhook_push(&v).unwrap();
    assert_eq!(ev.kind, "PushEvent");
    assert_eq!(ev.repo, "acme/backend");
    assert_eq!(ev.git_ref.as_deref(), Some("refs/heads/main"));
    assert_eq!(ev.sha.as_deref(), Some("cafe"));
}

#[test]
fn scan_archive_filters_to_subscribed_repos() {
    let idx = Index::from_ir(&test_ir());
    let lines = [
        r#"{"type":"PushEvent","repo":{"name":"google/uuid"},"payload":{"ref":"refs/heads/master","head":"a"}}"#,
        r#"{"type":"PushEvent","repo":{"name":"random/repo"},"payload":{"ref":"refs/heads/main","head":"b"}}"#,
        r#"{"type":"WatchEvent","repo":{"name":"google/uuid"},"payload":{}}"#,
        "",
        r#"not json"#,
        r#"{"type":"ReleaseEvent","repo":{"name":"dtolnay/anyhow"},"payload":{"action":"published"}}"#,
    ]
    .join("\n");
    let (scanned, triggers) = scan_archive(std::io::Cursor::new(lines), &idx).unwrap();
    assert_eq!(scanned, 5, "5 non-empty lines scanned");
    // uuid push (match) + anyhow release (match); random repo and WatchEvent ignored.
    assert_eq!(triggers.len(), 2, "{triggers:?}");
    assert!(triggers.iter().any(|t| t.label.contains("uuid")));
    assert!(triggers.iter().any(|t| t.label.contains("anyhow")));
}

#[test]
fn archive_url_format_and_utc_parts() {
    // 2024-01-02 07:30:00 UTC = 1704180600
    assert_eq!(utc_parts(1_704_180_600), (2024, 1, 2, 7));
    assert_eq!(
        archive_url(2024, 1, 2, 7),
        "https://data.gharchive.org/2024-01-02-7.json.gz"
    );
    // epoch
    assert_eq!(utc_parts(0), (1970, 1, 1, 0));
}

// --- webhook auth + routing -------------------------------------------------

const SECRET: &str = "shh";

fn ctx_with_secret() -> HttpCtx {
    HttpCtx::new(Index::from_ir(&test_ir()), std::sync::Arc::new(ReportOnly))
        .with_secret(Some(SECRET.to_owned()))
}

#[test]
fn signature_round_trips_and_constant_time_verifies() {
    let body = br#"{"hello":"world"}"#;
    let sig = webhook_signature(SECRET, body);
    assert!(sig.starts_with("sha256="));
    assert!(verify_signature(SECRET, body, Some(&sig)));
    // Wrong secret, tampered body, and missing/garbage signatures all fail.
    assert!(!verify_signature("other", body, Some(&sig)));
    assert!(!verify_signature(SECRET, b"tampered", Some(&sig)));
    assert!(!verify_signature(SECRET, body, None));
    assert!(!verify_signature(SECRET, body, Some("sha1=deadbeef")));
}

#[test]
fn webhook_fails_closed_without_a_secret() {
    let ctx = HttpCtx::new(Index::from_ir(&test_ir()), std::sync::Arc::new(ReportOnly));
    let body = br#"{"ref":"refs/heads/master","after":"x","repository":{"full_name":"google/uuid"}}"#;
    let (code, _) = handle_webhook(&ctx, body, None, "push", None);
    assert_eq!(code, 401);
}

#[test]
fn webhook_rejects_bad_signature() {
    let ctx = ctx_with_secret();
    let body = br#"{"ref":"refs/heads/master","after":"x","repository":{"full_name":"google/uuid"}}"#;
    let (code, _) = handle_webhook(&ctx, body, Some("sha256=00"), "push", None);
    assert_eq!(code, 401);
}

#[test]
fn webhook_accepts_signed_push_and_dedupes_redelivery() {
    let ctx = ctx_with_secret();
    let body = br#"{"ref":"refs/heads/master","after":"x","repository":{"full_name":"google/uuid"}}"#;
    let sig = webhook_signature(SECRET, body);

    let (code, b) = handle_webhook(&ctx, body, Some(&sig), "push", Some("delivery-1"));
    assert_eq!(code, 202, "{b}");
    assert!(b.contains("1 trigger"), "{b}");

    // Same delivery id -> deduped.
    let (code, b) = handle_webhook(&ctx, body, Some(&sig), "push", Some("delivery-1"));
    assert_eq!(code, 200);
    assert!(b.contains("duplicate"), "{b}");
}

#[test]
fn webhook_routes_issues_without_reactions_configured() {
    let ctx = ctx_with_secret();
    let body = br#"{"action":"opened","issue":{"number":1,"title":"t","body":"b","labels":[]},"repository":{"full_name":"google/uuid","default_branch":"main"}}"#;
    let sig = webhook_signature(SECRET, body);
    let (code, b) = handle_webhook(&ctx, body, Some(&sig), "issues", None);
    assert_eq!(code, 202);
    assert!(b.contains("reactions not configured"), "{b}");
}

#[test]
fn webhook_answers_ping() {
    let ctx = ctx_with_secret();
    let body = br#"{"zen":"keep it logically awesome"}"#;
    let sig = webhook_signature(SECRET, body);
    let (code, b) = handle_webhook(&ctx, body, Some(&sig), "ping", None);
    assert_eq!(code, 200);
    assert!(b.contains("pong"), "{b}");
}
