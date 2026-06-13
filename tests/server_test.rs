//! Integration test for the automation server's HTTP endpoint: start the real
//! webhook/health server on an ephemeral port and drive it over HTTP.

use std::sync::Arc;

use capyfun::ir::{Import, Ir, Monorepo};
use capyfun::server::{run_http, webhook_signature, Actor, HttpCtx, Index, ReportOnly};

fn ir_with_uuid_import() -> Ir {
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
        vendors: vec![],
        exports: vec![],
        harnesses: vec![],
        models: vec![],
        agents: vec![],
        prompt_templates: vec![],
        reactions: vec![],
    }
}

const SECRET: &str = "it's a secret to everybody";

// ureq treats non-2xx as an error; unwrap the status either way.
fn get(url: &str) -> (u16, String) {
    match ureq::get(url).call() {
        Ok(mut r) => (r.status().as_u16(), r.body_mut().read_to_string().unwrap()),
        Err(ureq::Error::StatusCode(code)) => (code, String::new()),
        Err(e) => panic!("{e}"),
    }
}

/// POST a signed `push` webhook (valid HMAC + headers), like GitHub would.
fn post_push(url: &str, body: &str, delivery: &str) -> (u16, String) {
    let sig = webhook_signature(SECRET, body.as_bytes());
    let req = ureq::post(url)
        .header("X-Hub-Signature-256", &sig)
        .header("X-GitHub-Event", "push")
        .header("X-GitHub-Delivery", delivery);
    match req.send(body) {
        Ok(mut r) => (r.status().as_u16(), r.body_mut().read_to_string().unwrap()),
        Err(ureq::Error::StatusCode(code)) => (code, String::new()),
        Err(e) => panic!("{e}"),
    }
}

/// POST without a signature (an unauthenticated caller).
fn post_unsigned(url: &str, body: &str) -> (u16, String) {
    match ureq::post(url).header("X-GitHub-Event", "push").send(body) {
        Ok(mut r) => (r.status().as_u16(), r.body_mut().read_to_string().unwrap()),
        Err(ureq::Error::StatusCode(code)) => (code, String::new()),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn webhook_and_health_endpoints() {
    let index = Index::from_ir(&ir_with_uuid_import());
    let actor: Arc<dyn Actor> = Arc::new(ReportOnly);
    let ctx = Arc::new(HttpCtx::new(index, actor).with_secret(Some(SECRET.to_owned())));
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    std::thread::spawn(move || run_http(server, ctx));

    let base = format!("http://{addr}");

    // Health check.
    let (code, body) = get(&format!("{base}/healthz"));
    assert_eq!(code, 200);
    assert_eq!(body, "ok\n");

    // A signed push for the subscribed repo + ref -> one trigger.
    let payload = r#"{"ref":"refs/heads/master","after":"deadbeef","repository":{"full_name":"google/uuid"}}"#;
    let (code, body) = post_push(&format!("{base}/webhook"), payload, "d1");
    assert_eq!(code, 202, "{body}");
    assert!(body.contains("1 trigger"), "{body}");

    // A signed push for an unsubscribed repo -> zero triggers (still accepted).
    let other =
        r#"{"ref":"refs/heads/main","after":"x","repository":{"full_name":"someone/else"}}"#;
    let (code, body) = post_push(&format!("{base}/webhook"), other, "d2");
    assert_eq!(code, 202, "{body}");
    assert!(body.contains("0 trigger"), "{body}");

    // An unsigned push is rejected (fail-closed HMAC).
    let (code, _) = post_unsigned(&format!("{base}/webhook"), payload);
    assert_eq!(code, 401);

    // A redelivery (same X-GitHub-Delivery) is deduped.
    let (code, body) = post_push(&format!("{base}/webhook"), payload, "d1");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("duplicate"), "{body}");

    // Unknown route.
    let (code, _) = get(&format!("{base}/nope"));
    assert_eq!(code, 404);
}
