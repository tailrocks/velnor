# A1 opstore: run 35136207272 attempt-2 admission rejections

## Log used

Attempt-2 log is `/tmp/a0/35136207272.log` (4 `Velnor rejected` lines: Docker/Velnor +
prepare-cargo). `/tmp/a0/35136207272-a1.log` is attempt 1 (0 rejections) — not used.

## Root cause

**Rule that fired: `store.masks`** — `OpsSink::remember_masks` process-lifetime cap
(256 patterns / 64 KiB) on slot-1's **pre-fix daemon build**. Every admission after the
cap was hit failed closed with exactly the observed GitHub-facing text
(`runner.rs` `REJECTED_REASON`, via `record_admission -> false` ->
`AdmissionPersistenceOutcome::Rejected`). No workflow field was wrong; both jobs were
rejected for daemon-side registry exhaustion, not for their content.

**Evidence chain (all observed, no edits):**

1. Per-job validation is deterministic and both jobs' projected inputs are clean
   (`Slug` charset + denied-substring check in `velnor-model/src/job_summary.rs`;
   projections via `ops.rs::project` can only fail on denied substrings, and neither
   job name / workflow `CI / PR` / `refs/pull/904/merge` / numeric job_uid contains one).
2. Twin-pair timing excludes transient store contention: Docker/Velnor failed at
   18:50:56 while Bun+Docs/Velnor **started the same second and succeeded**; prepare-cargo
   failed at 18:51:57 while OTF/Velnor **started the same second and succeeded**.
   Both rejections landed in ~2-3 s (validation-speed, not the 30 s deadline).
3. Runner assignment (jobs API): BOTH failures ran on `velnor-dogfood-slot-1`
   (bare static name); all three passing Velnor jobs ran on `slot-2/3-next-*`.
   prepare-cargo shares the `velnor-target-mvp` label with the passing jobs, so this
   is slot-1-specific, not label/host-class-specific.
4. Same jobs, same slot, 4 h later (run 35159690860, 22:51Z): Docker/Velnor and
   prepare-cargo both **succeed on `velnor-dogfood-slot-1-next-*`**. Excludes per-job
   validation and persistent slot-1 state (disk budget, schema, config).
5. Same signature on slot-1 (bare) 2.5 h EARLIER: run 35117067806 job 104878696567
   (Docker/Velnor, 16:19Z) shows the byte-identical 6-line rejection while -next-
   slots succeeded in that run. Streak 16:19 -> 18:52, then recovery after process
   replacement. In-memory registry fits; on-disk budget does not (prune runs every
   15 min in-process and would have cleared `Exceeded` inside the streak).
6. The identical signature on this exact fleet was root-caused the same day:
   commit `7ab8d438` (merged Sep 16 00:55Z, ~18 h before our failures):
   "Sentry `velnor-daemon@dogfood` slot-3 rejected every job from cycle 41 to 52
   with `REQUIRED operational-store write failed (store.masks)` ... every job brings
   ~6 unique per-job tokens, so after ~40 jobs the cap was hit and the process could
   never admit again until it was replaced."
7. Fix verified in-tree: `cargo test -p velnor-runner --lib
   long_lived_slot_worker_admits_every_cycle_with_unique_job_secrets` -> **ok**
   (66-cycle regression from that commit). Post-fix code cannot accumulate, so
   slot-1's 16:19-18:52 process necessarily predates the fix: **version skew**.

## Why the architecture allowed this class

1. Admission was coupled to a monotonic process-lifetime resource: append-only mask
   registry + fail-closed gate (`MAX_RETAINED_MASK_*`). Long-lived slot worker +
   unique per-job tokens = guaranteed eventual total admission failure until process
   replacement. (Fixed in-tree by `7ab8d438`: `JobMaskScope` per-job scoping.)
2. `record_admission() -> bool` erases WHICH of the 7 checks failed; the GitHub-facing
   completion then prints one generic reason plus a remediation ("correct the rejected
   workflow field/action/ref") that is actively wrong for infra causes (`store.masks`,
   `physical-budget`, `persist`) — sent this investigation to workflow diffing first.
3. Skew is invisible exactly when it matters: daemon build identity only surfaces in
   the `Velnor runner identity` step, which rejected jobs never execute. (`VELNOR_MANIFEST_VERSION=13`
   is a manifest-schema const, not a build id — identical on failing-era and healthy slots.)

## Proposed source fix (NOT applied — no edits, no commit)

The class fix is already merged (`7ab8d438`); the operational remainder (redeploy
slot-1) is observed done by 22:51Z. What remains is source work so the next
fail-closed rejection names its cause instead of blaming workflow fields:

**P1 — thread the failing check code into the rejection completion.**
`record_admission` (`ops.rs:532`) has exactly ONE production caller (`runner.rs:344`
closure); all others are tests. Change it to return the `required_failure` code,
e.g. `Result<(), AdmissionRejection>` with `code: &'static str` (`store.masks`,
`store.admission.validate`, `store.admission.physical-budget`, `store.admission.persist`,
...), extend `AdmissionPersistenceOutcome::Rejected` to carry the code, and render
per-class reason + remediation in the `operational_store` completion at
`runner.rs:7839-7866`: workflow-field remediation ONLY for `store.admission.validate`;
daemon-action remediation (inspect forensic line, restart/redeploy, check disk) for
infra codes. Add a unit test per code asserting the rendered remediation class.

**P2 — emit daemon build id in pre-admission telemetry.**
`run_queued_telemetry_fields` (`runner.rs:1405`) fires BEFORE the admission write, so
it is recorded even for jobs that are then rejected. Add
`daemon_build: crate::protocol::VELNOR_VERSION` (+ a build SHA if plumbed) so the
next skew-driven streak is attributable to a stale slot without daemon SSH.

**Not proposed:** changing the per-job mask bound. Residual note only: the bound
counts DERIVED line-patterns (multiline secrets split per line in
`job_secret_mask_values`), so one >256-line secret would fail a single job closed.
Not the cause here (~6 tokens/job); hardening that count is a separate follow-up.

## Confidence

Which rule (`store.masks`): high. Mechanism (pre-fix process-lifetime cap + stale
slot-1 process): high. Version-skew framing: medium-high (behaviorally proven; daemon
build string for the failing process is unrecoverable from GitHub-side data by design —
which is exactly what P2 fixes).
