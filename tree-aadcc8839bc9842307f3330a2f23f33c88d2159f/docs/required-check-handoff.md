# Required-check operator handoff

Status: prepared 2026-07-21; no repository policy changed.

## Decision required

Choose one policy for automatic pull-request validation:

1. **Velnor required** — require the `(...Velnor)` contexts below. GitHub is
   available through manual `lanes=github`/`lanes=both` comparison.
2. **GitHub required** — change the automatic-event default before protection;
   the current workflows do not emit GitHub contexts automatically.
3. **Both required** — change the canonical automatic-event matrix before
   protection. Requiring GitHub contexts today would leave every ordinary PR
   permanently pending.

Options 2 and 3 are workflow-standard changes, not branch-setting-only
operations. The current Velnor-default contract makes option 1 the only policy
that can be applied without first changing and re-proving the workflows.

## Preconditions

- Merge or push the exact program heads listed below.
- Run each applicable workflow once on the resulting `main`; GitHub only offers
  recently observed check names for selection.
- Confirm the selected check is emitted for an ordinary pull request and is
  not conditional on changed paths.
- Preserve all existing protection/ruleset settings. Add checks through the
  repository Settings UI or patch only the required-status-check subresource;
  never replace the full protection object from an incomplete template.

Read-only inspection on 2026-07-21 found `main` unprotected in every listed
repository except TermRock, whose branch protection exists without required
status checks.

## Exact Velnor contexts after delivery

| Repository | Delivery prerequisite | Required check context(s) |
|---|---|---|
| `jackin-project/jackin` | Merge PR #810 after its approved attestation capability and fresh proof | `ci-required`; `construct-required`; `docs required (Velnor)`; `DCO` |
| `ChainArgos/java-monorepo` | Merge PR #1753 including `4ef12b2` | `CI required (Velnor)`; `Docker required (Velnor)` |
| `ChainArgos/blockchain-nodes` | Merge PR #651 including `367817f` | `CI required (Velnor)` |
| `tailrocks/parallax` | Push direct-main commit `8190a6d` and complete Ubuntu/Velnor proof | `CI required (Velnor)` |
| `tailrocks/parallax-telemetry-playground` | Push direct-main commit `8fba93b` | `CI required (Velnor)` |
| `tailrocks/tablerock` | Resolve plan 057's current-code clippy STOP and deliver a compliant CI workflow | **Not selectable yet** |
| `tailrocks/holla` | Update PR #37 with `bb1a338`, prove, merge | `CI required (Velnor)` |
| `tailrocks/velnor` | Already merged | `CI required (Velnor)` |
| `tailrocks/ruxel` | Update PR #3 with `d8380df`, prove, merge | `CI required (Velnor)` |
| `tailrocks/termrock` | Push direct-main commit `f20f271` | `CI required (Velnor)` |
| `tailrocks/schemalane` | Update PR #3 with `8058791`, prove, merge | `CI required (Velnor)` |
| `tailrocks/pg-bigdecimal` | Update PR #2 with `dcdb6fa`, prove, merge | `CI required (Velnor)` |
| `tailrocks/tracing-request-level` | Update PR #2 with `106641a`, prove, merge | `CI required (Velnor)` |

## Per-repository UI procedure

For each row whose prerequisite is complete:

1. Open **Settings → Rules → Rulesets** (or **Branches** where classic branch
   protection remains in use).
2. Target the default branch `main`.
3. Enable **Require status checks to pass before merging** and **Require
   branches to be up to date before merging**.
4. Add every exact context from that row. Select the GitHub Actions app as the
   expected source when the UI offers source binding.
5. Save without enabling force-push or deletion allowances.
6. Open a harmless test PR and prove that every selected context appears and
   reaches success; then close it without merging.

## Read-only verification

GitHub's current REST version is `2026-03-10`. Verify classic protection with:

```bash
gh api \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  repos/OWNER/REPO/branches/main/protection/required_status_checks \
  --jq '{strict,contexts,checks}'
```

For rulesets, inspect every active ruleset and its resolved branch rules before
claiming enforcement:

```bash
gh api \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  repos/OWNER/REPO/rulesets --paginate
```

Completion evidence is the saved policy plus a fresh ordinary PR showing all
selected checks. A configured name without a fresh emitted check is not proof.
