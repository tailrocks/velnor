# Handoff

## Original Goal

Complete `velnor-github-first-dual-lane-goal.md` through G7. The goal file is the
authoritative specification; its SHA-256 is
`c9bdb89b302b3fc1ebfb8ff43123590f425f03dbd8855f62bf3a2b42b648b647`.

The required order is G0 estate/snapshot evidence, G1 GitHub-hosted generated
pipeline/bootstrap repair, G2 preview and stable APT/Homebrew publication with
real installation/upgrade proof, G3 migration of the fixed 32-repository fleet,
G4 OrbStack/Docker execution on the current macOS host, G5 runtime/generator
fix-release-install-regenerate pilot, G6 both providers with native checks and
one publisher, then G7 independent final audit. A later gate cannot be claimed
from an earlier partial result.

Hard requirements carried forward:

- Exact fixed 32-repository scope; no substitutions, extras, aliases, or stale
  28-repository `audit_ci` estate manifest as authority.
- Fresh default-branch and open-PR snapshots, all drafts/bots/forks, exact
  heads/bases/merge candidates, required check contexts/apps, workflow/source/
  event/provider/runner identities, child-run graph, and dependency/access/model
  evidence. Unknown is not success.
- Every expected workload and required check must be independently evidenced as
  terminal success. Queued, canceled, timed-out, skipped, failed, missing,
  empty, stale, or manual-only evidence is failure.
- No `lane_compare`, `audit_ci`, display-green, newest-run, dispatch-wrapper,
  candidate self-report, or opaque URL shortcut can establish a gate.
- GitHub-hosted macOS must use the newest official major/architecture label;
  `xcode-27` is the current arm64 preview label and Intel macOS 27 is an explicit
  blocker. Never invent `macos-27`, use `macos-latest`, use macOS 15, or silently
  fall back to an older major/architecture.
- No legacy paths, compatibility aliases, or deprecation shims. All PR merges
  require complete review-thread reading, actionable-feedback resolution,
  reread, required verification, final diff/SHA review, and resulting-main
  verification.

## Current State

Handoff timestamp: `2026-09-20T15:44:35Z`.

- Handoff branch: `codex/current-work-handoff-20260920`.
- Branch base/local source: `c48f2518799fd20cdcecbf6a4bf59d206f16beae`.
- `origin/main` at inspection: `386a5b63c515b1a92e61f579189c2cbed2d6ce09`.
- The local branch is one commit ahead of and 23 commits behind `origin/main`.
  Do not force-push it to `main`, reset it, or merge remote work without fresh
  review. The source branch used for this handoff is intentionally the current
  local transport-test checkpoint.
- `origin/codex/bootstrap-transport-fixtures` points to the same local commit
  `c48f2518799fd20cdcecbf6a4bf59d206f16beae`.
- Before this handoff, tracked source was clean. The handoff adds this file,
  the authoritative goal file, the six byte-exact G0 fixture subset plus its
  README, and a small test-helper correction. Large generated products remain
  local and are excluded; they are not source or evidence.

The current implementation is not complete. No G0, G1, G2, G3, G4, G5, G6,
or G7 gate is passed. The highest-priority continuation is G1 source/bootstrap
integration and its independent provenance/isolation proof; package/fleet/host
execution must remain downstream of a real hosted G1 pass.

## Completed Work

### Authoritative goal and source checkpoint

- `velnor-github-first-dual-lane-goal.md` is now preserved in the repository
  with the exact hash above. It is the source of truth for scope, sequencing,
  records, acceptance checks, and completion criteria.
- `c48f2518` added
  `crates/velnor-workflow/tests/bootstrap_transport.rs` (1,026 lines). The
  fixture exercises generated producer/acquire/execute transport shells,
  service/raw digest and source/tree identity mutations, run/job/artifact
  ambiguity, hostile output, fixed UID/environment expectations, and uploader
  contract checks. This is a test checkpoint, not a proven implementation:
  the generated source on this branch does not yet expose all expected named
  candidate steps, so the focused test currently fails before the intended
  assertions.
