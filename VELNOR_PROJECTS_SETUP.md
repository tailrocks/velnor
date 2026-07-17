# VELNOR_PROJECTS_SETUP — Estate CI Standardization Plan

**Status:** definitive research + implementation plan (2026-07-18, rev 2 — speed doctrine, budgets, dual-lane verification protocol)  
**Audience:** operators and agents preparing one PR per repo so **Velnor is the default runner**, with GitHub and dual-lane modes always available.  
**Related:** [`docs/strict-capability-contract.md`](docs/strict-capability-contract.md), [`docs/storage-and-disk-pressure-2026-07-18.md`](docs/storage-and-disk-pressure-2026-07-18.md), [`docs/rust-build-cache-hygiene-velnor.md`](docs/rust-build-cache-hygiene-velnor.md), [`docs/cache-gc-design.md`](docs/cache-gc-design.md), [`docs/perf-instant-cache-plan-2026-06-11.md`](docs/perf-instant-cache-plan-2026-06-11.md), [`docs/master-plan.md`](docs/master-plan.md)

This file is the **single planning artifact** for standardizing CI/CD across the estate. It defines the target contract, per-repo gap analysis, PR sequence, and Velnor runner development required to support that contract.

---

## 0. Executive summary

### What we want

One CI/CD shape for every listed repository:

| # | Requirement |
|---|-------------|
| 1 | **Velnor by default** on `push`, `pull_request`, `schedule`, and default `workflow_dispatch` |
| 2 | **GitHub selectable** via the same workflow (`lanes: github`) |
| 3 | **Both** in one run for parity (`lanes: both`) |
| 4 | **Ubuntu only** — GitHub lane is exactly `ubuntu-26.04`; Velnor runs the Ubuntu job image; **no macOS/Windows CI** in the standardized surface |
| 5 | **mise + `rust-toolchain.toml`** for tools and Rust; no `dtolnay/rust-toolchain` product path |
| 6 | **One caching model** — cargo registry/git, sccache, Docker layers, mise — identical YAML on both lanes |
| 7 | **Clarity** — job/step names explain purpose; no shards/batches; complex CI decisions in Rust |
| 8 | **One approach** — layered by complexity (library → product → monorepo), not divergent stacks |
| 9 | **Speed is the product** — every workflow meets explicit run-class budgets (§2.11); rerun-idempotency is the acceptance test; performance is measured, never guessed |
| 10 | **Portable YAML, hidden acceleration** — one workflow runs correctly on both runners; every Velnor speedup lives inside the runner, never in lane-specific YAML (§2.0 Law 1) |
| 11 | **Latest and greatest** — newest stable major of every action, tool, toolchain, and workflow feature; Renovate keeps pins current; no deprecated surfaces (§2.0 Law 2) |

**Policy override.** Older docs sometimes defaulted jackin-family repos to GitHub. **This plan defaults every listed repo to Velnor.** GitHub remains a permanent comparison lane.

### Estate snapshot (local trees, 2026-07-18)

| Repo | Org | Lanes today | Default | mise | rust-toolchain | sccache | Primary gaps |
|------|-----|-------------|---------|------|----------------|---------|--------------|
| jackin | jackin-project | Partial | **github** | yes | yes | yes | Flip default; pin ubuntu-26.04; kill macOS; absorb #810 principles |
| java-monorepo | ChainArgos | Full | **velnor** | yes | yes | yes | Inline matrix (no GitHub-only coordinator); keep as reference |
| blockchain-nodes | ChainArgos | Full | **velnor** | yes | n/a (rust-script) | no | `lane`→`lanes`; ubuntu-26.04; selected-lane sweep |
| parallax | tailrocks | none | github | yes | yes | yes | Full lanes; remove macOS; pin 26.04 |
| parallax-telemetry-playground | tailrocks | none | github | yes | mise only | no | Class E template |
| tablerock | tailrocks | none | **macos-15** | **no** | dtolnay | no | Rebuild from Class D |
| holla | tailrocks | none | github (26.04) | yes | yes | yes | Add lanes only |
| velnor | tailrocks | none | github | yes | yes | yes | Dogfood lanes + 26.04 |
| ruxel | tailrocks | none | github | yes | yes | yes | Lanes + pin; ansible/uv on Velnor |
| termrock | tailrocks | none | github+macOS | partial | mise+dtolnay | no | Modernize; no macOS; sccache |
| schemalane | tailrocks | none | github | **no** | dtolnay | no | Full Class D + Postgres |
| pg-bigdecimal | tailrocks | none | github | **no** | dtolnay | no | Class D |
| tracing-request-level | tailrocks | none | github | **no** | dtolnay | no | Class D |

**Only two repos already default to Velnor.** Eleven must migrate. Four still use `dtolnay/rust-toolchain`. Several still use `ubuntu-latest` or macOS.

### Non-negotiable gates before mass default-flip

1. Velnor fleet stable (no zombie/offline slots).
2. Org-level (or carefully managed multi-repo) JIT capacity for three orgs.
3. **Cache GC + budgets live** — warm stores without lifetime ownership will fill the host (jackin hygiene research).
4. Estate adapters green for the shared action surface.
5. Fixture proves the **inline lane matrix** expression.
6. **Per-repo timing baseline recorded** (cold / warm / no-change rerun, both lanes, §2.11) — a migration without numbers is not a migration.
7. **Velnor host cleaned to a recorded baseline** (§8 Phase 0.5) before the dual-lane verification campaigns — measurements on a polluted host don't count.

---

## 1. Velnor capability baseline

Velnor is a GitHub Actions–compatible self-hosted runner (Rust/tokio):

- **V2 only:** JIT → broker → run-service → Results Service (live logs).
- Each job runs in a fresh Docker container (`velnor/job-ubuntu`: mise, rust, sccache, mold, buildx).
- Known marketplace actions run via **native Rust adapters** (JS node sidecar = diagnostic fallback only).
- **Host-persistent stores** on the Velnor lane: `_velnor_cargo` (registry/git), `_velnor_mise`, `_velnor_sccache`, optional `_velnor_targets/<trust>/<repo>/<workflow>/<job>`.
- `actions/cache` / rust-cache / sccache-GHA become **no-ops** when paths are already host-persistent — **same workflow YAML on both lanes**.
- Trust scopes: `trusted` keeps Docker socket + secrets; other scopes refuse secrets and omit host Docker.

**Hard OS rule:** Velnor does not run Darwin. Combined with the Ubuntu-everywhere operator rule, **every `macos-*` job is removed, replaced by Ubuntu cross-build (`cargo-zigbuild` where valid), or documented as a proven product blocker** before migration is complete. There is no “GitHub-only macOS fourth lane” in the standard.

Law: `docs/mission.md`, `docs/master-plan.md` §3a, `docs/runner-usage.md`, `docs/perf-instant-cache-plan-2026-06-11.md`, `docs/reference/target-action-registry.md`.

---

## 2. The Standard CI Contract (“Velnor Estate Standard”)

Repos differ only in **which jobs exist**, not in **how lanes, tools, and caches are declared**.

### 2.0 Two overriding laws

These govern every rule below and every future change to the standard.

#### Law 1 — Portable YAML, hidden acceleration (the drop-in guarantee)

Velnor is a drop-in replacement for GitHub-hosted runners. The workflow is the
public contract; the runner is where the magic lives.

- **One YAML, all lanes.** Only `runs-on` differs, and only via the canonical
  matrix. Every standardized workflow must be fully correct on GitHub-hosted
  with Velnor absent from the universe.
- **All Velnor speed is runner-internal**: native Rust adapters,
  host-persistent stores, cache no-op transforms, git mirrors, container
  pre-create, async finalization. Each optimization must preserve observable
  step semantics — same step set, names, order, conclusions, and equivalent
  logs (log-format contract).
