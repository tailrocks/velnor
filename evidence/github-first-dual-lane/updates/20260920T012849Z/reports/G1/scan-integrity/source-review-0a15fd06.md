# G1 scan-integrity source review — 0a15fd06

Status: independent read-only review. No source edits, merges, protected-policy
writes, workflow dispatches, host/Docker operations, or authority writes.

## Frozen scope

- Candidate: `0a15fd06e002f57dca546d5c041754f1ec433508`
  (`codex/g0-estate-scope`), detached clean worktree
  `/tmp/velnor-scan-integrity-0a-review`.
- Immediate comparison base: `04a39cc3a0d75bf19007a4f3f654df38c9662212`.
- Previous independent review: `source-review-38345852.md`.
- Delta after `04a`: generated release/state only:
  `.github/workflows/release.yml` (+57) and
  `.github/ci/.github-actions-generator-state` digest updates.
- Current generated pin:
  `fdeed261bd2247a38db6922a7726cd45d3d6f31e`.

## Verification

| Check | Result |
| --- | --- |
| `cargo test -p velnor-workflow --all-features --lib --locked -- --test-threads=1` | **1756 passed** (180.99s) |
| Exact D19 check with current baseline | **Failed**, output below |
| `cargo clippy -p velnor-workflow --all-features --all-targets --locked -- -D warnings` | Passed at `04a`; source is unchanged in this delta |
| Worktree | Clean detached review tree |

Exact D19 command, using the candidate binary and current baseline:

```text
target/debug/velnor-workflow --check --plain --providers github-hosted,velnor \
  --baseline-revision 0a15fd06e002f57dca546d5c041754f1ec433508
```

```text
error: revision fdeed261bd2247a38db6922a7726cd45d3d6f31e has an unsupported or malformed closure entry: "120000 blob 47dc3e3d863cfb5727b87d785d09abf9743c0a72\\tcrates/velnor-workflow/CLAUDE.md"
```

The pinned tree proves the error is real:

```text
120000 blob 47dc3e3d863cfb5727b87d785d09abf9743c0a72  CLAUDE.md
120000 blob 47dc3e3d863cfb5727b87d785d09abf9743c0a72  crates/velnor-workflow/CLAUDE.md
100644 blob bd824dcc346e9ecd7e3aea378dd37e560041e2a8  Cargo.lock
100644 blob bc9347eeaac887e0fbd0b90e533acc82aced99d4  crates/velnor-workflow/Cargo.toml
```

## Verdict

**Do not approve G1 scan-integrity or generated-release readiness.** The full
suite passing does not make the current generated configuration valid: the
authoritative D19 check rejects its declared source pin. Do not weaken the
closure validator, dereference symlinks, or convert mode `120000` to a regular
blob in the check. That would make source identity depend on a target outside
the typed Git tree and would defeat the closure boundary.

Correct repair: regenerate from a pin whose declared closure paths are typed
regular immutable blobs, or change the typed closure contract only through a
separate, explicitly reviewed symlink model with confined target identity and
tests. The current `Fetch D19 pin history` step in `release.yml:1643` only
fetches the commit; it cannot make an incompatible tree pass D19.

## Material residual findings

### F1 — declared source pin is incompatible with the strict closure contract

`closure.rs:136-189` runs `git ls-tree -r` under scrubbed Git object
environment. `validate_closure_listing` at `closure.rs:189-220` accepts only
`100644`/`100755` `blob` records with canonical paths. `policy.rs:1026-1040`
uses this for all expected pinned closures. Therefore the mode `120000` error
is the intended fail-closed result, not a missing-history-only test artifact.
The latest fetch step addresses only availability of `fdeed…`; it does not
address the tree contract.

### F2 — protected authority remains absent/unproven

The candidate still maps a selected operator Git SHA into the local immutable
baseline path. No typed protected-policy collector/attestation proves caller
App/installation identity, source-specific branch/ruleset checks, bypass
authority, or post-read protected-state equality. The prior authority trace
remains applicable: this is `baseline_policy_unproven`, not proof that policy
is unavailable or unprotected. No hosted release result should be treated as
protected approval from this source-only implementation.

### F3 — raw inventory is retained for fingerprints but not consumed by detectors

`s2/mod.rs:1316-1418` now captures untracked files, symlinks, and other entries
through `RawInventory`; this is a real improvement. But
`scan_target_with_baseline` at `s2/mod.rs:1733-1737` computes
`detector_inputs` only for the fingerprint. Capability inference calls
`scan::scan_shape_with_owned_paths`, which reaches the index-backed
`repository_files_with_owned_paths` in `scan/file_walk.rs:26-60`. The source
comment explicitly says the detector consumes only safe regular Git-index
files. Thus an untracked workflow/action or tracked symlink can affect the
raw fingerprint while remaining invisible to capability inference. This still
violates the accepted detector-input contract; the existing raw-inventory test
does not prove detector classification in a real Git worktree.

### F4 — in-process rollback still lacks post-mutation identity checks

Durable journal recovery and preflight identity checks are improvements, and
the durable/reviewed-preimage tests pass. However
`rollback_generated_files` at `s2/mod.rs:7865-7925` removes any regular path
for a `Missing` preimage and blindly renames staged old bytes for a regular
preimage. It does not compare the live post-write identity/mode before either
operation. A concurrent replacement after a write and before an in-process
rollback can therefore be deleted or overwritten. The durable journal covers
recorded crash progress, not this rollback path. Required regression: inject a
failure after each ordered output/sidecar mutation, replace the live target
(including same bytes/different inode and missing-preimage recreation), and
require fail-closed recovery without clobbering the replacement.

### F5 — Git-object override implementation is present but hostile proof is incomplete

`reject_git_object_overrides` and the scrubbed closure/baseline commands are
fail-closed (`s2/mod.rs:1435-1438,1608-1644`; `closure.rs:136-175`). Existing
closure and immutable-baseline tests pass, but no exact fixture covers a
replace ref, grafts, or alternates with a conflicting tree. Add those tests
before claiming the structural SI-B2/SI-B3 proof; do not relax the current
checks.

### F6 — latest Docker cache additions are observational, not connected to build cache

The generated delta restores `.velnor-docker-cache` and creates a `seed`
directory (`release.yml:168-193` and the analogous Velnor job), but the shown
delta does not pass that directory to Buildx `cache-from`/`cache-to` or a build
step. This is not a closure fix and should not be reported as cache-integrity
proof. The D19 history-fetch addition is similarly orthogonal to source-tree
validity.

## Required next actions

1. Fix the generated D19 pin/tree mismatch by repinning/regenerating with a
   closure-compatible source tree; rerun the exact `--check` command.
2. Keep strict regular-blob validation. Do not accept symlink entries through
   a blanket mode exception.
3. Connect raw detector inputs to capability inference, or document and prove
   a typed exclusion that is consistent with the accepted design; add a real
   Git-worktree untracked/symlink classification regression.
4. Add identity-safe in-process rollback failure tests and implementation.
5. Add replace/graft/alternate hostile fixtures. Separately acquire/prove the
   protected authority contract before any G1 protected claim.
