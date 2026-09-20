# GitHub-first dual-lane operator runbook

This runbook separates commands verified during G0 setup from commands that are
required later but remain pending. A pending command is a procedure, not proof
that its operation succeeded.

## Evidence and safety boundaries

- Source checkout: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-records`.
- Velnor source checkout used for the initial snapshot:
  `/Users/donbeave/Projects/tailrocks/velnor-project/velnor3`.
- Live evidence root (outside source):
  `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/`.
- Never commit live run logs, credentials, mutable success ledgers, or package
  dumps. Link compact records by immutable run URL, SHA, and digest.
- Before any destructive/release action, resolve exact repository, revision,
  channel, and destination. A stale or moving source invalidates evidence.

## External snapshot timeline and coverage

Paths in this timeline are logical paths relative to the external evidence root;
they are not source-tree files. `Observed` means the artifact exists and its
recorded shape/timestamps were read. It does not mean that the checker accepted
the artifact or that a gate passed.

- **[Observed] Baseline:** `G0/fleet/inventory.md` at
  `2026-09-19T16:34:19Z` recorded 32 `main` rows and 78 open-PR identities
  (75 ready, 3 drafts) across 12 PR-bearing repositories.
- **[Observed] Reconciliation set:**
  `G0/fleet/{requirements.json,main-verification.json,pr-checks.tsv,context-pages.json,handoff.json,main-revisions.tsv}`
  spans approximately `2026-09-19T17:03:32Z`–`17:05:57Z`; it reconciles
  78 baseline PR identities to 77 later heads, records tap PR #494 merged and
  Velnor PR #953 head-changed, and rechecks head stability at `17:02:04Z`.
  Its PR/check/workflow coverage is 12 repositories, not all 32.
- **[Observed] Later collector:** `G0/fleet/check-contexts-full.json` spans
  `2026-09-19T19:15:10.439Z`–`2026-09-19T19:28:02.940Z` and was serialized at
  `2026-09-19T19:29:58.430Z`; it contains 32 repositories, 76 open-PR rows,
  1,268 main and 1,742 PR observed check runs. This is timestamped evidence,
  not a current-forever claim. Its required-context/App, source-binding,
  workload, dependency, run, and child evidence still require review/enforcement.
- **[Observed, incomplete] Graph/access artifact:**
  `G0/fleet/dependencies-and-access.json` was observed at
  `2026-09-19T19:21:38Z` (SHA-256
  `f58da9d4ea2bc32ba8867cbb4897997bc48c6e4228a14ed4ec0710c056f67f60`). It
  covers the exact 32-row scope, 22 workflow-bearing rows, and 15 source-bound
  edges, while explicitly reporting partial/gapped graph coverage and mixed
  source checkout status. It is inventory evidence, not full graph validation.
- **[Pending] Authoritative use:** bind manifest, snapshot, and records
  artifacts by schema, digest, source revision, observation UTC, collector,
  pagination, and coverage before invoking the checker. A manual or available
  artifact is not proof until the checker consumes and validates those bindings.

## Latest checkpoint: verified boundary versus pending operation

The following are external, revision-bound observations captured on
2026-09-20. They were not run by this records worktree and do not authorize a
gate, mutation, merge, publication, or host operation.

| External observation | Result | Operational meaning |
| --- | --- | --- |
| Authority v3/v4 records | v3 remains frozen/unapproved; v4 is approval-required with no mutation (v4 Markdown SHA-256 `48b1e19ac7b9be59d78200f9e5de7b09e9a1de68fba9b41e3192b952ec022e52`) | Do not execute the authority transition. Six readiness classes remain: placeholders, real old-parser fixture, B closure/two renders, freeze/lease/watchdog/ruleset proof, complete Main-B census, and independent/owner approval with cleanup |
| Old-parser `static_files` feasibility | External MD/JSON hashes `8764712693e2883d05846de05a3c2137fb3d31e3ee97ab0ad129186f3ff960f4` / `01ad40d473aa26ae7e5e814ef7c3f93e854d682d2c359fa7588f0bba2b4943e9` show byte transport only | No actual B source, admission, provenance, or authority was proved |
| Checker public harness | Exact source `27eb094ccd545b642206ea3d52336e0ac74d6abd`; `27eb-harness-results.json` SHA-256 `a4e7d7f29289b16438602872867295a84cc39c5ad398b974bb0ef6bd192bead1` | Live self-authored use exits because authenticated collector/current API is unwired; offline fixtures are explicitly validation-only and fail closed |
| Bootstrap owner checkpoint | Exact `b981f43e8dfd70b4c628d29b0e7e9dce679ce537`; owner reports regenerated workflows/state and 1,727/1,727 library plus actionlint/fmt | Exact `G1/reviews/bootstrap-b981f43e-independent.md` rejects the G1 source checkpoint for archive/API-tree, freshness, provenance, legacy, fixture, and image-digest gaps; source full tests are not G1 evidence |
| APT/native/consumer source reviews | APT `91bdf6cc1d0a5c429c5c01f17bf15dbb153c661b` is blocked by `G1/reviews/apt-83e7ab4-91bdf6c-independent.md`; corrected workload index is exact-disjoint 32 but `not_evaluated`; native `a8d46536e7e11db0bbd5e207be802970362b751f`, action scanner `40ddcc02dde1ff07aff538ea2ca95da091379e17`, and fixture `d60c0e2211b2830c64e7489d05b4cfe0bc77d65f` remain changes-required | No G2/G3 admission; d60 still requires ZIP-only assertion; source findings do not authorize generated/runtime work |
| Scan candidate | Exact `0a15fd06e002f57dca546d5c041754f1ec433508` reports 1,756 full tests | `G1/scan-integrity/source-review-0a15fd06.md` rejects exact D19 closure plus authority, detector-input, rollback, and hostile-fixture gaps; no G1 approval |

### Pending authority-transition procedure

Do not replace the following pending items with a green test count, a local
binary, an old parser transport result, a newest-run shortcut, or a manual
ledger entry:

1. Resolve all v4 App/operator/watchdog/integration/source/PR/tree/lease and
   ruleset placeholders.
2. Run the real old parser against empty candidate input plus the selected
   `static_files` mappings. Record parser result, old `--plain --check`, old
   policy result, source/output bytes, and the raw current-tree failure.
3. Build the reviewed source-owned B generator, recompute source/runtime
   closure and reachable job graph, and compare two independent renders over
   workflows, actions, state, actionlint, manifests, and sidecars.
4. Prove disposable-writer freeze, signed lease/watchdog recovery, ruleset
   before/after hashes, and merge guards.
5. Capture complete Main-B run/attempt/job/check/artifact/release/native
   identity and child census, including provider and raw-digest bindings.
6. Obtain owner approval and separate `g0_reviewer` plus
   `authority_transition_review` approval, then remove temporary authority and
   prove the final cleanup. Until then, no source/GitHub/release/merge/G1
   operation is authorized.

## Current candidate-bound checkpoint

The following read-only observations were captured around
`2026-09-19T22:47:37Z`; they are external evidence references, not verified
merge instructions or gate results:
A separate 2026-09-20 reconciliation observed live `main` at
`1048337062ea625fada1b4f7c07f2feed75f60c7`, parent `b5a4b4af`; it reports
generator-rendering reproducibility only. The candidate observations below
remain timestamped `b5`-bound evidence, not current-main proof. See external
`G1/bootstrap-transition/VALIDATOR-ONLY-DESIGN-2026-09-20.md`.

- PR957 source `9e06`, revision `53`, is approved only by
  `G0/native-review/review-pr957-92387e88.md` (1,888 source tests plus
  fmt/clippy/check). Merge/live is blocked by unpublished D19, failed Policy
  candidate acquisition in run `35473052923`, and the separate typed
  validator dependency.
- PR960 is open at head
  `2c810f1b46ce8eddb5906fd4bdcc8ae23e78ed40` against base
  `b5a4b4afaa6ca807927cacc03659b570a895dd5c`; Policy was in progress at
  capture. PR962 is open at head
  `94b43578cad9720e569780d18dc966370ed47c11` against the same base, with
  required/Velnor-workflow hosted failures observed. Do not merge either
  without the complete paginated preflight below.
- PR961 is the historical open path at head
  `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303` on base `b5a4b4af`; its history
  contains unsigned `857` and DCO is `action_required`. It is not repaired or
  approved. PR963's `fb78d85d464fd5082e5c161922afd7942380fabc` signed
  replacement is historical to the `b5` snapshot; external comparison records
  the tree-equivalent replacement with `857` excluded. The current query
  reports head `c440d4db3fd59a9e4abd396d7a75e670c4f3d862` on API base
  `1048337062ea625fada1b4f7c07f2feed75f60c7`; no fresh exact-head review or
  approval was observed. The independent review of
  `0c1ec75753cf9f8044a3a2ff01c2d144e9c59132` does not transfer to `c440d4db`.
  Later remote main `e94b48406c4ed206fce2bbf39b788264e72cf39c` is a separate
  fresh census under review, while d20 is historical. Never force-push or
  override required checks.
- Product validation uses the separately typed validator-only
  product/publisher design owned by `/root/g0_inventory` and reviewed by
  `/root/g0_reviewer`; publication precedes separate PR957 pin adoption. Do
  not reuse a three-platform runtime or add platform-selection workarounds.
- Secure CAS/sourcegraph and collector/checker binding are unresolved. The
  `38345852` scan's five full-suite failures lack exact-parent baseline
  attribution; record them as attribution-pending, not baseline failures.

## Regular checkpoint and push procedure

Take a coherent checkpoint at each safe handoff, review disposition, or
substantive change. WIP is allowed, but label it in the commit subject and
external task record. These commands are the normal task-branch procedure;
never force-push and never stage the external evidence tree.

```bash
# [PENDING] read-only checkpoint facts
rtk git status --short --branch
rtk git rev-parse HEAD
rtk git diff --check
rtk git show -s --format='%H%n%B' HEAD