- **Sanctioned lane awareness in YAML — exactly two forms:**
  `matrix.config.writer` single-writer gating (mutation safety, not
  performance) and the `(${{ matrix.config.lane }})` job-name suffix
  (clarity). Nothing else. A step that branches on runner identity for
  performance is a design defect.
- **If a Velnor optimization would require a YAML change, the optimization is
  misdesigned.** Redesign it as a runner-internal transform declared in the
  capability manifest — the `actions/cache` always-warm no-op is the model.
- Enforcement: gate V-B lane parity + fixture-is-contract; `audit-ci` flags
  lane-conditional steps beyond the two sanctioned forms.

#### Law 2 — Latest and greatest everything

Extends the runner's latest-protocol hard rule (`AGENTS.md`) to the entire
estate surface:

- **Actions:** every `uses:` targets the latest stable major, SHA-pinned to
  the latest release at PR time with a `# vX` comment; Renovate is active in
  every repo and keeps pins current.
- **No deprecated surfaces:** no legacy workflow commands
  (`set-output`, `save-state`), no actions running end-of-life Node runtimes,
  no superseded inputs where the current major provides replacements.
- **Workflow features:** use the current platform capabilities where they
  serve the standard (typed `choice` inputs, `concurrency`, job-level
  `permissions`, composite actions) — never legacy patterns copied forward.
- **Tools and toolchains:** mise installs latest stable per committed
  `mise.toml` + `mise.lock`; `rust-toolchain.toml` pins current stable and
  Renovate bumps it promptly. Library MSRV coverage is a separate explicit
  job, never the CI toolchain.
- **OS:** GitHub lane = newest pinned Ubuntu LTS (`ubuntu-26.04` today);
  the Velnor job image tracks it (V1.6).
- **Native adapters track latest upstream behavior.** A new upstream action
  major triggers adapter + manifest + fixture update; falling behind is a
  defect, not a preference.
- **Freshness never bypasses verification:** every upgrade lands through the
  same gates (V-A fixture where behavior changes, V-B parity) as any other
  change.

### 2.1 Lane plumbing

**Input name:** always `lanes` (plural). Migrate blockchain-nodes `lane` → `lanes`.

```yaml
on:
  push:
    branches: [main]
  pull_request:
  schedule: # optional
    - cron: "0 4 * * 0"
  workflow_dispatch:
    inputs:
      lanes:
        description: |
          Runner lane(s).
          velnor = default self-hosted Velnor
          github = pinned GitHub-hosted Ubuntu comparison
          both   = same jobs on Velnor and GitHub in one run
        type: choice
        default: velnor
        options: [velnor, github, both]
```

**Canonical matrix — inline expression, no selector job:**

A small `matrix-setup` job that always runs on GitHub (or always on ubuntu-latest) is **rejected** as the standard. It creates a lying fourth state: a “Velnor default” run still burns a GitHub-hosted coordinator, and a pure `github` dispatch may still touch Velnor depending on ordering. Three static JSON records do not need a runner slot.

```yaml
strategy:
  fail-fast: false
  matrix:
    config: ${{ fromJSON(
      (github.event_name == 'workflow_dispatch' && inputs.lanes == 'both')
        && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true},{"lane":"GitHub","runner":"ubuntu-26.04","writer":false}]'
      || (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github')
        && '[{"lane":"GitHub","runner":"ubuntu-26.04","writer":true}]'
      || '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true}]'
    ) }}
runs-on: ${{ matrix.config.runner }}
```

Notes:

- Automatic events (`push` / `pull_request` / `schedule`) always resolve to **velnor** (the final `||` arm).
- `workflow_dispatch` honors `inputs.lanes` exactly.
- Prove this expression in `tailrocks/velnor-actions-fixture` before estate rollout; collapse to one line if the parser requires it.
- Job names: `Format and lint (${{ matrix.config.lane }})` → UI shows `(Velnor)` vs `(GitHub)`.
- Cache keys include `${{ matrix.config.lane }}` so dual-lane GHA caches never corrupt each other.
- **State-mutating jobs** (Renovate, Docker Hub, crates.io, Pages deploy): on `both`, secondary lane is **read-only / dry-run**. One writer (`writer: true`) only.
- Control/aggregate jobs use the **same selected matrix** (or pure expressions). A Velnor-default run must not silently consume GitHub-hosted coordinators.

### 2.2 Ubuntu / OS policy

Canonical prose lives in ChainArgos java-monorepo `.github/AGENTS.md` — adopt estate-wide:

| Lane | Runner | Notes |
|------|--------|--------|
| Velnor | `["self-hosted","velnor-target-mvp"]` | Job OS = Velnor Ubuntu image (track 26.04 direction) |
| GitHub | **`ubuntu-26.04` only** | Never `ubuntu-latest`; never unpinned 24.04 after migration |
| GitHub ARM | `ubuntu-26.04-arm` only if intentional | Do not invent labels |
| macOS / Windows | **Forbidden** in standardized workflows | Ubuntu cross-build or proven blocker |

Every repo gets `.github/AGENTS.md` (or `.github/workflows/AGENTS.md`) stating the three lanes + Ubuntu pin. Do not restate the full essay inside every workflow.

### 2.3 Toolchain standard

| Layer | Standard |
|-------|----------|
| Tools | `jdx/mise-action` only, **full commit SHA** + `# vX` |
| Versions | committed `mise.toml` + `mise.lock` |
| Rust | `rust-toolchain.toml` sole channel source; `profile = "minimal"`; components rustfmt/clippy; mise via `idiomatic_version_file_enable_tools = ["rust"]` |
| Forbidden | `dtolnay/rust-toolchain`, ad-hoc installers, language setup actions mise covers |
| Cargo tools | `cargo:<crate>` pins; `cargo-binstall` first when many cargo: tools |
| Order | mold + sccache **before** mise when mise installs tools that compile under `RUSTC_WRAPPER` (java-monorepo lesson) |
| mise cache write | `cache_save: ${{ github.ref == 'refs/heads/main' && matrix.config.writer }}` |

### 2.4 Caching standard (universal mandate)

Rerun of the same commit on a warm fleet must not re-download crates, recompile deps, reinstall tools, or rebuild unchanged Docker layers (`master-plan` §3a).

#### Rust — four layers on every compiling job

| Layer | GitHub lane | Velnor lane |
|-------|-------------|-------------|
| **1. Compiler cache** | `mozilla-actions/sccache-action` pinned `version: v0.16.0` + `RUSTC_WRAPPER=sccache` + **`SCCACHE_GHA_ENABLED=false`** (local disk; store lives only for the job = cold compiler baseline) | Selected backend store (`sccache \| kache \| off` per [strict contract](docs/strict-capability-contract.md)); Velnor owns `SCCACHE_DIR`; adapter owns setup/post reporting |
| **2. Cargo registry/git** | `actions/cache` on registry + git/db; key = lockfile + lane + os | Host `_velnor_cargo`; cache action no-ops |
| **3. Target dir** | Optional measured cache; prefer sccache+registry over blind Swatinem+target double-cache | Optional `VELNOR_CARGO_TARGET_PERSIST` buckets (hygiene §3) |
| **4. Result reuse** | jackin #810 exact-result proofs (advanced monorepos only) | Same YAML |

```yaml
env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: "0"   # required: sccache cannot cache incremental crates; kills session-dir bombs
  RUSTFLAGS: "-C link-arg=-fuse-ld=mold"
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "false"  # strict contract: local-only; GHA backend needs a separate explicit yes (§12.7)
  SCCACHE_CACHE_SIZE: 20G       # manifest-pinned budget (strict contract)
```

