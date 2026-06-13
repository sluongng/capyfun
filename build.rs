//! Compile the vendored Remote Execution API (REAPI) v2 protos into Rust.
//!
//! The proto set under `proto/` is copied verbatim from the buck2 fork's
//! `re_grpc_proto` / `google_grpc_proto` crates (which in turn track
//! `bazelbuild/remote-apis`). We compile everything into the one CapyFun crate —
//! no per-package split and no `extern_path` remap of the google packages — and
//! expose it via `tonic::include_proto!` in `src/remote/proto.rs`.
//!
//! System `protoc` resolves the `google/protobuf/*` well-known types from its
//! own include path; the remaining google deps (`api`, `longrunning`, `rpc`,
//! `bytestream`) and `build.bazel.semver` are vendored under `proto/`.

use std::io;

fn main() -> io::Result<()> {
    // The files we directly use; their imports (google.api / longrunning / rpc,
    // semver) are pulled in by protoc and generated alongside.
    let protos = &[
        "proto/build/bazel/remote/execution/v2/remote_execution.proto",
        "proto/google/bytestream/bytestream.proto",
    ];
    let includes = &["proto/"];

    for p in protos {
        println!("cargo:rerun-if-changed={p}");
    }
    println!("cargo:rerun-if-changed=proto/");

    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(protos, includes)
}
