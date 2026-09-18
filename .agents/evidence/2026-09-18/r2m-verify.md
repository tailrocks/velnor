# R2m verification: tailrocks/velnor#930 (feat/r2m-flip)

## VERDICT: HOLD — do not merge

Merge would turn `main` red: every unit job in the flipped tree skips (all 35 CI/PR
unit checks `skipped`), `ci-required` fails fail-closed, and Policy fails. Root cause
is a generator bug adopted by the flip, not environmental flake. Details in §5.

- Heads verified: `origin/feat/r2m-flip` = `11b5c1cb1d75222bcbbd2d0c6ab571277b38f93c`
  (3 commits on `d8138ef0`); `origin/main` = `d8138ef0572d6abfbece8f8904f36692fedcfb51`.
- Worktrees: `/tmp/r2m-pre` (d8138ef0, detached), `/tmp/r2m-post` (11b5c1cb, detached).
- Read-only except `/tmp`; nothing pushed, merged, or amended.

## 1. PIN ATOMICITY — PASS

- Pin line (`/tmp/r2m-post/.github-gen/velnor-workflow.toml:9`):
  `revision = "d8138ef0572d6abfbece8f8904f36692fedcfb51"` — full SHA of `origin/main`
  (`git rev-parse origin/main` identical).
- Product `velnor-workflow-runtime-v1-a3dc1e44d51b3cb2` published FROM d8138ef0
  (four independent links, not tag timing):
  1. Tag object is commit `d8138ef0` (`git cat-file -t` → `commit`).
  2. Release `targetCommitish` = `d8138ef0572d6abfbece8f8904f36692fedcfb51`.
  3. Release `manifest.json`: `revision` = d8138ef0…, `closure` = `a3dc1e44…4966`
     (prefix = tag suffix), `profile` = `release`, and all three product
     `binary_sha256` values equal the release asset digests byte-for-byte
     (Linux-X64 `16040044…`, Linux-ARM64 `84f20f13…`, macOS-ARM64 `fb274c7d…`).
  4. Publisher run `35212221609` (`Velnor workflow runtime products`, `push`):
     `headSha` = d8138ef0, conclusion `success`, all 5 jobs success including
     `Publish runtime products`; created 10:46:22Z, release published 10:49:27Z.
- ONLY pin change: flip commit `98868635` touches exactly 1 file
  (`.github-gen/velnor-workflow.toml`); the pin line is the sole pin *source*
  (setup action takes `rev` as input, no default; generator-state carries no pin).
  All other `d8138ef0` strings in the tree are regen-derived — proven by the
  deterministic regen in §3 (`--force` → clean).
- Freshness: `merge-base(feat/r2m-flip, main)` = `origin/main` = d8138ef0,
  re-fetched at report time. No HOLD-stale.
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on all 3 commits;
  PR `DCO` check `pass`.

## 2. FLIP COMPLETENESS — PASS (static; runtime bug scored in §5)

Tried to disprove; the static tree holds up. All PR claims below re-verified
independently (od-level where quoting mattered).

- Files: pre and post `.github/workflows/` identical (14 entries: `AGENTS.md` +
  13 `.yml`). Zero removed workflows/files → the "spec §2 citation" clause is not
  triggered. (Note: no R2m spec doc exists in the repo — searched `plans/` and the
  whole tree for `R2m`; the PR body is the operative spec. With zero removals,
  no quote is required.)
- Units: `project.toml` unit ids 17/17 identical pre/post. Per-unit jobs in
  `ci-pr`/`ci-main`/`release`: 17 github-side + 17 velnor-side each, both eras.
  Job-set diff = pure renames (`github-*`→`github-hosted-*`,
  `verify-github`→`verify-github-hosted`, `admit-runner`→`admit-provider`) plus
  exactly one deletion: `velnor-lane-admission` in `ci-pr` and `ci-main`
  (151→149 total). Deletion is observability-only: zero `needs:`/name references
  remain post-flip; machine telemetry preserved (`report-velnor-ci-outcomes`
  usages 10=10, action file byte-identical; `VELNOR_CI_REPORT` strings live there).
  `Velnor runner identity` summary steps: 6 files → 0 (disclosed delta 3).
