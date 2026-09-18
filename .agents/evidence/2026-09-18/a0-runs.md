# A0 failure-runs evidence (bastion campaign)

Collected: 2026-09-17 (UTC) via `gh api` + `gh run view --log-failed` (attempts where available).
Source of known signatures: `plans/bastion-three-provider-ci/evidence.md` §5.
No run is expired: all four runs returned metadata AND full `--log-failed` output.

## Repo correction (important)

The task brief placed all four runs in `tailrocks/velnor`. Live API says otherwise:

- `35129353335` → `tailrocks/velnor` ✔ (conclusion `failure`, attempt 1)
- `35136207272` → `tailrocks/velnor` ✔ (conclusion `failure`, attempt 2)
- `35114867283` → `tailrocks/velnor` returns **HTTP 404**; live run is in **`jackin-project/jackin`** (conclusion `failure`, attempt 1)
- `35094895601` → `tailrocks/velnor` returns **HTTP 404**; live run is in **`ChainArgos/java-monorepo`** (conclusion `failure`, attempt 1)

Raw failing logs saved alongside: `/tmp/a0/<run-id>.log` (+ `/tmp/a0/35136207272-a1.log` for attempt 1).

---

## Run 1 — velnor `35129353335` (CI / main · workflow_dispatch · main)

Metadata (`gh api repos/tailrocks/velnor/actions/runs/35129353335`):

```json
{"conclusion":"failure","status":"completed","run_attempt":1,
 "head_sha":"3353310c7648fca22698b6c0f4a69ab245127786",
 "head_branch":"main","event":"workflow_dispatch",
 "created_at":"2026-09-16T17:37:37Z","updated_at":"2026-09-16T17:40:01Z"}
```

Jobs (per_page=100): failing = `Policy`, `ci-required`, `Control / Required`. `Control / Planning` = **success**.

### Exact failure signature

Failing job: **Policy**, step **Enforce workflow policy** (`velnor-workflow policy --workflow-root … --head-sha 3353310c… --base-sha 3353310c…`):

```text
error: workflow policy failed: generated-tree
PASS pin-declared           .github-gen/velnor-workflow.toml [generator] revision = b9c3156cdb88e63c11b9e595a3e694b02238c09a
PASS pin-reachable          b9c3156cdb88e63c11b9e595a3e694b02238c09a is an ancestor of head 3353310c7648fca22698b6c0f4a69ab245127786 (inherited from the base branch)
PASS pin-monotonic          the declared pin is the base validator b9c3156cdb88e63c11b9e595a3e694b02238c09a
PASS entrypoint-pin         .github/workflows/ci-policy.yml installs and exports the declared pin (2 literals)
FAIL generated-tree         the tree differs from the render of velnor-workflow at b9c3156cdb88e63c11b9e595a3e694b02238c09a
       - .github/ci/.github-actions-generator-state: differs from the pinned render
       - .github/workflows/ci-main.yml: differs from the pinned render
       - .github/workflows/ci-policy.yml: differs from the pinned render
PASS pull-request-target    only .github/workflows/ci-policy.yml runs on pull_request_target, with the reviewed trigger set
PASS entrypoint-privileges  .github/workflows/ci-policy.yml holds contents: read only, references no secrets, and persists no credentials
PASS trusted-runners        every self-hosted job is gated on a trusted event and an approved runner
PASS action-pins            every action reference is a full-SHA pin or a reviewed local path
PASS workflow-structure     every workflow parses as GitHub would run it
PASS required-checks        .github/workflows/ci-pr.yml emits [ci-required]; live ruleset requires [DCO, Policy, ci-required]
policy: 11 rules, 1 failed
##[error]Process completed with exit code 1.
```

Downstream: `ci-required` → `required CI prerequisite policy did not pass: failure` (exit 1); `Control / Required` → exit 1.

Supporting facts (version-vs-release clause):
- `crates/velnor-runner/Cargo.toml` at head `3353310c…`: `version = "0.1.275"` (via contents API).
- `gh release view v0.1.275 --repo tailrocks/velnor` → **release not found**.
- Policy log shows the runtime built from source: `Finished 'release' profile [optimized] target(s) in 1m 53s`, `Installed package velnor-workflow v0.1.0 (https://github.com/tailrocks/velnor?rev=b9c3156c…#b9c3156c)`.

### Old → new → consequence

