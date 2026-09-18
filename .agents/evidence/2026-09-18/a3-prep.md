# A3 PREP — three consecutive full green bootstrap main runs

Read-only prep. No runs triggered. Authority: `plans/bastion-three-provider-ci/work-plan.md` STEP A3,
`spec.md` §2, `checklist.md` A3. Repo `tailrocks/velnor`, branch `main`, workflow `CI / Main`
(`.github/workflows/ci-main.yml`).

## 1. Bootstrap provider-scope declaration (ledger text, per work-plan §0.6)

Paste into the ledger before the streak starts; A3 is **labeled bootstrap, never triple-qualified**:

```text
A3 BOOTSTRAP SCOPE DECLARATION (work-plan §0.6)
Scope: generated hosted recovery/bootstrap provider set + existing healthy capacity only.
- Provider `github-hosted`: unit jobs run on GitHub-managed machines (caller jobs
  `github-<unit>`, lane legs `… / GitHub`). QUALIFIED by this streak.
- Provider `velnor` (native lane, caller jobs `velnor-<unit>`, lane legs `… / Velnor`):
  included ONLY as already-healthy existing execution capacity; green legs recorded,
  not qualification of the bastion target state.
- Provider `github-self-hosted` (Scale Set / official-runner lane): NOT IMPLEMENTED at A3
  (lands at D1/D3). Explicitly OUT of scope; absence is declared, never silently skipped.
This streak certifies bootstrap stability only. It is NOT three-provider qualification
(that gate is E2). Loss of any in-scope required result fails the streak.
Required checks stay enforced (incl. DCO); branch protection is never bypassed.
Author: <a3-executor>. Verifier: <ci/result-verifier, different person/agent>.
```

## 2. Expected-result set derivation (spec §2 identity)

Identity tuple per result:

```text
repository_id + source_sha + run_id + run_attempt + plan_digest
+ unit_id + provider + platform + command/profile/features/fixture_digest
```

Component sources (all read-only):

| Component | Source |
|---|---|
| `repository_id` | `tailrocks/velnor` (numeric repo id via `gh api repos/tailrocks/velnor --jq .id`) |
| `source_sha` | `head_sha` of the run (`gh run view --json headSha`); == `main` HEAD at push |
| `run_id` | CI/Main run databaseId |
| `run_attempt` | `gh api .../actions/runs/<id> --jq .run_attempt`; streak requires attempt 1 only (§4) |
| `plan_digest` | `sha256` over canonical plan record (below); generator has no native digest field |
| `unit_id` | one of the 17 Velnor units (`evidence.md` §2) + `plan`/`policy`/`ci-required`/`required` controls |
| `provider` | `github` (= github-hosted) or `velnor` lane leg; `github-self-hosted` declared absent (§1) |
| `platform` | `runner.os-runner.arch` of the job (`ubuntu-24.04`/X64 for controls + github lane) |
| `command/profile/features/fixture_digest` | unit workflow inputs: `ci-unit-<kind>.yml` + `seed_compat`, `seed_dependency_files`, `seed_freshness_files`, toolchain pins (e.g. `docker/build-mise.lock`, `rust-toolchain.toml`, `Cargo.lock`); digest = sha256 of the concatenated pinned files at `source_sha` |

Plan digest construction (Planning step prints `scope=`, `units=`, `full_units=` to stdout;
see `crates/velnor-workflow/src/runtime.rs` plan-output emission):

```sh
RUN=<run-id>
gh run view $RUN --repo tailrocks/velnor --log --job <plan-job-id> 2>/dev/null \
  | grep -E '^(scope|units|full_units)=' | sort > /tmp/a3-plan-$RUN.txt
REV=$(gh api repos/tailrocks/velnor/contents/.github/workflows/ci-main.yml?ref=<source-sha> \
  --jq .content | base64 -d | grep -m1 'rev:' | awk '{print $2}')   # generator pin, e.g. 7341ef4b…
{ echo "rev=$REV"; echo "repo=tailrocks/velnor"; echo "sha=<source-sha>"; cat /tmp/a3-plan-$RUN.txt; } \
  | sha256sum | awk '{print $1}'   # = plan_digest
```

