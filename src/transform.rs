//! Transform IR: the normalized, validated form of a `transforms = [...]` entry.
//!
//! Config-side transform values (constructed by the `replace`/`move`/`copy`/
//! `rewrite_message` value constructors in [`crate::config`]) are lowered here
//! into a [`Transform`], grouped by [`Phase`], and statically validated before
//! the engine touches Git.
//!
//! The vocabulary is closed and typed (see `docs/design/transformations.md`).
//! The imperative, structural transforms live here; the generative
//! `agent_transform` (a tip-phase transform) is added by a separate integration
//! and slots in as a new [`Transform`] variant with a `Phase::Tip` mapping —
//! the enum and its lowering are structured so that addition is mechanical.

use serde::Serialize;

/// When a transform runs relative to the import pipeline.
///
/// Structural transforms rewrite every mirrored commit (filter-repo style);
/// tip transforms apply once as a rebased layer on top of the mirror tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Applied per replayed commit, as the first-parent mirror is built.
    Mirror,
    /// Applied once, as a rebased layer on top of the mirror tip.
    Tip,
}

/// A single normalized, validated transform in an import/export pipeline.
///
/// All paths are relative to the imported subtree (upstream-shaped), so a
/// transform is portable. The variants here are the imperative, structural
/// transforms; each reports its [`Phase`] via [`Transform::phase`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transform {
    /// sed-like search/replace across blob contents of files matching `paths`.
    Replace {
        before: String,
        after: String,
        /// Glob patterns (subtree-relative) selecting the files to rewrite.
        paths: Vec<String>,
        /// When true, `before` is a regular expression and `after` may use
        /// `$N`/`${name}` capture references; otherwise both are literal.
        regex: bool,
    },
    /// Relocate a file or directory within the imported subtree.
    Move { src: String, dst: String },
    /// Duplicate a file or directory within the imported subtree.
    Copy { src: String, dst: String },
    /// Rewrite each commit message: optional body substitution plus trailer
    /// strip/add. The engine always preserves the `CapyFun-Origin`/
    /// `CapyFun-Import` trailers regardless of this transform.
    RewriteMessage {
        /// Optional body search text (a regex when `regex` is true).
        before: Option<String>,
        /// Replacement for `before`; required iff `before` is set.
        after: Option<String>,
        regex: bool,
        /// Trailer keys to remove (e.g. `Internal-Review`).
        strip_trailers: Vec<String>,
        /// Whole trailer lines to append (e.g. `Reviewed-by: ...`).
        add_trailers: Vec<String>,
    },
}

impl Transform {
    /// The phase this transform runs in, decided by its kind.
    pub fn phase(&self) -> Phase {
        match self {
            Transform::Replace { .. }
            | Transform::Move { .. }
            | Transform::Copy { .. }
            | Transform::RewriteMessage { .. } => Phase::Mirror,
        }
    }

    /// Human-readable kind, for diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Transform::Replace { .. } => "replace",
            Transform::Move { .. } => "move",
            Transform::Copy { .. } => "copy",
            Transform::RewriteMessage { .. } => "rewrite_message",
        }
    }
}
