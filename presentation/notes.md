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
**Transition:** "Here's the demo." → (run demo now, or after slide 6 — your call; see Demo section).

## Slide 5 — Use cases · 0:15
**Say:** "Scan every import for vulns; give it a goal like optimizing kernel changes for your specific hardware — which works because those profiles live in the tree; or migrate code against an upstream API. Same engine on export. And nothing's trusted blindly — Bazel CI verifies."
**Emphasis:** "agents propose, Bazel CI disposes" — this is your trust answer.

## Slide 6 — What's next · 0:10
**Say:** "Next: ship export to close the loop, scale onto Bazel's remote execution — dedup is free because output's content-addressed — and react to production signals, not just commits."
**Side-note callout (point at the box):** "And a side effect of config-as-code plus reproducible transforms: replay your own history through different agents and models to benchmark cost, speed, and quality — and because it's all normal Git history, you can bisect to find which change broke something."
**Close line:** "Import runs end-to-end today; export is the next edge, and it reuses all the same machinery."

---

## Demo · 1:00  *(hermetic — no network)*

**Preflight (off-screen, before you start):**
```sh
cd ~/work/misc/capyfun && cargo build --release && clear
```

**Run one command:**
```sh
examples/transforms/materialize-widget.sh
```

**Point at three beats (~15s each):**
1. `imported 2 commit(s) … tip layer: 1 patch commit` → "a real import, not a squash."
2. The history block — `CapyFun-Origin: <sha>` on each commit → "that's the commit map — provenance back to upstream."
3. `go.mod` → `toolchain go1.21.6` → "a tip patch layered on top of the faithful mirror."

**Close:** "History preserved, every file traceable, fully reproducible — and the same engine runs an LLM transform when you want one."

**Higher-risk alt (needs wifi):** `demo/run.sh` — lockfiles → `third_party/` tree (gen-cargo/npm → check → import/vendor). More impressive, but hits crates.io/npm/GitHub. Only if the network is solid.

---

## Q&A prep (anticipate these)

- **"How is this different from Copybara/ShipIt?"** — "Same model — origin, destination, recorded reference — but a Rust rewrite with a *closed, typed* transform vocabulary instead of a big DSL, plus generative (agent) transforms that stay reproducible via content-addressing. And it's import/export for one monorepo, not a sync pipe."
- **"Isn't letting an LLM rewrite code dangerous?"** — "Agents propose, Bazel CI disposes — every transform is verified imperatively, and the output is a content-addressed patch, so it's reviewable and reproducible, not a black box."
- **"Is export working?"** — "Import runs end-to-end today, including agent transforms. Export is modeled in config and IR — same transforms apply — but the execution that opens the PR is my next milestone. It reuses the import machinery."
- **"Why is import per-commit instead of a squash?"** — "Faithfulness: each mirrored commit maps 1:1 to its origin via the CapyFun-Origin trailer, so history and provenance survive, and re-import is incremental."
- **"Determinism with a non-deterministic LLM?"** — "Generation happens once; we materialize the agent's edits to a content-addressed patch and replay *that*. Re-imports are reproducible from the record; `--refresh` regenerates."
- **"What's actually built vs. designed?"** — "Built: import round-trip, imperative + generative transforms executing, vendoring, lockfile scaffolding, the GH-Archive event poller. 140+ tests. Designed/next: export PR, the acting reconciler, RBE scaling."

---

## Controls cheat-sheet
→ / Space — next · ← — back · **f** — fullscreen · **s** — toggle on-screen notes · **1–9** — jump · Home/End — first/last.
