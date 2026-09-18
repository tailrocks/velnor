# MAIN-DRIFT review: origin/main 33688938 -> 08ea1b07 (#917) + dead5ecb
Date: 2026-09-17 ~06:20 UTC. Reviewer: main-drift (read-only). Campaign branch @ 0477e14f.

## 0. Correction: origin/main is now dead5ecb, not 08ea1b07
- 08ea1b07 = Merge PR #917 (feat/pr994-generic-ci-capabilities), merged 05:57 UTC by donbeave.
- dead5ecb = "chore(ci): bump D19 pin to 08ea1b07 after PR994-capabilities merge" (06:00 UTC).
  Pin literals + tracking hashes only; closure UNCHANGED (81a6d4d42427c1f8, product published).
- Local `main` is stale @ 33688938. All conflict tests below target dead5ecb (real merge target).

## 1. #917 summary
Title: "feat(ci): generic CI/CD capabilities replacing static workflow overrides".
Recovering reusable CI/CD behavior from jackin#994 so Jackin migrates off static workflow
overrides. Slices A-H: platform prerequisites, prepared-tool handoff, closure/reuse,
docker multi-arch publish, release/preview/bindings/modes/archives/teardown, docs-site
pipeline, check profiles, renovate/policy remainder.
- 78 files, +27160/-914. Core: config/mod.rs (+2862), lib.rs (+1056), platform.rs (new),
  check_profiles.rs (new), docs_site.rs (new), prepared_tools.rs (new), release.rs (+4868),
  reuse.rs (new), runtime.rs (+2775), scan rust/swift, 13 regenerated workflow/state files,
  8 plans/*.md memos, velnorctl host.rs (2 lines: secret rename), Dockerfile (secret+ARG rename).
- Pin chain INSIDE #917 (8 commits): feat dc150d2f -> self-bump 16160cba -> docs c06965e5 ->
  fix 9e2c85c2 -> self-bump 07a64fa5 -> fix 0e6645af -> self-bump 34d38212 -> docs 3f58ffb5.
  Every generator change immediately followed by a pin-bump-to-own-tip commit.
- .github-gen revision: 7341ef4b (old main) -> 0e6645af (at 08ea1b07) -> 08ea1b07 (at dead5ecb).
- CI at head 3f58ffb5: Policy SUCCESS, but 7 FAILURE (4 Velnor lanes, Prepare Cargo,
  ci-required, Control/Required) — merged red by owner override. Failures are the SAME
  pre-existing environmental family seen on #916/#918; no new failure class. No velnor-runner,
  velnor-control, velnor-model, schemas, or bastion-plan files touched.

## 2a. Branch conflicts (merge-tree vs dead5ecb + live state)
LIVE NOTE: refs moved during this review — integration-14 merged main into #918 @ 06:10 UTC
(see below). Conflict data for #918 describes what the shepherd resolved, verified via 933239b2.

- #918 fix/policy-candidate-rendezvous: RESOLVED. Was e6839cfa (CONFLICTING), now 933239b2
  "Merge origin/main (dead5ecb)", MERGEABLE/BLOCKED, Policy FAILURE (expected: pin==base now,
  tree!=pin-render — the authorized red-merge posture). Merge message: only state-digest
  conflict, regen changed 2 digest lines, zero YAML drift, gates green. VERIFIED: diff
  dead5ecb..933239b2 == exact original fix (196+/42-, same 5 files); ir.rs one-word fix
  preserved verbatim (now line 1749); lib.rs fix region untouched by #917 (#917 has 0 mentions
  of policy_candidate_step). Ledger's "state digests only" CONFIRMED — my early "5 conflicts"
  reading was a merge-tree misread (only generator-state carried real markers).
- #916 feat/a2-producer-revision @ 1bee4f23 (unchanged, OPEN): ONE mechanical conflict
  (generator-state digests). runtime_products.rs AUTO-MERGES (#916 hunks at 16-501/997-1653,
  #917 test-only insertions at 645/677 — disjoint). Pin still 7341ef4b (stale; self-bump was
  reverted in 1bee4f23 per phase-1 plan). No semantic collision.
- Campaign docs/bastion-final-plan @ 0477e14f (128 files, unchanged): 7 files with REAL
  conflict markers (29 total): release.rs 12, lib.rs 5, generator-state 5, runtime.rs 3,
  config/mod.rs 2, release.yml 1, ci-unit-rust.yml 1. Cleanly auto-merging despite both-side
  edits: policy.rs (#917 writer_gate vs campaign policy work — disjoint), ir.rs, project.toml,
  ci-main/pr.yml, preview.yml, runtime_products.rs, velnorctl host.rs (secret rename disjoint
  from campaign host work). Real merge work concentrates in 4 source files where campaign A2/B1
  generator work meets #917's rewrites: release.rs (campaign apt_* schema keys vs slice-E
  rewrite), lib.rs, runtime.rs, config/mod.rs. Campaign NEW files (closure.rs, promote.rs,
  consumer_negatives.rs, apt.rs, policy/tests.rs, promote_atomic.rs, velnor_first_ci.rs) have
  no textual conflicts but MUST recompile against #917's changed lib.rs/config/runtime —
  no new Unit/ProjectConfig literals found in campaign diff (good), but callers of changed
  functions need a merged-tree build to prove.

## 2b. Producer/consumer/policy mechanics: UNCHANGED by #917
- #917's "producer/consumer/candidate/closure" vocabulary is a DIFFERENT domain (maintenance
  producers, docs consumer-owned build, EffectiveMatch path candidates, dependency-closure
  selection in new reuse.rs). reuse.rs (affected-selection/artifact-identity/result-reuse)
  does not touch the revision-product/candidate-publish/Acquire/validator rendezvous.
- policy.rs: purely ADDITIVE (writer_gate for maintenance writer + always()&& variant). No
  gate removed or redefined; #918's rendezvous semantics unaffected.
- Surfaces #918 relies on survive at dead5ecb: policy_candidate_step (present),
  `closure` runtime verb (present), --plain --check (present).
- Additive risk only: Unit gains platform/products/prerequisites/env/mbx/prepared_tools;
  ProjectConfig gains docs*/check_profiles/maintenance. Any exhaustive literal/destructure
  breaks on merge — campaign adds none, but the merged tree must be gate-proven.

