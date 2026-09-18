## Checkpoint 4 (2026-09-18, NEWEST FIRST — present reality; older entries below, never deleted)
Present reality: origin/main = 9f40f929 (#944 merged; B3 chain #939/#940/#941/#942/#944 linear on main, SHAs git-verified). Tags v0.1.275 (@4a7afad0) + v0.1.276 (@9f40f929) exist as annotated tag objects but NO GitHub Release objects → v0.1.275 RELEASE-NOT-PROVEN (5 gaps: cancelled run, no dispatch@tag, 0/14 assets, no OCI/attestations, CI red). APT feed FROZEN Sep-14 (max 0.1.274, #240 layout defect). Bastion GREENFIELD (Debian 13, no docker/runner, SSH ok). Open PRs #943 (typed Docker contexts) + #946 (0.1.277 bump + schema-2 promote fix), both MERGEABLE/BLOCKED. A0 refresh: 11-row drift, 5 blockers. Full detail: /tmp/campaign-ledger.md 2026-09-18 entries (B3 chain, tags, A0 refresh, v0.1.275 verdict, 9-row ledger addendum). Evidence: /tmp/a0-refresh-2026-09-18.md, /tmp/v0275-release-verify.md, /tmp/b3-release-plan.md. No source/checklist/spec/work-plan changes made by this agent.

---

# Bastion campaign checkpoint (2026-09-17 ~02:30 UTC)

Goal: execute bastion three-provider CI campaign (goal-d81ca696). Orchestrator-only main agent.
Branch docs/bastion-final-plan @ 3bb1e23c (11 integration batches merged, local gates green).
Live ledger: /tmp/campaign-ledger.md. Evidence: /tmp/*.md (a0/v-a0/a1/v-a1/a2/v-a2/fix/v-fix/b1/v-b1/c2/v-c2/d1/v-d1/d2/e1/f1/g1/c1/b2/b3/b4/a3/security/integration-N).

Gates: A0 DONE (4/4 certified). A1 fixes merged (preview/stalerev/opstore/timing/pub403).
A2 full (G1-G10) merged. B1 merged. C2-prereq merged. D1a+b+c merged. D1d wiring + D2a schema implementing.
PR #912 CI RED (expected): unblocked by PR #916 (producer-first split) merge → revision product → pin bump.
PR #916 was RED (self-bump chicken-and-egg, /tmp/pr916-red.md); pr916-fix restructuring to generator-only; re-review v-a2-split2 then merge via integration-6.
EXTERNAL BLOCKER: no GitHub App with Administration:write on tailrocks → live canary NO-GO until operator installs velnor-d1-canary App (/tmp/canary-perms.md).
Security audit: 8 findings (1 High F2 attestation gap + 6 fixable, F4 rejected per spec §4.3); secfix-1 implementing.
Next: #916 merge → product → pin bump → #912 green → merge to main → A3 streak → B2/B3/B4 → C/D/E/F/G.
Standing rules: git add -A BEFORE regen; dry-run=0 + check=0 in clean clone; repairs = new commits, never amend pushed.


## Checkpoint 2 (2026-09-17 ~03:00 UTC)
Branch @ 3bb1e23c (11 batches). secfix-1 CERTIFIED → integration-12 merging.
STRUCTURAL: no render-changing generator PR landable-green (unit vs Policy contradiction);
bypass FORBIDDEN. Resolution: render-neutral binary-side candidate trigger (trigger-fix author
on fix/policy-candidate-trigger off main) then 8-step unblock chain (ledger). #916 phase-1
(1bee4f23) waits for main pin move → rebased → normal candidate → green. integration-6 cancelled.
Trigger-fix MUST prove render-neutral (empty render diff) or report infeasible.


## Checkpoint 3 (2026-09-17 ~08:15 UTC)
Main chain: #918 merged @033ab546 (bootstrap) → #920 pin-bump merged @1ad8349a → #916 merged @a6fa8d4a=M_p (normal) → first revision-product ace4fa94864c59e5 PROVEN → #922 pin-bump merged @3b28a96c. Main pin now M_p. Base product revision-carrying + rendezvous-capable.
Campaign branch @8b7b4ac1 (main dead5ecb merged; gates 970/2371/2489). D1a+b+c+d merged. B1/C2/secfix merged. Awaiting: d2a-rest (rebase conflicts open, working) → verify → merge → d2b + swift-verify-fix; R2 bridge (r2-bridge-2) → review → merge R2m → pin-bump → #912 pin R2m → #912 green → main → A3.
External main drift: #917, #919 merged by owner; campaign absorbs via merges. Live ledger: /tmp/campaign-ledger.md.