**Compiler-cache law** ([`docs/strict-capability-contract.md`](docs/strict-capability-contract.md)):
local-only stores; exactly one of `sccache | kache | off` per job; only the
pinned action inputs at their approved values; no remote-backend environment;
cache reporting is owned by the action / native-adapter **post step** — build
steps never call cache CLIs. Consequences the estate must accept until §12.7 is
decided: the GitHub lane recompiles dependencies every run (its local store is
job-lifetime), while `actions/cache` on the **cargo registry** remains approved
on both lanes, so downloads stay warm. This supersedes any older
`SCCACHE_GHA_ENABLED=true` guidance in repo workflows and in earlier revisions
of this document.

**Estate default stack:** mold + sccache + cargo-registry cache.  
**Not default:** Swatinem/rust-cache *and* full target cache *and* sccache without measurement.

#### Docker / Buildx

| Concern | Standard |
|---------|----------|
| Login | secrets only on writer jobs |
| Builder | `docker/setup-buildx-action` (current major, SHA-pinned) |
| Build | build-push or bake, `--progress=plain` |
| Cache | registry `buildcache-*` for publish; GHA only for ephemeral PR builds; **unique scope per image** |
| Velnor | persistent local buildkit; may drop redundant `type=gha` export |
| Dockerfiles | apt/apk `--mount=type=cache`; pin digests; cargo-chef cook/final feature parity |

#### Other

| Content | Key | Notes |
|---------|-----|--------|
| Bun/Node | lockfile | store cache, not mutable cross-job `node_modules` by default |
| rust-script | sources hash | blockchain-nodes |
| Renovate | renovate dir | single-writer |
| uv/Python | lockfile | ruxel |
| Postgres | image pull / services | schemalane |

### 2.5 Host cache hygiene (from jackin `rust-build-cache-hygiene.mdx`)

