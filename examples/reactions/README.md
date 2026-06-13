# Reactions example

A worked `on_issue` reaction: when an issue on `acme/backend` is **labeled
`assign-agent`**, CapyFun runs a coding agent over a checkout of the repo and
opens a PR prototyping the issue. This is the generative counterpart to
import/export — see `docs/design/reactions.md`.

Layout (a CapyFun package per directory, Bazel-style):

```
SRC                              monorepo() anchor
tools/harness/SRC                harness(claude_code)
tools/models/SRC                 model(opus)
tools/agent/SRC                  agent(reviewer) = harness + model
tools/agent/prompts/SRC          prompt_template(prototype-issue)
tools/agent/prompts/*.tmpl       the prompt (uses {{issue_*}} context vars)
automation/SRC                   on_issue(...) — the reaction edge
```

## Try it

Inspect the lowered reaction:

```sh
capyfun check --root examples/reactions   # IR includes `reactions`
capyfun config --root examples/reactions  # lists the on_issue rule
```

Dry-run a reaction against a saved webhook payload (no clone, no agent, no PR):

```sh
capyfun react --issue testdata/issue-labeled.json \
  --root examples/reactions --dry-run
```

Run it for real. CapyFun authenticates as a **GitHub App** to clone, push, and
open the PR; set `CAPYFUN_GITHUB_APP_ID` and `CAPYFUN_GITHUB_APP_KEY` (path to
the App's PEM private key), and have a logged-in agent harness (`claude`). For a
hermetic local demo, point `CAPYFUN_GITHUB_BASE` at a directory of local bare
repos instead:

```sh
capyfun react --issue testdata/issue-labeled.json --root examples/reactions
```

The webhook path (`capyfun serve`) drives the same reaction on a live `issues`
event, after verifying the `X-Hub-Signature-256` HMAC.