- Old (§5): "failed Policy on `generated-tree` against `b9c3156c…` while Planning succeeded; source `velnor-runner` was `0.1.275` with no matching `v0.1.275` release; ~2 min building policy runtime from source."
- New: log-failed re-fetched 2026-09-17 reproduces the signature **byte-for-byte** (same 3 differing files, same 11-rules/1-failed tally); Planning still `success`; Cargo version still `0.1.275`; `v0.1.275` release still absent; build time `1m 53s` in-log.
- Consequence: **signature still reproduces; run NOT expired.** Revalidation input stands.

---

## Run 2 — velnor `35136207272` (CI / PR · pull_request · 904/merge)

Metadata:

```json
{"conclusion":"failure","status":"completed","run_attempt":2,
 "head_sha":"62a74bf58993c8c073d47274897f0db88cbd6159",
 "head_branch":"feat/ci-immutable-runtime-products","event":"pull_request",
 "created_at":"2026-09-16T18:43:48Z","updated_at":"2026-09-16T18:56:14Z"}
```

### Attempt 1 — the §5 race signature (STILL REPRODUCIBLE in logs)

Jobs (attempt 1): only failure = **`Control / Planning`** (+ downstream `ci-required`, `Control / Required`); all unit jobs `skipped`.

Exact error (`Control / Planning`, runtime-install step, env `INSTALL_REV=48a66ad7d56636f8bfa6069fbaf810d0089fef39`, `CLOSURE=f1f88c200e5b3b82548e1e2392b44b5704c33fa34c71f67f3de1a2225d6236ce`):

```text
Cache not found for input keys: velnor-workflow-v3-Linux-X64-f1f88c200e5b3b82548e1e2392b44b5704c33fa34c71f67f3de1a2225d6236ce
release not found
##[error]no runtime product for revision 48a66ad7d56636f8bfa6069fbaf810d0089fef39 (closure f1f88c200e5b3b82); the mainline runtime-product publisher builds it after merge
##[error]Process completed with exit code 1.
```

### Attempt 2 (latest) — DIFFERENT failures; Planning now succeeds

Jobs (attempt 2): `Control / Planning` = **success**. Failures:

1. **`Docker · Docker / Velnor`** — Velnor fail-closed admission rejection (no workflow command executed):

```text
##[group]Velnor rejected job (operational_store)
##[error]Velnor rejected this job before workflow execution.
phase: operational_store
reason: operational store rejected the sanitized admission row; job failed closed before execution
effect: no declared workflow command was executed
remediation: correct the rejected workflow field/action/ref or add the exact reviewed capability to Velnor, publish and deploy that Velnor release, then rerun
##[endgroup]
```

2. **`Control / Prepare Cargo / prepare-cargo`** — identical `operational_store` rejection text (same 6 lines).
3. **`Rust · velnor-workflow / GitHub`** — unit CI command failure:

```text
error: revision 48a66ad7d56636f8bfa6069fbaf810d0089fef39 is not a commit of /home/runner/work/velnor/velnor
error: CI command failed for unit rust-velnor-workflow with exit status: 1
##[error]Process completed with exit code 1.
```

(Context: step `rust-velnor-workflow: cd -- 'crates/velnor-workflow' && mbx run --locked … -- --plain --check ../..`; env `HEAD_SHA=b2e22d834057de0c43c54ae557ab50a307772371`, `BASE_SHA=3353310c7648fca22698b6c0f4a69ab245127786`.)

Downstream: `ci-required` → `selected CI job velnor-docker did not pass: failure` (exit 1); `Control / Required` → exit 1.

### Old → new → consequence

- Old (§5): "failed Planning because product closure `f1f88c200e5b3b82` for revision `48a66ad7…` was unavailable until later."
- New: attempt-1 log re-fetched 2026-09-17 reproduces the race **verbatim** (`Cache not found …`, `release not found`, `##[error]no runtime product for revision …`); attempt-2 log shows Planning **success** (closure became available, exactly as "until later" predicts) with three NEW unrelated failures (2× `operational_store` admission rejections + 1× `revision … is not a commit` in `rust-velnor-workflow`).
- Consequence: **race signature still reproduces on attempt 1; run NOT expired.** Attempt-2 failures are new evidence outside §5 (admission fail-closed + stale-revision check) — record as additional revalidation inputs, do not conflate with the race.

---

## Run 3 — jackin `35114867283` (CI / main · push · main) — repo is `jackin-project/jackin`

Metadata:

```json
{"conclusion":"failure","status":"completed","run_attempt":1,
 "head_sha":"92f347ac39fbf0d6f9853168e2896a6c60522924",
 "head_branch":"main","event":"push",
 "created_at":"2026-09-16T15:22:14Z","updated_at":"2026-09-16T15:43:29Z"}
```

Jobs (44 total, per_page=100): `Policy` = **success**, `Control / Planning` = **success**. Failures (5):

