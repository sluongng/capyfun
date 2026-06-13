# widget (fixture upstream)

A stand-in for `github.com/acme/widget` — the origin that `//third_party/widget`
imports in this example. It is deliberately tiny but shaped to exercise the
preview transform pipeline in `../third_party/widget/SRC`:

- code lives under `pkg/` (the `move(pkg -> lib)` structural transform), and
- it references `acme.internal/log` (the `replace(acme.internal/ -> "")` scrub).

`../materialize-widget.sh` builds a local **bare** repository from this
directory (with a couple of commits of history), then runs the real
`capyfun import` against it via `CAPYFUN_GITHUB_BASE`, materializing the content
under `third_party/widget/` in a throwaway monorepo — first-parent history
preserved, each commit tagged `CapyFun-Origin`, and the toolchain patch applied
on top as a `CapyFun-Patch` tip commit.