- The handoff branch contains a helper hardening in that test: reusable
  `uses` jobs are skipped when searching inline steps, and missing named steps
  panic with a diagnostic instead of aborting the process. This prevents a
  misleading SIGABRT while the generator/source contract is incomplete.

### G0 fixture preservation

The following six files are byte-identical to the historical real-API
supplement from commit `5ed47768` and are preserved as offline source fixtures:

- `crates/velnor-tools/src/testdata/g0/real-api-fixture-supplement-20260920-085311/apps/dco-2.body.base64`
  (`c5deeff0aa7b0613c58398d7639bc2ac608f17a7a8201a454aa3a95a61c94939`)
- `.../apps/github-actions.body.base64`
  (`129b5f7309e4047f10bd574340f9ce1fcf968fe1108a19e81e621ed01ab6e0fd`)
- `.../apps/sonarqubecloud.body.base64`
  (`ebc216d06010de9cf8bcbcdf1c4ed3aa74e1fd69d955bc59880ffef09d155412`)
- `.../suites/homebrew-96059348318.body.base64`
  (`b61a17b0c2637ea0758b7b4502b375b0fddea14ccea6cfc39a68204dd8289c93`)
- `.../suites/velnor-96108219979.body.base64`
  (`3bf35452dc08648088574a0d320ca02b18a3d8096916fbaf580b2e05e299195d`)
- `.../suites/velnor-96108227551.body.base64`
  (`7440700eba83306e76af81e05edd47bf738beaf3f698c5f700b269601346e233`).

The added README records source manifest
`fea65b61ff1668ff58762066b207ac1c9534452d328080f1f0680e7cb571447e`, the
read-only/non-gate status, endpoint/page/source/suite/App/digest binding, and
the intentional exclusion of queued/list-only suites. These fixtures do not
establish live checks or a gate.

### Independent work completed elsewhere

These are useful, committed, independently reviewed checkpoints. They are not
silently merged into this handoff branch and none is a G0-G7 approval:

- Generic hosted contract: branch
  `codex/g1-generator-contract`, commit
  `6b0cfe376f9b6c27b7a0d34f3acc9d339f637f3d`; hostile/ruleset fixtures passed,
  but full library has one checked-in generated drift (`ci-runtime-products`)
  and generated adoption was not performed. Review
  `G1/reviews/generic-contract-6b0cfe37-hostile-test-20260920.md`, SHA
  `e218331cd2807cb1a16443445619944b093c38b0fb5fabde7ebf55fb9377531c`.
- Bootstrap alias/guard work: branch
  `codex/g1-bootstrap-isolation`, commit
  `59544788142f6da29d2a48b77895ee1928d0884f`; ProviderProofPending exits 78
  before API, and the wrong-job artifact replacement remains intentionally red.
  Review SHA `c84d6426a220912179caf43402d62c848a79b676055fb7724ff2e4c7d50d8b7e`.
- Bootstrap transport hostile fixtures: branch
  `codex/bootstrap-transport-guard-fixtures`, commit
  `72ab32376499c5409b439834d3f27f639bf7ffe7`; six guard tests, fmt, and
  clippy passed; no hosted/API approval.
- Checker: branch `codex/github-first-checker`, commit
  `c51088b325226f5bb601f49bcd017449abe0cac3`; `cargo test -p velnor-tools`
  passed 265 and clippy/check passed, but independent review
  `aa223b07fe8e0de4ce4c9d8c2f7900dca07699843a6cf2517373443517038b8c`
  requires changes: producer query/page/head-sha wire mismatches and ruleset
  clone provenance bypass. Collector integration remains absent.
- Collector: branch `codex/g0-collector-dedup-progress`, commit
  `c342c6012a46abf5d897d91e509f8d1d3258b919`; independent review
  `2ef788472a77da2a9d2fbc5dd2ccd0bf1e94d2f91b5fc28dd7fa223852cb47e3`
  rejects query/page/link/path binding, failed-source ledger, metadata
  durability, and package compilation. Historical PID 41318953 remains
  nonterminal/partial; do not claim collector success or restart it casually.
