# E1 matrix prep — unit×provider capture procedure (design only, no execution)

Status: **read-only design** for work-plan STEP E1 (`plans/bastion-three-provider-ci/work-plan.md` E1).
Authority: `plans/bastion-three-provider-ci/spec.md` §2 (providers/result identity) + §8 (hosted verifier/fault contract).
Inventory input: `/tmp/e1-inventory.md` — **17 units CONFIRMED**, no adds/drops vs `evidence.md` §2.

No command in this procedure mutates bastion, GitHub state, or any repo. All capture steps are reads
(`gh api`, `gh run view`, artifact/JUnit download, generated-tree inspection). Execution belongs to the
E1 author/verifier pair at campaign time.

## 1. Matrix shape (51 cells)

Units (file order, from inventory; `project.toml` blob `9f5ba332…`, 278 lines, identical on
`main@33688938` and campaign branch):

```text
bun-velnor, docker, docs, opentofu, rust-policy, rust-unit-collector,
rust-velnor-bench, rust-velnor-client, rust-velnor-control, rust-velnor-model,
rust-velnor-render, rust-velnor-runner, rust-velnor-tools, rust-velnor-workflow,
rust-velnor-workflow-contract, rust-velnorctl, rust-production-topology
```

Providers (spec §2 canonical IDs, exact strings — no aliases):

```text
github-hosted, github-self-hosted, velnor
```

Matrix = 17 units × 3 providers = **51 cells**. Every cell is independently executed and
independently evidenced. `production-topology` and every Docker-conformance unit are included on
**both** local engines (work-plan E1 action 2). No cell is omitted to fit one run; if hosted job
lifetimes or matrix limits force waves, waves persist per spec §8 and every cell still lands.

Identical-inputs rule (spec §2): each selected eligible Linux unit fans out with the SAME
source SHA, command/profile/features, fixtures, and test expectations on all three providers.
Only the provider lane (runner selector + provider-specific bootstrap) differs.

## 2. Per-cell capture record

One record per (unit × provider). Required fields — a cell without all of them is incomplete,
not green:

