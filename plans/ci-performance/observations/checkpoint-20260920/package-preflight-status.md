# Package preflight — frozen work in progress

Frozen at user's immediate commit/push request, 2026-09-20 18:09 UTC. Preserve this patch as a reviewable artifact; do not apply it as validated implementation.

Baseline: final bootstrap generator snapshot from `/tmp/velnor-bootstrap-97bac4c`, including the hosted-control selector correction. `manifest.json` records every baseline and WIP file hash. The incremental patch applies cleanly against that final bootstrap source (`git apply --check` passed). It excludes the independently integrated stable package-kind repair and bootstrap implementation.

Drafted:

- Typed runtime producer namespace (`plan`, `runtime`, `package-runtime`), with distinct package artifact prefix and exact expected producer binding.
- Shared stable/preview/validation package-lane context; validation omits publication event guards.
- Architecture-qualified Debian matrix artifact names and exact signer selection; publisher downloads the declared prefix into isolated directories, verifies the architecture set, and rejects duplicate destination paths before collection. Legacy guest-matrix writers also receive distinct names.
- Package-owned Cargo metadata declaration for the installation transaction lock; scanner records and validates an absolute `/run` path. Current package declares its real ancestor-owned lock. No maintainer script or production lock semantics changed.

Not implemented yet:

- Generated read-only reusable package validation graph, its required PR caller, native installation/activation verification jobs, and strict aggregate verdict.
- Propagation of the declared package lock into the installation command, absence behavior, and semantic matrix/collision/namespace/lock regression tests.
- Disposable hosted control placement for preflight (approved); persistent fleet verification callers remain intact.
- Preview/runtime-product concurrency narrowing; separate queued requirement.

Validation: initial repository scan passed before edits; patch application check passed. Draft source has NOT been formatted, compiled, linted, or tested. It is expected to have unused draft items until graph wiring is completed. Scanner normalization needs an explicit repeated-slash check and metadata type/absence tests. No Cargo build was run in this worktree; the integration author owned the compiler slot.

Design and evidence: `package-preflight-design.md`, `runtime-publication-review.md`, and the independent bootstrap reviews in the reliability observations. Full working files remain `/tmp/velnor-preflight-97bac4c`; frozen baseline remains `/tmp/velnor-failures/preflight-baseline`.