- Hosted routing/action source checkpoint: branch
  `codex/github-first-source-integration`, commit
  `649c48c0da8666ee3437a96c45399dd4f8bf32d8`; routing review SHA
  `c2f9389660b175ea917568f41e8d191cc3458a94246bc64d135cad0abeaf34e2`.
  Typed labels pass, but generated drift (1,836 pass + 1 failure/16 paths) and
  workspace runner mismatch block adoption.
- Immutable action pin update: branch `codex/action-pin-policy-update`, commit
  `e7d620cd62f2e05e41f56472ce91c987660b10e8`; five official action SHAs were
  refreshed and narrow checks pass, but full workflow tests expose generated
  Apple drift and migration fixtures still select macOS 15. Not adopted.
- APT schema fix: branch `dual-lane-apt-schema2`, commit
  `ce351c39a202054e8074953fb2b65a5a1b6b22a6`; ten canonical names are
  reserved before collision detection and focused tests pass. Live old `fdeed`
  schema-1 remains disconnected; no publication.
- Homebrew: branch `codex/github-first-homebrew`, commit
  `cf149419ca6df9d24de3b3140f8dd70c1242792e`; isolation and online audit
  pass, but no install/publication or native product handoff.
- Native product v3: branch/ref `983011728504efdeb2895f52681771d4f364f60b`;
  independent review SHA `03641cabb010b7e75c01e710e6a3a34b84f2a831ed0b67621d5ef8ebf105abca`
  rejects checked-in workflow/product wiring. Its worktree remains dirty with
  `release.rs` modified and `native_product_stable_steps.yml` untracked.
- Prefetch prototype: commit `28cf53e18ac1b8cb8bb1fb6a25c9372683305db1`,
  30 harness cases pass/reject as expected, but authority, delivery, redirect,
  network, runtime/image, and TOCTOU gates remain unproven.
- Records/evidence: docs branch `codex/github-first-records` at
  `a34ba9814da77120c7727c1e10cf3d978b326c43` has chronology/citation review
  pass only; evidence ref `evidence/github-first-dual-lane-20260919T181508Z`
  is packaging-only at `6676e0a303f7398ad4aabdcc779000c0cfe9be0b` with no
  attestation/gate. Current PR census has manifest SHA
  `ca9a0cbc4f1537a759b21ccdaeb28fea30befd31facde852825b735d03652a6d` and
  review SHA `b15422e03b5c0e9e6f16f5ec4c5c713123246ac1c8bdde98649061f67624df3c`;
  it found 9 PRs and 8 active unresolved review threads. Those threads are
  merge blockers under the required review gate.
- Authority transition has no executable approval. Review
  `G1/bootstrap-transition/AUTHORITY-PLAN-RECONCILIATION-9E5-4FA-2026-09-20.md`
  (SHA `18d75a1b5a0e29b0d46da1810966fa07e30a4632c0636775691327af89370bb8`)
  records unbound contexts, strict=false/actor-5 bypass, candidate fallback,
  and no App/ruleset/dispatch/merge mutation.

## Partially Completed Work

### G0

The exact scope/branch/PR inventory, typed checker, collector, dependency
graph, and real API fixtures exist in separate evidence/worktrees. They are not
one authoritative fresh envelope. The checker and collector have incompatible
query/page/link contracts and known compile/provenance defects. The 32-repo
snapshot and review census are capture-bound; they do not include complete
workflow/check/run/install evidence. G0 remains incomplete.

### G1

Generated producer/acquire/execute/verify architecture is being built, but the
current local test proves the source checkpoint is not yet wired: expected
candidate step names are absent from the generated workflow for the minimal
fixture. The intended contract still requires separate fresh hosted jobs,
base-owned acquisition/verification, exact workflow/run/attempt/job/repository
IDs, service and raw digests, exact source/tree proof, trusted artifact binding,
fixed `65532:65532`, `env -i` allowlist, no token/command-file/socket access,
network-none container, quota/timeout, safe archive extraction, and real H1-H9
hosted canary results. None is proven here.

