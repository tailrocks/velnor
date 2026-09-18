# PR tailrocks/velnor#933 verification — bump generator pin a6fa8d4a → 40206d9f (bridge validator)

- Branch: `fix/pin-bump-1-bridge` @ `35dad7d56cec7d9dbf6e9bc984c076afb26f1545`; base `origin/main` @ `40206d9fe60a8e0693046d1695762c62883a0578`
- Method: fresh detached worktree `/tmp/933-wt` at branch HEAD, diffed vs `origin/main`. Read-only on repo; nothing pushed/merged/amended.
- Full branch diff saved at `/tmp/933-diff.txt`; Policy log `/tmp/933-policy.log`; CI/PR log `/tmp/933-cipr.log`.

## 1. PIN — PASS

- `.github-gen/velnor-workflow.toml:9`: `revision = "40206d9fe60a8e0693046d1695762c62883a0578"` — exact match.
- Zero `a6fa8d4a` remnants under `.github/` + `.github-gen/`.
- Product linkage (triple):
  - Closure recomputed locally from the `40206d9f` tree with the exact `ci-runtime-products.yml` formula → `64fdd0bf7a8a5800e014bb303e983c5fbf5c460b27cbaa9eff550482d7ef22a0` — matches tag suffix.
  - Release `velnor-workflow-runtime-v1-64fdd0bf7a8a5800` exists, `targetCommitish = 40206d9fe60a8e0693046d1695762c62883a0578`, created 2026-09-17T13:15:43Z.
  - Release `manifest.json`: `"revision": "40206d9fe60a8e0693046d1695762c62883a0578"`, closure matches; 3 platform assets present.
- Single commit (`35dad7d5` only in `origin/main..branch`); `Signed-off-by: Alexey Zhokhov` present (DCO).
- Freshness re-checked at report time: `origin/main` still `40206d9f`, merge-base == HEAD — not stale.

## 2. PIN-DERIVED-ONLY — PASS

- 13 files changed, all under `.github-gen/` + `.github/workflows/` + `.github/ci/`; `crates/**`, `src/**`, `Cargo.*`: zero diff lines.
- Programmatic classification of ALL 52 hunks / 188 changed lines: every `+/-` line is either a pin-swap line (contains old or new full SHA), the `revision =` pin line itself, or a generator-state digest line (`config\t<hex16>` / workflow-output `<hex16>` digests for exactly the 11 regenerated workflow files). Non-pin-derived count: **0**.
- Changed line shapes observed: `rev:`, `BASE_PIN:`, `VELNOR_WORKFLOW_POLICY_REVISION:` (+ its `GITHUB_ENV` echo form), `EXPECTED_REVISION:`, `PINNED_REVISION:`, `velnor-workflow-runtime-<sha>-…` artifact names, state digests. No source/config/render change beyond pin derivation.

## 3. REGEN — PASS

In `/tmp/933-wt` (built `velnor-workflow` from branch sources, `cargo build -p velnor-workflow` clean):
- `velnor-workflow generate . --plain --force` → exit 0, `Result Generated 21 files`, `git status` clean (no bytes changed).
- `velnor-workflow generate . --plain --dry-run` → exit 0, `Dry-run: 0 files would change`.
- `.github-gen/velnor-workflow.toml:1` still `schema = 1` — no flip.

## 4. GATES (rerun locally) — PASS

| gate | result |
|---|---|
| workflow (`cargo nextest run -p velnor-workflow`) | 1702/1702 pass |
| runner (`cargo nextest run -p velnor-runner`) | 2208 pass, 4 skipped (pre-existing skips), 0 fail |
| contract (`cargo nextest run` in `crates/velnor-workflow-contract`, standalone workspace) | 6/6 pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean, exit 0 |
| `cargo fmt --all -- --check` | clean, exit 0 |
| `actionlint` (1.7.12, same as CI) | clean, exit 0 |

## 5. CI — PASS (with one expectation correction)

No wait needed: all 77 checks COMPLETED (20 SUCCESS, 50 SKIPPED, 7 FAILURE).

