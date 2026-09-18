# R2 verification: PR tailrocks/velnor#924 (branch fix/d2-bridge-r2)

- Verifier role: independent, read-only (writes only under /tmp). No push/merge/amend.
- Branch HEAD: `73356c2c9aab32f355f069d58b1b9ff581d1c1f5` (2 commits over main)
- Main HEAD: `a2840748c588ec1dcb2751aa190ab4a74e1614c5`
- Worktrees: `/tmp/r2-verify-wt` (branch), `/tmp/r2-main-wt` (main), both detached.
- CI run examined: `35203060271` (completed during verification).

## 1. BASELINE — PASS

Failing checks on #924 (7, sorted):
```
Bun · Bun package (velnor) / Velnor
ci-required
Control / Prepare Cargo / prepare-cargo
Control / Required
Docker · Docker / Velnor
Documentation · Documentation / Velnor
OpenTofu · OpenTofu / Velnor
```
`diff` of sorted fail-name lists #922 vs #924: **empty (FAIL-SETS-IDENTICAL)**.
The 2 extra names beyond the 5 named in the task (`Control / Required`,
`ci-required`) are aggregate rollups that also fail on merged #922 — pre-existing,
not new.

Rejection reason (`gh run view 35203060271 --log-failed`):
`Velnor rejected job (operational_store)` appears **5×** — once per leaf job
(Bun/Velnor, Docker/Velnor, Documentation/Velnor, OpenTofu/Velnor,
Control/Prepare-Cargo/prepare-cargo). Sample:
`##[group]Velnor rejected job (operational_store)` / `phase: operational_store` /
`reason: operational store rejected the sanitized admission row; job failed closed before execution`.

Completion wait: at first poll `Rust · velnor-runner / GitHub` was `pending`;
it completed **pass (5m49s)** during verification. Final tally: 7 fail / 20 pass /
42 skipping / 0 pending. Every `/GitHub`-variant check passes
(awk for non-pass `/GitHub` rows returned empty).

## 2. PIN — PASS

- Branch `.github-gen/velnor-workflow.toml:9`: `revision = "a6fa8d4a5096e6df37250b59a8105abfebdb9013"`
- Main `.github-gen/velnor-workflow.toml:9`:    `revision = "a6fa8d4a5096e6df37250b59a8105abfebdb9013"`
- `cmp` branch-vs-main file: **IDENTICAL** (whole file, not just the pin line).
- `merge-base(branch, origin/main)` = `a2840748...` = `origin/main HEAD`. ✔

## 3. SCHEMA-1 PRESERVATION — PASS

In `/tmp/r2-verify-wt`: `cargo build -p velnor-workflow` ok, then
`./target/debug/velnor-workflow --plain --force` → exit 0, "Generated 21 files".
- `git status --porcelain` after regen: **clean** (zero modifications).
- `diff -r .github/workflows` (branch worktree vs main worktree): **identical**.
- Full `diff -r .github` branch-vs-main: only difference is the expected
  generator-state fingerprint line (`scan fc874ae71dabcd22` → `scan 33f5782652175e4f`)
  from the branch's regen commit. No workflow content drift.

## 4. DISPATCH — PASS (with crate-name correction)

Task text names `crates/velnor-runner`, but the PR touches **zero** files there
(`git diff --name-only ... -- crates/velnor-runner` empty) and that crate
contains **no** `run_from_env`/`RunnerMode` (grep empty). The actual bridge is in
`crates/velnor-workflow`:
- `src/lib.rs::run_from_env`: diff vs main is a pure 5-line prepend —
  `if let Some(result) = s2::dispatch::run_if_s2() { return result; }`.
  The schema-1 path below is byte-unchanged (verbatim fall-through). ✔
- `src/s2/dispatch.rs::run_if_s2`: routes to the s2 provider pipeline
  (`super::run_from_env`) iff `--providers[=…]` flag present OR the local target's
  `.github-gen/velnor-workflow.toml` declares `schema = 2`; remote targets,
  unparsable CLI, missing/unparsable config, `version`/`closure` stay schema-1.
  Fail-closed both ways (destination pipeline's strict schema gate rejects
  foreign schemas; `s2::tests::state_schema_one_is_rejected_with_the_schema_move` passes).

Tests (run in branch worktree):
- `cargo test -p velnor-workflow --lib s2::dispatch`: **7/7 pass**
  (covers schema-2 routing ×3 incl. policy/flag variants, schema-1 stay ×4).
- `cargo test -p velnor-workflow --lib s2::`: **740/740 pass**, 0 failed.
- `cargo test -p velnor-runner` (as tasked): **all 14 suites ok, 0 failed**
  (2209 passed total, 4 ignored).

## 5. MIGRATION posture — PASS (expand-only)

`git diff origin/main..branch --stat`: 56 files, **+75813 / −3**.
The only edits to pre-existing files are additive/metadata:
1. `crates/velnor-workflow/src/lib.rs` (+6/−0): `mod s2;` + dispatch early-return.
2. `.github/ci/.github-actions-generator-state` (1-line scan-hash fingerprint update).
3. `tests/generic_surface_literals.rs`: admit `src/s2/estate.rs`, bump bare-slug
   occurrences 2→4 for the schema-2 fork.
All other changes are new files (`src/s2/**`, `tests/fixtures-s2/**`).
Nothing removed, no destructive step, schema-1 output byte-identical (§3).
Rollback path: revert the merge commit.

## 6. DCO — PASS

- `eb0303a3 feat(workflow): bridge provider-schema pipeline alongside schema-1 (R2)` —
  `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` ✔
- `73356c2c chore(ci): regen generator state for R2 bridge sources` —
  `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` ✔
PR `DCO` check: pass. (`%GS` empty = commits not GPG-signed; DCO requires the
trailer, not a signature — satisfied.)

## VERDICT: MERGE-OK

All six items pass. Failing CI checks are exactly the pre-existing environmental
set from #922; pin untouched; schema-1 output byte-identical; dispatch is a
purely additive bridge with both branches unit-tested; change is expand-only;
DCO trailers present. Not merged (out of scope for verifier).
