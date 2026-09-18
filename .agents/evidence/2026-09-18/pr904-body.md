## Summary

Planning and Policy no longer compile `velnor-workflow` from source on the normal path. CI consumes immutable, attested runtime products; generator changes build their candidate exactly once per platform/source identity as the software under test.

Supersedes #883 (closed): the regenerated `nightly.yml` on this branch is already the 114-line dispatcher with `-R "$GITHUB_REPOSITORY"`, so the #883 fix is subsumed.

## Architecture

- **Stage-0 — closure-keyed releases:** `closure.rs` computes a deterministic source-closure identity over everything that can affect the binary; product tags are `velnor-workflow-runtime-v1-<c16>` (keyed by source closure, not merge SHA).
- **Bootstrap:** `setup-velnor-workflow` is now product-acquisition only — resolve, download, dual attestation, then manifest/digest/self-report verification. No cargo fallback.
- **Stage-1 — candidate:** generator changes build the candidate exactly once per platform/source identity as the software under test; `--pin-build` / `--candidate-manifest` explicit, candidate bound by manifest closure + digest before execution; D19 `--check` is fail-closed.
- **Publishing:** candidate publish is head-anchored (merge==head fast path, one head build otherwise); provisioner is digest-first; policy jobs gain sparse base checkout; lint neutralizes fork mise config (`MISE_NO_CONFIG`).
- **Producer:** new `ci-runtime-products.yml` (push-to-main + dispatch only).

## Security model

- **PRT confinement:** untrusted PR code never executes during planning/policy; only digest-pinned, attestation-verified immutable products run on the normal path.
- **Safe-and-unavoidable:** the single candidate build per platform/source identity is the software under test itself — it cannot be eliminated without losing coverage of the change, and it is bound by manifest closure + digest before execution, so a substituted binary cannot pass verification.

## Gates (all green)

HEAD `9041c9cb`, base `origin/main 3353310c`, D19 pin revision `de6b927d`:

- Tests: 476 lib + 55 integration + 6 contract green
- `clippy -D warnings`, `fmt`, `actionlint` clean
- Regen idempotent: `--plain --dry-run` 0, `--plain --check` green

## Independent verifiers

- Old-validator transition: **CLEAR** (0 failed rules)
- Security spot-check: **clean** (zero cargo/`--pin-build` drift, candidate head-anchored + digest-bound, fork gates intact)
- YAML hygiene: **clean**

## Commits (5/5 DCO-signed)

- `e2f3f131` feat(ci): consume immutable runtime products instead of building velnor-workflow in CI
- `18d0b598` fix(ci): move producer CARGO_HOME to step env; forbid runner context in job env
- `04cb8947` chore(ci): bump D19 pin to 18d0b598
- `de6b927d` fix(ci): silence shellcheck SC2016/SC2129 in generated workflows
- `9041c9cb` chore(ci): bump D19 pin to de6b927d

Merge convention: human feature PR — merge-commit expected.