| job id | job name |
| --- | --- |
| 104858587644 | Swift · Apple · / GitHub |
| 104858587808 | Swift · Apple · Swift package (native/Design/Prototypes/UnifiedAgentUsage) / GitHub |
| 104858587843 | Rust · jackin-xtask / GitHub |
| 104865645345 | ci-required (downstream) |
| 104865679128 | Control / Required (downstream) |

### Exact failure signatures

1. **`Swift · Apple · / GitHub`** (unit `swift-package-native`), step `mise run desktop-xcframework` → `cargo xtask desktop xcframework`:

```text
error: desktop xcframework requires macOS (Apple Silicon)
[desktop-xcframework] ERROR task failed
error: CI command failed for unit swift-package-native with exit status: 1
##[error]Process completed with exit code 1.
```

2. **`Swift · Apple · Swift package (native/Design/Prototypes/UnifiedAgentUsage) / GitHub`** (unit `swift-package-native-design-prototypes-unifiedagentusage`):

```text
bash: line 1: swift: command not found
error: CI command failed for unit swift-package-native-design-prototypes-unifiedagentusage with exit status: 127
##[error]Process completed with exit code 1.
```

3. **`Rust · jackin-xtask / GitHub`** (unit `rust-jackin-xtask`), nextest `mbx nextest run --locked --all-features --package 'jackin-xtask'`, 1 of 303 tests fails:

```text
FAIL [   0.006s] (103/303) jackin-xtask::bin/jackin-xtask desktop::tests::release_workflow_invokes_canonical_mise_tasks
thread 'desktop::tests::release_workflow_invokes_canonical_mise_tasks' (3193) panicked at crates/jackin-xtask/src/desktop/tests.rs:159:33:
reading /home/runner/work/jackin/jackin/crates/jackin-xtask/../../.github/workflows/release.yml: No such file or directory (os error 2)
Summary [   0.221s] 106/303 tests run: 105 passed, 1 failed, 0 skipped
error: test run failed
error: CI command failed for unit rust-jackin-xtask with exit status: 100
##[error]Process completed with exit code 1.
```

4. Downstream: `ci-required`, `Control / Required` → exit 1.

Note on nextest-exclusion clause (§5: "Default nextest excluded `dind_e2e`, `session_send_e2e`, `usage_broker_e2e`, `load_options_e2e`"): the failing xtask log shows a plain `mbx nextest run --locked --all-features --package 'jackin-xtask'` invocation with nextest profile `default`; no filter/exclude flags for those e2e names appear in the `--log-failed` output (log-failed covers only failing jobs, and the passing-job logs that would show the exclusion were not requested). The exclusion claim is therefore **not contradicted but not re-proven from this log pull** — re-resolve from workflow files at execution.

### Old → new → consequence

- Old (§5): "Swift-on-Ubuntu failures (`desktop xcframework requires macOS (Apple Silicon)`, `swift: command not found`); desktop test read a deleted `release.yml`."
- New: all three signatures reproduce **verbatim** in re-fetched logs (same units, same exit statuses 1/127/100, same panic site `desktop/tests.rs:159:33`); Policy + Planning both `success`; run_attempt 1 (no other attempts).
- Consequence: **all three signatures still reproduce; run NOT expired.** Revalidation inputs stand; nextest-exclusion clause needs a workflow-file re-read (out of scope for log pull).

---

## Run 4 — chainargos `35094895601` (CI / main · push · main) — repo is `ChainArgos/java-monorepo`

Metadata:

```json
{"conclusion":"failure","status":"completed","run_attempt":1,
 "head_sha":"235e479b150aeb949bc8a5190fba5b84f6303c80",
 "head_branch":"main","event":"push",
 "created_at":"2026-09-16T12:16:42Z","updated_at":"2026-09-16T12:18:22Z"}
```

Jobs (75 total, per_page=100): `Control / Planning` = **success**; **71 skipped**; failures (3) = `Policy` (id `104789709640`), `ci-required`, `Control / Required`. 71 skipped == all 71 units skipped ✔.

### Exact failure signature

Failing job: **Policy** (id `104789709640`), step 6 **Enforce workflow policy** (`velnor-workflow policy … --no-pin-build`, `HEAD_SHA=BASE_SHA=235e479b…`, `VELNOR_WORKFLOW_POLICY_REVISION=1279c4f92c97b75dc4cc627f122e119f8a5eae16`):