## 2c. Ledger baselines
INTACT: revision-product closure 81a6d4d42427c1f8 (dead5ecb msg) + all 14 v1-* products;
A2 producer/consumer/rendezvous model; #918 content certification (fix byte-identical atop
new main); E1 unit inventory SHAPE (project.toml: only +1 watch path, no unit add/remove);
impossibility proofs + user authorization for #918 red-merge.
STALE / REBASE: velnor main 33688938 -> dead5ecb; main pin 7341ef4b -> 08ea1b07 (campaign
branch + #916 still pin 7341ef4b); A1-gates "541 passed" (-p velnor-workflow; #917 adds
~dozens of tests; workspace now ~4101 per #917 body); A1 timing baselines (render changed);
ledger line 3 (#912 head 38ffbfd7 — actually 0477e14f) and Live-SHA block need re-resolve;
any recorded render bytes/hashes from 33688938-era regens.

## 2d. #917's green Policy vs our models: CONFIRMS (precisely)
- Mechanism: at head 3f58ffb5, pin=0e6645af (own-tip generator) != base=33688938 with a
  different closure -> pinned early-exit impossible -> green came via CANDIDATE path.
  Pin..head = one docs-only commit -> closure-clean -> publisher(HEAD)==Acquire(pin)==
  validator(HEAD) jointly satisfiable WITHOUT the rendezvous fix. This is EXACTLY the
  impossibility result's stated escape hatch ("satisfiable only if pin..head closure-clean").
- Slogan correction: "NO render-changing generator PR can land green" is imprecise as an
  absolute — #917 is a +27k render-changing generator PR that went Policy-green via
  self-tracking pin discipline (3 self-bump-to-tip commits). The precise theorem stands;
  #917 is its first large-scale confirmation, not a counterexample.
- Caveat: #917 was NOT fully green (7 env failures, owner override). Policy-green +
  required-red is the same posture the campaign already treats as the transient norm.

## 3. Impact verdict: MODERATE — no model breakage, real merge work queued
(a) #918: no action (shepherd done, fix intact, posture correct). #916: trivial (1-file
    mechanical). Campaign branch: 4-file real merge (release.rs worst) + merged-tree gates.
(b) No mechanics change; additive-only policy/struct evolution.
(c) Pins + test-count + render baselines stale; closures/products/models intact.
(d) Confirms models; refines the slogan; proves self-bump-to-tip as a working discipline.

## 4. Required actions (orchestrator-owned)
1. Re-resolve ledger Live SHAs (main dead5ecb, pin 08ea1b07, #912 head 0477e14f) — A0-refs
   style mini-pass; update stale ledger lines (3, 8-9, DRIFT note: #918 now 933239b2).
2. Campaign branch: merge dead5ecb (4 source files: release.rs, lib.rs, runtime.rs,
   config/mod.rs + regen), then FULL gates in clean scratch (build + 4101-scale tests +
   dry-run/check) before any further integration batch. Do NOT let batches pile on 33688938.
3. #916: after #918-merge + pin-bump (per ledger chain), rebase (expect 1 mechanical
   conflict), re-verify producer-only scope against new runtime_products.rs, reland checks.
4. Rebaseline A1-gates counts (541 -> new -p velnor-workflow total) and A1-timing after the
   campaign merge; re-confirm E1 17/17 units render under new capabilities.
5. Adopt #917's self-bump-to-tip pattern as the documented discipline for ALL future
   campaign generator PRs (each generator commit immediately followed by pin-bump commit);
   keep authorized #918 red-merge as the class fix that makes it unnecessary thereafter.
6. Correct the campaign slogan in working docs: "render-changing generator PR lands green
   only if pin..head closure-clean (self-tracking pin) or rendezvous fixed" — cite #917.
7. Local hygiene: advance local `main` to dead5ecb (fast-forward, no surgery) so merge-bases
   stop resolving against 33688938.
