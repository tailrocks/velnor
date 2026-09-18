# secfix-1 — audit batch 1 (bastion) evidence

Branch: `fix/security-audit-batch1` (from `origin/docs/bastion-final-plan` @ `137ca1f9`)
Commit: `40ca27645e283110c2a3fad3f349ad49efd0b2e5` (signed-off, pushed)
Audit: `/tmp/security-audit.md` (8 findings, 0 criticals)

## F1 [MEDIUM] diagnostics redaction + perms + state-dir deletion — FIXED
- `supervise.rs`: `redact_inspect` keeps Id/Name/State/NetworkSettings/Config.Labels
  only; `Config.Env` (JIT blob) never touches disk. Unparseable inspect is
  withheld (failure recorded, nothing written) — fail closed.
- All four diagnostic files `0600`, dir `0700` (`write_owner_only` in
  `worker/mod.rs`, shared with F5; mode-at-create + enforce-after-write).
- `Supervision::release_state()`: deletes the state dir after permit release;
  refuses dirs without a `diagnostics/` child. Wired into the lifecycle test.
- Tests: `diagnostics_redact_container_env_before_writing`,
  `diagnostics_land_owner_only` (unix), `unparseable_inspect_is_withheld_never_persisted_raw`,
  `release_state_deletes_only_exported_dirs`, + integration assertion in
  `full_lifecycle_holds_one_permit_until_confirmed_cleanup`.

## F2 [HIGH] feed attestation — FIXED
- `release.rs` APT feed verify job: after `apt-fetch`, before `apt-verify`,
  `gh attestation verify` every `incoming/*.deb` against `--repo {source}` +
  pinned `--signer-workflow {source}/.github/workflows/ci-release-package-signer.yml`
  + `--source-ref` (stable: `refs/tags/$version`; preview: `refs/heads/{branch}`)
  + `--source-digest $commit`; empty fetch fails closed. `GH_TOKEN` added to step env.
- Record/manifest: verified NOT attested (no Attest step covers them; sidecar
  checksums only) — debs are the attested subjects, per the task's parenthetical.
  Doc comment at `render_sign_deb_job` updated (consumer-lane claim now true).
- Test: `feed_verify_attests_every_fetched_deb_before_apt_verify` (subjects,
  flags, fetch < attest < verify ordering).

## F3 [MEDIUM] redacted Debug — FIXED
- Manual presence-only `Debug` for `GitHubAppAuth`, `PemJwtProvider`,
  `InstallationAccessToken` (`credentials.rs`), `AdminToken` (`client.rs`),
  `RunnerSpec` (`runner.rs`), `ProvisionPlan` (`mod.rs`), mirroring `ActionsAuth`.
- Tests: `debug_renders_redact_key_material`, `admin_token_debug_redacts_the_header`,
  `runner_spec_debug_redacts_the_jit_blob`, `provision_plan_debug_redacts_the_jit_blob`.

## F4 [MEDIUM] unbounded — REJECTED (accepted risk, spec §4.3)
- No backstops added: spec §4.3 mandates unbounded; installer fails closed on
  surviving ceilings. Accepted-risk justification recorded on `QUOTA_FLAGS`
  (`container.rs`) + pointer in `github_adapter.rs`: any admitted job can DoS
  the host; same-repo PRs untrusted for capacity.
- Test: `quota_strip_list_covers_every_ceiling_flag` pins the full 11-flag
  strip list (both spellings) + `--shm-size` exclusion.

## F5 [MEDIUM] JIT via argv — FIXED
- `RunnerSpec::write_env_file` (0600, single-line enforced, fail closed on
  multiline) + `create_args_with_env_file` (`--env-file`, zero blob bytes in
  argv); `ensure_runner` deletes the file immediately after create on both
  success and failure paths. Contract comments updated (old "never written
  to disk" claim corrected).
- Container/config removal: teardown already removes the runner container
  first, before any other deletion (order pinned by
  `cleanup_exports_before_first_deletion_in_order`); the container-config copy
  is unavoidable (image contract, same as ARC) and now the ONLY remaining copy
  after F1+F5. No earlier removal is possible — the container runs the job.
- Tests: `jit_env_file_carries_the_image_input` (+0600 unix),
  `jit_env_file_rejects_multiline_blobs`, rewritten
  `runner_argv_joins_dind_netns_without_publishing` (no blob/JITCONFIG in argv),
  `runner_provision_creates_then_adopts` (env-file argv + deleted),
  `runner_provision_deletes_the_env_file_when_create_fails`.

## F6 [LOW] preview rollback validation — FIXED
- Feed template `prior` step: `case "$rollback" in ''|*[!0-9A-Za-z.+:~-]*)`
  fail-closed gate before first use (exact `valid_pool_version` charset;
  rejects `/`), mirroring the stable `last-publish` gate.
- Test: `preview_rollback_version_is_charset_validated_before_use` (gate + ordering).

## F7 [LOW] slot install→exec TOCTOU — FIXED (re-hash)
- `workflow_pinned_policy_runtime_velnor`: `check_slot()` re-hashes the slot
  path against the proven digest immediately before EACH exec (`--closure`,
  `--revision`); swap fails closed. Doc comment updated.
- Test: `velnor_provisioner_rehashes_the_slot_before_each_exec` (both
  toolchains, install < check < closure < check < revision < export ordering);
  the e2e stubbed-execution test still passes (valid bash).
- Regen: `ci-unit-rust.yml` + `release.yml` (+10 lines each, check_slot only).

## F8 [INFO] slug collision — FIXED
- `slug()` = `s<set>-<sanitized>-<8hex>` where 8hex = sha256(canonical id)[0:4].
  Stable across processes/restarts (never `DefaultHasher`); idempotency kept.
- Updated 19 exact-name expectations across 6 files (incl. integration test).
- Test: `slug_hash_disambiguates_colliding_sanitizations` (`a/b` vs `a b`
  differ on all object names; re-derivation stable).

## Regen / gates
- Regen: `velnor-workflow generate . --force` only; diff = F7 hunk + generator
  state. F2/F6 templates render no file in this repo (no apt declaration).
- `cargo fmt --all --check` = clean; workspace clippy `-D warnings` = 0 errors;
  `cargo check --workspace --all-targets` = 0 errors.
- `cargo nextest run --workspace --locked --features velnor-runner/test-support`:
  4102 passed, 5 skipped, 0 failed (incl. 3/3 scaleset_worker integration).
- Clean clone @ 40ca2764: `--dry-run` exit 0 ("0 files would change"),
  `--check --pin-build` exit 0 ("Generated files are current", D19 candidate
  leg; pin promotes post-merge via `promote --rev HEAD`).

## Follow-ups (out of scope)
- F2 defense-in-depth from the audit (deb-extract `--no-overwrite-dir`/symlink
  hardening) not done — digest-before-extract already holds; flag for batch 2.
- `secrecy`/`zeroize` for PEM/token storage (F3 suggestion) not adopted.
- Residual container-config JIT copy until teardown (F5, unavoidable).
