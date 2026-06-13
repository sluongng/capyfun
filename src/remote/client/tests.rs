//! Tests for the REAPI client.
//!
//! The pure pieces (endpoint normalization, the auth interceptor) are tested
//! offline. The end-to-end CAS/AC round-trip requires a real server and is gated
//! on `BUILDBUDDY_API_KEY` — it is skipped (not failed) when the key is absent,
//! so `cargo test` stays hermetic by default.

use tonic::service::Interceptor;
use tonic::Request;

use super::*;
use crate::remote::digest::sha256_digest;

#[test]
fn normalize_endpoint_schemes() {
    assert_eq!(
        normalize_endpoint("grpcs://remote.buildbuddy.io"),
        ("https://remote.buildbuddy.io".to_owned(), true)
    );
    assert_eq!(
        normalize_endpoint("grpc://localhost:1985"),
        ("http://localhost:1985".to_owned(), false)
    );
    assert_eq!(
        normalize_endpoint("https://example.com"),
        ("https://example.com".to_owned(), true)
    );
    assert_eq!(
        normalize_endpoint("http://example.com"),
        ("http://example.com".to_owned(), false)
    );
    // Bare host defaults to TLS.
    assert_eq!(
        normalize_endpoint("remote.buildbuddy.io"),
        ("https://remote.buildbuddy.io".to_owned(), true)
    );
}

#[test]
fn auth_interceptor_injects_header_when_keyed() {
    let mut interceptor = AuthInterceptor::new(Some("secret-key")).unwrap();
    let req = interceptor.call(Request::new(())).unwrap();
    let got = req.metadata().get(API_KEY_HEADER).unwrap();
    assert_eq!(got.to_str().unwrap(), "secret-key");
}

#[test]
fn auth_interceptor_no_header_when_unkeyed() {
    let mut interceptor = AuthInterceptor::new(None).unwrap();
    let req = interceptor.call(Request::new(())).unwrap();
    assert!(req.metadata().get(API_KEY_HEADER).is_none());
}

#[test]
fn auth_interceptor_rejects_invalid_key() {
    // A newline is not a valid HTTP header value.
    assert!(AuthInterceptor::new(Some("bad\nkey")).is_err());
}

/// Live CAS + Action Cache round-trip against BuildBuddy. Skipped unless
/// `BUILDBUDDY_API_KEY` is set (and `BUILDBUDDY_ENDPOINT`/`_INSTANCE_NAME`
/// optionally override the defaults).
#[test]
fn live_cas_and_ac_roundtrip() {
    let cfg = RemoteConfig::from_env();
    let Some(_) = cfg.api_key.as_ref() else {
        eprintln!("skipping: BUILDBUDDY_API_KEY not set");
        return;
    };

    let client = RemoteClient::connect(&cfg).expect("connect to BuildBuddy");

    // Upload a small blob and confirm it is then present (not missing) and reads
    // back byte-identically.
    let data = b"capyfun reapi smoke test blob v1".to_vec();
    let digest = sha256_digest(&data);
    let blob = Blob {
        digest: digest.clone(),
        data: data.clone(),
    };

    let uploaded = client.upload_missing(&[blob]).expect("upload");
    assert!(uploaded <= 1, "uploaded count within expectations");

    let missing = client
        .find_missing_blobs(vec![digest.clone()])
        .expect("find_missing");
    assert!(missing.is_empty(), "blob should be present after upload");

    let read = client
        .batch_read_blobs(vec![digest.clone()])
        .expect("read");
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].data, data);
    assert_eq!(read[0].digest, digest);

    // An Action digest we never executed should be a clean cache miss, not an
    // error.
    let never_run = sha256_digest(b"capyfun action that was never executed");
    let hit = client.get_action_result(never_run).expect("get_action_result");
    assert!(hit.is_none(), "expected an Action Cache miss");
}