FULL-run rule: `scope=full` and `units == full_units` covering all 17 units
(`bun-velnor docker docs opentofu rust-policy rust-unit-collector rust-velnor-bench
rust-velnor-client rust-velnor-control rust-velnor-model rust-velnor-render rust-velnor-runner
rust-velnor-tools rust-velnor-workflow rust-velnor-workflow-contract rust-velnorctl
rust-production-topology`). Affected-only scope does NOT count toward the streak.

Expected success/skip partition (fixed BEFORE judging the run; mirrors the in-run
`ci-required` gate logic, `ci-main.yml` `ci-required` job):

- MUST be `success`: `Control / Planning`, `Policy`, every selected-unit caller job
  (`github-<unit>` + `velnor-<unit>`) with its active lane leg (`… / GitHub`, `… / Velnor`),
  `Control / Prepare Cargo` legs for selected rust units, `ci-required`, `Control / Required`.
- MUST be `skipped` (and only these): `Control / Velnor admission` (non-fork push),
  non-selected units (none on a FULL run), and the inactive lane leg inside each reusable
  unit workflow (callee `ci-unit-*.yml` fans each caller into a `/ GitHub` + `/ Velnor` leg;
  the leg that does not match the caller lane skips by design — this is why the flat job
  list shows each unit name twice).
- ANY `failure`, `cancelled`, `timed_out`, `action_required`, `stale`, or `neutral` on ANY job
  fails the run. ANY skipped-where-success-expected, success-where-skip-expected (identity
  mismatch), missing, or duplicate-conflicting result fails the run. Skipped/cancelled/missing
  entries fail the gate per work-plan A3.3.

## 3. Per-run evidence capture (exact commands)

```sh
REPO=tailrocks/velnor
# 3a. Consecutive main runs (A3.2): newest-first; streak = 3 in a row, same workflow, all green
gh run list --repo $REPO --branch main --workflow ci-main.yml --limit 5 \
  --json databaseId,displayTitle,workflowName,conclusion,headSha,attempt,createdAt,url \
  --jq '.[] | "\(.databaseId) \(.conclusion) att=\(.attempt) | \(.headSha) | \(.createdAt) | \(.url)"'

# 3b. Per-run core identity (run URL, SHA, attempt)
RUN=<run-id>
gh api repos/$REPO/actions/runs/$RUN \
  --jq '{url: .html_url, sha: .head_sha, attempt: .run_attempt, conclusion, event, created_at, updated_at}'

# 3c. Complete flat job list with conclusions (expected-set comparison input)
gh run view $RUN --repo $REPO --json jobs \
  --jq '.jobs[] | "\(.name) :: \(.conclusion)"' | sort | uniq -c | sort -rn

# 3d. Per-job timings (queue + run duration)
gh run view $RUN --repo $REPO --json jobs \
  --jq '.jobs[] | [.name, .conclusion, .startedAt, .completedAt] | @tsv'

# 3e. Attempt enumeration (every attempt ever created for the run; streak allows exactly one)
gh api repos/$REPO/actions/runs/$RUN/attempts/1 --jq '{attempt: .run_attempt, conclusion}' # 404 past last
gh api repos/$REPO/actions/runs/$RUN --jq .run_attempt   # must be 1; >1 = rerun happened → streak void

# 3f. Plan outputs for plan_digest (§2) + FULL-scope proof
gh run view $RUN --repo $REPO --json jobs --jq '.jobs[] | select(.name=="Control / Planning") | .databaseId'
gh run view $RUN --repo $REPO --log --job <plan-job-id> | grep -E '^(scope|units|full_units)='

# 3g. Sibling-workflow guard: no red main runs on Preview / runtime-products for the same SHAs
SHA=<source-sha>
gh run list --repo $REPO --branch main --limit 10 \
  --json databaseId,workflowName,conclusion,headSha,attempt,url \
  --jq --arg s "$SHA" '.[] | select(.headSha==$s) | "\(.workflowName) \(.databaseId) \(.conclusion) att=\(.attempt) \(.url)"'
# every line for the streak SHAs must read `success att=1` (see §5: Preview currently red → must be fixed first)

# 3h. Required-check enforcement proof (DCO + branch protection intact, no bypass)
gh api repos/$REPO/branches/main/protection --jq '{required_status_checks, enforce_admins, required_signatures}'
gh pr list --repo $REPO --state merged --limit 3 --json number,mergeCommit  # streak SHAs must arrive via protected merge, DCO-signed
git log --format='%H %s %GS' -3   # DCO signoff trailers on the merged commits
```

