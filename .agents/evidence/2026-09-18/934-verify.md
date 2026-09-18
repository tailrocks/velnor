# PR #934 verification — bump generator pin 027f4753 → ec399527 (post-flip green)

- PR: tailrocks/velnor#934, branch `fix/pin-bump-2-post-flip` @ `33eda2c4`, base `main` @ `ec399527`
- Verifier worktree: `/tmp/934-verify-wt` (detached HEAD `33eda2c4`), read-only elsewhere; no push/merge/amend
- CI runs: ci-pr `35238536723` (completed/failure — velnor-only), Policy `35238536089` (completed/success)
- Baseline: PR #930 checks (round-4 final head), ci-pr run `35235701293`

## 1. PIN — PASS

- `revision = "ec3995277f82473777f18969e58ea76f63e54cfd"` exact in `.github-gen/velnor-workflow.toml` (sole line changed in that file).
- Product linked to the flip merge:
  - Release `velnor-workflow-runtime-v1-12309460ffb8359c`: `target_commitish = ec399527…`, notes "built from ec399527…", assets `manifest.json` + 3 platform binaries.
  - `manifest.json`: `revision = ec399527…`, `closure = 12309460ffb8359c…`.
  - Publisher run `35237562834` (ci-runtime-products.yml, main push): success, `head_sha = ec399527…`.
  - Cross-check: local `velnor-workflow closure --rev=ec399527` = `12309460ffb8359c…` — matches release tag.
  - `ec399527` = "Merge pull request #930 … R2m: atomic pin-bump + schema-2 default flip" (2 parents — merge-commit method).