- No new hard skips: zero added `if: false`; zero `&& false` pre and post;
  `== 'skipped'` 27=27 (PR says "28→28" under a slightly different count; stable
  either way); matrices 9 blocks = 9 blocks, sole delta is a key rename
  (`lane: github`→`provider: github-hosted`, same 2 entries/runners) in one release
  matrix. No shrunk matrices.
- Commands (correct comparison: s1 splits `github_*`/`velnor_*` lanes, s2 unifies):
  16/17 units byte-identical on both lanes and both scopes. Docker: PR commands ==
  pre github-lane (velnor lane converges to the cached+secret form); full commands
  move to the buildx+GHA-cache path with same Dockerfile/tag/context — exactly as
  disclosed. Post `ci-unit-docker` exports `GITHUB_TOKEN` on both lanes (pre: 1).
- Required checks fail-closed per provider admission, verified in `ci-required`
  script: admitted+skipped → fail, unadmitted+ran → fail, unselected+success →
  fail (0 → 70 `outside the expected set` guards: strictly stricter than s1).
  Fork/Bot coherence: `PROVIDER_ADMITTED_*_TRUSTED` envs carry the fork exclusion,
  so docker skips coherently on both providers on fork PRs.
- Caches: `mbx`/`bundle`/`docker_seed` keys gain
  `${{ inputs.provider }}-${{ inputs.unit_platform }}-${{ inputs.unit_trust }}`;
  compat digests rotate (`8a4ee3c72d39`→`743cc362f16c` etc.); `rustup`/`cargo_bin`/
  `mold` keys byte-identical; hashFiles lists and paths unchanged. One cold-cache
  cycle is inherent. `actionlint.yaml`: 1-line diff (drops dead `velnor-host-docker`).
- The 4 disclosed s2-vs-s1 deltas (trust routing now provider-atomic, fork/Bot
  docker skip on both lanes, notice-job/identity-step removal, nightly
  `runner`→`providers` input) were each confirmed present exactly as described,
  with justification in the PR body (committed s2 design + coherent required
  checks). The fork-skip delta is a real coverage reduction on fork PRs (docker
  unverified pre-merge there); accepted as disclosed design, flagged for reviewers.
- Brief discrepancies (no verdict impact): the brief mentions "3-provider fanout"
  and "operational_store leaves" — the actual tree has 2 providers
  (`github-hosted`, `velnor`; no `github-self-hosted` jobs exist) and no
  `operational_store` jobs at all.

## 3. SOURCE-FIX LEGITIMACY — PASS

Commit `24d1df91` touches exactly 7 files, all
`crates/velnor-workflow/src/**` or `crates/velnor-workflow/tests/**`. No config,
no generated files (the "+2 config lines" allowance is unused). Each change is
required by the flip:

- `s2/primitives/ir.rs` — (a) report inputs `ci_provider: <provider>` →
  `ci_lane: <lane>` (`github-hosted`/`github-self-hosted`→`github`,
  `velnor`→`velnor`): without it the s2 tree passes an undefined input to the
  report action (actionlint fails, telemetry-schema enum violated). (b) new
  `docker_build_token_env_for_members` + threading: s2 unifies docker commands
  across providers, so every provider's checks step must export `GITHUB_TOKEN`
  for the `--secret id=github_token` build. Both flip-required; both tested.
- `s2/primitives/mod.rs` + `s2/primitives/release.rs` — re-export/thread the
  token helper into kind reusables and release verification (release docker
  checks run secret-passing commands). Flip-required; tested
  (`release_docker_verification_exports_the_build_token`, negative on rust kind).