Source: [jackin rust-build-cache-hygiene](https://github.com/jackin-project/jackin/blob/0cbada6dc0cd2adfc603bffd17287145520d374c/docs/content/docs/roadmap/rust-build-cache-hygiene.mdx)  
Mapped in detail: [`docs/rust-build-cache-hygiene-velnor.md`](docs/rust-build-cache-hygiene-velnor.md)

July 2026 audit (jackin host): single targets → **460 GiB**, 3k+ incremental session dirs, alternate `CARGO_TARGET_DIR`s duplicating near-identical artifacts. Root class: **caches created for speed with no lifetime owner**.

Live Sentry audit (2026-07-18): root XFS was **84% used** with about **432 GB
physical in Velnor persistent target trees** and about **158 GB in the two
largest BuildKit stores**. Current target GC cannot select those accumulators,
and their paths collapse to `unknown-repository/unknown-workflow`. The accepted
storage layout, capacity controller, lease model, and kache decision are in
[`docs/storage-and-disk-pressure-2026-07-18.md`](docs/storage-and-disk-pressure-2026-07-18.md).

Velnor already implements the jackin three-layer model (shared downloads, shared compiler results, scoped targets). The missing half is **hygiene**. Mass default-flip of 13 repos multiplies write pressure — this is a **stability** gate, not polish.

| Rule | Application on Velnor |
|------|------------------------|
| Every cache class labeled + owned | `_velnor_cargo`, `_velnor_mise`, `_velnor_sccache`, `_velnor_targets`, `_velnor_caches`, `_velnor_artifacts`, buildkit |
| Bounded sccache | `SCCACHE_CACHE_SIZE=20G` per strict contract; changing it versions the manifest; fleet-wide sizing belongs to the capacity controller |
| Path-normalized sccache keys | inject `SCCACHE_BASEDIRS` for `/__w` + job home mounts |
| No global `CARGO_TARGET_DIR` | keep trust/repo/workflow/job buckets; never one target for all repos ([Cargo #12516](https://github.com/rust-lang/cargo/issues/12516)) |
| `CARGO_INCREMENTAL=0` enforced | runner default + workflow; eliminate `target/**/incremental` bombs |
| Destructive GC before disk-floor park | finish `cache-gc-design.md` reaper (lock + in-use scopes + forensics) |
| Doctor / du inventory | logical + physical bytes by class/scope + exact prune command |
| kache evaluation optional | content-addressed dedup may help multi-bucket disks; **not** a GC substitute; keep sccache default until soak proves kache on Linux bind-mounts |

**Two-store model (workflow + runner agree):**

1. Shared downloads + compiler results (cargo registry/git + sccache) — warm across jobs/repos.  
2. Bounded per-scope build outputs (ephemeral `./target` or opt-in `_velnor_targets/...`) — disposable under policy.

### 2.6 Workflow clarity (jackin PR #810)

PR [jackin-project/jackin#810](https://github.com/jackin-project/jackin/pull/810) is a **clarity and reuse reference**, not a mandate that every library clone full xtask machinery.

**Estate law (L1 — every repo):**

1. **One crate / one purpose = one complete job.** No numbered shards, batches, or transport parts.
2. **Readable names.** `Test bitcoin-processor-app (Velnor)`, not `job-3`.
3. **Thin `run:` blocks.** Call prepared tools or one direct command; no multi-screen Bash implementing CI decisions.
4. **Semantic boundaries only.** Split jobs when meaning/ownership differs, not for transport mechanics.

**Optional (L2/L3 — monorepos that pay for it):**

5. Exact-result reuse / input-identical skip (measured ~19s full CI repeat on jackin).  
6. Performance audit gate (fail on download/compile markers when should be warm).  
7. Rust-owned routing, cache contracts, artifact transport (`*-xtask` / `velnor-tools`).  
8. Runner-neutral bootstrap (no host Node/`unzip` assumptions that differ by lane).

**Do not copy into tiny libraries:** Docs semantic contracts, multi-workflow proof graphs, full target-seed versioning — unless scale justifies it.

### 2.7 Job taxonomy (shared vocabulary)

| Stage | Examples | Runner |
|-------|----------|--------|
| Route | Inline static lane matrix | No selector job |
| Classify | paths-filter / changed paths | Selected lane matrix |
| Policy / hygiene | actionlint, gitleaks, zizmor, reuse | Selected lane matrix |
| Format / lint | fmt, clippy, deny | Selected lane matrix |
| Test | nextest / integration | Selected lane matrix |
| Build image | bake / build-push | Selected lane; writer rules |
| Package / release | zigbuild, deb, crates.io | Selected lane; single writer |
| Docs / pages | site build + deploy | Build on matrix; deploy once |
| Required aggregate | `ci-required` | Same selected results only |

### 2.8 Shared productization

| Piece | Purpose |
|-------|---------|
| `setup-rust-ci` composite | mold → sccache → mise → component check → registry cache |
| `cache-cargo-registry` | jackin-style offline verify (large workspaces) |
| `aggregate-needs` | required status merge |
| `tailrocks/velnor-ci-actions` (optional) | one upgrade point for tailrocks |
| `velnor-tools audit-ci` | fail on `ubuntu-latest`, missing lanes, dtolnay, missing sccache on compile jobs, missing `concurrency`/`timeout-minutes`, uncommented `fetch-depth: 0`, double-cache stacks, lane-conditional steps beyond the two sanctioned forms (§2.0 Law 1), action majors behind latest upstream release, deprecated workflow commands (§2.0 Law 2); **perf mode:** fail a warm run that logs dependency `Downloading`/`Compiling` or tool-install markers |

### 2.9 Pinning & security

- Full commit SHA pins + Renovate.  
- Minimal permissions; write only where needed.  
- actionlint (+ zizmor where present) on workflow changes.  
- `GITHUB_TOKEN` same-repo; dedicated tokens for cross-repo / Docker Hub / crates.io.

### 2.10 Pipeline speed standard (every repo, both lanes, same YAML)

Caching (§2.4) is necessary but not sufficient. These rules remove the other
latency classes found in the 2026-07-18 estate scan (§3.0):

1. **`concurrency` on every workflow.** PR-triggered runs set
   `cancel-in-progress: true`; push/release groups serialize without cancel.
   A stale run occupying a Velnor slot is estate-wide queue latency.
2. **`timeout-minutes` on every job.** Budget ≈ 2× measured warm p95
   (floor 10). A hung job parks a shared slot until the 6-hour GitHub default;
   on a 10-slot pool that is a 10% capacity loss per zombie.
3. **Checkout stays shallow.** `actions/checkout` default depth 1 is the
   standard. Every explicit `fetch-depth: 0` carries a comment naming its
   consumer (changelog, ancestry gate, README-freshness check) or is removed.
   `persist-credentials: false` unless the job pushes. Velnor lane: the host
   git-mirror store (V1.11) makes checkout O(delta).
4. **Never compile tooling in CI.** Cargo tools install via mise `cargo:`
   backend with `cargo-binstall` available (prebuilt binaries); the Velnor job
   image pre-bakes the estate tool set. A `Compiling` line during setup is a
   defect (`audit-ci` perf mode).
5. **Debuginfo policy.** Compile jobs set
   `CARGO_PROFILE_DEV_DEBUG=line-tables-only` (Rust ≥1.71): backtraces keep
   file:line, target size and link time drop sharply. Roll out per class with
   a before/after measurement; do not mix values within one persistent target
   bucket (it forks the compilation hash).
6. **Fail fast, never serialize.** Cheap gates (fmt, actionlint) are separate
   fast jobs, not `needs:` prerequisites of long jobs; independent jobs never
   chain. Lane matrices keep `fail-fast: false` (parity needs both lanes to
   finish).
7. **Path filtering by job-level classify.** Monorepos/mixed repos skip jobs
   whose inputs did not change via a classify job + `if:` (paths-filter native
   adapter) — never workflow-level `paths:`/`paths-ignore` on required checks
   (skipped-at-workflow-level hangs required statuses).
8. **Docker speed.** `cache-from`/`cache-to` `type=registry,mode=max`, unique
   scope per image; cargo-chef for Rust images; apt/apk `--mount=type=cache`;
   `provenance: false` on non-release builds (attestation only on
   writer/release jobs). Velnor lane: persistent local BuildKit is the hot
   path; registry cache is the cross-host fallback.
9. **No double caching.** One stack: sccache + registry cache (+ optional
   measured target persist on Velnor). Swatinem/rust-cache stacked on top of
   sccache + `actions/cache` doubles save time and churns the 10 GiB GitHub
   cache quota (jackin: 6× Swatinem + 32× `actions/cache`; parallax: 58 cache
   steps — both collapse to the standard stack).
10. **Heavy policy scans off the hot path.** cargo-audit / cargo-deny run on
    lockfile-touch filter + weekly schedule, not on every PR commit, unless
    the repo's policy makes them a required gate.
11. **The job summary tells the story.** Every compile job ends with the cache
    effectiveness report emitted by the setup action / native adapter **post
    step** (hit rate, bytes, store) — never an ad-hoc stats step in the build
    script (strict contract). Warm-run audits and `audit-ci` perf mode parse
    this summary.

### 2.11 Run classes, latency budgets, measurement

**Run classes** (the vocabulary every timing claim uses):

| Class | Definition | Acceptance |
|-------|------------|------------|
| **cold** | Empty stores (new pool, post-GC, or fresh cache key) | Tool floor; no budget, but recorded |
| **warm** | Stores warm; new commit touching source | Dependencies never rebuilt; only changed crates compile; no tool installs |
| **no-change rerun** | Re-run of a just-green commit | Zero downloads, zero dependency compiles, zero image layer builds; wall = orchestration + tests |

**Initial budgets (2026-07-18 — revise only with campaign measurement, never by feel):**

| Metric | Velnor lane | GitHub lane |
|--------|-------------|-------------|
| Queue pickup, free slot | ≤ 3 s (measured 1–3 s parity) | GitHub-controlled (record only) |
| Pickup → first user step | ≤ 5 s warm (V1.10/V1.11) | ≤ 40 s |
| Setup steps warm (mold+sccache+mise+registry restore) | ≤ 5 s (host stores no-op) | ≤ 60 s |
| No-change rerun wall — Class D library | ≤ 60 s | record |
| No-change rerun wall — Class B product | ≤ 90 s | record |
| Warm Class A `rust.yml` critical path | ≤ 2 m 30 s (p3 projection; measured 382 s → target) | record |
| Warm Class C bake job | ≤ 90 s (measured 62 s) | record |
| Job completion → slot free (teardown) | ≤ 2 s visible (V1.9 async finalize) | n/a |

**Measurement protocol** (method of
[`docs/ci-performance-report-2026-06-11.md`](docs/ci-performance-report-2026-06-11.md)):

1. Fleet health green (doctor) before timing anything.
2. Three rounds per workflow per lane: cold, warm1 (new commit), warm2
   (no-change rerun). Dispatch `lanes=both` where dual-lane.
3. Record per job: queue wait, exec time, wall; Velnor span data
   (`logs/trace.jsonl`); backend hit rates from the adapter summary (§2.10.11).
4. The migration PR description carries the repo's timing table; each phase
   ends with a refreshed `docs/ci-performance-report-<date>.md` for the estate.
5. Regression gate: `audit-ci` perf mode (§2.8) fails warm runs showing
   dependency download/compile or tool-install markers.

### 2.12 Uniform shape — identical names everywhere

"One approach" is literal: for every concern a repo has, every repo uses the
**same file names, job ids, input names, key shapes, and branch name**. A
maintainer moving between repos must find zero naming drift.

| Surface | Canonical form |
|---------|----------------|
| Program branch | `velnor-estate-standard` — **one branch per repo carries that repo's entire program work**, velnor itself included (runner features + dogfood CI on the same branch) and the fixture too. Per-feature commits, never per-feature branches. **Sole exception: jackin** — its one branch is the existing head branch of [PR #810](https://github.com/jackin-project/jackin/pull/810) (operator decision, §12.5). |
| Workflow files | `ci.yml` (always); `release.yml` (publish/tag); `docs.yml` (docs + Pages); `preview.yml`; `renovate.yml`. One concern = one filename, identical in every repo. Extra repo-specific workflows allowed; they follow every other rule. |
| Dispatch input | `lanes` exactly as §2.1 — same description text, default, options order |
| Job ids | `rust`, `integration`, `audit`, `build-image`, `docs`, `release`, `ci-required`; display names `<Purpose> (${{ matrix.config.lane }})` |
| Concurrency groups | `<workflow-name>-${{ github.ref }}` |
| Cache keys | §2.4/§9 shapes verbatim (`cargo-registry-<lane>-<os>-<lockhash>`, …) |
| `.github/AGENTS.md` | Identical shared template text (three lanes, ubuntu-26.04, standard stack); repo-specific lines appended **below** the shared block |
| Env block, step order, timeout tiers | §2.10/§9 verbatim |

Enforcement: `audit-ci` (§2.8) checks names and shapes, not just behavior.

---

## 3. Per-repository analysis and target configuration

Local clones under `/Users/donbeave/Projects` (see §10).

### 3.0 Perf-marker scan (local trees, 2026-07-18)

Mechanical grep over `.github/workflows` of all 13 repos, against the §2.10
rules. `conc` = workflow files with a `concurrency` block / total; `t-o` =
files with any `timeout-minutes`; `fd0` = occurrences of `fetch-depth: 0`.

| Repo | conc | t-o | fd0 | mold | Cache stack today | Worst markers |
|------|------|-----|-----|------|--------------------|---------------|
| jackin | 8/13 | 11 | 8 (1 commented) | **no** | 6× Swatinem **+** 32× `actions/cache` + sccache | 48× `ubuntu-latest`, 1× `macos-latest` |
| java-monorepo | 3/5 | **0** | 0 | yes | sccache + cache (clean) | coordinators on `ubuntu-latest` |
| blockchain-nodes | 2/2 | 1 | 0 | n/a | `actions/cache` only, no compiler cache | rust-script recompile path |
| parallax | 5/6 | 4 | 3 | **no** | **58×** `actions/cache` + sccache | `macos-15` job, `ubuntu-latest` ×29 |
| parallax-telemetry-playground | 1/1 | 0 | 0 | no | **none** | zero caching of any kind |
| tablerock | **0/1** | 0 | – | no | none (dtolnay) | `macos-15` only |
| holla | 3/5 | **0** | 2 | yes | sccache + cache (clean) | cleanest repo; lanes + timeouts missing |
| velnor | 2/4 | **0** | 0 | yes | sccache + cache (clean) | `ubuntu-latest` ×10 |
| ruxel | 1/1 | 0 | 0 | no | sccache + cache | no mold |
| termrock | 1/4 | 0 | 2 | no | 3× Swatinem, no sccache | dtolnay, no pin |
| schemalane | **0/2** | 0 | 0 | no | 1× Swatinem, dtolnay | no concurrency at all |
| pg-bigdecimal | **0/2** | 0 | 0 | no | 2× Swatinem, dtolnay | no concurrency at all |
| tracing-request-level | **0/2** | 0 | 0 | no | 2× Swatinem, dtolnay | no concurrency at all |

Systemic classes (structural fixes, not per-file patches): missing
`timeout-minutes` nearly everywhere (11 of 13 repos have zero or near-zero
coverage); double-cache stacks in the two biggest repos; mold absent outside
java-monorepo/holla/velnor; `concurrency` absent exactly where runs are
cheapest to cancel (libraries). The §9 template and `audit-ci` encode all four
so the class cannot re-enter.

### 3.1 jackin-project/jackin

**Role:** Most sophisticated pipeline (construct, capsule, docs, preview, release). Source of advanced CI ideas.

**Already good:** dual-lane on major workflows; rich mise; composites; AGENTS policy; sccache; PR #810 direction.

**Target PR:**

| Item | Change |
|------|--------|
| Default | `lanes` default **velnor**; AGENTS: Velnor automatic, not GitHub |
| Ubuntu | GitHub = `ubuntu-26.04` everywhere (drop 24.04 / latest) |
| Matrix | Replace matrix-setup jobs with inline expression |
| macOS | Remove; zigbuild/portable coverage only |
| #810 | Absorb L1 everywhere; L2/L3 stay jackin-owned |
| Fleet | org/repo JIT, Docker socket, high slots, trusted |
| Perf | Add mold estate-wide; collapse 6× Swatinem + 32× `actions/cache` to the standard stack; `concurrency` on all 13 workflows (8 today); `timeout-minutes` on the 2 uncovered files; justify-or-drop the 7 uncommented `fetch-depth: 0` |

**Workflows:** `ci.yml`, `construct.yml`, `docs.yml`, `hygiene.yml`, `jackin-dev.yml`, `preview.yml`, `release.yml`, `rust-nextest.yml`, policy files.

### 3.2 ChainArgos/java-monorepo

**Role:** Production Velnor-default reference; instant-cache proven.

**Already good:** `lanes` default velnor; sccache+mold+mise; per-package jobs; `.github/AGENTS.md`; Docker bake + registry cache.

**Target PR:**

| Item | Change |
|------|--------|
| Matrix | Inline expression; no GitHub-only selector that lies about mode |
| GitHub pin | Confirm **ubuntu-26.04** in every workload `runs-on` |
| Optional | `setup-rust-ci` composite; later xtask for clippy package selection |
| Perf | `timeout-minutes` on all jobs (0 today); inline matrix also removes the 8× `ubuntu-latest` coordinator spend |

Keep as **Class A reference** for multi-package Rust.

### 3.3 ChainArgos/blockchain-nodes

**Role:** Docker image factory (base → build → leaves); exists-sweep.

**Already good:** Velnor default; staged names; registry buildcache; concurrency for long publishes.

**Target PR:**

| Item | Change |
|------|--------|
| Input | `lane` → `lanes` |
| GitHub | `ubuntu-26.04` |
| Matrix | Inline; exists-sweep on selected lane(s) |
| QEMU | prefer `docker/setup-qemu-action` |
| Toolchain | rust-script-only is OK (Docker-primary); document |
| Perf | `timeout-minutes` on the uncovered file; verify bake definitions carry registry `cache-from`/`cache-to` `mode=max` per image; non-empty `hashFiles()` in the rust-script cache key (empty-hash key-collapse class, p3 defect #3) |

**Velnor needs:** trusted Docker, large disk, multi-arch buildx, long drain.

### 3.4 tailrocks/parallax

**Role:** OTel fan-out product; Rust + Bun UI; cosign/syft releases.

**Today:** strong scripts/sccache/bun cache; **no lanes**; macOS matrix.

**Target PR:** full lane matrix default velnor; pin 26.04; remove macOS (zigbuild on Ubuntu); clarity renames; optional later xtask for classify scripts. Perf: collapse the 58 `actions/cache` steps to the standard stack; add mold; justify-or-drop 3× `fetch-depth: 0`; `timeout-minutes` beyond the current four files.

### 3.5 tailrocks/parallax-telemetry-playground

**Role:** Polyglot OTel/Sentry sample (Rust + Java).

**Target:** Class E — lanes + mise + sccache + registry + fmt/clippy/test; Java via mise if needed.

### 3.6 tailrocks/tablerock

**Role:** Early terminal DB workbench.

**Today:** **non-compliant** (macos-15 only, dtolnay, no mise).

**Target:** Replace with Class D `ci.yml`; add mise + rust-toolchain; drop macOS host CI entirely.

### 3.7 tailrocks/holla

**Role:** Adaptive dev-env CLI; deb + zigbuild releases.

**Today:** ubuntu-26.04, mise, sccache, mold — **missing only lanes**.

**Target PR:** inline lanes default velnor; single-writer releases; Ubuntu cross-builds only. Perf: add `timeout-minutes` (0 today); comment the 2 `fetch-depth: 0` uses or drop them.

### 3.8 tailrocks/velnor

**Role:** The runner — must dogfood the standard.

**Target:** lanes default velnor; ubuntu-26.04 (10× `ubuntu-latest` today); keep GitHub for fleet-break recovery; deb/image prefer Velnor (Docker socket). Perf: `concurrency` on release/renovate workflows; `timeout-minutes` everywhere. The runner's own repo missing its own standard is the first credibility test.

### 3.9 tailrocks/ruxel

**Role:** Rust Ansible executor + uv oracle.

**Target:** lanes + pin; verify ansible/uv via mise host store on Velnor job image. Perf: mold + `timeout-minutes`.

### 3.10 tailrocks/termrock

**Role:** Ratatui library + Bun docs + Pages.

**Target:** remove dtolnay; SHA-pin actions; sccache; lanes; no macOS; docs build on Ubuntu; deploy once (writer). Perf: replace 3× Swatinem with the standard stack; `concurrency` on the 3 uncovered workflows; `timeout-minutes`; comment or drop 2× `fetch-depth: 0`.

### 3.11 Library trio — schemalane, pg-bigdecimal, tracing-request-level

**Today:** minimal `dtolnay` + Swatinem; no mise/sccache/lanes.

**Target Class D:**

```text
ci.yml
  rust (lane matrix): checkout → mold → sccache → mise → registry cache
    → fmt → clippy → nextest → package (if publishable)
  integration (schemalane): + Postgres service/testcontainers
  audit (optional): cargo-audit via mise

release.yml
  single-writer publish on main/tags
```

Add `mise.toml`, `mise.lock`, `rust-toolchain.toml`, `.github/AGENTS.md`. The
§9 template already carries `concurrency`, `timeout-minutes`, and the standard
cache stack — none of the three repos has any of them today (§3.0).

### 3.12 Estate third-party action surface

| Action | Status for Velnor |
|--------|-------------------|
| checkout, cache, artifacts | native (+ persistent no-op) |
| mise-action, sccache-action, setup-mold | native |
| rust-cache | native (prefer not primary stack) |
| paths-filter | native |
| docker login/buildx/bake/build-push | native family |
| pages / deploy-pages | partial — complete for jackin/termrock |
| attest-build-provenance | **gap — verify/native** |
| renovatebot | image path |
| dtolnay/rust-toolchain | **eliminate** (no product adapter) |
| cargo-deny-action | prefer mise `cargo-deny` |
| fsfe/reuse-action | prefer mise reuse |

---

## 4. Reference patterns (copy from where)

| Pattern | Source |
|---------|--------|
| Lanes + package job names + sccache order | java-monorepo `rust.yml` (evolve to inline matrix) |
| Policy prose | java-monorepo `.github/AGENTS.md` + jackin workflow AGENTS |
| Clarity / reuse / audit | jackin PR #810 (principles L1; machinery L2/L3) |
| Docker factory | blockchain-nodes `build-publish.yml` |
| Instant-cache semantics | Velnor `docs/perf-instant-cache-plan-2026-06-11.md` |
| Clean Ubuntu CLI baseline | holla (add lanes only) |
| Cache ownership / budgets | jackin rust-build-cache-hygiene → Velnor GC |

---

## 5. Repo classes (templates)

| Class | Repos | Shape |
|-------|-------|--------|
| **A** Production monorepo | jackin, java-monorepo | Many workflows; full lanes; path filters; docker+sccache; optional #810 L2/L3 |
| **B** Product/CLI | parallax, holla, velnor, ruxel, termrock | ci + release (+ docs/preview); lanes; zigbuild on Ubuntu |
| **C** Docker factory | blockchain-nodes | lane matrix on builds; registry cache; exists-sweep |
| **D** Library | schemalane, pg-bigdecimal, tracing-request-level, tablerock | single ci + release; mise+sccache+lanes |
| **E** Playground | parallax-telemetry-playground | Class D + polyglot mise tools |

---

## 6. Fleet & operations (before flipping defaults)

### Labels

- `self-hosted` + `velnor-target-mvp` (current estate label).  
- Future multi-host: atomic label migration only.

### Registration

13 repos × 3 orgs (`jackin-project`, `ChainArgos`, `tailrocks`):

1. Prefer **org-level JIT** + runner groups (Velnor P0).  
2. Interim: per-repo JIT sharing host stores (ops-heavy).  
3. Production private → `VELNOR_TRUST_SCOPE=trusted` + Docker; forks never share that pool.

### Capacity (start; measure)

| Pool | Repos | Slots (start) |
|------|-------|----------------|
| ChainArgos | java-monorepo, blockchain-nodes | 10 + 4 (current units) |
| jackin-project | jackin | 6–10 |
| tailrocks | rest | 8–12 shared |

Keep `VELNOR_JOB_CPUS` / `VELNOR_JOB_MEMORY` daemon caps.

### Job image / mise needs

| Need | Who |
|------|-----|
| rust/fmt/clippy/mold/sccache/mise | all (image) |
| protoc, GraalVM/node/python | java-monorepo |
| zig + zigbuild | holla, velnor, parallax, jackin, ruxel |
| bun | jackin, parallax, termrock |
| uv/ansible | ruxel |
| buildx/qemu | docker factories |
| postgres services | schemalane — **verify `services:`** |

---

## 7. Velnor runner development backlog

Prioritized by unblocking estate default-flip and stability. Detail for cache: §2.5 + `docs/rust-build-cache-hygiene-velnor.md`.

### P0 — before mass default-flip

| ID | Item |
|----|------|
| V0.1 | Org-level JIT + multi-repo fleet |
| V0.2 | Stable fleet (heal zombies; dual health signals) |
| V0.3 | Host-persistent caches remain correct (HOME/CARGO truthful) |
| V0.4 | Native adapters green for estate action set |
| V0.5 | `services:` / job-container parity (Postgres) |
| V0.6 | Secret Docker login on trusted pools only |
| V0.7 | **Destructive cache GC** (shared lock, in-use scopes, budgets, TTL/LRU, forensics) — run before disk-floor park |
| V0.8 | **Cache accounting** in doctor/`cache du` (logical+physical, class/scope, high-water alerts) |
| V0.9 | **Inject** `SCCACHE_CACHE_SIZE`, `SCCACHE_BASEDIRS`, default `CARGO_INCREMENTAL=0` on job containers |
| V0.10 | Fixture proves **inline lane matrix** expression |
| V0.11 | **Canonical storage contract** (`/var/lib`, `/var/cache`, `/run`, `/var/log`), catalog, resolved-path/status CLI, and legacy-root migration |
| V0.12 | **Filesystem capacity controller** with active leases, target generations, per-class budgets, BuildKit ownership, job reservations, hysteresis, and hard emergency reserve |
| V0.13 | **Reclaim before accept**: reserve worst-case space before advertising a slot, automatically GC toward the reservation, refine it on acquisition, and never silently reject an assigned job |
| V0.14 | **Strict Rust capability manifest**: validate full job/ref/input/value/backend surface before side effects; exact errors; no ignored inputs, approximation, or unknown-action fallback |

### P1 — parity quality

| ID | Item |
|----|------|
| V1.1 | attest-build-provenance native or documented |
| V1.2 | Pages deploy full parity |
| V1.3 | QEMU / multi-arch reliability |
| V1.4 | Dynamic slot autoscaling |
| V1.5 | Local composite actions always resolvable |
| V1.6 | Job image track ubuntu-26.04 |
| V1.7 | Consistent backend stats in logs via adapter post step (warm-run audits; strict contract owns reporting) |
| V1.8 | Target-bucket keep-newest-N + profile-aware keys when persist enabled |

### P1-perf — job latency (ranked by [`docs/p3-performance-design-2026-06-11.md`](docs/p3-performance-design-2026-06-11.md); verify current status before scheduling — several may be partially landed since 2026-06-11)

| ID | Item | Measured basis |
|----|------|----------------|
| V1.9 | **Async job finalization** — post completion/result upload *before* container/network/workdir teardown and log zip | kills 2–8 s typical, 30–34 s contended trailing gap (p3 #6) |
| V1.10 | **Container pre-create at slot acquire** — job-container boot ∥ checkout; persistent per-slot docker network | 2–4 s/job + removes contended iptables churn (p3 #7) |
| V1.11 | **Host bare git-mirror store** (`_velnor_git`) — delta fetch into mirror, local clone in checkout adapter | checkouts 5–16 s → <1 s (p3 #5, CONFIRMED) |
| V1.12 | Overlap JIT re-registration with job completion (slot turnaround) | 1–3 s/job (p3 #9) |
| V1.13 | Reflink (`FICLONE`) cache restore/save on XFS — O(metadata) instead of full byte copy | p3 defect #5 |
| V1.14 | **Per-step timing + cache report in the job summary** — adapter post steps publish queue wait, step wall, hit/miss, bytes, store identity | the "clear what's going on" mandate; feeds `audit-ci` perf mode |
| V1.15 | Queue-pickup + pickup→first-step SLOs in doctor, backed by `trace.jsonl` spans | §2.11 budget enforcement |
| V1.16 | Productize dual-lane compare: `velnor-tools compare` — per-workflow timing + log diff between lanes of one `both` run | builds on `.velnor-compare` / lane-compare-watch |

### P2 — scale / superiority

| ID | Item |
|----|------|
| V2.1 | Multi-host shared stores |
| V2.2 | Compiler-cache backend seam (`sccache | kache | off`): **local-only approved surface**, capped sccache remains default; Kache canary; separate stores/budgets; never stack wrappers |
| V2.3 | Native sccache + Kache local experiment using `strict-capability-contract.md`; GHA/S3 modes require separate future approval |
| V2.4 | Fewer node actions (mise for reuse/gitleaks patterns) |
| V2.5 | Fixture expansion for every new estate action |

### Explicit non-goals

- macOS job execution.  
- Classic runner protocol.  
- Supporting `dtolnay/rust-toolchain` as product path.  
- Velnor-only YAML that cannot run on GitHub.  
- Lane-conditional YAML beyond writer-gating and the lane name suffix (§2.0 Law 1).  
- Pinning to old action majors / deprecated inputs for convenience (§2.0 Law 2).  
- One global `CARGO_TARGET_DIR` for all repos.

---

## 8. Implementation plan (PR sequence)

Do **not** open 13 PRs on day one.

**Delivery model — one branch per repo (§2.12).** The entire program lands
as exactly ONE branch named `velnor-estate-standard` in every repository —
velnor included (all runner work + dogfood CI on that one branch) and the
fixture included. Phases below order the opening and merging of those
branches; they never split a repo's work across branches. Every dual-runner
verification (V-B three-lane dispatch, V-C timing) runs **from the repo's
program branch before merge** — the same configuration must be proven on
GitHub and on Velnor for everything the branch changes.

### Phase 0 — Contract + runner readiness

1. Land / keep this document as law.  
2. Snippet pack: inline matrix + `setup-rust-ci` (fixture first).  
3. Close V0.7–V0.10 (GC, budgets, matrix proof) enough for fleet safety.  
4. Confirm fleet labels and org JIT approach.  
5. **Host baseline cleanup**: before any verification campaign, clean the
   Velnor host to a recorded baseline — stale runner registrations, leftover
   velnor Docker resources, legacy `unknown-repository` store trees,
   over-budget BuildKit stores — inventory-first, supervised, owned-resource
   deletion only (never broad prune). Cold/warm measurements on a polluted
   host are not trustworthy. The durable fix remains V0.7–V0.13; this is the
   one-time test-bed reset.

### Phase 1 — Reference alignment

| # | Repo | Focus |
|---|------|--------|
| 1 | java-monorepo | Inline matrix; truthful mode; preserve ubuntu-26.04 |
| 2 | blockchain-nodes | `lanes`; pin; selected-lane sweep; QEMU action |
| 3 | jackin | Default **velnor**; 26.04; no macOS; AGENTS; #810 L1 |

### Phase 2 — Dogfood + mid-tier

| # | Repo | Focus |
|---|------|--------|
| 4 | velnor | Lanes + dogfood |
| 5 | holla | Lanes only |
| 6 | ruxel | Lanes + pin + ansible/uv |
| 7 | parallax | Full lanes; no macOS |

### Phase 3 — Libraries + cleanup

| # | Repo | Focus |
|---|------|--------|
| 8 | termrock | Modernize Class B |
| 9 | schemalane | Class D + Postgres |
| 10 | pg-bigdecimal | Class D |
| 11 | tracing-request-level | Class D |
| 12 | parallax-telemetry-playground | Class E |
| 13 | tablerock | Replace with Class D |

### Phase 4 — Enforcement

1. `velnor-tools audit-ci` against all 13.  
2. Fixture snippets for standard patterns.  
3. Reconcile `docs/master-plan.md`, `docs/mission.md`, `AGENTS.md` with **Velnor default everywhere**.  
4. Required-check strategy (operator: final state = Velnor required).

### Dual-runner verification protocol (every repo, every migration PR)

The same YAML must be **proven**, not assumed, on both runners. Four gates,
in order; a repo is "migrated" only after V-D.

| Gate | What | Pass condition |
|------|------|----------------|
| **V-A Fixture first** | Any *new* pattern (inline matrix, `services:`, backend selection, new action) lands in `tailrocks/velnor-actions-fixture` before any estate repo | Both lanes green in the fixture; capability manifest covers the pattern |
| **V-B Lane parity** | On the migration PR: dispatch `lanes=velnor`, `lanes=github`, `lanes=both` once each | All green; identical job/step sets (names, order, conclusions) across lanes; `velnor-tools compare` (V1.16) log/timing diff clean for one representative workflow |
| **V-C Perf acceptance** | Cold + warm + no-change rerun timed on both lanes (§2.11 protocol) | Velnor warm/no-change meets class budget; zero download/compile/tool-install markers on warm (`audit-ci` perf mode); timing table pasted into the PR description |
| **V-D Post-merge soak** | One week of automatic events on Velnor default | Doctor green; queue-pickup SLO held; no lost/zombie jobs attributable to the repo's workflows |

Any V-B/V-C divergence is fixed **in Velnor, never by weakening the
workflow** (fixture-is-contract rule, applied to the estate).

### Per-PR checklist

- [ ] `.github/AGENTS.md` — three lanes + ubuntu-26.04  
- [ ] No lane-conditional steps beyond writer-gating + lane name suffix (§2.0 Law 1)  
- [ ] Every `uses:` at latest stable major, SHA-pinned at PR time; Renovate active (§2.0 Law 2)  
- [ ] `workflow_dispatch.inputs.lanes` default `velnor`  
- [ ] Inline canonical matrix; `runs-on: ${{ matrix.config.runner }}`  
- [ ] Job names include `(${{ matrix.config.lane }})`  
- [ ] `mise.toml` + `mise.lock` + `rust-toolchain.toml` (if Rust)  
- [ ] No `dtolnay/rust-toolchain`; no unpinned actions  
- [ ] sccache + cargo registry cache on compile jobs; `CARGO_INCREMENTAL=0`; `SCCACHE_GHA_ENABLED=false` (strict contract)  
- [ ] No Swatinem/rust-cache alongside the standard stack; no tool compilation during setup  
- [ ] `concurrency` group on every workflow; PR runs `cancel-in-progress: true`  
- [ ] `timeout-minutes` on every job (≈2× warm p95, floor 10)  
- [ ] `fetch-depth: 0` only with a comment naming the consumer; `persist-credentials: false` unless pushing  
- [ ] Docker cache-from/to if images; `provenance: false` off the release path  
- [ ] Single-writer on `both` for mutators  
- [ ] No macOS/Windows jobs  
- [ ] V-B: `lanes=velnor`, `github`, `both` dispatched once each, all green  
- [ ] V-C: cold/warm/no-change timing table in PR description; budgets met (§2.11)  
- [ ] Velnor rerun-idempotency smoke (no crate download wall)

---

## 9. Ideal minimal Class D `ci.yml`

Illustrative — pin SHAs at PR time.

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
  workflow_dispatch:
    inputs:
      lanes:
        description: velnor (default) | github | both
        type: choice
        default: velnor
        options: [velnor, github, both]

permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: "0"
  CARGO_PROFILE_DEV_DEBUG: line-tables-only
  RUSTFLAGS: "-C link-arg=-fuse-ld=mold"
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "false" # strict contract: local-only compiler cache
  SCCACHE_CACHE_SIZE: 20G

jobs:
  rust:
    name: Format, lint, and test (${{ matrix.config.lane }})
    timeout-minutes: 20
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON((github.event_name == 'workflow_dispatch' && inputs.lanes == 'both') && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true},{"lane":"GitHub","runner":"ubuntu-26.04","writer":false}]' || (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && '[{"lane":"GitHub","runner":"ubuntu-26.04","writer":true}]' || '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true}]') }}
    runs-on: ${{ matrix.config.runner }}
    steps:
      - uses: actions/checkout@<pin> # v7  (default fetch-depth: 1 — keep shallow)
        with:
          persist-credentials: false
      - uses: rui314/setup-mold@<pin> # v1
      - uses: mozilla-actions/sccache-action@<pin> # v0.0.10
        with:
          version: v0.16.0
          disable_annotations: "false"
      - uses: jdx/mise-action@<pin> # v4
        with:
          install_args: rust cargo:cargo-nextest
          cache_save: ${{ github.ref == 'refs/heads/main' && matrix.config.writer }}
          github_token: ${{ secrets.GITHUB_TOKEN }}
      - name: Cache Cargo registry
        uses: actions/cache@<pin> # v6
        with:
          path: |
            ~/.cargo/registry/cache
            ~/.cargo/registry/index
            ~/.cargo/registry/src
            ~/.cargo/git/db
          key: cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: |
            cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-
      - name: Ensure Rust components
        run: rustup component add rustfmt clippy
      - name: Format
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --locked --all-targets --all-features -- -D warnings
      - name: Test
        run: cargo nextest run --workspace --locked --all-features
      # Cache reporting is owned by the sccache action / Velnor adapter post
      # step (strict contract) — no ad-hoc `sccache --show-stats` build step.
```

---

## 10. One configuration system (options)

| Approach | Pros | Cons | Rec |
|----------|------|------|-----|
| A. Copy-paste YAML | Simple | Drift | OK short-term libraries |
| B. Vendored composites | Readable | Still N copies | Mid-term |
| C. `tailrocks/velnor-ci-actions` | One upgrade | Cross-org trust | **Best for tailrocks** |
| D. Generate from xtask | Max consistency | Heavy | Monorepos only |
| E. `velnor-tools audit-ci` | Prevents regression | Does not install | **Always** |

**Combination:** C for tailrocks, B/A for ChainArgos/jackin (mature in-repo actions), **E always**.

---

## 11. Success metrics

| Metric | Target |
|--------|--------|
| Default lane | 100% listed repos default `velnor` on automatic events |
| Manual modes | `github` and `both` work on every workload workflow |
| Ubuntu | Zero `ubuntu-latest` / macOS on standardized jobs; GitHub = `ubuntu-26.04` |
| Toolchain | Zero `dtolnay/rust-toolchain` |
| Warm rerun | No dep download/compile wall on Velnor same-commit rerun |
| Queue pickup | ≤ 3 s on a free Velnor slot (SLO in doctor, V1.15) |
| Warm setup overhead | ≤ 5 s Velnor lane; ≤ 60 s GitHub lane (§2.11) |
| No-change rerun wall | Class D ≤ 60 s; Class B ≤ 90 s; Class A critical path ≤ 2 m 30 s; Class C bake ≤ 90 s — re-baselined each campaign |
| Perf regression gate | `audit-ci` perf mode green on warm reruns estate-wide |
| Hygiene markers | Zero missing `concurrency`/`timeout-minutes`; zero uncommented `fetch-depth: 0`; zero double-cache stacks (§3.0 table all-green) |
| Cache stability | Steady-state under budgets; GC skips active/cross-trust; no incremental growth |
| Clarity | Job name alone explains purpose + lane; every compile job ends with the adapter cache report (V1.14) |
| Runner | All P0 items closed before tailrocks mass flip |

---

## 12. Open decisions for the operator

1. **Required checks:** GitHub lane required, optional, or dispatch-only once Velnor is default?  
2. **Org JIT timeline:** block mass onboarding until shipped, or temporary N repo-level daemons?  
3. **Shared `velnor-ci-actions`?**  
4. **Which macOS artifacts need a zigbuild proof before migration?**  
5. **jackin PR #810:** ANSWERED (operator, 2026-07-18) — the entire jackin
   program delivery lands **on top of PR #810's head branch**; that PR stays
   jackin's single program PR (the §2.12 one-branch rule maps to that
   existing branch for jackin only; every other repo uses
   `velnor-estate-standard`).  
6. **Public forks?** → non-trusted pool without secrets/Docker.  
7. **GitHub-lane compiler cache:** the strict contract makes the GitHub lane a
   cold-compile baseline (`SCCACHE_GHA_ENABLED=false`, job-lifetime store).
   Approve the GHA sccache backend **for the GitHub lane only** (separate
   explicit yes per contract), or accept the cold baseline as the price of a
   truthful comparison lane? Recommendation: keep cold until the first
   estate campaign quantifies the cost, then decide on data. If approved, it
   must land as a manifest-declared adapter transform — one YAML sets the GHA
   backend, the GitHub lane consumes it, Velnor preflight treats the same
   environment as satisfied by the host store — never as lane-conditional
   YAML (§2.0 Law 1).  
8. **Continuous parity cadence:** extend the canonical matrix with a
   `schedule → both` arm so a weekly scheduled run keeps GitHub parity
   verified continuously, or rely on manual `lanes=both` dispatches only?

---

## 13. Appendix — local clone map

| Repo | Path |
|------|------|
| jackin | `/Users/donbeave/Projects/jackin-project/jackin` |
| java-monorepo | `/Users/donbeave/Projects/ChainArgos/java-monorepo` |
| blockchain-nodes | `/Users/donbeave/Projects/ChainArgos/blockchain-nodes` |
| parallax | `/Users/donbeave/Projects/tailrocks/parallax-project/parallax` |
| parallax-telemetry-playground | `/Users/donbeave/Projects/tailrocks/parallax-project/parallax-telemetry-playground` |
| tablerock | `/Users/donbeave/Projects/tailrocks/tablerock` |
| holla | `/Users/donbeave/Projects/tailrocks/holla-project/holla` |
| velnor | `/Users/donbeave/Projects/tailrocks/velnor-project/velnor` |
| ruxel | `/Users/donbeave/Projects/tailrocks/ruxel` |
| termrock | `/Users/donbeave/Projects/tailrocks/termrock` |
| schemalane | `/Users/donbeave/Projects/tailrocks/schemalane` |
| pg-bigdecimal | `/Users/donbeave/Projects/tailrocks/pg-bigdecimal` |
| tracing-request-level | `/Users/donbeave/Projects/tailrocks/tracing-request-level` |

---

## 14. Appendix — sources

- Local workflows / `mise.toml` / `rust-toolchain.toml` for all 13 repos.  
- Velnor: mission, master-plan, runner-usage, instant-cache, action registry, cache-gc-design, rust-build-cache-hygiene-velnor.  
- ChainArgos java-monorepo `.github/AGENTS.md` (lanes + ubuntu-26.04).  
- jackin workflow AGENTS + PR [#810](https://github.com/jackin-project/jackin/pull/810).  
- jackin [`rust-build-cache-hygiene.mdx`](https://github.com/jackin-project/jackin/blob/0cbada6dc0cd2adfc603bffd17287145520d374c/docs/content/docs/roadmap/rust-build-cache-hygiene.mdx).

---

*Next: Phase 0 fixture proof of the inline matrix + Phase 0 cache GC/budgets; verify current status of V1.9–V1.13 against the runner tree; then Phase 1 reference PRs (each through gates V-A→V-D) after operator answers §12.*
