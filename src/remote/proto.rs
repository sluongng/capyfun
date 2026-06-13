//! Generated REAPI v2 + google protobuf bindings.
//!
//! `prost` emits each package as a flat unit and cross-references siblings with
//! `super::` chains keyed off the protobuf package path, so the module tree here
//! must mirror the package hierarchy (`build.bazel.remote.execution.v2`,
//! `google.bytestream`, …). `google.protobuf.*` well-known types are mapped to
//! `prost_types` automatically and need no module. See `build.rs`.
#![allow(clippy::all, clippy::pedantic, missing_docs)]

pub mod build {
    pub mod bazel {
        pub mod semver {
            tonic::include_proto!("build.bazel.semver");
        }
        pub mod remote {
            pub mod execution {
                pub mod v2 {
                    tonic::include_proto!("build.bazel.remote.execution.v2");
                }
            }
        }
    }
}

pub mod google {
    pub mod api {
        tonic::include_proto!("google.api");
    }
    pub mod bytestream {
        tonic::include_proto!("google.bytestream");
    }
    pub mod longrunning {
        tonic::include_proto!("google.longrunning");
    }
    pub mod rpc {
        tonic::include_proto!("google.rpc");
    }
}

/// The REAPI v2 message + client namespace, re-exported for ergonomic use.
pub use build::bazel::remote::execution::v2 as reapi;

#[cfg(test)]
mod tests {
    use super::reapi;
    use prost::Message;

    /// The core REAPI messages compile and round-trip through prost encoding.
    #[test]
    fn messages_roundtrip() {
        let dir = reapi::Directory {
            files: vec![reapi::FileNode {
                name: "main.rs".to_owned(),
                digest: Some(reapi::Digest {
                    hash: "deadbeef".to_owned(),
                    size_bytes: 7,
                }),
                is_executable: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = dir.encode_to_vec();
        let back = reapi::Directory::decode(bytes.as_slice()).expect("decode");
        assert_eq!(dir, back);
        assert_eq!(back.files[0].digest.as_ref().unwrap().size_bytes, 7);
    }

    /// Action / Command carry the fields we drive the engine seam with.
    #[test]
    fn action_and_command_fields() {
        let cmd = reapi::Command {
            arguments: vec!["claude".to_owned(), "-p".to_owned()],
            output_paths: vec!["out.patch".to_owned()],
            ..Default::default()
        };
        let action = reapi::Action {
            command_digest: Some(reapi::Digest {
                hash: "c0ffee".to_owned(),
                size_bytes: cmd.encode_to_vec().len() as i64,
            }),
            input_root_digest: Some(reapi::Digest::default()),
            do_not_cache: false,
            ..Default::default()
        };
        assert_eq!(cmd.output_paths, ["out.patch"]);
        assert!(!action.do_not_cache);
    }

    /// The gRPC client stubs for the three services we use are generated.
    /// (Type-checked only — no connection is made.)
    #[test]
    fn grpc_clients_exist() {
        fn _assert_types() {
            type _Cas<T> =
                reapi::content_addressable_storage_client::ContentAddressableStorageClient<T>;
            type _Ac<T> = reapi::action_cache_client::ActionCacheClient<T>;
            type _Exec<T> = reapi::execution_client::ExecutionClient<T>;
        }
    }
}
