"""Reusable CapyFun macros for importing GitHub repositories.

This is a *library* (the `.bzl` of CapyFun): it defines macros and constants but
declares no source rules itself. A top-level builtin call here is an error —
rules are only instantiated from SRC files (directly or via these macros).

Loaded from SRC files with a `//`-anchored path:

    load("//lib/github.star", "vendored")
"""

def vendored(name, repo, ref = "refs/heads/main", patches = []):
    """Import an upstream GitHub repo as a vendored dependency.

    Convention: defaults the tracked ref to the upstream main branch. This is a
    macro: it expands to a single `github_import` builtin call and adds no new
    projection logic. With distributed SRC files, the import lands in the
    package directory that declares it (`into` defaults to the SRC's package),
    so this macro does not compute a destination path.
    """
    github_import(
        name = name,
        repo = repo,
        ref = ref,
        patches = patches,
    )
