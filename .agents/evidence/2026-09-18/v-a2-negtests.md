# VERDICT: CERTIFIED — a2-negtests (G6+G7)

- Branch: `feat/a2-consumer-negatives` @ `4284f246f00f0e07d299f5536cc4a479f839ac23`
- Parent: `38ffbfd7` exactly (claimed base confirmed; single-commit diff)
- Diff: 2 files, +1495/−0 — `consumer_negatives.rs` (new, `#[cfg(all(test, unix))]`) + 2-line `lib.rs` module decl. No production code touched.
- Scratch worktree: `/tmp/wt-a2-negtests` (detached @4284f246, left in place, clean)

## Proof (all run in scratch worktree)

- `cargo test -p velnor-workflow --lib consumer_negatives`: **19 passed / 0 failed**
- `cargo test -p velnor-workflow`: **ALL GREEN** — lib 505/0, integration 2+6+5+9+33, 0 failed (matches author evidence exactly)
- `cargo clippy -p velnor-workflow --all-targets --all-features -- -D warnings`: clean (exit 0)
- `cargo fmt -p velnor-workflow -- --check`: clean (exit 0)

## Execution check (not string assertions)

- Single funnel: all runners → `run_script` → `Command::new("bash")` (line 679). Real step bodies: setup-action steps extracted from shipped `action.yml` (`composite_step_body`), Velnor provisioner rendered via `workflow_pinned_policy_runtime_velnor`, candidate tail via `policy_candidate_step` with drift-pinned extraction.
- 18/19 tests invoke bash through `run_setup_*` / `run_velnor_provisioner` / `run_candidate_tail` (machine-checked per test body).
- The 1 non-bash test (`setup_action_declares_no_checksum_input`) is inherently static: it proves NO checksum/digest/sha256 input exists — absence has no behavior to execute. Accepted; the N6 rejection behavior itself is executed via the candidate tests.
- Harness trust boundaries verified in source: stub `gh` refuses `attestation verify` without pinned `--owner` + `--signer-workflow`; `jq` is logging shim over REAL binary (`exec "$REAL_JQ"`); real `git` over fixture history; expected closures computed via `crate::closure`; hermetic (`GITHUB_SERVER_URL` → dead local path).

## Coverage vs G6+G7

- N1 missing product: setup-action + provisioner, fail closed naming producer ✓
- N2 wrong digest: setup-action + provisioner + candidate-recompute ✓
- N3 wrong manifest: 7 mutations incl. same-16-prefix (tag locator-only) ✓
- N4 untrusted signer: asset + manifest subjects, log proves pinned flags ✓
- N5 lookup confusion: unresolvable pin fails closed; fetched pin resolves to true closure ✓
- N6 PR-checksum-as-trust: foreign closure rejected after digest gate passes + no-checksum-input guard ✓
- G7 cold consumer: empty cache → full verify path, exactly 2 attestation verifications, accept filter ran on both paths, Verify step asserted unconditional (no `if:`) = zero waivers ✓

## Notes (not defects)

- Author's out-of-scope finding (provisioner `git fetch` lacks `-C "$CHECKOUT_PATH"`) confirmed present in the rendered script the suite executes; correctly left for the G3 owner.
- No merges, no pushes, no edits made by verifier.
