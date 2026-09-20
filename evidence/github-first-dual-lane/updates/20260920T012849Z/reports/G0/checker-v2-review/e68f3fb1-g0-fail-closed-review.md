# Exact checker review: e68f3fb120baca17c39607a2a8c5f0a52d7131e0

## Bounded verdict

The requested fail-closed corrections are present and passed the independent
public-CLI regressions. This is a bounded source review only; it is not G0/G3
or producer-integration approval. The commit correctly rejects caller-owned
`--live` input before reading it, marks every offline result validation-only,
and rejects ancestor symlinks in the CAS root.

## Exact source and tests

- Repository: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-checker`
- Detached tree: `/private/tmp/velnor-checker-review-e68`
- Exact commit: `e68f3fb120baca17c39607a2a8c5f0a52d7131e0`
- Parent reviewed baseline: `f6a0887b38a42d5c6a7e342ffa3c5a8837d25cc3`
- Product tree remained clean; no source edits were made.

Checks, with target `/private/tmp/velnor-checker-target-e68`:

- `cargo test --locked --all-features --package velnor-tools -- --nocapture` — **250 passed**.
- `cargo fmt --all -- --check` — pass.
- `cargo clippy --locked --all-features --package velnor-tools --all-targets -- -D warnings` — pass.
- `git diff --check f6a0887b..e68f3fb1` — pass.
- `cargo build --locked --all-features --package velnor-tools --bin velnor-tools` — pass.

## Public CLI regression evidence

Harness and result:

`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/e68f3fb-public-cli-harness/regression-results.json`

The harness reuses all 26 prior f6 serialized hostile fixtures (including the
complete locally authored fresh capture and symlink variants), invokes the
exact e68 binary, and contacts no network.

### Live path

All 26 cases, including the prior `fresh-self-authored` capture, return exit 1
with exactly:

`Error: --live is unavailable: trusted authenticated collector/current-API reconciliation is not wired; offline files cannot authorize a gate`

No fixture is parsed. A separate invocation using nonexistent manifest,
snapshot, evidence, and CAS paths returns the same error, proving the refusal
happens before caller-file access. This closes the f6 bypass where a synthetic
fresh typed inventory/CAS passed `--live`.

### Offline path

All 26 cases return JSON `status: "fail"` and include exactly one
`offline-validation-only` finding (plus any expected structural hostile
findings). The complete baseline and self-consistent source replay therefore
cannot authorize G0 offline. The implementation forces `report.status =
"fail"` after structural validation.

### Ancestor-symlink path

The real `root-ancestor-symlink` fixture uses the prior harness's actual
`root-parent-link/store` path. e68 returns:

- `g0-storage-root: cannot open evidence root: Not a directory (os error 20)`
- `offline-validation-only`

The prior f6 run incorrectly accepted this root. `RawEvidenceStore::open`
now walks every absolute/relative path component with descriptor-relative
`openat(... O_DIRECTORY|O_NOFOLLOW ...)`; parent components are rejected and
the final `sha256` directory/object remain no-follow. The dedicated unit test
`symlinked_ancestor_is_rejected_before_store_open` passes.

## Source assessment

`check_paths_live` now bails unconditionally with the trusted-collector
message, so there is no local JSON/CAS path that can masquerade as current API
authority. `check_paths` retains deterministic structural validation for
fixtures but always appends `offline-validation-only`, sorts findings, and
forces failure. This is the correct interim contract while authenticated
collector/current-API closing-head reconciliation remains unwired.

No broad G0/G3 approval is issued. Future collector wiring still needs its own
exact source review and independent producer-authority evidence.