- `s2/mod.rs` — (a) `DOCKER_BUILD_GITHUB_TOKEN_SECRET` seeding into PR/full
  seed commands + cache-export build (matches s1/release behavior on every
  provider; idempotence tested). (b) actionlint allowlist universe-scoping
  (`selectors.values()` → universe providers only): without it the flip would
  adopt a weakened lint contract admitting labels that can never reach a
  `runs-on` (tested: `bastion-scale-set` absent). Preventive but flip-load-bearing:
  the regen output depends on it. (c) test migration: s2 renders the dogfood
  tree byte-for-byte now (`checked_in_workflows_match_the_generator_byte_for_byte`
  moves to s2); s1-refusal re-subjected to synthetic s1 config. Flip-required.
- `lib.rs` — s1 surface-wide tests re-subjected to synthetic s1 subjects
  (fixture renders, cloned restricted units, release-surface repo, microvm repo);
  s1 byte-for-byte test becomes `schema1_pipeline_refuses_the_schema2_repository`
  (fail-closed mirror). Two adjusted guards are subject-driven, not weakened:
  `hosted_jobs > 1`→`>= 1` (fixture renders fewer jobs; the budget validator still
  asserts every job) and `preview#identity`→`preview#build` (fixture's preview
  surface; the full-history validator still scans all files). No invariant deleted.
- `tests/lane_pairing.rs`, `tests/velnor_first_ci.rs` — placement transplants
  read the repo's `[workflow.selectors.velnor] runs_on` (s1 keys no longer exist
  in repo config); `pull_request_on_velnor = true` hardcoded in one helper,
  matching `automatic_providers` behavior. Flip-required.
- No hand edits to generated files: `./target/debug/velnor-workflow --plain
  --force` (built from `/tmp/r2m-post`) → exit 0, "Generated 21 files",
  `git status` clean; `--plain --dry-run` → "Dry-run: 0 files would change",
  exit 0.
- Regen-commit fixture (`pre_parameterization_cache_keys.rs`, 228 lines):
  uniform renames + provider-platform-trust segments + digest rotations only;
  hashFiles lists/paths unchanged. Legitimate re-capture with migration comment.

## 4. GATES — PASS (all rerun in /tmp/r2m-post, all green)

- `cargo test -p velnor-workflow`: **1705 passed, 0 failed** (lib 1588 + 15
  integration binaries totalling 117 + 0 doc-tests). Matches PR's "1588 lib".
- `cargo test -p velnor-runner`: **2209 passed, 0 failed, 4 ignored**. Matches PR.
- Contract suite from `crates/velnor-workflow-contract`: **6/6** (2 + 4).
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean (exit 0).
- `cargo fmt --check`: clean. `actionlint -config-file .github/actionlint.yaml`: clean.

## 5. CI — FAIL (conclusive; nothing left in flight)

Terminal state (re-checked at report time; both runs `failure`, head `11b5c1cb`):
checks = **3 fail** (`Policy`, `ci-required`, `Control / Required`), **2 pass**
(`Control / Planning`, `DCO`), **35 skipping** (every unit/provider job).

- Policy (run `35216809916`, 47s): **took the candidate path** with candidate
  `velnor-workflow-candidate-f7400bffdc7afc50-Linux-X64` (head closure
  `f7400bffdc7afc50…`, `BASE_PIN` = a6fa8d4a…) — then failed:
  `no same-repository PR run published candidate …`. The sibling CI/PR run
  published exactly 1 artifact (the pinned-runtime stage from Planning); **zero
  candidate artifacts**, because all 35 unit jobs skipped before stage-1 packaging.
- `ci-required` failed fail-closed and *correctly*: `expected CI job
  github-hosted-bun-velnor was skipped: a skipped expected result cannot pass`
  (first of the expected-set violations). This also proves `plan.outputs.units`
  propagated non-empty (else nothing would be "expected").
