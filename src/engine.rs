//! Git rewrite/projection engine.
//!
//! Houses the JOSH-shaped tree-prefix primitive and the commit replay logic for
//! import (and later, export). This is the only module that performs Git and
//! network I/O.
//!
//! Implemented starting in milestone M3.