# [PENDING] stage only the owned source files, then sign and push normally
rtk git add docs/ci/github-first-dual-lane/SPEC.md \
  docs/ci/github-first-dual-lane/PLAN.md \
  docs/ci/github-first-dual-lane/RUNBOOK.md
rtk git commit -s -m 'WIP: describe coherent checkpoint' \
  --trailer 'Co-authored-by: Codex <codex@openai.com>'
rtk git push origin BRANCH:BRANCH
rtk git ls-remote origin refs/heads/BRANCH
```

Record the exact branch, local SHA, remote SHA, clean/dirty result, validation
commands, review status, and blockers in the external session graph. The push
is a durable WIP/ready checkpoint only; it does not imply review approval,
merge, publication, or gate success. Keep mutable session state, raw logs,
review snapshots, and final ledgers outside this source tree.

## Mandatory PR merge preflight

This is required before every recovery, migration, release, or generated-output
PR merge. The commands below are procedures, not evidence until run against the
exact candidate SHA and recorded externally.

1. Read every paginated review, issue comment, inline review comment/thread,
   bot comment, requested-change event, and feedback added after the latest
   candidate update. Do not stop at the first page or a review summary.

   ```bash
   # [PENDING] replace OWNER/REPO and PR with the exact candidate
   gh api --paginate repos/OWNER/REPO/pulls/PR/reviews
   gh api --paginate repos/OWNER/REPO/issues/PR/comments
   gh api --paginate repos/OWNER/REPO/pulls/PR/comments
   gh pr view PR --repo OWNER/REPO --json reviews,comments,latestReviews,reviewDecision
   ```

   Bind the snapshot to the candidate before reading dispositions, and retain
   each item’s commit ID (or explicit `null` for a comment without one):

   ```bash
   # [PENDING] PR must be the exact candidate; PR is numeric for GraphQL
   gh api repos/OWNER/REPO/pulls/PR \
     --jq '{head_sha:.head.sha,base_sha:.base.sha,head_ref:.head.ref,base_ref:.base.ref}'
   gh api --paginate repos/OWNER/REPO/pulls/PR/reviews \
     --jq '.[] | {id,state,commit_id,user:(.user.login // null),submitted_at}'
   gh api --paginate repos/OWNER/REPO/pulls/PR/comments \
     --jq '.[] | {id,commit_id,in_reply_to_id,user:(.user.login // null),created_at,updated_at}'
   gh api --paginate repos/OWNER/REPO/issues/PR/comments \
     --jq '.[] | {id,commit_id:null,user:(.user.login // null),created_at,updated_at}'

   # [PENDING] read every review thread's resolution/outdated state
   gh api graphql --paginate \
     -F owner=OWNER -F name=REPO -F number=123 \
     -f query='query($owner:String!, $name:String!, $number:Int!, $endCursor:String) {
       repository(owner:$owner, name:$name) {
         pullRequest(number:$number) {
           headRefOid baseRefOid
           reviewThreads(first:100, after:$endCursor) {
             nodes { id isResolved isOutdated path line
               comments(first:100) {
                 nodes { id databaseId author { login } createdAt updatedAt commit { oid } }
                 pageInfo { hasNextPage endCursor }
               }
             }
             pageInfo { hasNextPage endCursor }
           }
         }
       }
     }'

   # [PENDING] for every thread whose nested comments pageInfo hasNextPage=true
   gh api graphql --paginate -F threadId=THREAD_NODE_ID \
     -f query='query($threadId:ID!, $endCursor:String) {
       node(id:$threadId) { ... on PullRequestReviewThread {
         comments(first:100, after:$endCursor) {
           nodes { id databaseId author { login } createdAt updatedAt commit { oid } }
           pageInfo { hasNextPage endCursor }
         }
       }}
     }'
   ```

   The REST inline-comment pages provide complete comment content; GraphQL
   `reviewThreads` supplies resolution and outdated state. Reconcile IDs across
   both outputs. Any failed page, `hasNextPage` left unresolved, missing
   candidate head/base binding, or missing per-item commit disposition blocks
   merge.

   Use the review-comment data and GitHub's thread state to account for every
   inline thread, including resolved and bot-authored items. If pagination or
   thread state cannot be verified, the merge is blocked.
2. Inspect the actual candidate code, configuration, generated output, tests,
   and documentation. Fix every valid finding, then rerun affected tests and
   docs checks. Record the reason and evidence for every rejected suggestion.
3. After each fix or new feedback event, repeat step 1 from a fresh snapshot.
   Verify required CI, the final candidate SHA, and the final diff:

   ```bash
   # [PENDING] exact candidate only
   gh api repos/OWNER/REPO/pulls/PR --jq '{head_sha:.head.sha,base_sha:.base.sha}'
   gh pr checks PR --repo OWNER/REPO
   gh pr diff PR --repo OWNER/REPO
   rtk git diff --check BASE_SHA...HEAD_SHA
   ```

4. Do not merge while any review, comment, thread, requested change, or CI
   result is unread, unverified, or actionable. Record the complete review
   snapshot, finding dispositions, required checks, final diff check, and
   resulting main SHA under the external evidence root. A green subset or
   stale review never clears this preflight.

## Verified G0 setup commands

These commands ran successfully on 2026-09-19. They verify local setup only.

```bash
rtk --version
# rtk 0.49.0

