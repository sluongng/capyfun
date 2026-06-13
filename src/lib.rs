//! CapyFun: code import/export subsystem for the TinyTree monorepo.
//!
//! The crate is organized as a pipeline:
//!
//! ```text
//! config  -> evaluate a CapyFun Starlark config into captured declarations
//! ir      -> normalize declarations into a deterministic, serializable IR
//! validate-> statically reject invalid IR before any Git mutation
//! engine  -> rewrite Git objects and replay/export commits
//! ```
//!
//! Config evaluation is pure; all Git and network I/O lives in [`engine`].

pub mod agent;
pub mod cargo;
pub mod config;
pub mod engine;
pub mod gomod;
pub mod ir;
pub mod npm;
pub mod reconcile;
pub mod server;
pub mod status;
pub mod transform;
pub mod validate;
pub mod vendorgen;
