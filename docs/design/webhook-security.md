# CapyFun Webhook Security

This document specifies authentication and hygiene for the automation server's
webhook endpoint: verifying the GitHub HMAC signature, and deduping/debouncing
events.

Read `../../CLAUDE.md` first, and `automation.md` for the server and its Safety
requirements (this doc makes the "verify webhook HMAC signatures; dedupe events"
line concrete).

> **Status:** **W1 (HMAC) and basic W2 (delivery-id dedup) are implemented**
> (`src/server.rs`, alongside the reactions work — see
> [`reactions.md`](reactions.md)). `POST /webhook` now fails closed without
> `CAPYFUN_WEBHOOK_SECRET`, verifies `X-Hub-Signature-256` over the raw body with
> a constant-time `hmac`+`sha2` compare (401 on failure), routes by
> `X-GitHub-Event`, and dedupes redeliveries by `X-GitHub-Delivery` via a bounded
> FIFO. Remaining: cross-ingress dedup by `(repo, ref, sha)` and the W3
> per-target debounce window.

## Why it matters

The webhook now drives a real reconcile (`ReconcileActor`), and a reconcile runs
**arbitrary upstream content through transforms** — including, for an
`agent_transform`, a coding agent over that content (see `automation.md` Safety).
An unauthenticated endpoint lets anyone forge a `PushEvent` for a subscribed repo
and trigger that pipeline at will: wasted work at best, an abuse vector for the
agent sandbox at worst. Authentication is a prerequisite for exposing the
endpoint at all.

Note the level-triggered design bounds the blast radius: a forged event can only
ask CapyFun to reconcile a target *it already tracks*, against the *real*
upstream state (the commit map is the source of truth, not the event payload). So
a forged event cannot inject content — but it can force-run the pipeline.
Authentication closes that, and dedup/debounce keep honest bursts cheap.

## HMAC verification

GitHub signs each webhook delivery with HMAC-SHA256 over the **raw request body**
using the App/repo's configured webhook secret, sent as:

```
X-Hub-Signature-256: sha256=<hex>
```

Verification:

1. Read the **raw body bytes** (verify before/independently of JSON parsing —
   parse-then-reserialize changes bytes and breaks the MAC). The current handler
   already reads the body into one buffer; verify against that exact buffer.
2. Compute `HMAC-SHA256(secret, raw_body)`.
3. **Constant-time** compare to the hex in the header. Reject on mismatch or a
   missing/`sha1`-only signature.
4. On failure respond **401** and do not parse or act. On success, proceed to
   `parse_webhook_push` → `match_event` → `ReconcileActor::act` as today.

### The secret

The webhook secret is a credential, handled like model credentials elsewhere in
CapyFun: **never** stored in the repo. Read from the environment (e.g.
`CAPYFUN_WEBHOOK_SECRET`), or a `credential = "env:NAME"`-style reference if the
server config grows one. If no secret is configured, the endpoint refuses webhook
POSTs (fail closed) rather than accepting unsigned payloads — with a clear
startup warning so the operator knows the webhook path is disabled until a secret
is set. `--once` polling and `GET /healthz` are unaffected.

### Crypto dependency

GitHub mandates HMAC-**SHA-256**, so the existing `blake3` dep does not apply.
Add the RustCrypto pair `hmac` + `sha2`, and use `Mac::verify_slice` (constant-
time) for the compare rather than rolling our own `==`. These are small, pure-
Rust, widely-used crates consistent with the project's libgit2/starlark stack.

## Event hygiene (dedupe + debounce)

Because reconcile is idempotent, dedupe and debounce are **optimizations, not
correctness** — but they keep the agent/transform pipeline from running
redundantly under honest event storms (a busy upstream, a webhook redelivery, the
same push seen via both GH Archive and a webhook).

- **Dedupe** by GitHub's delivery id (`X-GitHub-Delivery`) for exact
  redeliveries, and by `(repo, ref, sha)` for the same logical push arriving
  through multiple ingress paths. A small bounded LRU of recently-seen keys
  suffices; missing the cache just means an extra (idempotent) reconcile.
- **Debounce/batch** per target: collect a short window of triggers for a target
  and reconcile once, so a burst of pushes coalesces into a single run. The
  `ReconcileActor` already deduplicates target labels within one batch; this
  extends that across a time window.

Neither changes the correctness contract: a dropped or duplicated event only
shifts latency, never the converged state.

## Invariants (proposed)

- The webhook endpoint **rejects any unsigned or mismatched payload** (401) and
  does not parse or act on it; with no secret configured it fails closed.
- Verification is over the **raw bytes**, using a **constant-time** compare.
- The secret is **never** stored in the repo (env/credential reference only),
  consistent with how model credentials are handled.
- Dedupe/debounce are optimizations; correctness rests on idempotent reconcile +
  the commit map, never on exactly-once delivery.

## Milestones

- **W1 — HMAC verification.** Add `hmac`+`sha2`; verify `X-Hub-Signature-256`
  over the raw body with a constant-time compare; 401 on failure; fail closed
  when no secret is set. Unit tests with a known secret/body and matching,
  mismatching, and missing signatures.
- **W2 — Dedupe.** Bounded LRU on `X-GitHub-Delivery` and `(repo, ref, sha)`;
  cross-ingress dedup with the GH Archive path.
- **W3 — Debounce.** Per-target time-window batching before reconcile.

## Open questions

- **Per-installation secrets:** a GitHub App can have one webhook secret; a
  multi-tenant deployment may need per-installation secrets keyed by the delivery
  payload's installation id. Out of scope for v0 (single secret).
- **Replay window:** should deliveries carry a freshness/timestamp check, or is
  delivery-id dedup enough? (Likely enough, given idempotency.)
- **Config surface for the secret:** env var now; whether `serve` grows a typed
  `webhook(secret = "env:…")` rule later is a config-model question.