- **Policy: SUCCESS** — `Velnor workflow policy` run 35232734770, Enforce step: 11 rules, 0 failed (`pin-declared`, `pin-reachable`, `pin-monotonic`, `entrypoint-pin`, `generated-tree`, `pull-request-target`, `entrypoint-privileges`, `trusted-runners`, `action-pins`, `workflow-structure`, `required-checks` all PASS).
- **Correction to the task's parenthetical:** the PIN-MATCH *no-wait* fast path did NOT occur, and could not have. `pull_request_target` runs the BASE `ci-policy.yml` (`BASE_PIN=a6fa8d4a`), and the `40206d9f` closure (`64fdd0bf…`, recomputed) differs from the `a6fa8d4a` closure (`ace4fa94…`, recomputed) — so the same-closure early exit was inapplicable by design. Log proof: `Acquire candidate generator product` ran 14:19:12Z→14:24:02Z (~4m50s poll) until candidate artifact `velnor-workflow-candidate-5063ab07089eee99-Linux-X64` was published (14:23:50Z); Enforce env shows `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` + `VELNOR_WORKFLOW_PINNED_BINARY` bound (fast path would clear the manifest). `generated-tree` then PASSed against the `40206d9f` render. SUCCESS via the designed candidate path — not a PR defect.
- **Every executed GitHub-lane check SUCCESS** (all `… / GitHub` variants green, incl. all Rust units, Planning, Policy).
- **Fail set (7), all environmental:**
  - 5 leaf failures — `Bun/Docker/Documentation/OpenTofu … / Velnor` + `Control / Prepare Cargo / prepare-cargo`: every one fails with `Velnor rejected job (operational_store)` / `operational store rejected the sanitized admission row; job failed closed before execution` / `no declared workflow command was executed`. Self-hosted control-plane admission rejection before any workflow code ran.
  - 2 rollups — `ci-required`, `Control / Required` (aggregate the 5 above; `mergeStateStatus: BLOCKED` follows from them).
- **No new failures:** PR #932 (merged as `40206d9f`) carries the byte-identical 7-failure set; the SHA-only diff cannot cause admission-row rejections (identical failure on a different tree, GitHub twins green). Note: main-push runs skip Velnor lanes, so #932's PR run is the apples-to-apples baseline; main@`40206d9f`'s own push run additionally shows Policy red on pre-existing pin-check drift (`generated files differ … rerun generate` vs the a6 render, then 15-min candidate timeout) — this PR's regen-clean tree resolves that drift on merge.

## 6. UNBLOCK PROOF — PASS

Merged tree == branch HEAD (single commit on current main). Post-merge `pull_request_target` runs the NEW `ci-policy.yml`:

- `.github/workflows/ci-policy.yml:62` — `rev: 40206d9fe60a8e0693046d1695762c62883a0578` (setup action installs the 40206d9f product)
- `.github/workflows/ci-policy.yml:70` — `BASE_PIN: 40206d9fe60a8e0693046d1695762c62883a0578`
- `.github/workflows/ci-policy.yml:172` — `VELNOR_WORKFLOW_POLICY_REVISION: 40206d9fe60a8e0693046d1695762c62883a0578`
- `.github/workflows/ci-policy.yml:176` — Enforce passes `--workflow-root "$WORKFLOW_ROOT"`

The `40206d9f` binary contains the R2 bridge dispatch: `run_from_env` (`crates/velnor-workflow/src/lib.rs:5377` @40206d9f) calls `s2::dispatch::run_if_s2()` (`s2/dispatch.rs:38`), which routes `policy --workflow-root <schema-2 root>` to the s2 pipeline (`wants_s2_runtime`, `s2/dispatch.rs:71-81`; raw-TOML `schema` peek in `dir_is_schema2`, `:123-137`, no struct parse). Combined with §1's published-product proof, the merged tree makes `pull_request_target` run the bridge binary that dispatches schema-2 trees to `s2/policy` — the R2m unblock.

## VERDICT: MERGE-OK

All six items PASS. Caveats (non-blocking, pre-existing): `ci-required` is red on environmental Velnor `operational_store` admission rejections identical to #932 (merge will need the same override path #932 used); Policy took the designed ~5-min candidate path rather than the no-wait fast path because the pin changes the closure. No merge performed.
