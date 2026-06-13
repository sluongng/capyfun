# CapyFun Reactions: event-triggered generative automation

This document specifies **reactions** — CapyFun running a coding agent in
response to a GitHub event and opening a PR with the result. The first reaction
is `on_issue`: an issue labeled for an agent gets a prototype PR.

Read `../../CLAUDE.md` first, then `automation.md` (the level-triggered
reconciler and the GitHub App's role) and `transformations.md` (the
`agent`/`harness`/`model`/`prompt_template` rules and the agent execution path
this reuses).

> **Status:** `on_issue` is implemented. The config rule (`src/config.rs`), IR
> lowering + validation (`src/ir.rs`), the reaction engine (`src/react.rs`), the
> GitHub-App `Forge` (`src/forge.rs`), and the webhook wiring with HMAC
> verification (`src/server.rs`) are all in place, with `capyfun react
> --issue <payload> [--dry-run]` as the manual entry point and
> `scripts/smoke-react.sh` exercising the full clone→agent→commit→push→PR loop
> hermetically. `on_push` / `on_tag` reactions reuse this exact shape and are the
> natural next increment.

## Thesis: import/export move code; reactions *react*

The reconciler (`automation.md`) reacts to upstream **pushes/releases** by moving
code across a tree boundary — the action is always an import, vendor, or export.
A **reaction** is the generative counterpart: it reacts to a GitHub event by
running a declared **agent** over a checkout of the repo and opening a PR. The
event is the trigger; the reaction is a coding-agent run, not a tree move.

This reuses the machinery that already exists for `agent_transform`
(`transformations.md`): the `harness`/`model`/`agent`/`prompt_template` rules,
the `AgentInvocation` + `AgentRunner` execution seam, and credential resolution.
A reaction is "point that same agent at a freshly-cloned repo, with the issue as
context, and turn its edits into a PR."

### A deliberate scope change

CapyFun's invariants said it is **"Not a review product, CI system, or forge."**
An issue→agent→PR bot *is* forge automation. We take that on deliberately: it is
a natural evolution of the agent-transform engine, not a new subsystem. The
invariant is amended (see `CLAUDE.md`): CapyFun does not *replace* the forge's
review process — it still **opens a PR for a human to review**, never merges —
but it does now *act on* forge events and open PRs in response.

## The `on_issue` rule

A reaction is a typed rule, instantiated in an SRC file like any other (closed
vocabulary — no arbitrary code). It composes the existing agent tool rules:

```python
# //automation/SRC
on_issue(
    name   = "prototype-assigned",
    repo   = "acme/backend",            # owner/name the App is installed on
    action = "labeled",                 # optional; default = opened + labeled
    label  = "assign-agent",            # optional; default = any label
    agent  = "//tools/agent:reviewer",  # an `agent` rule, by label
    prompt = template("//tools/agent/prompts:prototype-issue"),
)
```

Lowering resolves `agent` and `prompt_template` against the same tables as
`agent_transform`, validates the `repo` slug and the `action` against a closed
set (`opened`/`edited`/`reopened`/`labeled`/`assigned`), and folds reactions into
the per-package name-uniqueness check. See `examples/reactions/` for a worked
package set.

### Issue context vars

