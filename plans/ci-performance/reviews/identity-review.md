# V-IDENTITY-001 independent review

Verdict: **HOLD for implementation**. The failure is real and the existing
Git-tree closure contract is internally consistent, but a local source build
can still claim the clean `HEAD` product.

Review-state note: the HOLD above is the pre-implementation verdict. A
bounded implementation is now present in the shared checkout and has focused
local proof; independent final review, all-target checks, clean generated
output, and real CI are still pending. It is not an acceptance verdict.

## Follow-up closure audit (2026-09-20)

The first implementation had two additional false-identity paths. A tracked
source symlink could point outside the declared closure; changing the resolved
file changed Cargo's compiled value while the symlink link text and Git v1
line stayed fixed. The ignored-output predicate also accepted any path with a
`target` component, so ignored `src/target/generated.rs` could be imported by
`#[path]` and change the binary without changing the closure.

The helper now canonicalizes each symlink target, requires an existing regular
file inside a declared closure root, and rejects broken, escaping, or
directory targets. It recognizes only a repository-root `target/` prefix as a
proven Cargo output; nested `target` directories under source roots fail
closed. The tracked `crates/velnor-workflow/CLAUDE.md -> AGENTS.md` link needs
no filename exception because `AGENTS.md` is inside the declared crate root.

Two actual Cargo fixtures in `identity::tests` prove the failure mechanism:
the binary value changes after modifying the external symlink target or
ignored nested target source while `git ls-tree -- src` stays equal. The
post-repair helper rejects each fixture. Focused identity tests pass (4),
default closure tests pass (17), and library Clippy passes with `-D warnings`.
All-target Clippy is still blocked only by parent-owned pre-existing failures
in `tests/rust_order_render.rs` and `src/s2/primitives/watch.rs`; this unit is
not accepted until the independent all-target and real-CI review completes.

## Finding

`crates/velnor-workflow/build.rs` computes both embedded values from
`git rev-parse HEAD` and `git ls-tree -r HEAD`. Cargo compiles the working tree.
The temporary fixture recorded in `experiments/V-IDENTITY-001.md` changed a
tracked closure source without changing `HEAD`; the rebuilt executable retained
the old revision and old closure while the committed file produced a different
closure. The unconditional missing-file Cargo sentinel also recompiles the
crate on every invocation, including an unchanged warm build.

This is an identity bug, not merely excess Cargo work: `source_candidate_contract`
then writes a self-manifest from `HEAD` and the executable's digest. A dirty
executable can therefore satisfy that temporary manifest. A source-only
self-manifest is not CI provenance and cannot replace the attested candidate
artifact path.

## Contract review

The current committed-tree contract is cross-compatible and should remain the
artifact identity for the bounded repair. `src/closure.rs`, `build.rs`, the
candidate publisher, and runtime-product verification all use the sorted raw
`git ls-tree -r <rev> -- <paths>` lines plus the same version/features/profile
footer. The lines retain Git mode, type, object ID, and path, so tracked regular
files, executable files, and symlinks are represented without following a link.
The merge fast path is also sound when it compares the merge and PR-head
closures; a differing merge revision must remain a separate `build_revision`.

Do not silently replace this with a filesystem-byte digest in this change. That
would diverge from the shell/API producer and would require a versioned contract
for symlink targets, filesystem modes, ignored files, untracked files, and
feature/profile selection. A filesystem identity can be a later separate
contract, carrying both `committed_closure` and `working_tree_closure` in a
receipt.

## Bounded repair

The strongest incremental repair is a worktree closure with the *same v1
canonical line format*, plus a separate local binding. It fixes the enabling
condition instead of rejecting the requested dirty local workflow.

1. Extract the pure canonicalizer and closure-path constants into a no-cycle
   source module included by both `build.rs` and `src/closure.rs`. For a clean
   checkout it must emit the exact existing `git ls-tree -r HEAD` lines and
   therefore the exact existing digest. For a worktree build, replace each
   tracked entry's object ID with the Git blob hash of the bytes actually read,
   preserve the actual Git mode/type (100644/100755/120000; reject unsupported
   file types), remove deleted entries, and add non-ignored untracked files
   under closure directories. Sort the resulting lines with the existing
   footer. Use raw symlink target bytes for mode 120000; never follow a link
   while hashing. Reject or explicitly account for a symlink target whose
   resolved content is outside the declared closure.
2. Treat ignored paths under closure roots as an error unless they are an
   explicit, proven Cargo output such as `target/**`; do not silently hash
   `node_modules`, caches, or arbitrary ignored files. Parse NUL-delimited Git
   output or reject paths that cannot be represented by the current v1 quoted
   line form. This keeps names, modes, symlinks, and untracked/ignored policy
   deterministic.
3. Embed the worktree closure from `build.rs`. Add an explicit clean/dirty
   provenance value for local use; keep `--revision` as Git `HEAD` provenance
   and keep CI's clean detached worktree requirement. A dirty binary's digest
   is a local worktree product, not a Git revision candidate.
