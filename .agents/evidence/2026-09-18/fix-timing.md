# A1 FIX evidence — main Docker full-build GHA cache

Branch: `perf/main-docker-gha-cache` (from `origin/docs/bastion-final-plan`)
Commit: `511ffd7e` (signed off, pushed to origin)
Baseline: `/tmp/a1-timing.md` (Docker/GitHub `Run unit checks` 12m56s = ~90% of main 14m24s wall)

## Change (generator only, no hand-edited YAML)

`crates/velnor-workflow/src/lib.rs`, `materialize_capability_commands` (docker
mutable-mount-seed branch):
- New `docker_hosted_full_command(command, unit_id)` mirroring
  `docker_hosted_pull_request_command` minus `--target ci`: converts
  `docker build` → `docker buildx build --load`, appends seed context +
  `--cache-from/--cache-to type=gha,scope={unit_id},mode=max` (idempotent,
  skipped when already present).
- The `github_full` loop now maps every docker build command through it; the
  `velnor-cache-export` second command is appended after, byte-unchanged.
- Removed the now-used `scope` from the `let _` tuple (then the tuple itself).

Regen via generator only: `cargo run -p velnor-workflow -- generate . --plain --force`.
Regenerated files (all generator-written):
- `.github/ci/project.toml`: first `github_full_commands` entry is now
  `docker buildx build --load --file 'Dockerfile' --tag local-ci:dockerfile '.' --build-context ... --cache-from type=gha,scope=docker,mode=max --cache-to type=gha,scope=docker,mode=max`;
  second (export) entry unchanged. `velnor_*` lanes untouched.
- `.github/workflows/ci-main.yml`, `ci-pr.yml`: only `seed_compat`
  `9c97194a807b` → `4ad4d42d64a2` (compat digest covers the command recipe, so
  one docker-seed generation goes cold; the scope=docker GHA layer cache PRs
  keep warm is independent of it, so the first main run still gets GHA hits).
- `.github/ci/.github-actions-generator-state`: digest updates.

No cargo/source-build fallback added. No workflow-interface or cache-key changes.

## Test

New `tests::hosted_docker_full_build_reuses_the_pull_request_gha_layer_cache`
(scans a temp Dockerfile repo + `mutable_mount_seed` generation row, asserts
2 hosted full commands: first is buildx + both scope=docker flags + no
`--target ci`; second is the flag-less `velnor-cache-export`).

## Verification (all in worktree, after regen)

- `cargo test -p velnor-workflow`: all suites ok (lib 487 passed, 0 failed;
  +2/+6/+5/+9 integration/doc suites ok). Note: the pre-regen run failed only
  on `checked_in_workflows_match_the_generator_byte_for_byte` (expected drift
  before regen); green after.
- `cargo clippy -p velnor-workflow --all-targets`: 0 errors, 0 warnings.
- `cargo fmt -p velnor-workflow -- --check`: clean.
- `actionlint`: exit 0.
- `generate . --plain --dry-run`: "Dry-run: 0 files would change".

## Deferred (pre-existing, out of scope)

Paired-arch (`Dockerfile.amd64`+`.arm64`) repos render a `case/esac` wrapper
around two `docker build` invocations; the seed/context flag appends (old and
new) attach at the end of the wrapper rather than inside each invocation.
Pre-existing shape, untouched by this fix; this repo has a single Dockerfile
so regen output is correct.
