//! Build a REAPI `Command` + `Action` for one generative transform.
//!
//! Mirrors the buck2 fork's `command_executor.rs`: a `Command` carries the
//! harness `arguments`, the declared `output_paths` (CapyFun's is the
//! materialized patch), `environment_variables`, and a `Platform`; the `Action`
//! references the command digest, the input-root digest, a timeout, and the
//! `do_not_cache` flag.
//!
//! **The Action digest is the cache key**, so it must be canonical and stable:
//! environment variables, platform properties, and output paths are sorted. It
//! must also exclude the BuildBuddy credential — the API key travels as a gRPC
//! metadata header (see [`super::proto`] / the design doc), never as an env var
//! that would enter the digest and bust the cache on rotation. This builder has
//! no credential parameter, enforcing that by construction.

use super::digest::{message_digest, Blob};
use super::proto::reapi;

/// The inputs needed to build an Action for one agent transform.
#[derive(Debug, Clone, Default)]
pub struct ActionSpec {
    /// The harness invocation, e.g. `["claude", "-p", "<prompt>"]`.
    pub arguments: Vec<String>,
    /// Paths the action must produce, relative to the working directory. For an
    /// `agent_transform` this is the single materialized patch (e.g. `out.patch`).
    pub output_paths: Vec<String>,
    /// Non-secret environment variables. The credential must NOT appear here.
    pub env: Vec<(String, String)>,
    /// Executor selection / capability properties (e.g. `container-image`,
    /// `OSFamily`, `dockerNetwork`).
    pub platform: Vec<(String, String)>,
    /// The digest of the input-root `Directory` (see [`super::digest`]).
    pub input_root: reapi::Digest,
    /// Optional execution timeout in seconds.
    pub timeout_secs: Option<i64>,
    /// When true the result is not written to the Action Cache (used to force a
    /// fresh agent run).
    pub do_not_cache: bool,
}

/// A built Action: its digest (the cache key), the command digest, and the
/// `Command` + `Action` blobs that must be in the CAS before `Execute`.
#[derive(Debug, Clone)]
pub struct BuiltAction {
    pub action_digest: reapi::Digest,
    pub command_digest: reapi::Digest,
    /// The serialized `Command` and `Action`, ready for `BatchUpdateBlobs`.
    pub blobs: Vec<Blob>,
}

fn platform(props: &[(String, String)]) -> reapi::Platform {
    let mut properties: Vec<reapi::platform::Property> = props
        .iter()
        .map(|(name, value)| reapi::platform::Property {
            name: name.clone(),
            value: value.clone(),
        })
        .collect();
    // Canonical order: by (name, value).
    properties.sort_by(|a, b| a.name.cmp(&b.name).then(a.value.cmp(&b.value)));
    reapi::Platform { properties }
}

/// Build the `Command` + `Action` for `spec` and compute the Action digest.
pub fn build_action(spec: &ActionSpec) -> BuiltAction {
    let plat = platform(&spec.platform);

    let mut environment_variables: Vec<reapi::command::EnvironmentVariable> = spec
        .env
        .iter()
        .map(|(name, value)| reapi::command::EnvironmentVariable {
            name: name.clone(),
            value: value.clone(),
        })
        .collect();
    environment_variables.sort_by(|a, b| a.name.cmp(&b.name));

    let mut output_paths = spec.output_paths.clone();
    output_paths.sort();

    // `Command.platform` is deprecated in REAPI v2; the platform lives on the
    // Action, which is what BuildBuddy reads.
    let command = reapi::Command {
        arguments: spec.arguments.clone(),
        environment_variables,
        output_paths,
        ..Default::default()
    };
    let (command_digest, command_bytes) = message_digest(&command);

    let action = reapi::Action {
        command_digest: Some(command_digest.clone()),
        input_root_digest: Some(spec.input_root.clone()),
        timeout: spec.timeout_secs.map(|seconds| prost_types::Duration {
            seconds,
            nanos: 0,
        }),
        do_not_cache: spec.do_not_cache,
        platform: Some(plat),
        ..Default::default()
    };
    let (action_digest, action_bytes) = message_digest(&action);

    BuiltAction {
        action_digest: action_digest.clone(),
        command_digest: command_digest.clone(),
        blobs: vec![
            Blob {
                digest: command_digest,
                data: command_bytes,
            },
            Blob {
                digest: action_digest,
                data: action_bytes,
            },
        ],
    }
}

#[cfg(test)]
mod tests;