4. Replace the local `source_candidate_contract` temporary self-manifest with
   a typed direct binding to the binary's embedded worktree closure and digest.
   It may drive a local `--check`, but it must never satisfy the external
   candidate artifact/attestation path. CI still requires the published
   manifest, binary digest, closure report, clean producer, and attestation.
5. Keep the unconditional Cargo sentinel. Removing it is a separate freshness
   experiment: an input-driven rerun model must cover tracked changes, mode and
   symlink changes, relevant untracked files, Git provenance, and feature/profile
   inputs before it can replace the sentinel.

The clean-tree compatibility is real: unchanged files produce the same Git
blob IDs and lines, so existing runtime products and API/shell closure checks
continue to accept them. The actual incompatibility is intentional and must be
typed: a dirty worktree digest cannot equal the Git closure used to locate an
immutable candidate, so local direct rendering needs its own binding. The
remaining work is implementation complexity (filesystem walking, Git quoting,
mode/symlink portability, and ignored-output policy), not a contract blocker.

A clean-worktree guard remains a useful producer invariant and a defense for
the CI path, but it is insufficient as the local fix because it discards the
explicit dirty-build requirement and leaves the build script's false identity
model intact.

## Required tests

- Clean fixture succeeds; tracked staged and unstaged edits under each closure
  root produce a different worktree closure and the embedded binary reports
  that closure, never the `HEAD` closure.
- Relevant untracked source files change the worktree closure. Relevant ignored
  files fail closed; a proven Cargo `target/**` output is excluded. Test a
  tracked executable-mode flip and the tracked `120000`
  `crates/velnor-workflow/CLAUDE.md` symlink replacement.
- A symlink's raw target bytes change its line digest without following it. A
  symlink whose resolved target changes outside the declared closure must fail
  or be covered by an explicit target-content rule.
- A deleted tracked file, file-to-directory replacement, regular-file-to-
  symlink replacement, and unsupported special file all take deterministic
  fail-closed paths.
- Rust canonicalization, build-script output, shell `LC_ALL=C sort`, and the
  recursive-tree/API fixture remain byte-identical for regular, executable,
  symlink, nested, and path-with-space entries.
- Candidate and CI profile fixtures retain `tui`/debug versus empty/release
  feature-footers. Add a future hyphenated-feature fixture or reject it
  explicitly; Cargo's `CARGO_FEATURE_*` spelling is not reversible in the
  current code.
- Merge fixture: equal head/merge closures may reuse bytes but must report the
  audited PR revision separately from `build_revision`; a closure-changing
  merge must force a clean PR-head build.
- Direct `--check` from a clean checkout uses the local contract. A dirty
  checkout uses its worktree binding and cannot satisfy an external Git
  candidate manifest; no test may treat that local binding as proof of a CI
  artifact.

Acceptance requires the clean/dirty boundary test and the producer/runtime
cross-checks above before any Cargo freshness optimization is accepted.

## Independent review: implementation recheck (2026-09-20)

**Verdict: HOLD.** The implementation fixes the original dirty-`HEAD` false
identity for the covered worktree cases, and the clean Git-v1 contract remains
byte-compatible, but staged additions are currently omitted from the dirty
closure. That can make compiled source bytes absent from the reported
identity. The existing candidate environment test also still encodes the old
one-sided manifest contract.

### Passing evidence

The focused identity suite passed:

```text
cargo test --locked -p velnor-workflow identity::tests -- --nocapture
cargo test: 4 passed, 1948 filtered out (21 suites, 2.50s)
```

The four fixtures cover tracked edits, deletion, mode changes, untracked
inputs, external source symlinks, and ignored nested `target` source. They
also run Cargo and prove that changing the external symlink target or ignored
nested source changes the compiled value while the clean Git-v1 lines stay
unchanged; the helper then fails closed. The closure suite's shell parity and
clean-line tests were already passing in the parent validation set.

The local source candidate uses an in-memory binding of the current binary
digest plus worktree closure. It does not manufacture a manifest. The CI
runtime publisher builds a clean detached checkout, compares the binary's
closure and revision, and publishes an attested binary/manifest. The policy
candidate path requires both explicit binary and manifest slots, checks the
manifest revision and clean candidate closure, hashes the binary before any
execution, then checks the binary's closure. Those paths do not treat a
source-only self-manifest as CI provenance.

### I-1: staged additions escape the worktree closure

`worktree_lines` discovers additions only through:

```text
git ls-files --others --exclude-standard -z -- <closure paths>
```

That excludes index entries already staged. An isolated Git fixture produced
this exact result: `HEAD` contained only `src/lib.rs`; after creating and
staging `src/generated.rs`, the helper's `--others` input was empty while
`git ls-files --cached` contained `src/generated.rs`. Cargo reads the staged
file from the worktree and can compile it, but neither the baseline tree nor
the added-file pass contributes it to the closure. A staged rename has the
same omission for its new path; `git add -N` is another index-state variant.