| # | Field | Content | Source |
| - | ----- | ------- | ------ |
| 1 | `unit_id` | one of the 17 IDs above | planner output |
| 2 | `provider` | exact canonical ID | planner output |
| 3 | `engine_identity` | `github-self-hosted` → official runner version + Scale Set worker identity; `velnor` → native Velnor engine identity (executor version, NOT described as DinD, spec §5.4); `github-hosted` → GitHub runner image/version | job logs + runner metadata + provisioning IDs |
| 4 | `image_digests` | validated digests (never `latest`): official runner image, private DinD image (official lane), local job/toolchain images (both local lanes) | provisioning records + `docker inspect` digests |
| 5 | `selected_tests` | exact test selector used (command/profile/features + fixture digest) | planner + job log |
| 6 | `junit_counts` | tests/passed/failed/skipped/errors from JUnit XML, per cell | JUnit artifact preserved OUTSIDE disposable containers (spec §8) |
| 7 | `cache_report` | cache namespace + hit/miss + integrity-verified reuse (compiler-cache reuse is legitimate; another lane's test report substituted is fraud, spec §2) | job log + cache records |
| 8 | `timing_report` | queue + provisioning + setup/compile/test/cache/cleanup phase timings | runner/job metadata + telemetry |
| 9 | `cleanup_receipt` | owned-resources-removed record + permit released exactly once | occupancy/capacity ledger |
| 10 | `result` | green/red + full result-identity tuple (§3) | hosted verifier aggregate |

Engine-identity notes:

- Official lane: record runner version SEPARATELY from Velnor/package/protocol refs (spec §5.2);
  verify actual tool content of the official image (never assume hosted catalog).
- Native lane: record native executor identity + mediated-lease API usage; never call it DinD.
- Hostname printed by a job is NOT placement evidence (spec §8). Placement is proven by
  runner/job metadata × provisioning IDs × engine versions.

## 3. Correlation method (run/attempt/job IDs → matrix cells)

Result identity per spec §2 (fixed BEFORE execution):

```text
repository_id + source_sha + run_id + run_attempt + plan_digest
+ unit_id + provider + platform + command/profile/features/fixture_digest
```

Correlation procedure:

1. Fix the expected result set before execution: enumerate all 51 identity tuples from the
   trusted planner output (`source_sha` = run head SHA, `plan_digest` = planner digest).
   This list is the gate — it cannot grow or shrink after first job start.
2. For each observed job, join on:
   - GitHub side: `run_id` + `run_attempt` + `job_id` (all API pages enumerated;
     attempts distinguished — `gh api repos/tailrocks/velnor/actions/runs/<id>/attempts/<n>/jobs`
     paginated, never first-page-only).
   - Display name exposes provider (`Rust · <unit> / <provider> / bastion`, work-plan D2).
   - Management side: provisioning IDs (Scale Set request/JIT/runner IDs; native worker IDs)
     × engine versions × image digests.
   - Outcome side: tests/JUnit counts × cache/timing reports × cleanup receipts (§2 fields 5–9).
3. Match each observed job to exactly one expected tuple. Mismatch classes that FAIL the gate
   (spec §2 + work-plan D2 action 4): missing, skipped, cancelled, timed-out, failed,
   duplicate-conflicting, identity-mismatched, stale-attempt, wrong-provider report.
4. Reruns revalidate exact identity + outcome provenance; a later reporter or cancellation
   must NEVER overwrite an already-failed required result with success (spec §8).
5. Record per cell: `run_url`, `run_id`, `run_attempt`, `job_id`, `job_url`, expected tuple,
   observed tuple, match verdict.

## 4. Planner-declared exception format

Real platform/trust exceptions are declared by the trusted planner BEFORE expansion —
never reclassified after failure (spec §2, work-plan E1 action 3). Format (one entry per
excluded cell or cell group, committed to the ledger BEFORE the run starts):

```text
exception:
  unit_id: <id or "all">
  provider: <canonical id>
  class: platform | trust | unaffected-unit
  reason: <typed capability or trust rule that excludes this cell>
  declared_by: <planner identity + plan_digest>
  declared_at: <timestamp strictly before run creation>
```

Rules:

- `class: platform` cites the unsupported TYPED capability (spec §2 list); unknown selectors
  fail explicitly, never silently become hosted.
- `class: trust` cites the controller-side rule (spec §6); e.g. untrusted-fork lane stays hosted.
- `class: unaffected-unit` cites planner affected-analysis (genuinely unaffected units only).
- Diagnostic/reduced subsets are labeled `coverage: reduced` and can NEVER count as full
  qualification.
- Anything not in this pre-declared list that fails to produce a green cell FAILS E1.

## 5. Fail-fast-disabled verification

Matrix fail-fast MUST be disabled for qualification (spec §2, work-plan D2/E1). Verification
(read-only, on the final generated tree):

1. Inspect every generated workflow containing a provider matrix: `strategy.fail-fast`
   must be literally `false` (not absent-with-default-assumed, not conditional).
2. Prove by expansion test: the generator's provider-expansion test asserts `fail-fast: false`
   on all qualification matrices; a fixture with fail-fast enabled/missing fails the test.
3. Prove behaviorally on the qualification run: after any cell fails, sibling cells in the
   same matrix still start and complete (run timeline shows no matrix-wide cancellation).
   A run where one red cell cancelled siblings is VOID for E1.

## 6. Per-provider independence rule

Each provider's execution stands on its own evidence — a hosted green NEVER certifies a
local lane (spec §2, work-plan E1 action 4). Enforcement:

1. Every cell has its OWN §2 record (own engine identity, own JUnit counts, own timing,
   own cleanup receipt). Sharing any of these across providers voids both cells.
2. Legitimate sharing: compiler/toolchain cache REUSE with namespace isolation
   (repo ID, trust, provider, platform, image/toolchain/ABI — spec §6). Cache reuse is
   content-addressed and integrity-verified; it is not result substitution.
3. Forbidden sharing: copying a test report, JUnit XML, artifact, or verdict from one
   provider's cell into another's. The hosted watchdog correlation (§3) must show distinct
   provisioning IDs + engine versions + JUnit artifacts per provider.
4. Aggregation rule: the required aggregate for a unit is green ONLY if ALL non-excepted
   provider cells for that unit are green on their own evidence. One red local cell with
   two green siblings = unit RED = E1 gate not met.

## 7. Sole-ownership proof command set (read-only)

Proves `velnor-workflow` sole ownership of the FINAL Velnor tree (work-plan E1 action 5,
spec §3.1). Run against the exact qualification SHA in a disposable checkout; no writes:

```sh
# 0. Pin identity: exact SHA under test (from qualification run head_sha).
git rev-parse HEAD

# 1. Exact regeneration: trusted generator rebuilds tree, dry-run reports 0 diffs.
./target/debug/velnor-workflow --plain --force
./target/debug/velnor-workflow --plain --dry-run   # expect: 0 files

# 2. Ownership inventory: every .github/workflows/*.yml + referenced local actions +
#    generated manifests listed; unexpected (hand-written/stale) files flagged → must be none.
./target/debug/velnor-workflow --plain --ownership-inventory

# 3. Local-reference resolution: all local action/manifest refs resolve inside the tree.
./target/debug/velnor-workflow --plain --check-local-refs

# 4. Structured policy: clean on the generated tree.
./target/debug/velnor-workflow --plain --policy-check

# 5. actionlint: clean on all generated workflows.
actionlint .github/workflows/*.yml

# 6. Provider expansion: all three canonical providers expand per eligible unit;
#    no aliases, no legacy mode, no inference from runner.environment/labels.
./target/debug/velnor-workflow --plain --check-provider-expansion

# 7. Native-platform routing: native-capable units route to the velnor lane correctly.
./target/debug/velnor-workflow --plain --check-native-routing

# 8. Fail-closed aggregation: strict expected-result set enforced; any
#    missing/skipped/cancelled/failed cell fails the aggregate (negative fixtures).
cargo test -p velnor-workflow -- aggregation
```

(Exact flag spellings are illustrative of the §3.1 mechanical checks — resolve each against
the implemented `velnor-workflow --help` at execution time and record the resolved command.
The CHECK SET is normative: exact-regen + ownership-inventory + local-refs + policy +
actionlint + provider-expansion + native-routing + fail-closed-aggregation.)

Gates on the final tree (also read-only runs, must all be green):

```sh
cargo test -p velnor-workflow
cargo clippy --all-targets -p velnor-workflow -- -D warnings
cargo fmt --check
```

## 8. E1 evidence bundle checklist

- [ ] 51-cell matrix (§1), each cell with the 10-field record (§2).
- [ ] Pre-execution expected-result tuple list + planner digest.
- [ ] Per-cell correlation row: run/attempt/job IDs → expected tuple (§3).
- [ ] Pre-declared exception list in §4 format (possibly empty), timestamped before run creation.
- [ ] Fail-fast-disabled proof: generated-YAML inspection + expansion test + run-timeline behavior (§5).
- [ ] Per-provider independence attestation: distinct provisioning/JUnit/cleanup evidence per cell (§6).
- [ ] Sole-ownership proof outputs: commands in §7, all green.
- [ ] Independent verifier sign-off (author ≠ verifier, work-plan §0.2).
