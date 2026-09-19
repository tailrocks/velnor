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