The bootstrap design decisions are strict: candidate code is never semantic
authority; no newest/first artifact; no workflow dispatch shortcut; no old
checker/candidate fallback; producer path is the direct `candidate_producer` in
`ci-pr.yml`; candidate artifact namespace has exactly one uploader, while the
distinct plan/runtime artifact namespace has its own contract. The existing
main branch still has generated/pin drift and the old validator path.

### G2–G7

APT/Homebrew source contracts and native product work are narrow, non-published
checkpoints. No real preview/stable feed/tap install/upgrade proof exists. Fleet
migration, OrbStack execution, runtime fix/release/regeneration, dual-provider
parity, singular publisher, and independent final audit remain unexecuted.

## Remaining Work

1. **Rebase/reconcile before source integration.** Fetch current remote refs and
   read this handoff plus the canonical goal. Keep this branch as a record; do
   not merge 23 remote-main commits blindly. Compare c48 changes with
   `origin/main` 386a5b63 and the exact PR/review revisions. For every candidate
   PR, read all REST/GraphQL review comments, inline threads, bots, requested
   changes, and latest head/base SHAs before any merge.

2. **Finish G0 as one fail-closed authoritative record.** Repair the c510/c342
   query/page/link/path contract; compile the package; bind exact fixed-32 names,
   current default and PR heads, rulesets/check Apps, workflows/reusable actions,
   expected workloads/platform/provider eligibility, dependency graph, access
   gaps, and effective model/session settings. Preserve inaccessible/unknown
   rows. Recollect fresh final default/PR heads after the ledger is complete.
   Acceptance is a nonempty expected workload set, complete pagination/raw
   provenance, exact API object IDs/attempts/conclusions, and checker rejection
   of all hostile mutations. No self-attested record can pass.

3. **Finish G1 source/bootstrap.** Start from current remote source and merge
   only reviewed, corrected slices. Repair the generated producer/consumer
   rendezvous (static candidate artifact and complete manifest versus old
   dynamic closure contract), regenerate every affected workflow/action/state
   file, and make fixed-point tests pass. Replace local source-only transport
   expectations with actual generated steps. Prove base-pinned hostile fixture
   bytes/hashes, producer token/command-file/direct-upload rejection, final
   image manifest/config digest, fixed UID/env/mount/PID/network/quota/timeout,
   archive hardlink/symlink/traversal rejection, exact API source/run/job/artifact
   binding, terminal success, expiry/duplicate rejection, and independent
   verify-side re-read. Run actionlint, fmt, clippy, focused hostile binaries,
   and a real hosted canary. Acceptance is the full G1 gate, not string tests.

4. **Only after G1, publish G2.** Integrate corrected typed APT/Homebrew
   schemas, then publish preview and stable packages/tags/assets/feeds/taps.
   Verify immutable asset/signing/channel/version identities with clean real
   install, upgrade, channel-switch, uninstall, and sibling-binary checks on
   supported platforms. A release required by the manifest cannot downgrade
   install applicability to N/A.

5. **After G2, migrate G3.** Generate both providers for every exact-scope
   repository. Reconcile current open PRs and resulting-main runs; require
   nonempty platform/architecture/provider workloads, native checks, required
   contexts/apps, child graphs, and actual terminal successes. Never treat
   unsupported/unknown as executed success.

6. **After the hosted fleet passes, execute G4/G5.** On the current macOS host,
   use OrbStack with every Velnor payload inside Docker. Prove macOS/OrbStack/
   Docker identity, image/source digests, resource/cancel/recovery/cache/
   observability behavior, then fix runtime/generator defects, publish/install
   corrected packages, regenerate affected consumers, and prove the pilot.

7. **Finish G6/G7.** Prove both provider lanes on the same PR candidate and
   resulting main, logical workload/platform/locked-dependency equivalence,
   native-only checks, and one authoritative publisher. Then an independent
   reviewer must audit current branch/PR revisions, all review threads, source,
   generated state, packages, runtime, and immutable evidence digest. Owner and
   reviewer identities alone are insufficient.

