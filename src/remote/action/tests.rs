//! Tests for REAPI Action/Command construction and the action-digest cache key.

use prost::Message;

use super::*;
use crate::remote::digest::sha256_digest;

fn spec() -> ActionSpec {
    ActionSpec {
        arguments: vec!["claude".into(), "-p".into(), "modernize".into()],
        output_paths: vec!["out.patch".into()],
        env: vec![("CAPYFUN_MODEL".into(), "claude-opus-4-8".into())],
        platform: vec![
            ("OSFamily".into(), "Linux".into()),
            ("container-image".into(), "docker://capyfun/agent:pinned".into()),
        ],
        input_root: sha256_digest(b"input-root-stand-in"),
        timeout_secs: Some(600),
        do_not_cache: false,
    }
}

/// Building the same spec twice yields the same Action digest (it is the cache
/// key, so it must be stable).
#[test]
fn action_digest_is_deterministic() {
    let a = build_action(&spec()).action_digest;
    let b = build_action(&spec()).action_digest;
    assert_eq!(a, b);
}

/// The emitted blobs are exactly the Command and the Action, each addressed by
/// its own digest, and the Action references the Command digest + input root.
#[test]
fn blobs_are_command_and_action() {
    let built = build_action(&spec());
    assert_eq!(built.blobs.len(), 2);
    // Each blob's digest matches sha256 of its bytes.
    for b in &built.blobs {
        assert_eq!(b.digest, sha256_digest(&b.data));
    }
    let action_blob = built
        .blobs
        .iter()
        .find(|b| b.digest == built.action_digest)
        .unwrap();
    let action = reapi::Action::decode(action_blob.data.as_slice()).unwrap();
    assert_eq!(action.command_digest.unwrap(), built.command_digest);
    assert_eq!(action.input_root_digest.unwrap(), spec().input_root);
    assert_eq!(action.timeout.unwrap().seconds, 600);
    assert!(!action.do_not_cache);
}

/// Env vars, platform properties, and output paths are sorted into canonical
/// order regardless of input order, so digest is order-independent.
#[test]
fn canonical_ordering() {
    let mut s = spec();
    s.env = vec![
        ("ZZZ".into(), "1".into()),
        ("AAA".into(), "2".into()),
    ];
    s.output_paths = vec!["b.patch".into(), "a.patch".into()];
    let built = build_action(&s);
    let cmd_blob = built
        .blobs
        .iter()
        .find(|b| b.digest == built.command_digest)
        .unwrap();
    let cmd = reapi::Command::decode(cmd_blob.data.as_slice()).unwrap();
    let env_names: Vec<_> = cmd.environment_variables.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(env_names, ["AAA", "ZZZ"]);
    assert_eq!(cmd.output_paths, ["a.patch", "b.patch"]);

    // Platform lives on the Action (Command.platform is deprecated) and is sorted.
    let action_blob = built
        .blobs
        .iter()
        .find(|b| b.digest == built.action_digest)
        .unwrap();
    let action = reapi::Action::decode(action_blob.data.as_slice()).unwrap();
    let plat_names: Vec<_> = action
        .platform
        .unwrap()
        .properties
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(plat_names, ["OSFamily", "container-image"]);

    // Same spec with env listed in a different order → identical digest.
    let mut s2 = s.clone();
    s2.env.reverse();
    s2.output_paths.reverse();
    assert_eq!(build_action(&s2).action_digest, built.action_digest);
}

/// Changing the input root changes the Action digest (cache invalidation).
#[test]
fn input_root_change_busts_cache() {
    let base = build_action(&spec()).action_digest;
    let mut s = spec();
    s.input_root = sha256_digest(b"a-different-subtree");
    assert_ne!(build_action(&s).action_digest, base);
}

/// The arguments (prompt/harness) are part of the cache key.
#[test]
fn arguments_change_busts_cache() {
    let base = build_action(&spec()).action_digest;
    let mut s = spec();
    s.arguments.push("--extra".into());
    assert_ne!(build_action(&s).action_digest, base);
}

/// The credential is NOT a builder input, so it cannot enter the digest: two
/// runs that would carry different API keys (passed elsewhere as a header)
/// produce the same Action digest. This guards the "key out of the cache key"
/// invariant structurally.
#[test]
fn credential_not_in_digest() {
    // The spec has no place for a credential at all; identical specs that differ
    // only in an out-of-band key are byte-identical here.
    let a = build_action(&spec()).action_digest;
    let b = build_action(&spec()).action_digest;
    assert_eq!(a, b);
    // And no env var named like a credential leaks in by default.
    let built = build_action(&spec());
    let cmd_blob = built
        .blobs
        .iter()
        .find(|b| b.digest == built.command_digest)
        .unwrap();
    let cmd = reapi::Command::decode(cmd_blob.data.as_slice()).unwrap();
    assert!(cmd
        .environment_variables
        .iter()
        .all(|e| !e.name.to_lowercase().contains("api_key")));
}
