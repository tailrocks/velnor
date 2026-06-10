# Velnor Mission

Read this first; every decision and trade-off serves it. The execution
sequence lives in [master-plan.md](master-plan.md) (the top-level plan);
runner-internal implementation detail lives in [roadmap.md](roadmap.md).

## What Velnor is

Velnor is a **GitHub Actions-compatible, self-hosted runner built for Rust
projects** — not a generic CI runner. It appears to GitHub as a self-hosted
runner (V2 JIT / broker / run-service / Results Service only), runs assigned
Linux jobs in Docker containers, and executes the known action surface through
**native Rust adapters** instead of marketplace JavaScript or Docker action
bundles.

Because it is self-hosted on owned hardware and Velnor authors its own
execution and output, it is **faster, cheaper, nicer, and more informative**
than GitHub-hosted runners — proven in production: 2.5–3× faster Rust jobs on
`ChainArgos/java-monorepo`, with live per-line log streaming in the GitHub UI.

## The four pillars

1. **Fastest possible.** Beat GitHub-hosted runners on wall-clock and on
   queue-to-first-log latency.
   - Exploit the beefy host: parallel slots, warm caches, warm broker
     sessions; eventually dynamic slot autoscaling and multi-host scale-out.
   - Native Rust adapters skip JS/Docker action startup entirely.
   - Aggressive Rust-aware caching (cargo registry/git, target dirs, sccache,
     buildkit layers) — we own the host and the storage.
   - CI/CD speed is a first-priority mandate across the whole estate
     (master-plan §3a): everything cacheable is cached, everything
     parallelizable runs in parallel, and a rerun of an unchanged pipeline
     must re-download and recompile nothing.

2. **Rust-first, not generic.** Everything is prioritized around Rust.
   - Pre-cached and pre-optimized for the **latest** toolchain and tools —
     exact-pinned in the job image and bumped by Renovate, never `latest`
     tags. This is a hard rule.
   - The job image and adapters assume modern Rust CI (fmt, clippy, check,
     test, nextest, mise, mold, sccache, Docker/Buildx) plus the estate's
     shared tooling (gh, hadolint, just, protoc).

3. **Nicer, more informative output.** Velnor authors its own log stream, so
   the UI experience is **equal-or-better** than GitHub — live per-line
   streaming, grouped, colored, timestamped — never less informative.
   (User-command output passes through verbatim; Velnor frames it but does
   not rewrite it.)

4. **Cheaper.** Self-hosting on owned hardware avoids paying per-minute for
   the same (or better) capability. Faster + warm caches + parallelism
   compound the savings.

## Hard rules (inherited by every prompt and change)

- **The mandates in [master-plan.md](master-plan.md) §3a are law**: universal
  caching with rerun-idempotency, performance-first, estate unification
  (jackin is the pipeline source of truth), the three-repo dual-lane policy
  (agent-brown, java-monorepo, blockchain-nodes — Velnor default, github/both
  selectable), and switch-readiness for every other estate repo.
- **No JavaScript product path** — every `uses:` in the estate gets a native
  Rust adapter matching the latest upstream behavior (master-plan P3b); the
  node sidecar is a diagnostic fallback only.
- **Fixture is the contract** — fix Velnor, never weaken
  `tailrocks/velnor-actions-fixture`.
- **`actions/runner` is the protocol source of truth** —
  <https://github.com/actions/runner>. Don't guess.
- **Latest protocol path only** (Results Service, V2 broker, JIT configs;
  latest crate majors).
- **Rust-first automation** — prefer `velnor-tools` subcommands over new
  shell/Python.
- **Operations are bulletproof by design** (master-plan P1, shipped in
  0.1.5–0.1.6): the daemon never exits on failures, diagnoses bad credentials
  precisely, reports via sd_notify, and the doctor timer makes a dead fleet
  loud.

## How to use this in a change

Continuously ask:

- Is this **faster** than the GitHub-hosted equivalent? If not, why — and can
  parallelism or warmer caches close the gap?
- Is this **Rust-optimal** — latest toolchain, best cache strategy?
- Is the **output** at least as informative and nicer than GitHub's?
- Does this keep Velnor **cheaper** to operate than hosted minutes?
- Does it hold across the **whole estate** (propagate improvements; no
  per-repo drift)?

Capture timing and cache evidence so improvements are measurable.