rtk git status --short --branch
# ## codex/github-first-records (clean at the initial snapshot)

rtk git -C /Users/donbeave/Projects/tailrocks/velnor-project/velnor3 rev-parse HEAD
# abe9ad82a2d4d01b706bbc6122ab6ccb150faad9

rtk proxy codex --version
# codex-cli 0.155.0

rtk proxy date -u +%Y-%m-%dT%H:%M:%SZ
rtk proxy uname -a
rtk proxy sw_vers
```

Verified settings/evidence commands:

```bash
rtk proxy awk '/^model =|^model_reasoning_effort|^\[agents\]|^default_subagent_model|^default_subagent_reasoning_effort|max_concurrent_threads_per_session/' \
  /Users/donbeave/.codex-chainargos/config.toml

rtk proxy sqlite3 /Users/donbeave/.codex-chainargos/logs_2.sqlite \
  "select datetime(ts,'unixepoch'),feedback_log_body from logs where thread_id='01a0ba6f-f806-7d31-9abb-c828b3dc9e4e' and feedback_log_body like '%model=%' order by ts desc limit 1;"

rtk proxy sqlite3 /Users/donbeave/.codex-chainargos/logs_2.sqlite \
  "select datetime(ts,'unixepoch'),feedback_log_body from logs where thread_id='01a0ba74-0f82-7800-9937-fb9d22de7e3d' and feedback_log_body like '%model=%' order by ts desc limit 1;"