## Important Decisions and Reasoning

- **Do not overwrite `origin/main`.** Local c48 is a transport-test checkpoint
  based on abe9, while origin/main is 386a with 23 additional commits. A
  dedicated handoff branch preserves both histories and makes the divergence
  explicit.
- **Do not commit generated binaries/VM images.** The root contains gigabytes
  of candidate/runtime/guest artifacts produced during experiments. They are
  machine outputs, not source or durable evidence; `.git/info/exclude` now
  ignores them locally without deleting them.
- **The test-helper correction is intentionally narrow.** The test was aborting
  on reusable `uses` jobs while looking for inline `steps`; skipping those jobs
  exposes the real missing generated candidate step instead of masking it with
  SIGABRT. This does not make the producer implementation pass.
- **Static source tests are not security proof.** Existing reports repeatedly
  reject string-only assertions, self-authored manifests, aggregate green
  checks, same-name artifact matching, local-only Git proof, and missing child
  provenance. Keep hostile binaries and independent API evidence mandatory.
- **Latest-platform policy is strict.** The official runner matrix evidence says
  Ubuntu 26 x64/arm and `xcode-27` arm64 preview are current; Intel 27 is not
  offered. An Intel requirement blocks; it does not authorize macOS 26/15,
  `macos-latest`, invented labels, or architecture substitution.
- **Target-specific product data stays outside generic S2.** Generic renderer
  code may carry neutral typed schema; Velnor product/workflow/path/pin details
  belong in target `.github-gen` sources/static mappings. Do not add forbidden
  `[policy.validator]` fields to the old deny-unknown config or hardcode fleet
  names in the generic engine.
- **No authority transition is inferred.** Ruleset/context names, App identity,
  workflow dispatch, merge/ref CAS, lease/watchdog, and publisher trust need an
  executable reviewed protocol. A successful wrapper or manual dispatch cannot
  replace it.
- **Current model config is factual, not historical.** This session reads
  `~/.codex-chainargos/config.toml` as `model=gpt-5.6-luna`, root effort `max`,
  subagent model `gpt-5.6-luna`, subagent effort `max`, and max 256 threads.
  Earlier records claimed Astra/low orchestration; recheck before any new wave.
  Recent attempted extra agents hit the platform usage limit, so no new agent
  result is assumed beyond the completed reports listed here.

## Known Issues / Risks / Blockers

- Focused local test currently fails: `rtk cargo test --locked -p
  velnor-workflow --test bootstrap_transport` reaches the generator, then the
  four tests cannot find `Build candidate generator` in generated `ci-pr.yml`.
  Before the helper fix this appeared as four `key not found or not a mapping`
  panics; after the fix the failure is an explicit missing-step panic. This is
  expected source/test contract drift, not a passing security test.
- Full library/fixed-point generation is not green on the known worktrees:
  independent reports record generated `ci-runtime-products.yml` or other
  generated Apple/macOS drift. Re-run against the current reconciled branch;
  do not copy old pass counts as current proof.
- c510 checker has exact producer wire/page/head-SHA mismatches and a ruleset
  same-repository clone acceptance path; c342 collector has query/page/link/path
  binding, failed-source ledger, durability, and compile defects.
- Existing main CI/Preview evidence records generated-tree/pin drift and an
  unavailable exact-head candidate artifact; green Runtime/Nightly/Maintenance
  jobs do not clear required Policy/Preview failures.
- Native product review rejects checked-in workflow/product wiring. APT and
  Homebrew are source-only; no published packages or install evidence exist.
- The PR census contains active unresolved review threads. Do not merge any PR
  until every thread is read, analyzed against code, fixed or explicitly
  rejected with reasoning, reread after changes, and reverified on the final
  SHA/resulting main.
- `origin/main` contains newer work than this branch. Any report referring to
  old bases, old PR heads, or old timestamps is historical only until refreshed.
