# CI speedup research — 2026-08-25

Measured program to make Velnor the fastest CI/CD platform for the estate
(`ChainArgos/java-monorepo`, `jackin-project/jackin`, `tailrocks/tablerock`,
`tailrocks/parallax`). Every number below comes from the GitHub API job records
and bake logs of completed runs fetched 2026-08-24/25, or from live sentry
inspection. Non-normative research; the unified-CI contract and strict
capability contract still govern execution.

## 1. Measured baselines (successful runs, main/dispatch)

| Repo / run | Dominant job | Job wall | Queue | Notes |
|---|---|---:|---:|---|
| java-monorepo `rust-docker.yml` | Build and publish Docker images (Velnor) | **23–28 min** | ≤2 min | single bake step = 27.8 min; one stage (`processor-compare-app builder 4/4`) = **1194 s** |
| parallax dispatch run | velnor lane | 5.5 min | **425 s** | queue ≫ run; fleet starvation at peak |
| tablerock dispatch run | velnor lane vs github lane | **5.2 vs 3.55 min** | 96 s | Velnor lane slower than GitHub lane |
| jackin PR check sweep | 6 micro-jobs | 0.3–0.5 min each | 20–53 s each | overhead-dominated: queue+boot ≈ work |
| jackin CI (push/main) | native-usage-menu-bar | **1592 s** of a 1613 s run | — | macOS packaging blocks Linux CI |

## 2. The dominant bottleneck, root-caused

`java-monorepo/.github/workflows/rust-docker.yml` bakes all affected Rust
service images with:

```yaml
*.cache-from=type=gha,scope=rust-workspace
*.cache-to=type=gha,scope=rust-workspace,mode=max
```

`type=gha` is a **GitHub-hosted-only backend** (needs
`ACTIONS_CACHE_URL`/`ACTIONS_RESULTS_URL` + `ACTIONS_RUNTIME_TOKEN`). On
Velnor it is absent, so every bake is fully cold: crates.io re-download plus a
~1200 s workspace dependency compile inside one builder stage, repeated per
run. The same class of bug exists in jackin `construct-ci-e2e` (gha scope),
tablerock's native trio and parallax's `cargo-target` tarball caches
(sha-keyed, per-commit upload).

**Program P1 — Velnor-native BuildKit cache service.** Implement the GitHub
cache contract natively in velnor-runner so `type=gha` works on every lane with
byte-identical YAML:

- v1 Twirp `_apis/artifactcache` (reserve/PUT/GET) and v2 Results-Service
  CacheService (CreateCacheEntry/FinalizeCacheEntryUpload/GetCachedContentURL),
  auto-selected exactly as buildkit does via `ACTIONS_CACHE_SERVICE_V2`.
- Export `ACTIONS_CACHE_URL`, `ACTIONS_RESULTS_URL`, `ACTIONS_RUNTIME_TOKEN`,
  `ACTIONS_CACHE_SERVICE_V2` into step env.
- Blob store under the daemon-shared durable root, scoped by
  `<repo>/<scope>`, honoring the estate cache policy: bounded cardinality,
  LRU + age eviction, trust-scope separation, quota accounting.

Effect estimate: java-monorepo Velnor bake 28 → ~5 min warm (dep-tree layers
hit); fixes tablerock lane parity and parallax/jackin gha-scoped caches with
zero YAML changes (portability law satisfied).

### Measurement correction — 2026-08-25

The current Java workflow uses registry cache as the primary backend and keeps
`type=gha` as an `ignore-error` secondary. The Velnor lane proved that the
GitHub endpoint passthrough is usable: run `32832916776` imported both `gha`
and registry manifests and completed its Docker job in **188 s**; the
preceding no-registry-primary run `32830746424` took **504 s**. That is a
316-second (**62.7%**) reduction, before any native Velnor CacheService work.

Velnor still does not implement a CacheService/Twirp backend; it forwards the
workflow endpoint and token. Native cache service remains a separate research
track, not the current Java bottleneck. Do not claim the old 23–28 minute cold
result for the post-PR #1976 workflow.

## 3. Ranked bottleneck list

1. **Fleet admission latency**: parallax waited 425 s queued; jackin micro-jobs
   lose more time to queue+JIT boot than to work. Work items: keep N pre-warmed
   JIT registrations per slot instead of discard-per-cycle, measure pickup SLO
   from forensics `job-timing` records, and expose a fleet saturation signal so
   capacity grows before queues form.
2. **Conditional native BuildKit CacheService**: Velnor currently forwards
   GitHub's cache endpoint and the Java registry-primary workflow is fast, but
   workflows without a registry cache still depend on GitHub's remote service.
   Implement only after a scoped protocol canary proves a material win.
3. **kestra-build-publish.yml (java-monorepo)** builds four images with bare
   `docker buildx` and no `cache-from/to`; only an exists-check skips them.
   Give it the registry-buildcache pattern below.