This is a correctness blocker for the requested dirty/add identity. Merge
the HEAD tree with the current worktree/index path set, or otherwise enumerate
all current paths (including staged additions) before hashing. Add a Cargo
fixture where a staged-only module changes the compiled value while the
closure must change. Keep staged deletion and tracked modification coverage.

### I-2: compile-time imports need a closure-boundary guard

The closure helper hashes the declared file set. A scratch Cargo fixture with
`include_str!("../outside.txt")` changed its compiled value after only
`outside.txt` changed, while the `src` Git tree and computed identity stayed
the same. Current Velnor shipped includes found by `rg` are within the crate
closure (`templates/release-package-signer.yml` and the generic-surface test
fixture); the outside-import fixture is therefore a demonstrated architectural
gap, not a current production escape.

Before claiming that the digest covers everything compiled, either add a
static closure check for `include!`, `include_str!`, `include_bytes!`, and
`#[path]` targets that cross `CLOSURE_PATHS`, or make the contract explicit
that every compile-time import must be inside those paths and add a regression
test that rejects an outside import. The existing Rust scanner's `include_str!`
checks are for project scanning and do not prove the build identity's closure.

### I-3: stale integration fixture (contract mismatch)

The focused candidate tests pass the source-candidate cases, but the
candidate-filtered integration run has one exact failure:

```text
candidate_manifest_env_fallback_binds_the_env_slot_candidate
error: VELNOR_WORKFLOW_CANDIDATE_MANIFEST requires VELNOR_WORKFLOW_CANDIDATE_BINARY
```

The fixture sets `VELNOR_WORKFLOW_PINNED_BINARY` plus only the candidate
manifest. The strict two-slot contract is the intended security boundary;
update the fixture and generated handoff to provide the candidate binary with
its manifest, then rerun all targets. Do not restore one-sided fallback.

### Performance and trust notes

Every current worktree entry calls `git hash-object` through a new process,
even when the clean path eventually returns raw Git lines. That is a measured
design cost (one process per entry) and should be benchmarked or batched after
correctness; it is not a reason to remove the dirty-byte check.

The local binding is suitable only for local source `--check`. CI acceptance
still depends on clean checkout/build invariants, binary/manifest digest and
closure checks, and workflow attestation. No review evidence permits a dirty
source binary or its self-report to stand in for that artifact provenance.

Resolve I-1, resolve or contract-test I-2, and repair the stale fixture before
accepting this identity unit. No source edits were made in this review.

## Bounded repair follow-up (2026-09-20)

I-1 is repaired in `crates/velnor-workflow/src/identity.rs`. The dirty path
set now unions the `HEAD` tree, stage-0 index paths, and nonignored untracked
paths. Unmerged index stages fail closed. The regression fixture
`staged_add_delete_rename_and_restage_inputs_are_reflected` proves staged-only
addition, tracked staged deletion, staged rename, and a restaged rename all
change the observed closure as appropriate. `batched_git_blob_hashes_match_single_blob_hashes`
also covers a path containing spaces and compares batched output with the
single-object reference.

Regular files now use one `git hash-object --stdin-paths --no-filters` process
per closure invocation; symlink link text retains the separate byte-input
path because Git follows filesystem symlinks for path batching. A scratch
400-file measurement was 0.0294 seconds batched versus 9.4709 seconds for
400 child processes (322x). This is command-level evidence, not a CI speedup
claim. Identity tests are 7/7 and library Clippy is clean.

The stale integration fixture now sets both
`VELNOR_WORKFLOW_CANDIDATE_BINARY` and `VELNOR_WORKFLOW_CANDIDATE_MANIFEST`.
The strict pair contract remains intact; its focused test is 1/1.

### Closure contract for compile-time imports

The source closure is a declared set of tracked paths plus the feature/profile
footer. It is not a general Rust dependency graph. Current literal production
`include_str!` targets are:

```text
src/lib.rs                 -> templates/release-package-signer.yml
src/s2/mod.rs              -> templates/release-package-signer.yml
src/primitives/release.rs  -> tests/generic_surface_literals.rs
src/s2/primitives/release.rs -> tests/generic_surface_literals.rs
src/closure.rs             -> build.rs (test contract)
src/s2/closure.rs          -> build.rs (test contract)
```

All resolve under `CLOSURE_PATHS`. The existing scanner's literal parser and
symlink resolution are reusable for a future closure-boundary test, but they
currently cover `include_str!` only. `include_bytes!`, `include!`, `#[path]`,
macro-generated paths, build-script outputs, proc-macro reads, and arbitrary
environment inputs need separate dependency analysis. The implementation
therefore makes no blanket claim for those inputs: producers must keep the
declared closure complete and clean/isolated, while a dedicated static import
closure check can be added without pretending to prove arbitrary Rust
expansion.