- `.git/info/exclude` is local-only and uncommitted by design. The generated
  directories it hides are intentionally retained on disk; verify they are not
  accidentally staged with broad `git add`.
- Root `AGENTS.md` on this branch lacks the latest-version wording now present
  in the separate `codex/latest-macos-policy` branch at `92387e88`. That branch
  is not merged here; apply/review its short latest-version rule as a separate
  change after reconciling against current main.

## Validation Performed

### Passing

- `rtk --version`: `rtk 0.49.0`.
- Goal file line/byte/hash check: 344 lines, 55,652 bytes,
  SHA-256 `c9bdb89b302b3fc1ebfb8ff43123590f425f03dbd8855f62bf3a2b42b648b647`.
- `git fetch` completed; refs and divergence above were verified locally.
- Fixture body hashes match the historical six files from `5ed47768` exactly;
  README records the source manifest and non-gate boundary.
- `.git/info/exclude` patterns match the local guest/runtime/candidate output
  directories; no generated binary or VM image is staged.

### Failing / incomplete

- `rtk cargo test --locked -p velnor-workflow --test bootstrap_transport`:
  **failed**, 4 tests failed at the missing generated `Build candidate
  generator` step. The failure is captured above.
- Before the helper correction, the same test command failed with four
  `key not found or not a mapping` panics while indexing reusable `uses` jobs;
  that test-harness defect is fixed in this handoff branch.
- `rtk cargo fmt --all -- --check`: passed after formatting the helper edit.
- `rtk git diff --check`: run again after the handoff commit is required; it
  was clean for the pre-handoff source/test diff.
- Full `velnor-workflow` library and full fleet/gate checks were not run to
  completion for this handoff because the focused source/test contract already
  fails and the branch is behind current main. No passing result is implied.

## How to Continue

1. Check out this branch and read `velnor-github-first-dual-lane-goal.md` and
   `HANDOFF.md` before touching source:

   ```sh
   git fetch origin
   git switch codex/current-work-handoff-20260920
   git status --short --branch
   ```

2. Keep this branch as an immutable handoff record. Create a fresh working
   branch from current `origin/main`; inspect the 23-commit divergence and all
   PR review threads before cherry-picking any slice.

3. Reproduce the focused failure, then inspect generator config/source and
   generated `ci-pr.yml`/`ci-policy.yml`. Make the producer/acquire/execute
   contract coherent before adding more fixtures. Run:

   ```sh
   rtk cargo test --locked -p velnor-workflow --test bootstrap_transport -- --nocapture
   rtk cargo fmt --all -- --check
   rtk cargo clippy --locked -p velnor-workflow --all-targets -- -D warnings
   rtk git diff --check
   ```

4. Before every PR merge, read all reviews/threads/bots, address valid issues,
   reread, rerun required CI, inspect final diff/SHA, then verify resulting-main
   checks. Never use the evidence ref or historical reports as source
   attestation.

5. Resume G0/G1 only after the source branch is reconciled. Keep final evidence
   outside the source revision it attests. Record every unknown as blocked or
   pending, never as success.

## Definition of Done for the Original Goal

The original goal is done only when the exact goal document’s deterministic
G0–G7 gate passes on current source and PR revisions and an independent final
reviewer attests the external evidence digest. Specifically: the exact fixed 32
scope is live and complete; generated GitHub-hosted PR/main/bootstrap paths are
correct and independently proven; preview/stable APT and Homebrew packages are
published and really installed/upgraded; all 32 repositories pass both required
hosted lanes with native-platform checks; the actual macOS OrbStack pilot runs
all Velnor payloads in Docker; fixes are released/installed/regenerated and the
pilot is re-proven; both providers are generated/merged with equivalent
workloads and one publisher; every required job/check/child run is terminal
success with immutable source/event/provider/runner/package identity; all review
feedback is resolved; and G7 independently audits current branch/PR revisions.
No stale, self-attested, manual-only, unsupported, skipped, queued, canceled,
failed, or missing evidence may remain. Until then, status is incomplete.
