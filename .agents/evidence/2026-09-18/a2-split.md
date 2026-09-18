# A2 SPLIT author evidence: producer-first landing

Date: 2026-09-17. Diagnosis input: /tmp/a1-pr912-red.md (F1+F2 + sequence step 1).

## Deliverable

- Branch: `feat/a2-producer-revision` (OFF `origin/main` 33688938, isolated worktree /tmp/a2-producer-worktree)
- PR: https://github.com/tailrocks/velnor/pull/916 (to main, NOT merged)
- Commits (both `git commit -s`):
  - `51e635aff74fdffab138d5751249ca7105b678c4` generator change + regen
  - `f4d46b3b` D19 pin bump to 51e635af + regen (PR-914 flow)

## Scope proof (producer half ONLY)

Diff vs origin/main (15 files): `runtime_products.rs`, `ci-runtime-products.yml`,
pin file, generator state, pin-embedding renders. Verified:
- Consumer sources byte-identical to origin/main: setup action (both copies),
  `lib.rs` (provisioner), `release.rs` (no G10).
- All consumer-lane workflow deltas (ci-unit-rust, release, preview, ci-pr,
  ci-main, ci-policy, units, maintenance) are pin-substitution only.
- `MANIFEST_ACCEPT_FILTER` unchanged (no revision clause); producer still
  evaluates the exact consumer filter over the assembled manifest.
- No `--source-ref` in setup action or provisioner (f79ab248 excluded);
  `--source-ref` appears only in the producer smoke test (2x, G2 producer half).

## Composition bug found + fixed (campaign merge is broken here)

The docs/bastion-final-plan merge composes 754c1cd6's assemble `env` trim
(CLOSURE-only) with 65beb2ed's body reading `$HEAD_SHA` under `set -u`:
the merged assemble step dies on an unbound variable. This split restores
`HEAD_SHA` to the assemble `env` (GH_TOKEN/TAG stay out) + a test pinning it.
Cause class: sibling template/env edits merged without an env-coverage check;
`bash -n` cannot see it. Enabling condition partly removed by the new test.

## Gates (all green, observed in the worktree)

- `cargo test -p velnor-workflow --locked --all-features`: 546 passed, 0 failed
  (491 lib + 2 + 6 + 5 + 9 + 33 integration).
- `cargo clippy --locked --profile test --all-targets --all-features -p velnor-workflow -- -D warnings`: exit 0.
- `cargo fmt -p velnor-workflow -- --check`: exit 0.
- `actionlint` (whole tree): exit 0.
- Generator: `--plain --dry-run` shows 0 updates; `--plain --check` exit 0
  ("Generated files are current"). Regen via generator only (`--force` after
  review, no hand-edits to rendered files).

## Old-consumer proof (/tmp/a2-old-consumer-proof.sh, ALL PASS)

- Old download + cache-verify filters (extracted from the branch setup action)
  accept a revision-carrying manifest: exit 0.
- Old PATH step: no `--revision` probe; setup action never reads `.revision`.
- Producer's exact `jq -n` assembly program output accepted by unchanged filter.

## Expected CI (documented in the PR body)

Planning/Policy cannot go green pre-merge by construction: the pin resolves to
a new closure whose product the main push builds AFTER merge (this PR's guard
closes the PR-914 branch-dispatch hatch). Planning fails with the
self-describing `no runtime product ... builds it after merge` error; policy
lacks its candidate artifact. Landing needs review + override; then the main
push publishes the first revision-carrying product and the consumer-half PR
(pin bump to the merge commit) goes green on the candidate path.
