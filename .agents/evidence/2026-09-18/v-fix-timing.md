# VERDICT: CERTIFIED — fix-timing (perf/main-docker-gha-cache @ 511ffd7e)

Scope: validation only. No edits, no merges, no pushes. All proof runs in scratch worktree `/tmp/v-fix-timing-wt` (detached HEAD 511ffd7e, clean).

## Identity
- `origin/perf/main-docker-gha-cache` == `511ffd7e8a20bbce85eb6cba1fcb6b863514d8fa` (fetched from origin this session).
- Single commit on parent `38ffbfd7` ("docs(bastion): fix markdownlint errors"), which is an ancestor of `origin/docs/bastion-final-plan`.
- Commit touches exactly 5 files: `crates/velnor-workflow/src/lib.rs`, `.github/ci/project.toml`, `.github/workflows/ci-main.yml`, `.github/workflows/ci-pr.yml`, `.github/ci/.github-actions-generator-state`. Matches `/tmp/fix-timing.md` claim.

## Diff inspection (511ffd7e^..511ffd7e)
- New `docker_hosted_full_command(command, unit_id)` in `materialize_capability_commands` docker mutable-mount-seed branch: `docker build` → `docker buildx build --load`, appends seed `--build-context` + `--cache-from/--cache-to type=gha,scope={unit_id},mode=max`, idempotent (skipped when present). Mirrors `docker_hosted_pull_request_command` minus `--target ci`. ✓
- `github_full` loop maps every docker build command through it; `velnor-cache-export` second command appended after, byte-unchanged (verified: second array element identical before/after; all other units' `github_full_commands` lines identical; `velnor_*` lanes untouched). ✓
- Regen output matches claim exactly: first `github_full_commands` entry now buildx + both `scope=docker` flags, no `--target ci`; yml diffs are only `seed_compat 9c97194a807b → 4ad4d42d64a2`; generator-state digest updates only. ✓
- New test `tests::hosted_docker_full_build_reuses_the_pull_request_gha_layer_cache` asserts: 2 hosted full commands, first is buildx + both scope=docker flags + no `--target ci`, second is flag-less `velnor-cache-export`. ✓
- NO cargo/source-build fallback: full-commit grep for cargo/fallback hits only pre-existing context lines (watch list, `seed_dependency_files`, unchanged `mise run` context line). No workflow-interface or cache-key changes. ✓

## Proof runs (worktree @ 511ffd7e)
- `cargo test -p velnor-workflow --lib hosted_docker_full_build_reuses_the_pull_request_gha_layer_cache`: **1 passed** (486 filtered → 487 lib tests). ✓
- `cargo test -p velnor-workflow`: **all ok** — lib 487 passed/0 failed; integration 2+6+5+9+33 passed; doc 0. ✓
- `cargo run -p velnor-workflow -- generate . --plain --dry-run`: **"Dry-run: 0 files would change"**, exit 0. ✓
- `cargo clippy -p velnor-workflow --all-targets`: **0 warnings, 0 errors**. ✓
- `cargo fmt -p velnor-workflow -- --check`: **clean**. ✓
- `actionlint` (1.7.12): **exit 0**. ✓

## Note (info, not a mismatch)
`origin/docs/bastion-final-plan` tip (`96ccc0f1`) has moved past the fix's parent via 2 merges (`fix/pin-fetch-in-tool`, `fix/preview-guest-upload`). A later merge may need regen reconciliation on shared generated files (e.g. generator-state); content of this fix itself is unaffected and certified as-is.