```

The settings output is recorded in `SPEC.md`, `STATUS.md`, and the external
session record. These commands do not prove agent capacity, CI, or gate status.

## Pending: G0 inventory and records

Run with a fresh UTC checkpoint and save compact output under the external
evidence root. Use pagination and preserve event semantics.

```bash
# [PENDING] authenticated repository/default-branch inventory for all 32 rows
gh repo view OWNER/REPO --json nameWithOwner,defaultBranchRef,defaultBranch
gh api --paginate repos/OWNER/REPO/pulls?state=open\&per_page=100
gh api --paginate repos/OWNER/REPO/commits?sha=BRANCH\&per_page=100
gh run list --repo OWNER/REPO --limit 100 --json databaseId,headSha,event,status,conclusion,url
gh api --paginate repos/OWNER/REPO/commits/REV/check-runs?per_page=100

# [PENDING] validate source record and exact manifest
jq -e '.schema_version == 1 and .manifest_id == "github-first-dual-lane-2026-09-19" and .scope.expected_repository_count == 32 and (.repositories | length == 32)' \
  docs/ci/github-first-dual-lane/fleet.json
```

Record unavailable access as an explicit blocker. Empty API output is not
evidence of no PRs, no checks, or no workflow.

## Pending: generator/bootstrap and hosted-first selection

The configuration and generator command below are observed source conventions;
the command has not been run by this records task. The owning G0/G1 agent must
run it from the exact candidate revision and attach output/digests.

The latest owner checkpoint is exact
`b981f43e8dfd70b4c628d29b0e7e9dce679ce537`, with regenerated workflows/state
and owner-reported 1,727/1,727 library plus actionlint/fmt results. This is an
external report, not a command verification by this records task. Exact review
`G1/reviews/bootstrap-b981f43e-independent.md` rejects the G1 source
checkpoint; archive/API-tree, freshness, provenance, legacy, fixture, and
image-digest gaps remain. Do not treat the older `7c`/`5cdf` abbreviations or
the owner test count as G1 evidence.

```bash
# [PENDING] inspect source config/schema and CLI help
rtk proxy sed -n '1,240p' .github-gen/velnor-workflow.toml
rtk proxy mbx run --locked --manifest-path crates/velnor-workflow/Cargo.toml -- --help