The prompt template is rendered with issue-specific context vars (the reaction
analogue of `transformations.md`'s import context vars):

| Var | Meaning |
|---|---|
| `{{issue_title}}` | the issue title |
| `{{issue_body}}` | the issue body |
| `{{issue_number}}` | the issue number |
| `{{issue_url}}` | the issue's HTML URL |
| `{{repo}}` | the `owner/name` the issue is on |

User `vars` (literal strings or `//`-anchored file labels) are substituted first,
exactly as for `agent_transform`, then the issue context vars.

## The flow

For a matched issue event:

1. **Clone** the repo at its default branch into a temp checkout (URL + auth from
   the `Forge`).
2. **Branch** `capyfun/issue-<n>`.
3. **Run the agent** in that checkout (the `AgentRunner` edits files in place,
   reusing the `LiveRunner` that drives `agent_transform`).
4. **No edits → no PR** (idempotent at the no-change boundary).
5. **Commit** the edits as CapyFun, with `CapyFun-Agent: <id>` and
   `CapyFun-Issue: <repo>#<n>` provenance trailers.
6. **Push** the branch and **open a PR** (base = default branch, body `Closes
   #<n>`) via the `Forge`.

Both the `Forge` and the `AgentRunner` are traits, so the whole loop runs
hermetically in tests against local bare repos with a fake agent.

## GitHub App identity (the `Forge`)

Cloning a private owned repo, pushing a branch, and opening a PR all need an
identity. CapyFun authenticates as a **GitHub App**:

1. Mint a short-lived RS256 **App JWT** from the App private key (`iss` = app id,
   `iat`/`exp` ≤ 10 min).
2. Exchange it for an **installation access token**
   (`POST /app/installations/{id}/access_tokens`). The installation id comes from
   the webhook payload (`installation.id`).
3. Use that token for git clone/push (`x-access-token:<token>@github.com/...`) and
   the PR REST call (`POST /repos/{owner}/{repo}/pulls`).

The App id and private-key path are read from the environment
(`CAPYFUN_GITHUB_APP_ID`, `CAPYFUN_GITHUB_APP_KEY`), never the repo — consistent
with model-credential handling. A `LocalForge` (driven by `CAPYFUN_GITHUB_BASE`,
the same escape hatch imports use) clones/pushes local bare repos and records the
PR request, for hermetic demos and tests.

## Webhook authentication (HMAC) — `webhook-security.md` W1, done here

A reaction runs a coding agent over repo content, so the webhook endpoint must
not be forgeable. `POST /webhook` now:

- **fails closed** when `CAPYFUN_WEBHOOK_SECRET` is unset (rejects all POSTs with
  a startup warning; `/healthz` and polling are unaffected);
- verifies `X-Hub-Signature-256` (HMAC-SHA256 over the **raw body**, constant-time
  compare) before parsing or acting — `401` on mismatch/missing;
- routes by `X-GitHub-Event` (`issues` → reactions, `push`/other → reconcile,
  `ping` → pong);
- **dedupes** by `X-GitHub-Delivery` (a bounded FIFO) so a redelivery does not
  re-run the agent.

As in `webhook-security.md`, the level-triggered design already bounds the blast
radius (a forged event can only ask CapyFun to act on a repo it already declares
a reaction for); HMAC closes the force-run gap, and dedup keeps honest bursts
cheap.

## Safety

Reactions execute a coding agent over repo content and open PRs, so the
`automation.md` Safety notes apply with extra force:

- **HMAC fail-closed** is the gate; never expose the endpoint without a secret.
- **PR-for-review, never auto-merge** — a human (or policy) on the destination
  approves.
- Run the agent in a **sandbox** with least-privilege tokens and a network policy
  (still a follow-on; today the agent runs in a temp checkout).
- Re-run idempotency relies on delivery-id dedup plus the fixed
  `capyfun/issue-<n>` branch.

## Milestones

- **R1 — `on_issue` (done).** The rule, IR + validation, reaction engine,
  GitHub-App `Forge`, webhook HMAC + routing + dedup, `capyfun react` CLI,
  example, and a hermetic smoke test.
- **R2 — `on_push` / `on_tag`.** Same rule shape + matcher + engine, triggered by
  push/tag events (e.g. run an agent on a release tag).
- **R3 — agent sandbox.** Network/filesystem policy around the agent run.

## Open questions

- **PR lifecycle:** update an existing prototype PR vs. open a new one when an
  issue is re-triggered? (Today: a fixed `capyfun/issue-<n>` branch; re-runs rely
  on dedup.)
- **Comment-driven triggers:** `issue_comment` (`/capyfun prototype`) as an
  action, alongside `labeled`.
- **Per-installation webhook secrets** for a multi-tenant App (see
  `webhook-security.md` open questions).
- **Cost controls:** rate-limit / budget reactions so a label storm cannot fan
  out unbounded agent runs.
