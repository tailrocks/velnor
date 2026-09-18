# M3 watch: post-pinbump-2 main @ 231253b3 (merge of #934)

Head: `231253b306e47cf90a6949ae05f1461a99c6286f` — "Merge pull request #934 from tailrocks/fix/pin-bump-2-post-flip"
Parents: `ec399527` (main side, post-flip) + `33eda2c4` (pin-bump-2 branch). Merge touches workflow/gen files only (no runtime-closure inputs).

## Runs on head (all completed)

| Run | URL | Result |
|---|---|---|
| CI / Main (push) | https://github.com/tailrocks/velnor/actions/runs/35240541658 | completed **failure** (expected baseline shape, see below) |
| Runtime products (push) | https://github.com/tailrocks/velnor/actions/runs/35240540373 | completed **success** |
| Preview (push, info only) | https://github.com/tailrocks/velnor/actions/runs/35240541236 | completed failure (guest-payload EACCES, out of scope) |

No separate Policy workflow run exists on push: `ci-policy.yml` triggers only on `pull_request_target` + `workflow_dispatch` (verified in file head). "Policy" below = the **Policy job inside CI/Main** (job 105267556808). PR-side Policy run 35238536089 (pull_request_target on #934) was success.

Plan digest: **`e4e3afe95955da1d`** (Planning log `plan_digest=...`; ci-required env `PLAN_DIGEST`; verdict line `verdict binds plan digest e4e3afe95955da1d`). SELECTED_UNITS = 17 units × providers [github-hosted, velnor].

## CI/Main per-job conclusions (73 jobs)

**Success (19):** Policy; Control / Planning; all 17 `/ GitHub · hosted` legs:
bun-velnor, docker, docs, opentofu, rust-policy, rust-production-topology, rust-unit-collector, rust-velnor-bench, rust-velnor-client, rust-velnor-control, rust-velnor-model, rust-velnor-render, rust-velnor-runner, rust-velnor-tools, rust-velnor-workflow, rust-velnor-workflow-contract, rust-velnorctl.

**Failure (7):** `Bun · velnor — bun-velnor / velnor`; `Docker · velnor — docker / velnor·trusted`; `Documentation · velnor — docs / velnor`; `OpenTofu · velnor — opentofu / velnor`; `Control / Prepare Cargo / prepare-cargo`; `ci-required`; `Control / Required`.

**Skipped (47, all structural):** non-selected backend matrix legs (e.g. `rust-X / velnor`, `rust-X / prepare-cargo` under github-hosted callers and vice versa) + 13 `Rust · X · velnor` jobs via the prepare-cargo guard cascade (see §3).

## Expectation checks

### 1. Policy GREEN via pin-check fast path — CONFIRMED
- Policy job: success. Runtime log line (job 105267556808, 15:29:34Z):
  `pin ec3995277f82473777f18969e58ea76f63e54cfd shares the base closure and renders the tree; the Stage-0 validator renders`
- tree==render proven by: `Result Generated files are current in /home/runner/work/velnor/velnor/policy-checkout`
- Candidate path NOT taken: zero runtime occurrences of `falling through to the candidate path` in the Policy log (only script-text echoes). Pin = first parent `ec399527` (prior main HEAD); no generator change in flight, so no PR candidate-product download/execution.

### 2. All 17 github-hosted legs EXECUTE and PASS — CONFIRMED (17/17 success, counted above)

### 3. Velnor-side + prepare-cargo ran-and-failed-environmentally, zero skips among the executed set — CONFIRMED
All 5 executed velnor-side jobs (bun, docker, docs, opentofu, prepare-cargo) failed identically at admission, zero commands executed (job logs 105267761862, 105267762265, 105267762396, 105267762729, 105267762828):
```
##[error]Velnor rejected this job before workflow execution.
phase: operational_store
reason: operational store rejected the sanitized admission row; job failed closed before execution
effect: no declared workflow command was executed
```
Structural note (by design, not a skip of an expected execution): the 13 `Rust · X · velnor` jobs skip via the guard in `ci-main.yml` (`if:` requires `needs.prepare-cargo.result == 'success' || 'skipped'`); prepare-cargo's environmental failure trips the guard, so they skip by cascade. Cross-matrix legs skip by backend non-selection. None of these are executed-set skips.

### 4. ci-required + Control/Required red ONLY on velnor-admission violations — CONFIRMED
- ci-required emitted exactly one runtime verdict: `expected CI job velnor-bun-velnor did not pass: failure` (the admission-rejected bun job above), then exit 1. All evaluated github-hosted expected jobs passed (evaluation reaches velnor-bun-velnor second, after github-hosted-bun-velnor success).
- Control/Required is a pure mirror: `needs: [ci-required]`, step `if: needs.ci-required.result != 'success'` → `run: exit 1` (ci-main.yml ~L2421). Log is just `exit 1`. Red only because ci-required is red.

### 5. Zero `was skipped` among expected results — CONFIRMED
The literal string `was skipped` appears in the ci-required log only inside unexecuted script `case` branches; zero runtime `was skipped` verdict lines were emitted (single runtime verdict is `did not pass: failure`). Note: ci-required exits at the first non-passing expected job, so later expected velnor-rust results (needs=`skipped` via the §3 guard cascade) are never evaluated into verdicts.

### 6. Runtime products publish for 231253b3 — CONFIRMED (resolve-hit, no rebuild needed)
- Run 35240540373 success: `Resolve runtime closure` success; `Build`/`Publish` skipped because the product already exists.
- Resolved tag (log env): `TAG: velnor-workflow-runtime-v1-12309460ffb8359c` — identical to the release published by run 35237562834 from prior main `ec399527` (manifest `revision: ec399527…`, `closure: 12309460…`, assets: manifest.json + Linux-X64/ARM64 + macOS-ARM64 binaries).
- Linkage: release `target_commitish = ec3995277f82473777f18969e58ea76f63e54cfd` = first parent of merge 231253b3. Correct: the merge touches no closure inputs (`crates/velnor-workflow`, `Cargo.*`, `rust-toolchain*`, `.cargo`), so the closure — and product — is unchanged and the skip-rebuild is the designed outcome.

## Out-of-scope observation
Preview run 35240541236: `Guest payload x86_64/aarch64` failed with `EACCES: permission denied, scandir '.../dist/microvm/work/rootfs-tree/lib/ssl/private'` (real packaging error, not admission). Unrelated to the CI/Main verdict criteria; flagged, not counted.

## VERDICT: GREEN-MAIN
All six expectations hold with log evidence cited above. Main @ 231253b3 is in the expected post-pinbump-2 state: Policy fast-path green, 17/17 github-hosted legs green, red confined to velnor-admission failures + their designed cascades/mirrors, runtime product correctly resolved without rebuild.
