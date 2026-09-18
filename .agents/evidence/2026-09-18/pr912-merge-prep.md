# PR-912 MAIN-LANDING PROCEDURE (merge prep, read-only design — NO MERGE here)

PR #912: `docs/bastion-final-plan` (campaign branch) → `main`, repo `tailrocks/velnor`.
Authority: ledger IMPOSSIBILITY RESULT + UNBLOCK CHAIN steps 6–8 (superseded trigger
mechanics, surviving landing shape), A3 prep `/tmp/a3-prep.md`, gates `pr912-gates.md` A3.
Executor: merge shepherd (fresh; integration-6 stood down). Verifier: different agent.

Pin vocabulary: M_p = merge commit of the rebased producer-only PR (#916 lineage) on main.
The campaign branch was pin-bumped to M_p and regenerated, so #912's tree == render(M_p).

## 0. Preconditions (must hold before §1)

- Rendezvous-fix bootstrap (PR #918) already merged per the user authorization; main healed.
- Revision product for M_p published and verified (chain step 4 analogue).
- #912 branch: pin == M_p, regen exact (`--plain --force` then `--dry-run` = 0 files,
  `--check` = 0, proven in a CLEAN CLONE per the standing regen rule).

## 1. Pre-merge checklist (ALL must pass; any fail = stop, no merge)

1. **PR CI fully green at pin M_p.** `statusCheckRollup` on the PR head: every required
   check `success` — `Control / Planning`, `Policy`, all unit callers + active lane legs,
   `ci-required`, `Control / Required`, DCO. Zero `failure/cancelled/timed_out/
   action_required/stale/neutral`. Inactive lane legs + `Control / Velnor admission`
   `skipped` by design only.
2. **Branch updated with main.** `git merge-base --is-ancestor origin/main <pr-head>`
   true; no "behind main" badge. If behind: `git merge origin/main` on the branch (merge
   commit, no rebase — repairs land as NEW commits, never amend of pushed), re-regen if
   the merge touched generated inputs, re-prove green, restart this checklist.
3. **DCO.** Every commit on the branch carries `Signed-off-by`; DCO check `success`.
4. **No bypass.** Branch protection intact (`required_status_checks` + signatures enforced,
   `enforce_admins` true); merge goes through the protected path only. No admin override,
   no `--force`, no required-check skip.
5. **Regen exact at the PR head.** In a clean clone at the PR head SHA: `--plain --force`
   then `--plain --dry-run` = 0 files AND `--plain --check` = 0. Also `actionlint`,
   structured policy, `cargo test -p velnor-workflow`, contract tests, clippy
   `-D warnings`, `cargo fmt --check` green (a3-prep §5 item 2).
6. **Record the pin.** `M_p = <sha>`, revision-product URL/digest for M_p, PR head SHA,
   PR CI run URLs — paste into the ledger before merging.

## 2. Merge method: merge commit (exact command)

No squash, no rebase — history of the campaign branch is preserved and DCO trailers stay
attached. Fast-forward is impossible (main moved since branch-off); use an explicit merge:

```sh
git fetch origin
git checkout main && git pull --ff-only origin main
git merge --no-ff --no-commit origin/docs/bastion-final-plan
git status --short   # expect clean index, no conflicts
git commit -s -m "Merge pull request #912 from docs/bastion-final-plan" -m "<one-line campaign summary>"
git push origin main
M_912=$(git rev-parse HEAD)   # the merge commit; record in ledger
```

If conflicts: resolve, re-regen if generated files involved, re-run §1 item 5, then commit.
Preferred alternative with identical result: `gh pr merge 912 --merge` (merge-commit mode)
after §1 passes — same protections enforced server-side.

## 3. Post-merge verification: main CI at M_912 (EXPECTED transient Policy RED)

Watch the push trio at M_912 (`gh run list --branch main --limit 3`, §3a commands from
`/tmp/a3-prep.md`): `CI / Main`, `Velnor workflow runtime products`, `Preview`.

**Expected state: Policy RED, everything else green.**

Explanation (tree==campaign render vs pin M_p render): Policy's same-closure rule demands
`tree == render(pin)`. The instant M_912 lands, main's tree is the campaign tree
(regenerated at M_p against the *branch* tree — new docs, generator inputs, scan digest
over branch content), but the declared pin is still M_p. Rendering M_p's generator over
the *new merged tree* yields a different `generator-state`/scan digest than the committed
one, so `Policy → FAIL generated-tree` (same signature as the a1-pr912-red F2: tree
differs from render at the pinned rev). This is mechanical pin-lag, not a product defect:
the tree is exactly the green-at-M_p campaign render; only the pin pointer hasn't caught
up. `Control / Planning` and all unit callers stay green because the runtime product for
M_p exists and the tree is self-consistent; Preview/runtime-products conclusions are
recorded as observed (A1 fixes ride along in #912, so Preview's EACCES signature must be
gone — if it recurs, that is a REAL failure, not the accepted transient).

**Acceptance:** exactly one RED job (`Policy`, generated-tree only) + green aggregators
except the ones `Policy` feeds. Any other red (Planning, any unit, Preview EACCES,
runtime-products) = REAL failure → stop the chain, diagnose, fix forward. Record the full
flat job list (§3c a3-prep) in the ledger.

## 4. Immediate pin-bump PR to the merge commit (heals main)

Open within the ~30 min window; mechanical, no content changes:

```sh
git checkout -b chore/pin-bump-m912 main && git pull --ff-only origin main
# bump generator pin(s) to M_912 ($M_912 from §2), then standing regen rule:
git add -A
velnor-workflow --plain --force
velnor-workflow --plain --dry-run    # must print 0 files
velnor-workflow --plain --check      # must print 0
git add -A && git commit -s -m "chore: pin generator to M_912 <sha>"
git push -u origin chore/pin-bump-m912
gh pr create --base main --title "chore: pin generator to M_912" \
  --body "Mechanical pin-bump healing the accepted #912 transient Policy red (§3). Tree==render(M_912). No content changes."
```

**Why it goes green via the candidate path:** pin M_912 ≠ base pin M_p, so the candidate
exception engages (`VELNOR_WORKFLOW_CANDIDATE_MANIFEST` non-empty): units validate
against the freshly published M_912 candidate product instead of the M_p product, and
Policy compares tree vs render(M_912) — which matches by construction after the regen.
Expected: full green PR CI. Merge it with the same merge-commit method (§2), then verify
main CI at the new HEAD is FULL green (all three sibling workflows). Main is now healed;
the transient window closes.

## 5. Product publication watch

After each merge (M_912 and the pin-bump merge), confirm the publisher emitted the
revision product for the new HEAD SHA before the next step depends on it:

```sh
SHA=<merge-sha>
gh run list --repo tailrocks/velnor --branch main --limit 6 \
  --json workflowName,conclusion,headSha,url \
  --jq --arg s "$SHA" '.[] | select(.headSha==$s)'
# publisher run for $SHA must reach success; record product manifest URL + digest
```

Gate: the pin-bump PR (§4) must not merge until the M_912 product is published (its
candidate path consumes it); the A3 streak (§6) must not start until the pin-bump merge's
product is published.

## 6. A3 streak handoff

Handoff conditions: main HEAD = pin-bump merge commit, pin == HEAD, tree == render(HEAD),
main CI FULL green (CI/Main + runtime-products + Preview, `success att=1`), products
published. Then:

1. Paste the §1 A3 BOOTSTRAP SCOPE DECLARATION (from `/tmp/a3-prep.md`) into the ledger
   with named author + independent verifier.
2. The next `main` push (first commit containing A1+A2+#912) becomes streak-run-1
   candidate iff FULL + green per a3-prep §2/§4; then two more consecutive FULL greens.
3. Streak SHAs must all contain #912 + the pin-bump (pre-fix SHAs never count).

## 7. Transient-red acceptance record (paste into ledger at execution)

```text
#912 TRANSIENT-RED ACCEPTANCE (UNBLOCK CHAIN step 7)
Main is already red at chain time; the A3 streak is measured only after main returns to
green (§6), never across the transient window. Accepted red: Policy generated-tree ONLY
on the M_912 push trio, cause = pin-lag (tree==campaign render at M_p vs pin still M_p),
healed by the immediate mechanical pin-bump PR to M_912 (green via candidate path).
Any other red job on the M_912 trio is a REAL failure and stops the chain.
M_p=<sha> PR-head=<sha> M_912=<sha> pin-bump-PR=<n> healed-main-HEAD=<sha>.
Author: <shepherd>. Verifier: <different agent>. DCO + branch protection never bypassed.
```
