# Independent Homebrew owned-fix review

Verdict: BLOCKED for the requested no-host-global-write boundary. The exact checkout binding and generated CI migration are correct, but the audit helper mutates the host Homebrew repository and leaves an owner directory behind. This is not G2 or install/publication approval.

Review date: 2026-09-20

## Exact pins

- Source remote: `https://github.com/tailrocks/homebrew-velnor.git`
- Branch: `codex/github-first-homebrew`
- Reviewed tip: `257e7fc8805e1d501b97aaef54bb14a880b39444`
- Reviewed tree: `e1342e61b7dc2b7685ea22c9fd8210d43026d265`
- Prior fix: `b4698e481b832e8154abefc25123c21352c9d5b0`
- Comparison base: `be6d455e2075c83e26b80fd2e871bc3b279064f3`
- Remote branch resolves to the reviewed tip.
- Isolated detached tree: `/private/tmp/velnor-homebrew-257-review`; clean, no source edits.

The two commits change five files, 57 insertions and 8 deletions from `be6d455`. The `257e7fc` delta only refreshes the generated scan/config state; `b4698e4` adds the audit helper and typed task wiring.

## Findings

### Exact checkout binding: PASS

`scripts/homebrew-audit-checkout.sh:4-7,14-17,24-38` resolves the repository root from the script location, requires the checked-out stable formula and both stable/preview templates, copies exactly `Formula/velnorctl.rb` into a fresh tap, and invokes `brew audit` with a generated `owner/repo/velnorctl` formula reference. It does not audit an ambient formula name or fall back to the repository's global tap contents.

A fake-brew harness verified the copied formula's SHA-256 exactly:

```text
velnor-contract-audit/velnor-contract.DymVuf/velnorctl 29feb24e0746cd9f9f94edf65b7100ea0276c18e73f5e0f1a4baf7788fd1fbb6
```

The generated random tap directory prevents ordinary concurrent name collisions. The `brew` executable itself is resolved through `command -v brew` (`scripts/homebrew-audit-checkout.sh:14`); no executable path is pinned. The generated runner also retains its separate Linuxbrew PATH bootstrap at `.github/workflows/ci-unit-homebrew.yml:203-217`; this is tool provisioning, not formula selection.

### Host-global mutation: FAIL / blocker

The helper writes directly below the host Homebrew prefix:

- `scripts/homebrew-audit-checkout.sh:19-24` obtains `brew --repository`, creates `$brew_repository/Library/Taps/velnor-contract-audit`, and creates the temporary tap there.
- `scripts/homebrew-audit-checkout.sh:38` runs `brew audit --online` without setting `HOMEBREW_NO_AUTO_UPDATE`; Homebrew may also update global metadata/cache state.
- `scripts/homebrew-audit-checkout.sh:29-32` removes only `$tap_root`, never the owner directory created at line 23.

Using a fake brew whose repository was a disposable directory, the success, audit-failure, and SIGTERM cases all removed the random tap root but left the host-style global path:

```text
<fake-repository>/Library/Taps/velnor-contract-audit
```

The audit-failure case returned `99`; the SIGTERM case returned `143`; both retained that owner directory. Thus the temporary formula is bounded, but the tap parent and potentially Homebrew's online-update state are not. This fails the requested strict “no host global write” and leaves shared global state across concurrent/failed runs.

### Cleanup/recoverability: PARTIAL

`trap cleanup EXIT` runs on normal completion, command failure, and the tested TERM path, and removes the generated tap tree. It cannot remove global Homebrew metadata changes and does not recover the newly-created `velnor-contract-audit` parent. A killed process or host-level interruption also has no stronger cleanup mechanism.

### Formula/template census and preview coverage: PASS, intentionally asymmetric

The tracked Formula census is exactly:

```text
Formula/velnorctl.rb
Formula/velnorctl.rb.template
Formula/velnorctl-preview.rb.template
```

The stable source formula is explicitly documented as a historical source-built baseline (`README.md:21-25`), so it is the only concrete formula copied to the audit tap. The templates contain unresolved generation tokens and are not directly audited as formulas. The helper checks both templates (`scripts/homebrew-audit-checkout.sh:15-17`), while the contract fixture exercises preview generation and archive metadata at `scripts/test-package-update.sh:487-499`, including preview rollback rejection at `:498-499`. Stable generation is exercised at `:366-375` and later reruns. `scripts/package-update.sh:682-708` selects the stable or preview template explicitly and rejects unresolved tokens.

### Typed detector migration: PASS; required workloads retained

- `.github-gen/velnor-workflow.toml:15-19` excludes `Formula/**`, `Casks/**`, and `Brewfile` from the unscoped detector.
- `.github-gen/velnor-workflow.toml:30-31` keeps the Homebrew paths in the unit watch set and declares both `homebrew-audit` and `homebrew-contract-test`.
- `.github/ci/project.toml:28-32` uses `mise run homebrew-audit` and `mise run homebrew-contract-test` for all GitHub/Velnor PR/full command vectors; the old global `brew audit --strict --online` command is absent.
- The clean generator output still reports one Homebrew unit and both commands; no workload was dropped.

### Published generator pin: PASS

The exact Velnor source snapshot `b9c3156cdb88e63c11b9e595a3e694b02238c09` was built without installation. Its binary reports the same revision. A fresh network clone of the Homebrew branch at `257e7fc` was checked with:

```text
/private/tmp/g2-homebrew-generator-b9/target/debug/velnor-workflow --plain --check
```

Result: every generated file was reported `= unchanged`, including `.github/ci/project.toml`, all Homebrew workflows, fleet support, and `.github/ci/.github-actions-generator-state`. The check also rendered the Homebrew unit as:

```text
PR: mise run homebrew-audit ; mise run homebrew-contract-test
```

The installed local `velnor-workflow` binary was a different revision and was not used for this check.

## Verification commands

All commands were read-only against product source except disposable fake-brew/temp fixtures.

- `rtk bash scripts/test-package-update.sh`: PASS — `Homebrew contract fixture validation passed`.
- `rtk bash -n scripts/homebrew-audit-checkout.sh scripts/package-update.sh scripts/test-package-update.sh scripts/test-gh-provider.sh scripts/verify-macos-binary.sh`: PASS.
- `rtk ruby -c Formula/velnorctl.rb`, both templates: PASS (`Syntax OK`).
- `rtk actionlint`: PASS, exit 0.
- `rtk git diff --check be6d455e2075c83e26b80fd2e871bc3b279064f3..257e7fc8805e1d501b97aaef54bb14a880b39444`: PASS.
- Fresh clone status: clean, exact `257e7fc`, remote branch exact.
- Fresh clone pinned-generator `--plain --check`: PASS, all generated files unchanged.
- Real Homebrew audit/install was not run because the helper's host-global mutation is the review blocker.

## Explicit residual blockers

The central empty aggregate, pagination, and non-native/clean-client requirements remain blockers. This Homebrew CI fix does not establish producer authority, native macOS publication, or G2 install proof.

Final verdict: generated typed migration and exact formula handoff are bounded-pass; strict audit isolation is blocked until the temporary tap and Homebrew metadata use a disposable non-global state with complete cleanup.
