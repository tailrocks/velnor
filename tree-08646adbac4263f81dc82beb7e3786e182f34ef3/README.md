# Implementation Plans

This directory holds only **open work**. Completed plan files are removed —
their history lives in git (the 2026-07-03 deep-audit generation, plans
001–032, is fully DONE except the approval-gated plan 015 row below; see the
pre-cleanup tree at tag/commit `17136f9` for the archived files).

The single active execution prompt for the whole program is
[`docs/prompt.md`](../docs/prompt.md) — pass its
body to `/goal`. There are no other active prompts; the old `prompts/`
goal-prompt system is retired (all its sequences completed 2026-06-11).
The live machine-checkable completion ledger is
[`docs/program-requirement-evidence.tsv`](../docs/program-requirement-evidence.tsv);
the concern inventory it references is `config/estate-repositories.json`.

Each executor: **read the plan fully before starting**, honor its STOP
conditions, run every verification command, and update your row below when
done. Plans are self-contained — an executor needs only the plan file and the
repo.

Verification gates used by every velnor code plan (from `mise.toml`):

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo nextest run --workspace --locked`
- `actionlint`

## Outstanding from the 2026-07-03 generation

| Plan | Title | Priority | Effort | Status |
|------|-------|----------|--------|--------|
| 014 | Harden the systemd unit (sandboxing) without breaking Docker/bind mounts | P2 | S | DONE: v0.1.49 apt deployment; hardened unit ran green fixture jobs; live score 7.9 |
| 015 | Purge committed run-log HTML captures with channel tokens + commit policy | P2 | S | BLOCKED: HEAD/guard are clean; capture commit `55ed22f` remains reachable from current heads and 98 release tags through `v0.1.99`; exact freeze, backup, rewrite, rollback, reclone, and verification procedure is prepared but requires explicit coordinated-window confirmation |

---

# Program 033–060 — Estate CI standardization (2026-07-18, velnor `48b04ad`)

Source of law: `VELNOR_PROJECTS_SETUP.md` (rev 2) + `docs/strict-capability-contract.md`
+ `docs/storage-and-disk-pressure-2026-07-18.md`. Code facts verified against
the tree at `48b04ad` by 6 parallel read-only audits, key excerpts re-read
directly. Estate repo facts verified against the local clones on 2026-07-18
(per-plan target SHAs in each plan's Status block).

**Delivery model (binding — VELNOR_PROJECTS_SETUP.md §2.12/§8):** the entire
program lands as exactly **one branch named `velnor-estate-standard` per
repository** — velnor included (ALL runner plans 033–046, the dogfood CI of
050, and 058's docs commit to that single velnor branch as a commit series;
per-plan work = commits, never separate branches), the fixture included
(041), and each estate repo (047–057). **Sole exception — jackin (049):**
its single branch is the existing head branch of
jackin-project/jackin PR #810; the whole jackin delivery stacks there and
merges with that PR (operator decision 2026-07-18, setup-doc §12.5). Every
V-B three-lane dispatch and V-C timing run executes **from the repo's
program branch before merge** — both runners must prove the exact same
configuration. Uniform shape is concern-based law: every concern common to
multiple repositories or required by a repository's class/product surface uses
the same canonical workflow filename, job ids, input names, step order, pins,
environment, cache keys, timeout/concurrency, writer gate, aggregator, and
`.github/AGENTS.md` shared block. Missing required concerns are added;
genuinely non-applicable concerns are classified and omitted, never replaced
with no-op jobs (§2.12; `audit-ci` must enforce both coverage and equivalence).

## Execution order & status — runner (velnor)

| Plan | Title | Priority | Effort | Depends on | Maps to | Status |
|------|-------|----------|--------|------------|---------|--------|
| 033 | Strict capability manifest; no unknown-action fallback | P0 | L | — | V0.14; contract gates 1–3,6,7 | DONE: versioned ref/input manifest, pre-side-effect validation, diagnostic-only Node fallback, export/check CLI, eight focused tests, and all gates green |
| 034 | Compiler-cache backend seam (sccache v0.16.0 baked, kache, off) | P0 | L | 033 | contract gates 4–5; V2.2/V2.3 | DONE |
| 035 | Canonical storage contract + fail-closed identity | P0 | L | — | V0.11; P0 unknown-repo bug | DONE: canonical resolver/catalog/CLI, legacy readability, fail-closed identity, packaging, and all gates green |
| 036 | Capacity controller — leases, reservations, reclaim-before-accept | P0 | L | 035 (037 couples) | V0.12/V0.13 | DONE: serialized reservations, emergency floor/hysteresis, lease-safe shortfall reclaim, explicit backpressure, doctor output, live host proof, and all gates green |
| 037 | Destructive cache GC + physical accounting | P0 | M/L | 035 | V0.7/V0.8 | DONE: guarded destructive GC, leader lock, physical budgets/history, owned-builder boundary, live cleanup, and all gates green |
| 038 | Job-env defaults (SCCACHE_CACHE_SIZE/BASEDIRS, CARGO_INCREMENTAL=0) | P0 | S | — | V0.9 | DONE: defaults, precedence tests, docs, and all runner gates green |
| 039 | Org-level JIT + multi-repo fleet ops | P0 | M | — | V0.1 | BLOCKED: org administration now succeeds, but only the unrestricted Default group exists and the five-slot fleet remains repository-scoped to `tailrocks/velnor`; creating/configuring the restricted group requires explicit operator authority, and Sentry SSH host trust still requires operator acceptance |
| 040 | `services:` parity (host/port env, alias) | P0 | M | — | V0.5 | DONE: V2 service tokens, shared-network aliases, runtime port context, Postgres-shape tests, and all gates green |
| 041 | Fixture: inline matrix, backend jobs, services job, registry sync | P0 | M | (033/034/040 soft) | V0.10; gate V-A | DONE: fixture audits and tools tests green; GitHub `29634753256`, Velnor `29634781889`, both/parity `29634836133` green; Kache canary skipped per documented fallback |
| 042 | Estate adapter completion (Pages, attest, composites, login gate) | P0/P1 | L | 033 | V0.4/V0.6/V1.1/V1.2/V1.5 | DONE: full Pages V2/OIDC loop, explicit attest rejection, composites, denylist, and trust gate verified (577 tests) |
| 043 | Job lifecycle latency (async finalize, pre-create, JIT overlap) | P1 | L | — | V1.9/V1.10/V1.12 | DONE: completion-before-teardown and next-JIT overlap proved live on Sentry |
| 044 | Git-mirror store + reflink copies | P1 | M | 035 | V1.11/V1.13 | DONE: trust mirror live; checkout spans 407–454 ms; no credential persisted |
| 045 | Timing observability (job summary reports, doctor SLOs) | P1 | M | 043 soft | V1.14/V1.15/V1.7 | DONE: six live spans, versioned records/cache summaries, 611-test gate, and v0.1.55 doctor p50/p95 table verified |
| 046 | velnor-tools audit-ci + compare (incl. §2.12 uniform-shape rules) | P1 | M/L | — | V1.16; §2.8/§2.12 | DONE: 49 tool tests, full 611-test gate, real estate audits, and fixture parity/budget run `29636145660` pass |
| 059 | Velnor host baseline cleanup (operator-supervised; inventory now, destructive pass ideally post-035/037, always before the first verification campaign) | P0 | M | 035/037 preferred | §0 gate 7; §8 Phase 0.5 | DONE: committed before/after baseline, root 36%, zero unknown identities/stale Velnor objects, doctor green, both-lane smoke `29636145660` green |

## Execution order & status — estate (one PR per repo)

All estate plans HARD-depend on 041 (fixture-proven inline matrix — gate
V-A). Do not open estate PRs before 041's operator verification.

| Plan | Repo | Phase | Effort | Extra deps | Status |
|------|------|-------|--------|-----------|--------|
| 047 | ChainArgos/java-monorepo | 1 | M | — | IN PROGRESS — PR #1753 head `6f7ee98b` is pushed with nextest-only probes and trace-spike instructions, current action pins, and canonical concurrency; stale runs were cancelled. Fresh V-B/V-C and explicit human merge approval remain |
| 048 | ChainArgos/blockchain-nodes | 1 | S/M | — | IN PROGRESS — PR #651 head `492b96a` includes the shared package fixes, current metadata/concurrency, and nextest-only instructions; local amd64 proof and 4/4 nextest pass. Fresh V-B/V-C and explicit human merge approval remain |
| 049 | jackin-project/jackin (stacks on PR #810) | 1 | L | 047 (pattern) | IN PROGRESS — PR #810 head `67b60bea` is pushed; standalone audit has zero errors, active commands are nextest-only, actionlint and 2,182/2,182 affected tests pass. Its documentation-example gate passes all 27 library packages and 263 xtask nextest tests. Fleet proof and approval of `docs/capability-proposal-attest-build-provenance-v4.md` remain |
| 050 | tailrocks/velnor (dogfood) | 2 | M | 039 (fleet label) | DONE and delivered — three lanes green, 33 s zero-install rerun passes Class B budget, GitHub recovery drill green, and whole-program PR #100 merged as `c493a2f` |
| 051 | tailrocks/holla | 2 | S | — | IN PROGRESS — PR #37 head `69b4dfc` is pushed and clean; fresh org-fleet V-B/V-C and merge remain |
| 052 | tailrocks/ruxel | 2 | S | — | IN PROGRESS — PR #3 head `e96ad0c` is pushed and clean; executable ignored gates and oracle instructions use nextest. Float summaries are canonical across Python versions; the full format, strict clippy, 215 nextest, dependency, 54 Python verifier, oracle, benchmark, and chaos gate passes. Fresh org-fleet V-B/V-C and merge remain |
| 053 | tailrocks/parallax | 2 | L | 051 (pattern), 042 (attest) | IN PROGRESS — direct-main delivery through `d949f5a` includes deterministic race coverage, nextest/trybuild examples, current action metadata, and canonical scheduled concurrency; local gates pass. Fresh Ubuntu V-B/V-C and the Darwin-release decision remain |
| 054 | tailrocks/termrock | 3 | M | 042 (Pages) | IN PROGRESS — direct-main delivery through `d14c626` passes the full local gate with 337 nextest tests and current action metadata. Fresh V-B/V-C remains |
| 055 | schemalane + pg-bigdecimal + tracing-request-level (Class D trio) | 3 | M | 040 (schemalane services) | IN PROGRESS — PRs #3/#2/#2 are pushed at `6005432`/`b9272fa`/`5abc3ad`; nextest and actionlint evidence is green. Fresh V-B/V-C and merge remain |
| 056 | tailrocks/parallax-telemetry-playground | 3 | S | — | IN PROGRESS — policy-compliant direct-main delivery through `973d9cb` replaces superseded PR #8 and carries nextest-only testing plus current action metadata; fresh V-B/V-C remains |
| 057 | tailrocks/tablerock | 3 | S | — | IN PROGRESS — trunk `9763873` retains the repaired strict-lint/Redis fixture baseline and adds its native macOS checkpoint, transitive XCFramework link metadata, and observable cancellation proof; 768/768 nextest, strict clippy, actionlint, and the shape-aware estate audit pass. Native run `29827520875` is fully green; V-B/V-C waits the org-fleet migration. |
| 058 | Phase 4: concern-based estate convergence, enforcement, docs reconcile, required checks | 4 | L | 046, 047–057 | IN PROGRESS — concern inventory and enforcement now include stable required aggregators; exact policy handoff exists. Delivery/proof, operator decisions, and final zero-error audits remain |
| 060 | Estate-wide nextest-only Rust testing | 4 | M | 041, 046 | IN PROGRESS — active program heads are migrated; former executable doctests have nextest-discoverable integration/trybuild coverage, and ignored gates preserve selection/output semantics. Velnor `1bd00cf` audits live scripts/config/instructions/Rust docs as well as workflows; current 13-repository result is zero errors and zero test-runner findings. Fixture PR #4 and estate PR delivery plus V-A/V-B/V-C remain |

Status values: TODO | IN PROGRESS | DONE | BLOCKED (one-line reason) |
REJECTED (one-line rationale).

## Gates (from VELNOR_PROJECTS_SETUP.md — binding)

- **Mass default-flip gates**: fleet stable (V0.2 — operational, no plan:
  verified by doctor + soak evidence, tracked by operator); org JIT (039);
  cache GC + budgets live (036+037); estate adapters green (042); fixture
  matrix proof (041); per-repo timing baselines (each estate plan's V-C);
  **host cleaned to recorded baseline (059) before any V-C campaign** — the
  §2.11 numbers reference that baseline.
- **Every estate PR passes V-A → V-D** (§8 of the setup doc): fixture-first,
  three-lane dispatch parity, perf acceptance with timing table,
  one-week soak. Lane divergence is ALWAYS fixed in the runner, never by
  weakening the workflow.

## Dependency notes

- 034 needs 033's input-validation engine + ref plumbing; the kache fixture
  job (041) stays disabled until 034 lands.
- 036 and 037 interlock: 037 provides the reclaim engine 036 drives; 036
  provides the leases 037's `in_use_scopes` consumes. Land 035 first; 036/037
  may proceed in parallel with the documented partial-landing fallbacks.
- 043's pre-create must hook AFTER 033's validation point (see 043 STOP #2).
- 045 consumes 043's lifecycle timestamps; 046's compare consumes 045's span
  names (soft — both degrade gracefully).
- 050 (velnor dogfood) needs the fleet registered for the velnor repo (039's
  tailrocks org migration or an interim repo-level daemon).
- 053/054 writer-gate attest/Pages steps until 042 completes those adapters.
- 058 runs last; it re-baselines the §2.11 budgets from measured data.
- 058 also owns the 2026-07-21 concern-based uniformity clarification: build a
  machine-readable per-repo concern inventory, converge every common/applicable
  concern to one canonical implementation, add missing class/product-required
  concerns, classify genuine non-applicability, and extend `audit-ci` to fail
  both missing-required coverage and canonical drift. It must not add fake jobs
  for concerns a repository does not have.
- 014's systemd hardening smoke and 059's host window share the same live
  host access — schedule together.

## Sequencing recommendation (waves)

1. **Wave A (runner P0, parallelizable)**: 033, 035, 038, 039, 040 — then
   034, 036, 037, 042 on their deps; 041 as soon as 033-adjacent pieces
   allow (fixture YAML work can start immediately; registry sync + kache job
   follow). 059 inventory (read-only) starts immediately; its destructive
   pass runs after 035/037 (or immediately before Wave B's first campaign
   window — see 059 STOP #4). 015's non-destructive half and 014's unit
   changes are Wave A fillers; their host/history steps join the 059 window.
2. **Wave B (estate Phase 1)**: 047 → 048 → 049 after 041 verified AND 059's
   baseline recorded.
3. **Wave C (runner P1 + estate Phase 2)**: 043, 044, 045, 046 alongside
   050–053.
4. **Wave D (estate Phase 3 + close)**: 054–057, then 058.

## Deliberately not planned (recorded so nobody re-audits)

- **V0.2 fleet stability** — operational gate, not a code plan; incident
  playbooks + doctor already exist (0.1.15/0.1.16 fixes); tracked by
  operator soak evidence.
- **V0.3 host-persistent cache truthfulness** — already implemented
  (perf-instant-cache plan); re-verified continuously by fixture +
  rerun-idempotency smokes; no new work identified.
- **V1.3 QEMU/multi-arch reliability, V1.4 dynamic slot autoscaling,
  V1.6 job image → ubuntu-26.04, V1.8 target-bucket keep-newest-N** —
  real roadmap items (master-plan P3) without measured urgency for the flip;
  revisit after 058's campaign data.
- **V2.1 multi-host shared stores** — blocked on 035/036/037 maturing;
  design only after single-host budgets hold.
- **GHA-backed sccache for the GitHub lane** — operator decision §12.7;
  if approved it becomes a manifest-declared adapter transform (see
  VELNOR_PROJECTS_SETUP.md §12.7), planned then.
- **Weekly scheduled `both` parity runs** — operator decision §12.8; a
  standard change (matrix schedule arm) requiring fixture-first; planned
  when decided (see 058 step 4).
- **2026-07-03 generation rejections** (edition bump, `rsa` Marvin advisory
  accept-and-document, duplicate transitive crates, doc-test CI step) —
  see the archived README at `17136f9` for rationale; still rejected.