- Fail set is NOT environmental — it is schema-2-shaped. Root cause (proven):
  the s2 caller condition over-escapes the plan-JSON needle. Ground truth via
  `od -c`:
  - caller (`ci-pr`/`ci-main` unit jobs): `contains(needs.plan.outputs.units,
    '\"unit_id\":\"docker\"')` — needle contains literal backslashes;
  - callee (`ci-unit-*`): `contains(inputs.selected_units, '"unit_id":"…"')` —
    bare quotes, matching the plan output `[{"unit_id":"docker",…}]` (verified
    in the Planning log: all 17 units × both providers present).
  - GitHub evaluated the caller's `contains()` FALSE for all 17 units × both
    providers (every other conjunct provably true: `always()`, plan `success`,
    non-dispatch event, non-fork/non-Bot). The s1 equivalent was quote-free CSV
    matching and worked.
  - Emitter: `aggregate_selected_unit_selector` at
    `crates/velnor-workflow/src/s2/primitives/ir.rs:2787`, wrong since the R2
    bridge commit `eb0303a3` — hence also wrong in the pinned generator at
    d8138ef0. The callee twin (`reusable_selected_unit_selector`, ir.rs:3309)
    is correct. No test covers the caller needle against expression semantics;
    string-presence assertions cannot catch this class (structural gap).
  - Scope: only `ci-pr.yml` + `ci-main.yml` carry the broken needle. Release
    uses event/ref-based selection (unaffected); nightly dispatches `ci-main`
    (affected transitively).
- New job names (`github-hosted-*`) exist but SKIP — they do not pass. Zero
  verification jobs executed on either provider, so everything downstream
  (callees, end-to-end candidate flow, release, nightly) is unexercised.
- No 35-minute wait was needed: CI is terminally red, not pending.

## 6. POST-MERGE PREDICTION + ROLLBACK

- If merged as-is, `main` goes red on the next `ci-main` (push/schedule/nightly):
  all 34 unit jobs skip via the same broken needle, `ci-required` fails
  fail-closed. Release selection would still gate on events/refs, but no release
  verification could be trusted while the aggregate suites cannot run. Policy on
  `main` (pin-regen comparison) would likely pass on determinism — irrelevant
  against a red required check.
- After a correct fix (see below), expected steady state: Policy green via the
  pinned path (post-merge pin-bump follows the normal flow); `ci-required` green;
  17 `github-hosted-*` + 17 `velnor-*` jobs per aggregate; one cold-cache cycle
  from the rotated provider-platform-trust keys.
- Rollback path: single `git revert` of the merge (or of the 3 commits) restores
  the schema-1 config, the s1 tree, and pin `a6fa8d4a…` cleanly — all flip state
  is inside the PR; no external state migrates (generator-state file reverts too).
  Cost of a merge-then-revert: two cold-cache cycles (s2 keys, then s1 keys again).
- Required fix (suggested, not authored): emit the caller needle with bare quotes
  exactly like `reusable_selected_unit_selector` (ir.rs:3309); add a regression
  test pinning caller/callee needle consistency (ideally one that evaluates the
  `contains()` under GitHub expression semantics, closing the structural gap);
  regen; re-run CI to green. Note the pinned generator at d8138ef0 carries the
  same bug, so the follow-up pin-bump must point at a commit containing the fix.

## Per-item scorecard

1. PIN ATOMICITY — PASS. 2. FLIP COMPLETENESS — PASS (static). 3. SOURCE-FIX
   LEGITIMACY — PASS. 4. GATES — PASS. 5. CI — FAIL (blocking). 6. predicted
   main red; rollback clean.

**VERDICT: HOLD** — items 1–4 pass, but item 5 fails on a flip-adopted generator
bug (`aggregate_selected_unit_selector` over-escaping → universal unit skip).
Do not merge until the needle is fixed, the tree regenerated, and CI is green
with the renamed jobs executing (not merely existing).
