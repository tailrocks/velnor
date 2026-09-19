# G1 scan-integrity handoff

## Effective settings

- Agent model: `gpt-5.6-luna`
- Reasoning effort: `max`
- Worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-scan-integrity`
- Branch: `codex/github-first-scan-integrity-corrected`
- Base: `12cc87b629802c294da9840325cb21087c020df6`
- Corrected full candidate: `3c46b9e83c9a0ca57be88743e49ecae27f731685`
- Candidate tree: `921391689cf0d64adf47bd9cc9e215704b5b7ab7`
- `rtk`: `0.49.0`

## Root cause

`crates/velnor-workflow/src/s2/scan/file_walk.rs` previously trusted generated
headers and prevalidated static output paths as scanner exclusions. A forged
header or a self-referential static workflow could therefore disappear from
scan/provenance and legacy-workflow detection. Whole-directory `.github`
exclusion was also unsafe because handwritten workflows/actions are real
inputs.

## Fix

- Keep `.github` in the repository walk so handwritten behavior remains visible.
- Exclude only exact paths independently recorded by the ownership sidecar,
  plus the exact host-env and sidecar artifacts. Generated/fleet headers are
  never authority.
- Reject static sources under `.github` after config path validation; static
  output declarations are never passed as a prevalidation scan bypass.
- Make legacy-workflow detection accept only current renderer outputs or
  sidecar-recorded stale outputs. Existing digest checks still protect stale
  deletion and generated drift.

## Evidence

Commands used from the isolated worktree with external target
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/scan-integrity-target`:

- `cargo test -p velnor-workflow --lib s2::scan::file_walk`: **5 passed**
- Focused churn, static self-source, and repeated-generation tests: **4 passed**
- `cargo test -p velnor-workflow --lib -- --skip checked_in_workflows_match_the_generator_byte_for_byte`: **1741 passed, 1 filtered**
- `cargo clippy -p velnor-workflow --lib --tests -- -D warnings`: **passed**
- `cargo fmt --all` and `git diff --check`: **passed**

The unskipped full library run was `1740 passed, 1 failed`: the one failure is
the pre-existing stale checked-in `release.yml` snapshot from source commit
`12cc`/PR952, not this scanner change. That snapshot must be regenerated only
after the combined reviewed generator changes are integrated.

## Regression claims

- Forged generated/fleet-header workflows remain scan inputs and fail the
  unmanaged-workflow check.
- Added/removed sidecar-recorded generated output leaves scan digest unchanged,
  while missing output still fails generated drift checks.
- Added/removed unmanaged workflows change scan digest and are rejected.
- Static self-source is rejected; a valid headerless static output is stable
  after first ownership recording and repeat `--check` is unchanged.
- A `git clone --depth=1 file://...` preserves the same provenance boundary.

## Review/next dependency

Independent review must target exact commit
`3c46b9e83c9a0ca57be88743e49ecae27f731685`; do not integrate rejected
`6409a08678683506c12c7d50820875c1fa703b9d` or
`7e9a2b5f0f6980f7be7a4ab9ee91d5a8f3d2da9f`. Do not regenerate checked-in
`.github` outputs from this isolated branch: the source base still awaits the
combined 12cc + bootstrap/config changes and reviewed candidate publication.