Record per run: run URL, source SHA, run_attempt, plan_digest, scope/units/full_units lines,
full flat job conclusion list, per-job timing table, sibling-workflow lines, protection snapshot.

## 4. Zero-rerun / zero-hidden-failure verification method

Verifier (≠ author) runs these; any FAIL voids the streak and restarts the count at the next
qualifying run:

1. **Zero reruns:** `run_attempt == 1` for all three runs (§3e); `gh run list` shows no
   `attempt>1` row for the streak SHAs; no `workflow_dispatch`/manual re-run events
   (`event == push`, or `schedule` only if that is the documented streak trigger — mixing
   event types across the three runs FAILs comparability).
2. **Consecutive:** the three runs are consecutive `CI / Main` completions on `main` — no
   intervening CI/Main run with any other conclusion between run 1 and run 3 (check §3a
   output ordering; an interleaved failure/cancel restarts the count).
3. **FULL scope:** `scope=full`, `units == full_units` == all 17 units, for each run (§3f).
4. **Complete expected set:** flat job list (§3c) matches the §2 partition exactly — zero
   failure/cancelled/timed_out anywhere; zero skipped-where-success-expected;
   `ci-required` + `Control / Required` success on every run.
5. **Zero hidden failures:** sibling workflows green for all three SHAs (§3g); `gh run view
   --log-failed` empty for each run; no failed step inside a success-conclusion job
   (`--jq '.jobs[] | select(.conclusion!="success" and .conclusion!="skipped")'` returns
   nothing); no `continue-on-error` masking (audit: `grep -rn continue-on-error
   .github/workflows/` at the streak SHA must show no unit-result masking).
6. **No protection bypass:** §3h shows required checks + DCO enforced at streak time; streak
   commits carry signoff and arrived through the protected path.
7. **Independent re-run of checks:** verifier re-executes §3a–§3h verbatim (not the author's
   pasted output) and diffs against the author's record. Mismatch = FAIL.

## 5. Current main state + what must land on main before the streak starts

Observed 2026-09-17 (read-only; re-verify at execution — facts, not pins):

- `main` HEAD: `33688938297eee3933997dbbf368ee20fb1779d4`.
- Latest push trio @`33688938` (2026-09-16T22:49Z): `CI / Main` #35159519365 **success**,
  `Velnor workflow runtime products` #35159518917 **success**, `Preview` #35159519217 **failure**.
- Preview failure signature (EACCES, both arches): `Upload guest payload` →
  `EACCES: permission denied, scandir '…/dist/microvm/work/rootfs-tree/lib/ssl/private'`,
  cascading skips (`Build/Sign/Replace preview deb`). Same signature on the two prior mains
  (`386ccc16`, `f16cc51a`); `f16cc51a` also had CI/Main + runtime-products red. Only ONE
  all-green CI/Main exists so far — no streak yet (need 3 consecutive).
- Display-name note: green CI/Main #35159519365 shows per-unit duplicate `/ GitHub` +
  `/ Velnor` legs with the inactive leg `skipped` — expected reusable-workflow fanout, not a gap.

Must land on `main` BEFORE run 1 of the streak (A3 depends on A1+A2):

1. **A1 — Preview EACCES root fix** (rootfs-tree perms / artifact staging; fix at source +
   regression test, not an `if-no-files-found` downgrade or permission-bypass hack).
2. **A1 — any remaining CI/Main live failures green** (currently green @HEAD, but the fix
   commits themselves must keep it green); regen exact (`velnor-workflow --plain --force`
   then `--dry-run` = 0 files); structured policy + `actionlint` clean; `cargo test -p
   velnor-workflow`, contract tests, clippy `-D warnings`, `cargo fmt --check` green.
3. **A2 — publish-before-pin implementation merged** (trusted R / candidate C, closure
   identity, negatives, zero cargo fallback, cold-consumer + atomic-promotion proofs).
4. **Streak SHAs must all contain 1–3** (fixes merged first; the three counted runs are
   pushes AFTER the final fix merge — a green run on a pre-fix SHA never counts).
5. **§1 declaration recorded in the ledger** with named author + independent verifier.

Streak start trigger: first `main` push containing all of the above → that run becomes
candidate run 1 iff FULL + green per §2/§4; then two more consecutive FULL greens.
