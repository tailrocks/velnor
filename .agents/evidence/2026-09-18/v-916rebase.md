# VERDICT: CERTIFIED — rebased PR #916 safe to merge normally

Reviewer: independent rebase reviewer (validation only; no edits, no merge).
PR: https://github.com/tailrocks/velnor/pull/916 (feat/a2-producer-revision).
New head: `dd8a1cea75db250d13f016913445324ae21f4be4` (merge `1bee4f23` + `0765af35`).
New base: `0765af35` = origin/main tip (post-rendezvous + #919); merge-base == tip; no drift at review end.
State: OPEN, MERGEABLE (was CONFLICTING), mergeState BLOCKED solely on noise-red ci-required (precedent shape).
Scratch: /tmp/v-916rebase-wt @ dd8a1cea, detached, tree clean. Logs: /tmp/v-916rebase-{test,policy,planning}.log.

## 1. Scope = producer-ONLY vs new base (proven, not trusted)

`git diff 0765af35 dd8a1cea` = exactly 3 files, 412+/77- (counts identical to certified old phase-1):
- `crates/velnor-workflow/src/primitives/runtime_products.rs` (producer change),
- `.github/workflows/ci-runtime-products.yml` (93-line producer render),
- `.github/ci/.github-actions-generator-state` (1 hash line: `ci-runtime-products.yml 96f29b76e5756893 -> 6050ae4a0b60bd31` — same target hash as old phase-1).
- Producer delta ordered-identical to the v-a2-split2-certified old delta: sorted +/- sets equal (394 lines each); full patch diff modulo hunk offsets differs ONLY in the `index` hash line (base file evolved via dc150d2f on other lines; merge combined cleanly). Workflow render patch ordered-identical.
- `MANIFEST_ACCEPT_FILTER` string byte-identical base vs head (line 74 -> 76 shift only).
- `git diff --quiet` IDENTICAL for all 10 consumer/policy/pin files: both setup-action copies, lib.rs, primitives/release.rs, policy.rs, closure.rs, ci-policy.yml, ci-pr.yml, ci-main.yml, velnor-workflow.toml. No consumer changes.
- Namelist contains no campaign code (no bastion/d2a/b1/c2/plans content).

## 2. Pin (STALE-CLAUSE REINTERPRETATION, documented)

- Head pin == base pin == `12b2570072a6294395c87ddea2b785750adfc3ed` (zero pin movement by the PR; pin file `git diff --quiet` identical).
- `git grep 51e635af <head>` = NONE (no self-bump). `git grep 7341ef4b <head>` = NONE (no stale pin).
- The brief's literal "pin still 7341ef4b" is UNSATISFIABLE on post-rendezvous main (7341ef4b was the pre-#917 pin; base pin is 12b25700) and contradicts the "3 files ONLY" clause (a 7341ef4b tree would carry pin-file diffs vs base = a pin revert + consumer-facing change). Correct criterion applied: PR moves no pin (pin == new base). Satisfied.

## 3. Gates in scratch worktree (all observed, all green)

- `cargo test --locked -p velnor-workflow`: exit 0; 868 passed / 0 failed / 16 suites (762 lib + 106 integration incl. 33 + 7 + 0 doc).
- `cargo clippy --locked -p velnor-workflow --all-targets`: exit 0, 0 warnings/errors.
- `cargo fmt -p velnor-workflow --check`: exit 0. `actionlint` (whole tree): exit 0.
- `--plain --dry-run`: exit 0, "0 files would change". `--plain --check`: exit 0, "Generated files are current" + designed candidate notice: tree matches candidate render `10a71b77b0967566...`, not pin 12b25700; bump after merge (phase-1 shape).

## 4. PR CI — fully green per campaign definition (settled: 20 pass / 42 skip / 7 fail / 0 pending)

Run 35193805797 + Policy 35193803123, all on head dd8a1cea:
- PASS: DCO, Control/Planning (13s), Policy (4m26s), ALL GitHub units (Bun/Docker/Docs/OpenTofu + 13 Rust incl. velnor-workflow 3m41s, the candidate publisher). No unit skipped that should run.
- Planning uses pin product: `rev: 12b25700...`, artifact `velnor-workflow-runtime-12b25700...-Linux-X64`; zero `51e635af`/`7341ef4b` refs. Chicken-and-egg gone.
- Policy via CANDIDATE path + HEAD-RENDEZVOUS (log job 105112184616, independently read):
  - `pin 12b25700... shares the base closure but the tree differs from its render; falling through to the candidate path`
  - polls `velnor-workflow-candidate-${head_candidate:0:16}` with `head_candidate` from `--rev="$HEAD_SHA" --candidate`; manifest gate vs head_candidate passed (no error)
  - `VELNOR_WORKFLOW_PINNED_BINARY` + `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` acquired and consumed by Enforce
  - `PASS generated-tree: the tree matches the candidate render (10a71b77b0967566...)` — SAME closure as the scratch `--check` notice (independent rendezvous confirmation)
  - `policy: 11 rules, 0 failed`
- FAIL set (7) check-NAME-identical to merged #917/#920/#919: Bun/Velnor, Docker/Velnor, Documentation/Velnor, OpenTofu/Velnor, Prepare-Cargo, ci-required, Control/Required. All 5 lane logs VERBATIM precedent (`Velnor rejected job (operational_store)` / `operational store rejected the sanitized admission row; job failed closed before execution` / `no declared workflow command was executed`). ci-required fails `selected CI job velnor-bun-velnor did not pass: failure` (5 lane `result: failure` inputs) — downstream-only; Control/Required is an exit-1 mirror. Precedent set: YES (#917, #920, #919 merged with this set).

## 5. Merge mechanics (reported, shepherd owns the merge)

- Ruleset requires DCO (PASS), Policy (PASS), ci-required (red noise-only). `mergeState=BLOCKED` is solely the precedent-identical ci-required red.
- Precedent (#920, /tmp/v-pinbump-918.md + /tmp/pinbump-918.md step 5): merge via REST engaging ruleset bypass (repo admin, NO --admin flag); "no new authorization needed: Policy+DCO green, noise pre-existing." #919 merged the same way. Same path applies here.
- No edits made by reviewer; no merge performed; worktree + logs left in place for audit.

## Verdict rationale

Producer-only 3-file scope proven vs new base with content-identical producer delta; pin == base with no self-bump (stale 7341ef4b literal documented and correctly reinterpreted); no consumer/campaign changes; scratch gates green (868/0, clippy/fmt/actionlint/dry-run/check); CI green per campaign definition with Policy candidate+head-rendezvous proven in-log and closure-cross-confirmed (10a71b77); red set name- and text-identical to three merged precedents; no drift. CERTIFIED.
