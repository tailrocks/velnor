# A2 provmech evidence — G3+G5+G8+G9 (bastion campaign)

- Branch: `feat/a2-provision-promotion`
- Commit: `a05b0e2a12405e737868a2bbf8872ce72169d674` (signed off, pushed to origin)
- Base: `origin/docs/bastion-final-plan` @ `96ccc0f1` (stalerev P2 live-pin provisioner kept; G3 builds on it)

## G3 — provisioner resolves from the product repo

`workflow_pinned_policy_runtime_velnor` (`crates/velnor-workflow/src/lib.rs`): the
consumer-repo `git fetch … "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY" "$PINNED_REVISION"`
is gone. Resolution is now the setup action's shape verbatim (modulo the rev var):
local `ls-tree` when the checkout contains the pin, else
`gh api repos/$PRODUCT_REPOSITORY/git/trees/…?recursive=1` with the truncation guard
and the same canonical-listing jq filter; `PRODUCT_REPOSITORY` pinned in step env
from the action coordinate.

Tests:
- `velnor_provisioner_resolves_the_closure_from_the_product_repository` — no `git fetch`,
  no `$GITHUB_REPOSITORY`, plus byte-level conformance of the resolution block against
  the setup action source.
- `velnor_provisioner_executes_product_repo_resolution` — executes the rendered script
  against a consumer checkout lacking the pin with stubbed `gh`/`git`/`install`: proves
  the API call hits `repos/tailrocks/velnor/git/trees/<pin>?recursive=1`, no fetch is
  attempted, manifest+asset download, and the verified slot is exported.
- `no_policy_job_builds_pull_request_code` updated: the old blanket `!contains("gh api")`
  (a proxy for "no ruleset lookup on Velnor") now asserts no `rulesets?` call and exactly
  one `gh api` — the product-trees fallback.
- Mutation: re-adding a `git fetch` line fails the executable test; reverted.

## G5 — atomic promotion mechanism with render≡stamp binding

New `velnor-workflow promote --rev SHA|HEAD [--repo] [--generator-repo] [--default-branch]
[--runners] [--message] [--dry-run]` (`src/promote.rs`, dispatched in `runtime.rs`):
1. resolves `HEAD` via the generator checkout, requires a full SHA pin;
2. verifies the running binary's own stamped source closure equals the pin tree's
   closure recomputed under the binary's own build identity (new `VELNOR_WORKFLOW_PROFILE`
   stamp + existing features stamp; `build.rs`, `SOURCE_FEATURES`/`SOURCE_PROFILE`) —
   render with X ⇒ stamp X — before any mutation;
3. requires a tracked-clean tree at the repo root, stamps `[generator] revision`
   byte-preserving (`stamp_pin`, section-scoped);
4. renders via the shared `render_tree` path extracted from `run()` (promote and plain
   regen cannot drift), writes, then proves determinism (fresh re-render identical),
   write integrity (disk == render), and metadata identity (ownership state bytes);
5. stages exactly the paths it wrote (`--untracked-files=all`, pre-existing untracked
   files never swept), creates one `git commit -s` (`chore(ci): bump D19 pin to <short>`);
   any failure restores recorded preimages (+ unstages, prunes created dirs).
`--dry-run` verifies end to end and restores. Policy/`--check` remediation text now
names the command. README documents it.

Tests (`tests/promote_atomic.rs` + `promote::tests`):
- `promote_commits_pin_metadata_and_tree_atomically` — real binary on a fixture repo:
  pin advances to the binary's own revision, exactly one signed-off commit carrying
  pin + state + workflows, tree clean; re-promote is a no-op without a second commit.
- `promote_refuses_a_pin_its_source_cannot_render` — foreign generator history refused
  naming the binding; pin/commit/tree untouched.
- `promote_dry_run_verifies_without_writing` — verifies, lists paths, restores byte-clean.
- `stamp_*` unit tests — byte preservation, section scoping, missing/malformed/doubled
  pins fail closed.

## G8 — platform→runner mapping folded into the closure

`platforms()` (`runtime_products.rs`) no longer reads `config.github_runner`/
`config.macos_runner`: all three builders are fixed constants in closure-covered
generator source (`ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-15`). A label change
cannot reselect builders (stale tag impossible); a mapping change is a source change
that mints a new closure and tag. No closure-format change, no manifest change
(G1 sibling untouched), no product invalidation. Owner render byte-identical
(`ci-runtime-products.yml` unchanged in regen).

Test: `producer_builders_are_fixed_closure_covered_infrastructure` — hostile lane
labels leave the matrix fixed, and the mapping file is proven under a `CLOSURE_PATHS`
entry via git toplevel. Mutation (spoofed builder) fails; reverted.

## G9 — closure-completeness guard

`closure::tests::crate_inputs_stay_closure_complete`: scans the crate manifest's
binary-feeding dependency sections (incl. `[target.*]`, dotted subtables; dev-deps
excluded by construction with rationale) plus workspace-inherited definitions for a
local `path` key (boundary-aware matcher, self-tested against lookalike names).
Mutation: a real `path` dep in `[build-dependencies]` fails naming the section;
manifest + lock restored. `build_script_mirrors_the_closure_spec` extended to pin
the `VELNOR_WORKFLOW_FEATURES`/`VELNOR_WORKFLOW_PROFILE` stamps promote depends on.

## Regen + gates

- Regen via generator only: `velnor-workflow . --plain --force` →
  `ci-unit-rust.yml`, `release.yml`, ownership state (generator 50); producer unchanged.
- `--plain --dry-run` reports 0 changes.
- `cargo nextest run --locked --all-features -p velnor-workflow`: 553/553 pass.
- `cargo clippy --locked --profile test --all-targets --all-features -p velnor-workflow
  -- -D warnings`: clean. `cargo fmt --check -p velnor-workflow`: clean.
- `velnor-workflow-contract` standalone: 6/6 pass.

## Notes for integrators

- `GENERATOR_REVISION` 49 → 50 (rendered bytes changed: provisioner step).
- Sibling-surface avoidance: no manifest-shape change (G1), no signer-line change (G2),
  no ordering change (G4); G6's negative-suite territory untouched (only the one
  `gh api` assertion this bundle's behavior legitimately moved).
- Residual (pre-existing, out of scope): a binary built from a dirty tree stamps its
  HEAD closure while behaving as dirty source; release products are clean-built by
  the producer, which is where that invariant is enforced.
