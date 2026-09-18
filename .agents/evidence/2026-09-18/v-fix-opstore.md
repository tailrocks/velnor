# Verdict: CERTIFIED — fix/opstore-rejection-cause

Branch `fix/opstore-rejection-cause` @ `915ed0725ed316cd240a86c26efb1a343c7a9808`
(base `docs/bastion-final-plan` @ `38ffbfd7`, merge-base confirmed; commit signed off).
Verified in scratch worktree `/tmp/v-opstore-wt` (detached HEAD = fix commit). No edits made.

## Contract checks (all from diff 38ffbfd7..915ed072 + rerun)

- `record_admission` returns failing code: YES — `Result<(), AdmissionRejection>`,
  `required_failure` returns the rejection; all 7 checks name themselves
  (`store.admission.identity/validate/instance/transition`, `store.masks`,
  `store.admission.physical-budget`, `store.admission.persist`).
- `Rejected` carries code: YES — `AdmissionPersistenceOutcome::Rejected(&'static str)`
  threaded through `persist_admission_on_blocking_pool_with`.
- Per-class remediation in `operational_store` completion: YES —
  reason renders `(...code...)`; workflow remediation ONLY for
  `store.admission.validate`, daemon remediation for all infra + unknown codes;
  deadline / worker-failure / store-unavailable arms take daemon remediation.
  Old caller-less wrappers gone (`rejection_log_lines`, `failed_acquired_job_step_log`
  have zero remaining callers); no shims — all callers migrated (clean build proves it).
- Daemon build id in pre-admission telemetry: YES — `daemon_build` =
  `VELNOR_VERSION[/VELNOR_SOURCE_SHA]` in `run_queued_telemetry_fields`
  (fires before admission write), registered in RunQueued contract
  (`telemetry.rs`) + checked-in JSON Schema.
- Unit test per code: YES — `admission_rejection_names_the_failing_check`
  (all 7 codes), 7× `admission_rejection_*_renders_*_remediation` + unknown-code
  fail-closed, 2× `run_queued_telemetry_*` (presence + real-sink wire contract).

## Proof (rerun, observed)

- Focused: `admission_rejection` 9/9 ok; `run_queued_telemetry` 3/3 ok;
  `admission_without_complete_identity_fails_closed` 1/1 ok;
  `blocking_admission` 2/2 ok.
- `cargo test -p velnor-runner --lib`: 2177 passed / 0 failed / 4 ignored —
  matches claimed counts exactly.
- `cargo test -p velnor-model --lib telemetry`: 24/24 ok (schema-mirror covered).
- `cargo clippy -p velnor-runner -p velnor-model --all-targets`: exit 0, no warnings.
- `cargo fmt -p velnor-runner -p velnor-model -- --check`: exit 0, clean.
- Worktree `git status`: clean.

No merges, no pushes. Verdict: CERTIFIED.
