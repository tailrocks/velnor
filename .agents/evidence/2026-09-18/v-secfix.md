# v-secfix — independent verification of secfix-1 (bastion batch 1)

Verdict: **CERTIFIED**

Target: branch `fix/security-audit-batch1`, commit
`40ca27645e283110c2a3fad3f349ad49efd0b2e5` (parent `137ca1f9`).
`origin/fix/security-audit-batch1` resolves to the same hash; commit
carries DCO `Signed-off-by`. Audit `/tmp/security-audit.md` (8 findings),
evidence `/tmp/secfix-1.md`. No edits made; no merges, no pushes; main
checkout untouched (HEAD `3bb1e23c`, only pre-existing
`?? .agents/memory/`).

## Per-finding result (diff `137ca1f9..40ca2764` inspected hunk by hunk)

- F1 [MEDIUM] diagnostics — FIXED, confirmed. `redact_inspect` keeps only
  Id/Name/State/NetworkSettings + Config.Labels; `Config.Env` (JIT) never
  reaches disk; unparseable inspect is withheld (failure recorded, nothing
  written). All 4 files `0600` via `write_owner_only` (mode-at-create +
  enforce-after-write), dir `0700`. `Supervision::release_state()` deletes
  the state dir, refuses dirs without a `diagnostics/` child, and is wired
  into the lifecycle integration test (permit release → delete → assert
  gone). Note: no production caller yet — consistent with the worker lane
  being library API awaiting daemon wiring (audit confirms the scale-set
  client is unwired; `Supervision::cleanup` likewise has no prod caller).
  `runner.log` stays byte-identical by disclosed design (no mask registry
  at this layer), mitigated by 0600 + deletion.
- F2 [HIGH] feed attestation — FIXED, confirmed. Rendered feed verify job
  runs `gh attestation verify` over every `incoming/*.deb` with `--repo`
  source + pinned `--signer-workflow {source}/.github/workflows/
  ci-release-package-signer.yml` + `--source-ref` (stable `refs/tags/
  $version`, preview `refs/heads/{default_branch}`) + `--source-digest
  $commit`, strictly after `apt-fetch` and before `apt-verify`; empty
  fetch fails closed; `GH_TOKEN` present. Signer pin verified against the
  real signer workflow used by the release publish lane. Record/manifest
  verified unattested (Attest steps cover only debs/tarballs/crates), so
  debs-only subjects are correct per the audit's parenthetical.
- F3 [MEDIUM] redacted Debug × 6 — FIXED, confirmed. Manual
  presence-only `Debug` (`<redacted>`) for `GitHubAppAuth`,
  `PemJwtProvider`, `InstallationAccessToken`, `AdminToken`,
  `RunnerSpec`, `ProvisionPlan`; all six `derive(Debug)`s removed;
  `ActionsAuth` mirror intact.
- F4 [MEDIUM] unbounded — REJECTED, confirmed faithful. Spec §4.3
  (`plans/bastion-three-provider-ci/spec.md`, "No CPU/RAM ceilings… No
  per-job disk/PID quotas… No per-job OOM containment… is claimed")
  mandates the behavior, so no backstop may be added. Accepted-risk
  record on `QUOTA_FLAGS` + pointer in `github_adapter.rs`, both
  accurate; test pins the full 11-flag strip list (both spellings) +
  `--shm-size` exclusion.
- F5 [MEDIUM] JIT via argv — FIXED, confirmed. `write_env_file` (0600,
  single-line enforced, fails closed on multiline) +
  `create_args_with_env_file` (zero blob bytes in argv; `JITCONFIG`
  occurs only in the const, env-file content, and tests).
  `ensure_runner` deletes the file before the create error propagates,
  covering success AND failure paths (failure-path test present).
  Teardown order (runner container removed first) pinned by the
  pre-existing `cleanup_exports_before_first_deletion_in_order` test.
  Residual container-config copy until teardown disclosed as
  unavoidable (image contract, same as ARC).
- F6 [LOW] preview rollback — FIXED, confirmed. Gate
  `case "$rollback" in ''|*[!0-9A-Za-z.+:~-]*)` is the exact
  `valid_pool_version` charset (verified against `apt.rs:221-226`),
  rejects `/`, sits after the emptiness/count guards and before first
  use in `-o`/URL, mirroring the stable `last-publish` gate.
- F7 [LOW] slot TOCTOU — FIXED (re-hash), confirmed. `check_slot()`
  re-hashes `$binary` (not the temp copy, both toolchains) immediately
  before EACH exec: install < check < --closure < check < --revision <
  export, swap fails closed. Regen is exactly +10 lines in each of
  `ci-unit-rust.yml` and `release.yml` + generator-state hashes.
- F8 [INFO] slug collision — FIXED, confirmed. Slug gains
  `-<8hex>` = sha256(canonical id)[0:4] (stable across restarts, never
  `DefaultHasher`); 19 expectation updates counted; adoption gate
  (canonical-id comparison) untouched; colliding-sanitization test
  covers all object names + re-derivation stability.

## Disprove attempts (both failed to disprove = pass)

- Print-a-secret probe (`/tmp/vsecfix-probe`, external crate on the
  public API only, canary secrets): all 5 externally-constructible
  structs print `<redacted>` with zero canary bytes; the 6th
  (`AdminToken`) is private and covered by the passing in-repo test.
- Unattested-deb probe (`/tmp/vsecfix-feed/harness.sh`): rendered a REAL
  feed workflow with the worktree binary from an apt `[[declare]]`
  fixture, executed the 50-line verify-step shell verbatim with stubbed
  `gh`/`velnor-workflow`/`gpg`. Unattested: exit 1, attestation
  attempted with full flags, apt-verify NEVER reached. Empty fetch:
  exit 1 on "no fetched debs to attest", apt-verify never reached.
  Attested control: exit 0, both debs verified, apt-verify reached.

## Gates (scratch worktree `/tmp/vsecfix-wt` @ 40ca2764, clean)

- 19/19 new + rewritten secfix tests pass (targeted nextest run).
- `cargo fmt --all --check`: clean. `cargo clippy --workspace
  --all-targets -- -D warnings`: 0 errors. `cargo check --workspace
  --all-targets`: 0 errors.
- Full `cargo nextest run --workspace --locked --features
  velnor-runner/test-support`: **4102 passed, 5 skipped, 0 failed** —
  exactly the claimed counts.
- Fresh clone `/tmp/vsecfix-clone` @ 40ca2764 with a `--revision`-pinned
  40ca2764 binary: `--dry-run` exit 0 ("0 files would change");
  `--check --pin-build` exit 0 ("Generated files are current").

## Accepted notes (disclosed, not mismatches)

`release_state` awaits the daemon-wiring PR (same as `cleanup`);
raw-log retention ends at permit release via the new primitive;
container-config JIT copy lives until teardown (unavoidable);
deb-extract symlink hardening and `secrecy`/`zeroize` remain flagged
for batch 2 per the evidence file.
