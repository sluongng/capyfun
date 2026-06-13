//! Static validation helpers over lowered IR.
//!
//! Each helper appends human-readable diagnostics (prefixed with the offending
//! rule's label) to an accumulator. The orchestration lives in [`crate::ir`];
//! diagnostics are sorted there for deterministic output.

use crate::ir::Import;

/// Validate that `repo` is a well-formed GitHub `owner/name` slug.
pub(crate) fn check_slug(label: &str, repo: &str, errors: &mut Vec<String>) {
    let parts: Vec<&str> = repo.split('/').collect();
    let ok = parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
    if !ok {
        errors.push(format!(
            "{label}: repo `{repo}` is not a valid \"owner/name\" slug"
        ));
    }
}

/// Validate a normalized relative path: non-empty, not absolute, and free of
/// empty, `.`, or `..` segments. `kind` names the field for diagnostics.
pub(crate) fn check_rel_path(label: &str, kind: &str, path: &str, errors: &mut Vec<String>) {
    if path.is_empty() {
        errors.push(format!("{label}: {kind} path is empty"));
        return;
    }
    if path.starts_with('/') {
        errors.push(format!(
            "{label}: {kind} path `{path}` must be relative, not absolute"
        ));
        return;
    }
    if path
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        errors.push(format!(
            "{label}: {kind} path `{path}` must not contain empty, `.`, or `..` segments"
        ));
    }
}

/// Two paths conflict when they are equal or one is an ancestor of the other.
fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
    // `short` is an ancestor of `long` iff `long` starts with `short/`.
    long.starts_with(&format!("{short}/"))
}

/// Reject imports whose destination directories overlap (equal or nested).
pub(crate) fn check_destination_overlap(imports: &[Import], errors: &mut Vec<String>) {
    for i in 0..imports.len() {
        for j in (i + 1)..imports.len() {
            if paths_overlap(&imports[i].dest, &imports[j].dest) {
                errors.push(format!(
                    "destination overlap: {} -> `{}` and {} -> `{}`",
                    imports[i].label, imports[i].dest, imports[j].label, imports[j].dest
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation() {
        let mut e = Vec::new();
        check_slug("//x:y", "acme/backend", &mut e);
        check_slug("//x:y", "acme/back.end_2-x", &mut e);
        assert!(e.is_empty(), "{e:?}");
        check_slug("//x:y", "noslash", &mut e);
        check_slug("//x:y", "too/many/slashes", &mut e);
        check_slug("//x:y", "acme/bad space", &mut e);
        assert_eq!(e.len(), 3);
    }

    #[test]
    fn rel_path_validation() {
        let mut e = Vec::new();
        check_rel_path("//x:y", "into", "a/b", &mut e);
        assert!(e.is_empty());
        check_rel_path("//x:y", "into", "/abs", &mut e);
        check_rel_path("//x:y", "into", "../up", &mut e);
        check_rel_path("//x:y", "into", "a//b", &mut e);
        check_rel_path("//x:y", "into", "", &mut e);
        assert_eq!(e.len(), 4);
    }

    #[test]
    fn overlap_detection() {
        assert!(paths_overlap("a/b", "a/b"));
        assert!(paths_overlap("a", "a/b"));
        assert!(paths_overlap("a/b/c", "a/b"));
        assert!(!paths_overlap("a/b", "a/c"));
        assert!(!paths_overlap("ab", "a/b"));
        assert!(!paths_overlap("a/bc", "a/b"));
    }
}
