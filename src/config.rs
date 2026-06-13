//! CapyFun Starlark config evaluation.
//!
//! Defines the typed builtins (`monorepo`, `import_commits`, `export_pr`) and
//! captures their calls into in-memory declarations. Evaluation is pure: no Git
//! or network I/O happens here.
//!
//! Implemented in milestone M1.
