# PR954 clippy repair review — `09520d01e1a767cf544419762c1a22218ed31743`

## Scope

- Reviewed in detached worktree `/private/tmp/velnor-pr954-review-09520d01`.
- Candidate parent/base: `7609366f5ad530c12a87addf98d0c49448009362`.
- Candidate ref and remote-tracking ref both point to `09520d01`.
- `git diff --name-status base..candidate` contains only
  `crates/velnor-workflow/src/s2/primitives/package_release.rs` (19 additions,
  6 deletions). No original PR branch or remote was mutated.
- Local `refs/review/pr954` remains the earlier original-PR head
  `27f47e5f4f12f388ba3bf0a97f21dd78f2244250`; the candidate is a descendant
  of that ref through the merged/reconciled base.

## Evidence

- `cargo nextest run --locked --all-features --package velnor-workflow --no-tests pass`:
  **1884 passed** (19 binaries, 21.572 s).
- `cargo fmt --all -- --check`: pass.
- `cargo clippy --locked --profile test --all-targets --all-features --package velnor-workflow -- -D warnings`:
  pass.
- Rendered candidate and base workflows with independently built generator
  binaries against the same candidate checkout. All 21 generated files were
  byte-identical. In particular:
  - `preview.yml`: 62,081 bytes, SHA-1
    `476e403f04117fa142e99937de80ee383bd5e612` and SHA-256
    `75763bd924aef4a7d7bc503fbd9d7b0bd7b364ecea4831f46b051438b639e0a3` on
    both renders.
  - `release.yml`: 238,611 bytes, SHA-1
    `cd0c976e1c7fc0feb2120fcb8569cb86ab39c746` and SHA-256
    `940d9874df46309698797ec54067c442b9cc8f94180ffa2ecf56d865cba7634e` on
    both renders.
  This proves the extracted immutable-publish script body emits the same bytes
  as the base implementation; publication asset/tag/consumer behavior did not
  change in this commit.
- `actionlint` on both rendered package workflows passes when given the
  generated runner-label config (`.github/actionlint.yaml`). The unconfigured
  invocation reports only the expected custom `velnor-target-mvp` label errors.
- `git diff --check`: pass.

## Finding: mixed-case extension tests are not end-to-end

The candidate changes `package_release.rs:197-207` to accept case-insensitive
`.yml`/`.yaml` suffixes and adds direct helper assertions for `release.YmL` and
`release.YaMl` (`package_release.rs:1796-1797`). Production schema-2 admission
still rejects these names before the primitive runs: `config/mod.rs:1711-1725`
requires `Path::extension() == "yml"` exactly, and `validate_declare_row` calls
that validator at `config/mod.rs:1692-1693`. The primitive's `ctx.file` reaches
`validate_workflow_file` only after that admission (`package_release.rs:82`).

Therefore the new mixed-case assertions exercise a private unreachable path;
they do not prove mixed-case workflow generation or signer-workflow behavior.
This is not a publication-byte regression, and the existing lowercase contract
remains unchanged. Resolve the contract deliberately before treating the
extension fix as fully approved: either keep the schema-2 file contract
lowercase and remove/replace the misleading mixed-case assertions, or make the
shared config admission and all workflow-file consumers consistently support
case-insensitive extensions with an end-to-end fixture. Do not claim the new
mixed-case behavior from the current test alone.

## Verdict

The script extraction, test split, clippy repair, exact diff scope, generated
publication bytes, and existing lowercase behavior are verified. **Exact source
approval is conditional on resolving the mixed-case admission/test mismatch**;
there is no G1 overall approval (policy/generated/bootstrap gates remain
outside this commit).

## Supersession

- PR954 merged as 0dc79895ff1c5e88be7c3822c437e1c5b5282e12 at
  2026-09-19T20:32:43Z; its final PR head was
  85edab7bbbc72c8c9b81c25cc0f742a81dbf004a.
- The standalone repair branch was fast-forwarded to
  abc06317247fed02c9c8e64a391fe62e3f1cab84 and remains a pushed repair
  record only. It is superseded by merged main; no PR is needed and no
  cherry-pick or merge claim is made.
- Merged main already contains the contract-consistent repair: lowercase
  yml/yaml extension matching, mixed-case rejection coverage, and the
  immutable-publish prelude extraction.
- PR954's final rollup was 23 COMPLETED/SUCCESS and 49
  COMPLETED/SKIPPED. Skipped entries are non-selected Velnor/manual lanes,
  not passing gates; this record makes no required-gate claim from them.
- Post-merge evidence at the checkpoint: runtime-products succeeded;
  preview failed the known stale-pin generated-tree check for four files; main
  policy remained in progress. These outcomes are outside this superseded
  Clippy repair and are not a G1 approval.
