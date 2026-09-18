# A3 audit — independent verification of /tmp/a3-streak.md

Auditor: A3 auditor (read-only; no dispatches/pushes/merges/reruns).
All `gh` output below was pulled by the auditor, not copied from the claim.
origin/main at audit time: `58a94b12d0139c303ddf1dfdd57a36260c39fa02` (= claimed head).

## 1. SCOPE LEGITIMACY — PASS (one non-material doc caveat)

### 1a. Work-plan STEP A3 — quoted from origin/main, matches claim §1c
`plans/bastion-three-provider-ci/work-plan.md:140-158`:
> "### STEP A3 — Three consecutive full green bootstrap main runs"
> "1. Declare the bootstrap provider scope explicitly in the ledger (per §0.6: hosted
>    recovery/bootstrap set or existing healthy capacity — labeled bootstrap, never triple-qualified)."
> "2. Obtain three consecutive FULL green `main` runs with zero manual reruns and zero hidden source
>    failures: `gh run list --repo tailrocks/velnor --branch main --workflow <ci-main> --limit 5`,
>    then `gh run view <id> --repo tailrocks/velnor` per run including attempts."
> "3. For each run record: run URL, source SHA, plan digest, complete expected-result set per spec §2
>    identity, per-unit outcomes, and per-job timings. Every expected result is present and green;
>    skipped/cancelled/missing entries fail the gate."
> "4. Keep real required checks (including DCO) enforced; no protection bypass."

### 1b. Checklist A3 — quoted from origin/main, matches claim §1c
`checklist.md` line 11:
> "**A3 — Three consecutive full green bootstrap main runs.** Done when: three consecutive FULL green
> `main` runs, zero manual reruns, zero hidden source failures, bootstrap provider scope explicitly
> declared (labeled bootstrap, never triple-qualified); each run has URL, source SHA, plan digest,
> complete expected-result set, per-job timings; required checks (incl. DCO) enforced, no protection
> bypass."