- Single commit: `git rev-list --count origin/main..branch` = 1. DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` present.
- Freshness: `merge-base == origin/main == ec399527`, reconfirmed after all checks via re-fetch — NOT stale.

## 2. PIN-DERIVED-ONLY — PASS

13 files changed, 0 under `crates/`. Every changed line classified (old/new SHA masked):

| shape | pairs |
|---|---|
| `revision = "<SHA>"` (pin line) | 1 |
| `rev: <SHA>` | 29 |
| `run: echo "VELNOR_WORKFLOW_POLICY_REVISION=<SHA>"…` | 27 |
| `EXPECTED_REVISION: <SHA>` | 12 |
| `name: velnor-workflow-runtime-<SHA>-…` (artifact names) | 7 |
| `VELNOR_WORKFLOW_POLICY_REVISION: <SHA>` | 2 |
| `PINNED_REVISION: <SHA>` | 2 |
| `BASE_PIN: <SHA>` | 2 |
| state digests (`config` + 11 output digests, non-SHA lines) | 12 |

All non-SHA lines are state-digest lines only. Zero `crates/**` changes. `schema = 2` in both the TOML and the state file, both sides. Zero remnants of `027f4753…` in `.github-gen`/`.github` on branch (82 hits of new SHA).

## 3. REGEN — PASS

Built `velnor-workflow` at branch HEAD (`cargo build --locked -p velnor-workflow`, ok), ran in verify worktree:
- `--plain --force .` → exit 0, "Generated 21 files", `git status` clean (no tracked mods, no untracked).
- `--plain --dry-run .` → exit 0, "Dry-run: 0 files would change".

## 4. GATES (rerun locally) — PASS

- workflow: `cargo test --locked -p velnor-workflow` — 1590 lib + all integration targets ok, 0 failed.
- runner: `cargo test --locked -p velnor-runner` — 2166 lib (+1 filtered unit) + all targets ok, 0 failed.
- contract: standalone crate (not a workspace member) `cargo test --locked` in `crates/velnor-workflow-contract` — 2 + 4 ok.
- clippy: `cargo clippy --locked --workspace --all-targets -- -D warnings` — exit 0.
- fmt: `cargo fmt --all -- --check` — exit 0.
- actionlint `1.7.12` — exit 0, no findings.

## 5. CI — PASS (mechanism correction noted)

Settled: 7 fail / 20 pass / 49 skipping / 0 pending (76 checks; waited, no 30-min timeout needed).

- **Policy SUCCESS** — 11 rules, 0 failed. `generated-tree`: "every generated file is byte-identical to the render of velnor-workflow at ec399527" (`TreeComparison::Pin` — PIN-MATCH outcome confirmed). Also `pin-reachable` (pin ancestor of head, inherited from base), `pin-monotonic` (pin descends from base validator `027f4753`).
- **Mechanism correction**: the PR-time Policy run did NOT take the Acquire `--check` early-exit fast path — it took the **candidate path** (Acquire step ran 15:11:11 → 15:20:57, ~9m46s polling, then downloaded `velnor-workflow-candidate-ae2681738c338f05-Linux-X64`). This is necessary and correct: `closure(pin=ec399527) = 12309460… ≠ closure(BASE_PIN=027f4753) = 64fdd0bf…`, so the running base validator is not the pin's renderer and the early-exit condition cannot hold pre-merge. Local recomputation confirms both closures and the head candidate closure `ae2681738c338f05…` (matches artifact name). The early-exit fast path applies **post-merge** (item 6). Tree == ec399527-render is confirmed either way.
- **17/17 github-hosted SUCCESS** — all `/ GitHub · hosted` legs pass (bun, docker, docs, opentofu, policy, topology, unit-collector, bench, client, control, model, render, runner, tools, workflow, workflow-contract, velnorctl), plus Planning, DCO, Policy.
- **Velnor-side ran-and-failed-environmentally** — 5 failures (bun, docker, docs, opentofu, prepare-cargo), all identical: `Velnor rejected job (operational_store)` … "job failed closed before execution", "no declared workflow command was executed". Rust-velnor legs skip behind failed prepare-cargo. Same signature as baseline (byte-identical reason lines).
- **Rollups excusable (environmental-only)** — `Control/Required` is a pure needs-gate (`exit 1`); `ci-required` verdict "expected CI job velnor-bun-velnor did not pass: failure", and the only 5 non-success needs are the velnor admission failures (18 success, 13 skipped).
- **No new failures vs baseline**: fail sets byte-identical (7 == 7, same names); pass set = baseline minus `Prune closed-PR cache` only (maintenance check attached to closed PR 930, N/A to open PR 934); skipping set = baseline minus `Cache retention` only (maintenance check). Baseline rollup verdict and velnor signatures identical.

## 6. POST-MERGE prediction

Merge (merge-commit method, as #930) produces M with parents `ec399527 + 33eda2c4`; M's tree == `33eda2c4` tree (base unmoved). Precision note: the pin revision `ec399527` will be **M's parent, not M itself** — "(pin==HEAD)" as literal commit equality does not hold under any merge method (merge/squash/FF). What holds and what Policy checks:

- **Policy GREEN via early-exit fast path**: on mainline, `pin == BASE_PIN == ec399527` (self-consistent), `closure(pin) == closure(base) == 12309460…`, so Acquire takes the `--plain --check` early exit (seconds, no candidate wait); `generated-tree` = Pin-match since `tree(M) == render(ec399527)` (proven by this PR's Policy + local regen); `pin-monotonic` trivially holds; `pin-reachable` holds (parent). No follow-up bump needed: M adds no generator-source changes, `closure(M) == 12309460…`.
- **Github legs EXECUTE green**: same 17/17 (content identical to PR CI).
- **Velnor environmental red**: same `operational_store` admission rejections; rollups red on velnor-only causes. Same as baseline.

## VERDICT: MERGE-OK

All six items PASS. Corrections for the record (not merge blockers): (a) PR-time Policy used the candidate path, not the Acquire early-exit — the early-exit fast path is a post-merge property; (b) post-merge, pin == M's parent with tree == pin-render, not pin == HEAD commit. No action taken on the PR (not merged, per instructions).
