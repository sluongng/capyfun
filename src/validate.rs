//! Static validation over the normalized IR.
//!
//! Rejects invalid configs before any Git mutation: path escapes, absolute
//! paths, overlapping import destinations, duplicate names, empty remotes/refs.
//!
//! Implemented in milestone M2.