```text
PASS pin-declared           .github/workflows/ci-policy.yml VELNOR_WORKFLOW_POLICY_REVISION: 1279c4f92c97b75dc4cc627f122e119f8a5eae16 (.github-gen/velnor-workflow.toml declares no [generator] revision, so the generator rendered its own)
PASS pin-reachable          not applicable: ChainArgos/java-monorepo consumes the generator from tailrocks/velnor; the pin is a commit of that repository, proven when the pinned generator is obtained
PASS pin-monotonic          not applicable: ChainArgos/java-monorepo consumes the generator from tailrocks/velnor; the pin is a commit of that repository, proven when the pinned generator is obtained
PASS entrypoint-pin         .github/workflows/ci-policy.yml installs and exports the declared pin (2 literals)
FAIL generated-tree         the tree differs from the render of velnor-workflow at 1279c4f92c97b75dc4cc627f122e119f8a5eae16
       - .github/ci/.github-actions-generator-state: differs from the pinned render
PASS pull-request-target    only .github/workflows/ci-policy.yml runs on pull_request_target, with the reviewed trigger set
PASS entrypoint-privileges  .github/workflows/ci-policy.yml holds contents: read only, references no secrets, and persists no credentials
FAIL trusted-runners        4 findings
       - .github/workflows/maintenance.yml: job prune-pr-cache: self-hosted jobs require a default-branch trusted-event gate
       - .github/workflows/nightly.yml: job dispatch-ci-main: self-hosted jobs require a default-branch trusted-event gate
       - .github/workflows/nightly.yml: job nightly-red-to-signal: self-hosted jobs require a default-branch trusted-event gate
       - .github/workflows/nightly.yml: job nightly-alert: self-hosted jobs require a default-branch trusted-event gate
PASS action-pins            every action reference is a full-SHA pin or a reviewed local path
PASS workflow-structure     every workflow parses as GitHub would run it
PASS required-checks        .github/workflows/ci-pr.yml emits [ci-required]; no live ruleset supplied (--ruleset-contexts)
policy: 11 rules, 2 failed
error: workflow policy failed: generated-tree, trusted-runners
```

Downstream: `ci-required` (`required CI prerequisite policy did not pass`), `Control / Required` → exit 1.

Note on the remaining §5 sub-claims (inspected tree lacked `.github/actions/` despite `report-velnor-ci-outcomes` references; Rust Docker wrong context + omitted bake targets): these are **tree/workflow-file observations, not log lines** — the re-fetched `--log-failed` output neither confirms nor contradicts them. Re-resolve from the audited tree at execution.

### Old → new → consequence

- Old (§5): "two failed rules (`generated-tree`, `trusted-runners`); all 71 units skipped; trust findings on maintenance/nightly jobs."
- New: log-failed re-fetched 2026-09-17 reproduces **exactly**: 11 rules / 2 failed, same single generated-tree diff (`.github/ci/.github-actions-generator-state`), same 4 trusted-runners findings (`prune-pr-cache` + 3 nightly jobs), skipped-job count = 71.
- Consequence: **signature still reproduces; run NOT expired.** Policy sub-signature stands; tree-level sub-claims need a tree re-read (out of scope for log pull).

---

## §5 comparison summary

| # | Run (true repo) | §5 signature | Still reproduces? | Expired? |
| --- | --- | --- | --- | --- |
| 1 | 35129353335 (tailrocks/velnor) | Policy `generated-tree` vs `b9c3156c…`, Planning OK; runner-src `0.1.275` w/o `v0.1.275` release; ~2 min source build | YES — verbatim (3 diff files, 11/1 tally, `1m 53s`, release still missing) | NO |
| 2a | 35136207272 attempt 1 (tailrocks/velnor) | Planning race: closure `f1f88c200e5b3b82` for rev `48a66ad7…` unavailable | YES — verbatim (`Cache not found`, `release not found`, `no runtime product …`) | NO |
| 2b | 35136207272 attempt 2 (tailrocks/velnor) | *(not in §5 — new evidence)* Planning OK; 2× `operational_store` admission rejections + `revision 48a66ad7… is not a commit` in `rust-velnor-workflow` | N/A (new) | NO |
| 3 | 35114867283 (jackin-project/jackin) | `desktop xcframework requires macOS (Apple Silicon)`; `swift: command not found`; desktop test reads deleted `release.yml` | YES — all three verbatim (exits 1/127/100; panic `desktop/tests.rs:159:33`) | NO |
| 4 | 35094895601 (ChainArgos/java-monorepo) | Policy `generated-tree` + `trusted-runners` (job `104789709640`); 71 units skipped; maintenance/nightly trust findings | YES — verbatim (11/2 tally, 1 tree diff, 4 findings, 71 skipped) | NO |