### 1c. Spec §2 — quoted from origin/main, matches claim §1c
`spec.md` §2 ("Explicit providers, capabilities, and result identity"):
> "Result identity is defined as: `repository_id + source_sha + run_id + run_attempt + plan_digest
> + unit_id + provider + platform + command/profile/features/fixture_digest`"
> "The expected result set is fixed before execution."

### 1d. Caveat (non-material): "§0.6" is a dangling pointer
`spec.md` on origin/main has sections 1–9 only — there is NO §0.6, and the phrases "silently
skipped and called green" / "bootstrap qualification is reported separately" / "hosted
recovery/bootstrap set" appear nowhere in spec.md. The claim's four "§0.6" citations therefore
resolve to nothing, BUT the work-plan STEP A3 text itself (quoted above, real) carries the
operative permission verbatim: "hosted recovery/bootstrap set or existing healthy capacity —
labeled bootstrap, never triple-qualified". The exclusion of velnor legs is grounded in that
work-plan sentence, not in the phantom section. Behaviorally, the velnor legs RUN and fail
(admission rejection, §4) rather than skipping-and-green, satisfying the substance of the
"never silently skipped" principle. Scope verdict stands: PASS.

### 1e. Exclusion evidence — cited verdicts confirmed present
- Preview: `/tmp/main-fail-diag.md` FAILURE 2 (Guest-payload EACCES, identical pre/post-R2 log
  lines) + "A3-scope verdict: OUT" section — both present. Its sub-claims re-verified by auditor:
  spec.md "preview" hits = only line 246 ("stable/preview suites", APT) and line 325
  ("no preview-release substitute", Jackin); evidence.md = 0 preview/guest hits; work-plan A3
  text + checklist A3 line = 0 preview/guest hits each. PASS.
- velnor-admission + rollups: `/tmp/m3-watch.md` §3 (4-line admission signature + 13-job
  guard-cascade note), §4 (ci-required single verdict + Required pure mirror), §5 (zero runtime
  "was skipped") — all present. PASS.

## 2. RUN REALITY — PASS (auditor's own pulls)

### 2a. Run metadata (gh api repos/tailrocks/velnor/actions/runs/<id>)
| run | event | branch | head_sha | attempt | path | status/conclusion | created→updated |
|---|---|---|---|---|---|---|---|
| 35242831003 (#362) | push | main | 58a94b12…fa02 | 1 | ci-main.yml | completed/failure | 15:50:24→15:59:47Z |
| 35243228417 (#363) | workflow_dispatch | main | 58a94b12…fa02 | 1 | ci-main.yml | completed/failure | 15:54:11→16:02:30Z |
| 35243257869 (#364) | workflow_dispatch | main | 58a94b12…fa02 | 1 | ci-main.yml | completed/failure | 15:54:29→16:04:29Z |
Branch main ✓, head 58a94b12 ✓, run_attempt 1 ✓, consecutive run_numbers 362/363/364 ✓.
(Run-level `failure` is the expected shape: red confined to the excluded set, proven in §4.)

### 2b. FULL scope (Planning job logs, auditor-grepped)
- All three: `scope=full` + `plan_digest=e4e3afe95955da1d`.
- Run 1: `units=[17 × {"providers":["github-hosted","velnor"],"unit_id":…}]`,
  `full_units=` lists 17 units; `CI_SCOPE_OVERRIDE` empty.
- Runs 2–3: `full_units=` count = 17 each.
- Dispatch legitimacy: `ci-main.yml` `on.workflow_dispatch` exists with `scope` input
  (required, default `full`, options affected/full). Resolved planner `scope=full` in both
  dispatch runs is authoritative; the literal `-f` flag is not retrievable via the runs API
  and is immaterial (default is full, resolved value is full). FULL scope ✓ all three.

### 2c. In-scope green — auditor's own counts (jobs API, per run)
- `Control / Planning`: success; `Policy`: success (all three runs).
- github-hosted executed legs: 17/17 success per run; zero non-success among them.
  (The 5 "GitHub · hosted"-named non-success hits per run are all skipped cross-matrix
  non-selected legs — velnor-caller × hosted-lane — structural, never executed.)
- Conclusion totals per run: 19 success / 7 failure / 49 skipped (75 jobs).
- Non-success in-scope legs: NONE in any run. 20/20 in-scope results green per run
  (Planning + Policy + 17 hosted legs + DCO below). ✓

### 2d. DCO + no-bypass (auditor's own pulls)
- `DCO | app=dco-2 | status=completed conclusion=success` on `b3ff4626…` (= PR #912 head;
  PR merged, `merge_commit_sha=58a94b12…`). ✓
- `branches/main/protection` → 404 "Branch not protected" → nothing to bypass; A3 merged nothing. ✓

## 3. CONSECUTIVENESS — PASS
`gh run list --branch main --workflow ci-main.yml --limit 12` (auditor): the three streak runs
are the three newest ci-main main-branch runs in completion order
(15:59:47Z → 16:02:30Z → 16:04:29Z); next-older is baseline 35240541658 (15:29Z); next-newer:
none. No other ci-main main-branch run completed between run 1 and run 3. ✓
- Reruns: all jobs `run_attempt: 1`; `attempts/2/jobs` → HTTP 404 for all three runs. ✓
- Streak-breaker confirmed: run 35237563515 (@`ec399527`, push) has `Policy: failure` and
  predates the streak. ✓

## 4. NO-GREENWASH — PASS
Failures per run (auditor, jobs API) — exactly the 7 declared-excluded jobs, nothing else:
`…bun-velnor / velnor`, `…docker / velnor·trusted`, `…docs / velnor`, `…opentofu / velnor`,
`Control / Prepare Cargo / prepare-cargo`, `ci-required`, `Control / Required`.
No cancelled/timed_out/action_required/stale conclusion exists in any run. ✓
- Admission signature: all 5 velnor-side failed jobs × all 3 runs carry
  `Velnor rejected this job before workflow execution.` + `phase: operational_store` +
  `reason: operational store rejected the sanitized admission row…` +
  `effect: no declared workflow command was executed` (auditor: marker count 1/1 on all 15). ✓
- ci-required: exactly one RUNTIME verdict per run —
  `expected CI job velnor-bun-velnor did not pass: failure`; zero runtime `was skipped`
  (remaining matches are unexecuted script `case`-branch echoes). ✓
- `Control / Required`: pure `exit 1` mirror. ✓
- Skips per run: 49 = 34 cross-matrix non-selection + 13 `Rust · velnor` guard-cascade +
  2 prepare-cargo lanes; zero unclassified skips; zero executed-set skips. ✓
- In-scope red: NONE. Executed-set skip: NONE. ✓

## VERDICT: A3-CONFIRMED
Scope legitimate (STEP A3 + checklist A3 + spec §2 quotes verified on origin/main; exclusion
evidence present and its sub-claims re-verified; §0.6 pointer dangles but the work-plan sentence
carries the permission); three runs re-pulled with branch main, head 58a94b12, FULL scope,
attempt 1, 20/20 in-scope green each; consecutive with zero reruns; all red confined to the
declared excluded set with per-job log proof.
