# VERDICT: CERTIFIED — a2-srcid (G1+G10)

Branch `feat/a2-source-identity` @ `65beb2edec4b186a1ba86052fd6a8429a6acb16c`,
base `origin/docs/bastion-final-plan` @ `38ffbfd7`. Diff is exactly one commit,
9 files (3 Rust sources + 1 action source + 5 regen outputs). No merges, no pushes.

## G1 — manifest revision end-to-end (verified in checked-in bytes)

- Producer: manifest program carries `revision: $revision`, assembly passes
  `--arg revision "$HEAD_SHA"`, assembled manifest re-checked vs `$HEAD_SHA`;
  build prove step gates `--revision` vs `git rev-parse HEAD`; publish spot
  check gates `--revision` vs `$HEAD_SHA`; smoke test gates installed
  `--revision` vs downloaded manifest `.revision`.
- `MANIFEST_ACCEPT_FILTER` gains `(.revision | test("^[0-9a-f]{40}$"))`; the
  identical anchored clause is present in all 4 checked-in YAML surfaces
  (producer, setup action, ci-unit-rust + release Velnor provisioners).
- Both consumers probe `--revision` after the closure gate and require
  `reported_revision == manifest_revision`; digest-before-exec ordering kept
  (revision probe after digest comparison, asserted by extended test).
- `--revision` stamp is real: rebuilt binary reports
  `65beb2edec4b186a1ba86052fd6a8429a6acb16c` == worktree HEAD, 40-hex.

## G10 — no-latest + tag disjointness (verified)

- `rendered_surfaces_never_select_latest`: 9 surfaces, >1000 lines scanned, only
  the known `re-run the LATEST preview run instead` prose line allowed — passes.
- `runtime_product_tags_are_disjoint_from_release_tags`: anchored to rendered
  `tags: ["v*"]` + `v[0-9]*)` gate; structural second-byte proof + sampled both
  directions — passes. Independent grep over the 4 touched YAML files: no
  `latest` token.

## Gates re-run in scratch worktree /tmp/v-a2-srcid-wt (detached 65beb2ed)

- `velnor-workflow --plain --dry-run .`: exit 0, "Dry-run: 0 files would change"
- 8 targeted tests (3 new + 5 extended): 8 passed
- `cargo test -p velnor-workflow`: 489 lib + 2/6/5/9/33, 0 failed
- `cargo clippy -p velnor-workflow --all-targets`: 0 warnings, exit 0
- `cargo fmt -p velnor-workflow -- --check`: clean
- actionlint 1.7.12 on 3 touched workflows: exit 0, no findings

## Disproof attempts (all rejection paths hold)

Using the RENDERED filter text extracted from checked-in YAML (not the Rust const):
- good manifest ACCEPTed; missing revision REJECTed; malformed (`short`)
  REJECTed; uppercase 40-hex REJECTed; mismatched `--revision` vs manifest
  REJECTed by the gate; matching ACCEPTed.

## Notes

- Semantics (well-formedness, not equality with requested rev) matches the
  closure-sharing design and is documented in the const comment; both consumers
  bind binary-to-manifest instead. Accepted as the intended G1 reading.
- Worktree left at /tmp/v-a2-srcid-wt, clean; disproof script at
  /tmp/v-a2-srcid-disprove.sh (one self-inflicted quoting bug in its first
  revision, re-verified clean with anchored clause — functional jq checks
  passed throughout).
