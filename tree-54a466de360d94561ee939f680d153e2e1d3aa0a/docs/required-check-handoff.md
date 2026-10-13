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
| `jackin-project/jackin` | Merge PR #810 head `28187d25` after explicit attestation-expansion approval, implementation, and fresh Velnor proof | `ci-required`; `construct-required`; `docs required (Velnor)`; `DCO` |
| `ChainArgos/java-monorepo` | Merge PR #1753 head `78697247` after fresh Velnor proof | `CI required (Velnor)`; `Docker required (Velnor)` |
| `ChainArgos/blockchain-nodes` | Merge PR #651 head `bb2548cc` after fresh Velnor proof | `CI required (Velnor)` |
| `tailrocks/parallax` | Complete exact-head proof for direct-main `705af2ca` | `CI required (Velnor)` |
| `tailrocks/parallax-telemetry-playground` | Direct-main `b6ec71e9` delivered; final proof reconciliation remains | `CI required (Velnor)` |
| `tailrocks/tablerock` | Complete exact-head CI and Native proof for trunk `f6591b04` | `CI required (Velnor)` |
| `tailrocks/holla` | Merge PR #37 head `94fc643a` after final combined proof | `CI required (Velnor)` |
| `tailrocks/velnor` | Merge PR #110 head `720ec17c` after signed release and fleet proof | `CI required (Velnor)` |
| `tailrocks/ruxel` | Merge PR #3 head `3d7684fa` after final combined proof | `CI required (Velnor)` |
| `tailrocks/termrock` | Direct-main `13b05d4c` delivered; final proof reconciliation remains | `CI required (Velnor)` |
| `tailrocks/schemalane` | Merge PR #3 head `fa090cc2` after final combined proof | `CI required (Velnor)` |
| `tailrocks/pg-bigdecimal` | Merge PR #2 head `c1ac5c8c` after final combined proof | `CI required (Velnor)` |
| `tailrocks/tracing-request-level` | Merge PR #2 head `84e6caeb` after final combined proof | `CI required (Velnor)` |

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