# [PENDING] check deterministic generated output
rtk proxy mbx run --locked --manifest-path crates/velnor-workflow/Cargo.toml -- --plain --check ../..

# [PENDING] regenerate only through the generator, then inspect the diff
rtk proxy mbx run --locked --manifest-path crates/velnor-workflow/Cargo.toml -- --plain --force ../..
rtk git diff --check
rtk git diff --stat
```

Required records: generator source revision and artifact digest, consumer SHA,
configuration/scan-state digests, generated tree digest, provider selection,
workflow/reusable graph, and cold-cache/shallow-checkout result. Do not hand
edit generated `.github` output; root owns source/config regeneration.

## Pending: required-check transition

The initial Velnor ruleset snapshot requires `DCO`, `ci-required`, and `Policy`.
Do not modify it during G0. Before any transition, write the old/new contract,
check-producing App, exact candidate SHA, equivalent hosted evidence, and
rollback. Add dual requirements only after both lanes report reliably; never
disable protection or use `continue-on-error`.

## Pending: package and release operations

These operations require publication-owner approval, exact reviewed revision,
and G1/G2 gates. They are procedures only:

```bash
# [PENDING] inspect product/runtime releases with identity/schema filtering
gh release list --repo tailrocks/velnor --limit 100
gh release view TAG --repo tailrocks/velnor --json tagName,isDraft,isPrerelease,targetCommitish,assets

