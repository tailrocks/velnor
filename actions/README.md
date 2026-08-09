# actions/

Reusable building blocks for the Velnor Actions fleet.

This directory is **canonical**, **headless**, and **generated**: its content is
produced from the shared class model, not hand-authored. Do not hand-edit generated
building blocks — change the class model or the declared data and regenerate. Every
external Action reference is pinned to a full 40-hex commit SHA; mutable tags or
branches are never used.

## Building blocks

- `run-gate/` — executes exactly one named gate command (`install`, `build`,
  `test`, `lint`, or `format`). It is lane-neutral: the command is identical on
  the Velnor lane and the GitHub-hosted lane; only the host runner differs. It
  contains no lane-specific, Velnor-specific, or GitHub-hosted-specific logic.
- `aggregate/` — emits the lane contract (`contract=success`) only after every
  applicable gate in the lane has already succeeded. A lane whose gate failed
  never reaches this step, so its contract output stays empty and the lane fails.
  Skipped work never produces `contract=success`.
- `cache-contract/` — validates the versioned trusted cache declaration hash,
  authority scope, full-peak quota reservation, byte attribution, cleanup state,
  and immutable materialization identity. It performs no restore/save itself and
  emits success only after every runtime authority field passes.

The callable workflows in `.github/workflows/ci-<class>.yml` reference these
composites at the immutable `fleet/block-sha` commit.
