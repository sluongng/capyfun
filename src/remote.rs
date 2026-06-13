//! Remote Execution API (REAPI) backend for generative transforms.
//!
//! Dispatches `agent_transform` work to a REAPI server (BuildBuddy Cloud) as an
//! Action whose declared output is the materialized patch, so the server's Action
//! Cache doubles as CapyFun's agent-output cache. See
//! `docs/design/remote-execution.md`. The vendored protos and their nested
//! module tree live in [`proto`].
//!
//! This module is split into focused submodules:
//! - [`proto`] — generated REAPI + google bindings.

pub mod proto;