4. **Registry-primary docker caching everywhere** (shipped pattern): jackin
   `construct.yml` already does `type=registry,ref=<image>:buildcache-<arch>`
   written by main, restored by both lanes. Ported to java-monorepo
   `rust-docker.yml` this cycle (PR chainargos/java-monorepo#1976) as the
   immediate fix while P1 lands; expected 28 → ~5 min warm without any runner
   change.
5. **Redundant target-layer caches**: parallax keys `target/` archives with
   `github.sha` (new archive per commit), tablerock native trio archives whole
   `target/`, jackin release stacks Swatinem/rust-cache on top of sccache.
   Delete them; sccache (already present) owns compiler outputs per doctrine.
6. **jackin macOS menu-bar job on main CI**: 26 min pushes dominated by one
   1592 s packaging job — move to desktop-cadence/release or split with
   DerivedData caching.
7. **parallax storage-integration cold bring-up**: 316 s warm vs 4841 s cold;
   seedable service containers collapse the cold case.

## 4. Unified workflow standard (cross-org)

Both research passes agree with the marked contract; the deltas are:

- One canonical workload-template shape next to generated `ci.yml`:
  classify (≤5 min metadata cap) → work (canonical inline `matrix.config`
  expression, explicit measured timeouts, single-writer flags) → required
  aggregate gate; plural `lanes` selector with org-derived defaults.
- Cache stack, exactly one mechanism per layer:
  - Rust lanes: mold/wild linker + sccache v0.16.0 local 20 G + shared cargo
    registry composite; never raw `target/` archives; never rust-cache beside
    sccache.
  - Docker lanes: registry-primary buildcache written by main writer
    (`mode=max`); `type=gha` permitted only as `ignore-error` secondary until
    P1 makes it real everywhere.
- Bump java-monorepo's `velnor-actions` pins 2026.8.30 → current mirrored
  release (last byte-level divergence).
- Fix jackin `.github/AGENTS.md` default-lane contradiction (contract:
  `jackin-project/*` defaults github).

Per-repo file deltas are enumerated in the audit transcript
(subagent session `ses_fc81af838ffeCJIQ5F34kFfS4n`) and reduce to: jmono pins
+ kestra cache + rust-docker cache (PR #1976), jackin AGENTS.md/rust-cache/
sccache-pin, tablerock native-trio target-cache removal, parallax sha-keyed
target-cache removal + mcp-evals lanes input.

## 5. External adoption verdicts (research session `ses_fc81b32e5ffenEeP0jeOER8uWU`)

ADOPT-NOW:

1. **wild linker** (`wild-linker/wild`) for Rust lanes — non-incremental
   already ≈ mold-class; `-Clink-arg=--ld-path=wild`; gate on link share ≥10 %
   of build. mold stays the C/C++-heavy fallback (MIT, 3.9× lld).
2. **Kache** (`kunobi-ninja/kache`) as second approved compiler-cache backend —
   blake3-of-normalized-invocation keys survive fresh clones/cross-runner;
   already pinned as canary in the storage decision; keep sccache alternative.
3. **aube** (`jdx/aube`) for Node install steps in jobs that still install JS
   tooling (claimed 2.5–6.9× installs).

BORROW-IDEA:

- **b0nkers** container-from-syscalls (~37 ms start) — research track for
  slot-pickup/container-start SLO once OCI surface exists.
- **canopy** crate decomposition — parity corpus for workflow/expression
  parsing tests in the fixture.
- **buffa** zero-copy protobuf views if Twirp serialization ever shows in
  traces.

SKIP (no CI-latency surface): ripgrep, tree-sitter, turso, fff, boa,
fast-down/core.

Compiler-front-end watchlist (do not block on): parallel rustc frontend
(MCP#1005), Cranelift backend for non-final test artifacts (10–15 % avg,
asm! gaps), `-Zshare-generics=y -Zthreads=N`.

## 6. Measurement protocol going forward

- Every change lands with before/after numbers from `gh api` job records
  (job wall + created→started queue split) on ≥3 runs of each affected repo.
- Runner-side: enable JSON span export on all pool daemons
  (`logs/trace.jsonl`) so pickup/boot/teardown percentiles become first-class;
  queue-to-acquire and queue-to-first-step fields now flow through versioned
  `job-timing` records and doctor SLO output, enabling elastic-capacity proof.
- Weekly scheduled `both`-lane parity runs remain the regression tripwire;
  extend lane-compare tooling to flag any Velnor lane >1.15× its GitHub twin.

## 7. Immediate actions shipped by this research

1. chainargos/java-monorepo#1976 — registry-primary bake cache (expected
   −23 min/run on the estate's biggest pipeline once main seeds the cache).
2. Velnor #365/#370 — queue timing records plus daemon-bound doctor probes;
   Sentry now reports 73 samples instead of a false empty result.
3. Velnor — Docker lifecycle gate: cross-daemon create/start/teardown control
   mutations are bounded at two host-wide permits by default (`/run/velnor/
   docker-lifecycle.lock` plus numbered sibling locks), while job containers
   remain concurrent. The pre-gate burst proved the root cause: isolated boot
   was 0.4–3.7s and teardown 0.6–5.9s, but an eight-slot burst produced
   11.6–70.5s boot and 12.1–42.1s teardown tails. v0.1.209 then proved that
   full serialization fixes contention but makes the gate itself the queue:
   broker pickup stayed below 1.1s while container boot reached 42.7s. The
   bounded gate preserves the race fix without turning eight slots into one.
   `VELNOR_DOCKER_LIFECYCLE_CONCURRENCY` permits controlled tuning from 1–8.
4. This document + adoption roadmap (P1 next implementation target).