# [PENDING] verify candidate package identities and digests
sha256sum DIST/*
dpkg-deb --info DIST/*.deb
dpkg-deb --contents DIST/*.deb

# [PENDING] clean APT client, using the real signed endpoint and scoped key
sudo apt-get update
sudo apt-get install velnor-runner
velnorctl --version
velnor-runner --version

# [PENDING] clean Homebrew client, published tap/formula only
brew update
brew install tailrocks/velnor/velnor
velnorctl --version
velnor-runner --version
brew test tailrocks/velnor/velnor
```

Verify preview/stable selection, upgrade, switching, uninstall, signing,
architecture, and sibling binary discovery. A local `dpkg -i`, an archive, a
formula pointing at a checkout, or a dry-run workflow is supplementary only.

## Pending: provider selection and hosted recovery

Provider selection must be typed/configured by the generator. The required
operational rules are:

- G1 recovery: automatic/default dispatch is GitHub-hosted; Velnor capacity is
  not a prerequisite. Record explicit recovery reason and candidate revision.
- G6 final: eligible trusted PR/main runs both GitHub-hosted and Velnor
  automatically. Explicit selection remains for diagnosis/recovery.
- Velnor outage leaves its required lane pending/failing. It cannot be silently
  replaced by hosted or counted green.
- Provider, actual runner/host identity, source object, checkout SHA, expected
  jobs, child runs, and check association are required in evidence.

## Pending: actual Mac/OrbStack lifecycle (G4/G5)

Run only after G3 hosted fleet exit and with the authorized current Mac. Record
the following before jobs: macOS/architecture, host identity, OrbStack and
Docker versions/context/socket, server OS/architecture, engine capacity,
Velnor package/source, and image digest/platform.

```bash
# [PENDING] inspect packaged operation and host identity
velnorctl --version
velnorctl host status
docker context show
docker version
orbctl version

# [PENDING] lifecycle; use repository's supported host commands and drain first
velnorctl host start
velnorctl host drain
velnorctl host stop
velnorctl host status
```

Prove registration/trust/routing, shared `max_jobs=N`, nested Docker/service
isolation, resource limits/retention, cold/warm cache, cancellation, restart,
disconnect/reconnect, observability, fork trust, parity, and released-package
operation. Do not prune unrelated Docker state or force Mac sleep/reboot.

## Pending: deterministic checker and final audit

The checker agent owns implementation and invocation details. The initial
`b3b6b2e` command is historical hygiene evidence only: its 216 tests/fmt/clippy
passed, but independent review rejected its semantic contract and its live run
returned 475 findings. Until the v2 command is committed and reviewed, do not
claim a checker completion or gate pass. The v2 invocation must read the
canonical manifest and external ledger at a fresh snapshot, then fail
stale/missing/queued/canceled/timed-out/skipped/failed required work. Fixtures
must cover stale SHA, skipped job, missing row, wrong provider, failed child,
and mismatched artifact.

Current checker work remains incomplete: the public CLI candidate rejects
CAS/sourcegraph handling, the shared-file-descriptor helper is under repair,
and the published 245-test candidate still has partial review with nested-child
and collector gaps. Collector credential-provider/repository-mapping security
work also requires independent review. These are unresolved evidence/implementation
findings, not checker or gate passes.

The exact user G0 acceptance matrix is in
[`SPEC.md`](./SPEC.md#exact-g0-acceptance-matrix). Before invoking the
authoritative checker, bind the exact 32-row manifest (goal lines 21–58), live
default-branch SHA/UTC, every open PR including drafts/bots/forks and tested
merge or merge-group identity, complete workflow/reusable-action/scanner/state/
trigger inventory, nonempty expected workloads, dependency/access gaps,
source/revision/digest records, and run/provider/host/checkout/job/child
identity. Reconcile every static manifest `default_branch`/`default_branch_sha`
to an independent live/default-branch snapshot and UTC observation; never trust
the manifest claim alone. Every required expected job must have an independently
verified terminal-success conclusion and complete child links/logs. Record a
typed dependency/dependent-workload graph with workload→child,
workload→required-check, workload→release, and workload→package edges; each
edge binds relation, stage, applicability, source revision, observation time,
evidence reference, and status. G0 inventories these edges; G2+ proves
applicable release/package execution. A fresh default/PR snapshot is required; newest-run, overall-green,
or `github.sha` shortcuts are invalid. `N/A` is exclusion-only.

Release/tag/feed/tap/install identity and functional results are G2+ stage
evidence, not a G0 prerequisite. `audit_ci` remains auxiliary default-SHA and
static-surface diagnostics. `lane_compare` remains diagnostic until the
assigned `/root/g1_run_operations` implementation and independent
`/root/g1_cache_semantics` review prove the full pair census, step alignment,
and complete paginated artifact/log inventory. Explicit job arguments, empty
census, skipped/unconcluded jobs, missing/empty logs, one-page `per_page=100`
artifact fetches, and watch-mode orphan/duplicate handling must all fail closed
in the authoritative checker, including unmatched pairs; helper output or self-attested records cannot
close G0/G7. Keep current unknowns and rejected exact candidates linked in the
external session record.

Use one canonical checker schema and command. Do not add or accept CLI/serde
aliases, flat-fleet coercion, merged release/install fallbacks, or precedence
shims for conflicting records. Collectors must migrate to canonical typed
records, emit one representation, reject duplicate/conflicting inputs, and
retain missing facts as incomplete. The exact invocation remains pending the
checker owner's committed schema/path contract; no guessed flags are verified.

## Recovery and rollback rules

1. Stop at the first invalidated dependency; record exact source/run/release
   identity and blocker externally.
2. Preserve immutable published assets. Repair forward or restore an allowed
   channel pointer; never replace same-version bytes/tag.
3. Reconcile running GitHub child runs before retry. Bound retries; repeated
   identical failures require diagnosis.
4. Reinstall a released fix, advance the generator/runtime epoch, regenerate
   affected consumers, and invalidate their evidence.
5. At G7 take a final UTC snapshot of all default tips/open PR heads. Any move
   reopens affected verification.
