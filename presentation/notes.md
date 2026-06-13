# CapyFun — speaker notes

**Format:** 2:00 talk (slides 1–6) + 1:00 demo (slide 7). Speaking rate ≈ 2.3 words/sec.
**In-deck notes:** press **s** to show the short version on-screen while rehearsing.

| # | Slide | Target |
|---|-------|--------|
| 1 | Intro | 0:15 |
| 2 | Thesis | 0:25 |
| 3 | The loop (climax) | 0:30 |
| 4 | Generative + composable | 0:25 |
| 5 | Use cases | 0:15 |
| 6 | What's next | 0:10 |
| — | **Demo** | 1:00 |

If you run long, cut from slides 1 and 6 — never from slide 3.

---

## Slide 1 — Intro · 0:15
**Say:** "I'm Son. At BuildBuddy I help big companies run Bazel in huge monorepos. On the side I'm building tinytree — a forge made for monorepos. This is its import/export engine, CapyFun."
**Emphasis:** establish credibility fast (Bazel + monorepos), then pivot to the project.
**Transition:** "So why monorepos, and why now?"

## Slide 2 — Thesis · 0:25
**Say:** "Coding agents are trained on simple filesystem tools, so a monorepo is the perfect context interface for them. But monorepo tooling is terrible unless you're Google or Meta. With an infinitely-scaling, lazily-loaded tree, you can version-control things you used to hide — your models, your external deps — and agents can reach all of it."
**Emphasis:** the punchline is "version-control the things you used to banish to LFS / .gitignore." That sets up the kernel-hardware use case later.
**Transition:** "The hackathon asks: where's the orchestration loop?"

## Slide 3 — The loop · 0:30  *(your differentiator — give it room)*
**Say:** "It's not inside one repo — it's *between* them. You have a monorepo, I have one, the OSS world is a mesh, all emitting and reacting to events. Each node runs a level-triggered reconciler — events are hints, the commit map is truth. My agents react to what your agents changed, transform it, and emit their own events. That's the outer loop: agents collaborating through code."
**Point at:** the dashed "outer loop" arc; then the legend line (inner vs outer).
**Optional zinger:** "The agents coordinate stigmergically — through the codebase, not a chat channel."
**Transition:** "So what does a transform actually look like?"

## Slide 4 — Generative + composable · 0:25
**Say:** "The interesting transforms are generative — run an agent over the incoming change. And agents are composed in Starlark, the language Bazel and Buck2 shops already use. Pick a harness, a model, template the prompt — all as code, swappable. The output is captured as a content-addressed patch, so the import stays reproducible."
**Point at:** the three pills (harness × model × prompt / content-addressed / reproducible).
**Footer side-note (if time):** "A side effect: because it's config-as-code and the output's reproducible, you can replay your history through different agents and models to benchmark cost, speed, and quality — and git-bisect to find which change broke something."
**Transition:** "Here's the demo." → (run demo now, or after slide 6 — your call; see Demo section).

## Slide 5 — Use cases · 0:15
**Say:** "Scan every import for vulns; give it a goal like optimizing kernel changes for your specific hardware — which works because those profiles live in the tree; or migrate code against an upstream API. Same engine on export. And nothing's trusted blindly — Bazel CI verifies."
**Emphasis:** "agents propose, Bazel CI disposes" — this is your trust answer.

## Slide 6 — What's next · 0:10
**Say:** "Import and export both run end-to-end today, with agent transforms verified and cached. Next: scale onto Bazel's remote execution — dedup is free because output's content-addressed — and react to production signals, not just commits."
**Close line:** "The round-trip works today; what's left is running it at fleet scale and reacting to more than commits."

---

## Demo · 1:00  *(hermetic — no network)*

**Preflight (off-screen, before you start):**
```sh
cd ~/work/misc/capyfun && cargo build --release && clear
```

**Run one command — the full loop (hermetic, no API key):**
```sh
demo/full-loop.sh
```

**Point at the beats (~12s each):**
1. STEP 1–2: upstream renames `Connect → New`; the import runs an **agent transform** to migrate the broken caller → "an agent edits code as a *config edge between repos*, not an ad-hoc Claude session."
2. STEP 3: `go test` passes → "the agent is *verified*, with a fail→feed-back→retry loop behind it."
3. STEP 4: export branch with the internal-only line scrubbed → "the other direction, as a PR — agents propose, review disposes."
4. STEP 5 + SUMMARY: `replay … cache 1h/0m`, model calls 0, $0.00 → **"the expensive model runs once on a cache miss; every replay is deterministic and free."**

**Close:** "Reproducible, reviewable, replayable LLM transforms across repo boundaries — the whole pitch, in one command."

**Backup beat (provenance):** `CapyFun-Origin` on each mirror commit, `CapyFun-Agent` on the transform commit — every file traces back to its origin and the edge that produced it.

**Measurable follow-up:** `scripts/eval-agents.sh` — the 3-fixture eval table (see `docs/evals.md`).

**Alt demos:** `examples/transforms/materialize-widget.sh` (mirror + tip patch, no agent); `demo/run.sh` (multi-language vendoring — needs wifi).

---

## Q&A prep (anticipate these)

- **"How is this different from Copybara/ShipIt?"** — "Same model — origin, destination, recorded reference — but a Rust rewrite with a *closed, typed* transform vocabulary instead of a big DSL, plus generative (agent) transforms that stay reproducible via content-addressing. And it's import/export for one monorepo, not a sync pipe."
- **"Isn't letting an LLM rewrite code dangerous?"** — "Agents propose, the verifier disposes — each transform runs a verifier (e.g. `go test`) with a fail→feed-back→retry loop, and only the *verified* state is materialized as a content-addressed patch. Reviewable and reproducible, not a black box."
- **"Is export working?"** — "Yes — import *and* export run end-to-end today. Export strips the prefix, pushes a branch, and opens a PR (or prints the `gh` command for a local/hermetic destination). The full-loop demo shows both directions."
- **"Why is import per-commit instead of a squash?"** — "Faithfulness: each mirrored commit maps 1:1 to its origin via the CapyFun-Origin trailer, so history and provenance survive, and re-import is incremental."
- **"Determinism with a non-deterministic LLM?"** — "Generation happens once; we materialize the agent's edits to a content-addressed patch keyed on (input subtree, prompt, agent identity) and replay *that*. Re-imports are reproducible and free; `--refresh` regenerates. The eval harness proves it with a deterministic mock executor."
- **"How do you show cost/quality?"** — "`scripts/eval-agents.sh` reports a table: success, verifier, runtime, replay time, and cache miss→hit. The model only runs on a cache miss; replays are $0.00."
- **"What's actually built vs. designed?"** — "Built: import + export round-trip, imperative + generative transforms executing with a content-addressed cache, the verify→retry loop, three executors (local/remote-REAPI/fixture), an eval harness, the reconciler, vendoring, lockfile scaffolding, the GH-Archive poller. 220+ tests. Designed/next: broader triggers, richer source rules, fleet-scale quota/spend governance."

---

## Controls cheat-sheet
→ / Space — next · ← — back · **f** — fullscreen · **s** — toggle on-screen notes · **1–9** — jump · Home/End — first/last.
