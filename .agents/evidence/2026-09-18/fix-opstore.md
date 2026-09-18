# A1 opstore fix evidence: rejection-cause diagnosability

Branch: `fix/opstore-rejection-cause` (from `docs/bastion-final-plan` @ 38ffbfd7)
Commit: 915ed0725ed316cd240a86c26efb1a343c7a9808 (signed off, pushed to origin)
Diagnosis: /tmp/a1-opstore.md (mask-cap class fix 7ab8d438 already merged; this is the remainder)

## P1 — failing check code threaded into the rejection completion

- `ops.rs`: new `AdmissionRejection { code: &'static str }`; `record_admission`
  returns `Result<(), AdmissionRejection>`; `required_failure` returns the
  rejection. All 7 checks name themselves: `store.admission.identity`,
  `store.admission.validate`, `store.admission.instance`,
  `store.admission.transition`, `store.masks`,
  `store.admission.physical-budget`, `store.admission.persist`.
- `runner.rs`: `AdmissionPersistenceOutcome::Rejected(&'static str)` carries
  the code through `persist_admission_on_blocking_pool_with`; the
  `operational_store` completion renders
  `operational store rejected the sanitized admission row (<code>); ...` plus
  per-class remediation: workflow-field text ONLY for
  `store.admission.validate`; daemon-action text (inspect forensic line,
  check disk, restart/redeploy) for all infra codes and any unknown future
  code. Deadline / worker-failure / store-unavailable arms also take daemon
  remediation. Remediation threads via `_with_remediation` completion
  variants; removed the two wrappers left caller-less
  (`rejection_log_lines`, `failed_acquired_job_step_log`).
- Callers migrated (no shims): `test_support.rs`, `executor.rs` tests,
  `runner.rs` tests.

## P2 — daemon build id in pre-admission telemetry

- `run_queued_telemetry_fields` emits `daemon_build` =
  `VELNOR_VERSION[/VELNOR_SOURCE_SHA]` (version alone on dev builds).
- Registered as optional `String` in the RunQueued executable contract
  (`velnor-model/src/telemetry.rs`) AND the checked-in JSON Schema
  (`schemas/velnor.telemetry.v1.json`) — the schema-mirror test failed
  without the latter.

## Tests (new)

- `ops::tests::admission_rejection_names_the_failing_check` — all 7 codes.
- `runner::tests::admission_rejection_*_renders_*_remediation` — 7 codes +
  unknown-code fail-closed, asserting rendered remediation line + code in reason.
- `runner::tests::run_queued_telemetry_names_the_deciding_daemon_build` +
  `..._satisfies_the_wire_contract` (real-sink emission proves contract acceptance).
- Strengthened: `admission_without_complete_identity_fails_closed` now pins
  `store.admission.persist` (missing run/attempt fails at the durable write,
  not the identity gate); blocking-admission test pins `Rejected(persist)`.

## Results (observed)

- `cargo test -p velnor-runner`: exit 0 — lib 2177 passed / 0 failed
  (4 ignored), all integration binaries ok, 0 FAILED.
- `cargo test -p velnor-model`: exit 0 — 132 + 4 + 6 + 5 passed, 0 failed.
- `cargo clippy -p velnor-runner -p velnor-model --all-targets`: exit 0, no warnings.
- `cargo fmt -p velnor-runner -p velnor-model`: clean; focused suites re-run green after fmt.
- `cargo check -p velnor-runner --all-targets`: no warnings.

## Notes

- Not changed (per diagnosis): per-job mask bound (multiline-secret
  derived-pattern counting) — separate follow-up.
- Design deviation from diagnosis sketch: `Rejected` carries the code string
  (not the full struct) to keep the outcome `Copy`; unknown codes fail
  closed to daemon remediation.
