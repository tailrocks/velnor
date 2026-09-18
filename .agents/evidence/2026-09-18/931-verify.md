# PR 931 verification: fix s2 caller-needle over-escaping

## VERDICT: MERGE-OK

All 6 items PASS. The R2m-blocking bug from `/tmp/r2m-verify.md` §5 is fixed at its
root (single shared needle — drift structurally impossible), the regression test
provably catches the original bug, the PR is render-neutral, pin/base/DCO hold,
all gates rerun green, and CI shows zero new failures vs baseline (Policy SUCCESS).

- Heads verified: `origin/fix/r2-caller-needle-unescape` = `38995b3b71c5b7ecadf85da90c12afc529035799`
  (1 commit on `d8138ef0`); `origin/main` = `d8138ef0572d6abfbece8f8904f36692fedcfb51`.
- Worktrees: `/tmp/931-post` (38995b3b, detached), `/tmp/931-pre` (d8138ef0, detached).
- Read-only except `/tmp`; nothing pushed, merged, or amended. Worktree restored
  pristine after probe edits (`git status`/`git diff` empty at report time).

## 1. FIX CORRECTNESS — PASS

The PR adds `selected_unit_needle()` (`s2/primitives/ir.rs:2711`) emitting
`'"unit_id":"{unit_id}"'` and routes BOTH `aggregate_selected_unit_selector`
(caller, :2717) and `reusable_selected_unit_selector` (callee, :3242) through it.
Single source: exactly 1 definition + 2 call sites (grep-confirmed); no drift possible.
Call sites embed the selector verbatim (`.join(" || ")`, `conditions.push(...)`,
`format!("... && {} && ...")` — no escaping layer), so emitter bytes = rendered bytes.

Od-level proof (temp probe test, since removed; raw bytes in `/tmp/931-odprobe.txt`):
- caller: `contains(needs.plan.outputs.units, '"unit_id":"docker"')`
- callee: `contains(inputs.selected_units, '"unit_id":"docker"')`
- byte `0x5C` (backslash) present in NEITHER expression; needle bytes after the
  haystack split are byte-identical. `od -c` of the caller needle:
  `'` `"` `u n i t _ i d` `"` `:` `"` `d o c k e r` `"` `'` — bare `0x22` quotes.

Bug-class hunt (independent): all 479 `unit_id` lines in
`crates/velnor-workflow/{src,tests}` scanned for the over-escape signature
(double backslash in Rust source = literal `\` emitted): **0 found**.
Negative control on the pre-fix tree finds exactly the 3 known lines (caller
emitter `ir.rs:2677` + 2 test expectations), proving the hunt works.
Full emitted-`contains(` inventory checked, all clean:
- s1 caller/callee/generic (`primitives/ir.rs:2777,3290,3101`): CSV quote-free —
  never had the bug, untouched.
- s2 caller/callee (`s2/primitives/ir.rs:2719,3244`): via shared helper, bare.
- s2 dynamic-input form (`s2/primitives/ir.rs:4012`,
  `contains(inputs.selected_units, format('"unit_id":"{0}"', inputs.unit))`): bare
  quotes — correct; third spelling but no escaping defect (not unified into the
  helper since it interpolates `inputs.unit` at runtime rather than a static id).
- s2 provider-dispatch gates + release admit gates: CSV `',x,'` forms, quote-free.
- Validators/test parsers (`s2/policy.rs`, `s2/mod.rs`, `policy.rs`, `lib.rs`):
  all bare-quote or CSV; the two stale over-escaped test expectations were fixed.

## 2. REGRESSION TEST ADEQUACY — PASS (defeat attempted, test holds)

New test `caller_and_callee_selectors_share_one_plan_json_needle` (`ir.rs:105`):
- On the fix: **PASS** (`1 passed`, verified explicitly).
- With ONLY the caller body reverted to the original over-escaped spelling
  (byte-verified via `od -c` against pre-fix `ir.rs:2677`, helper + test kept):
  **FAIL**, exit 101, on the first assertion, with a diagnostic showing the
  escaped vs bare needles. Defeat attempt confirms the test bites.

It catches the bug CLASS, not just the string:
1. caller==callee after haystack normalization (catches ANY future drift),
2. `!aggregate.contains('\\')` (directly targets over-escaping),
3. emitted caller contains needle `'"unit_id":"docker"'`,
4. that needle is a literal substring of sample plan JSON under `contains()`
   semantics — closing the structural gap named in r2m-verify §5.
The semantic chain is sound: plan JSON is `serde_json::to_string` (compact, no
spaces) with `unit_id` as first key (`s2/runtime.rs:1591`), so the needle is
always a substring; `is_unit_id` restricts the alphabet so no escapable chars
can enter the needle. The sample JSON mirrors the real emission faithfully.
Defense in depth: 2 render-level tests updated — full `ci-pr.yml` render asserts
the bare caller needle (`s2/mod.rs:14301`), and caller/callee gates are now parsed
by ONE parser (`s2/mod.rs:9853`), pinning identical spelling in rendered output.

## 3. RENDER-NEUTRAL — PASS

- `.github/workflows` diff post-vs-base: EMPTY. PR touches exactly 2 files, both
  `crates/velnor-workflow/src/**`; zero changes outside `crates/` (no state,
  config, or generated files — fingerprint question moot).
- Built binary + `--plain --force` → "Generated 21 files", `git status` clean.
- `--plain --dry-run` → "0 files would change", exit 0.
- CI independently confirms: Policy (candidate path) reports the tree matches
  the candidate render byte-for-byte (see §6).

## 4. PIN+BASE — PASS

- Pin `a6fa8d4a5096e6df37250b59a8105abfebdb9013`
  (`.github-gen/velnor-workflow.toml:9`) byte-identical pre/post.
- `merge-base(branch, main)` = `origin/main` = `d8138ef0`, re-fetched at report
  time. No HOLD-stale.
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on the commit; PR
  `DCO` check SUCCESS.

## 5. GATES — PASS (all rerun in /tmp/931-post, all green)

- `cargo test -p velnor-workflow`: **1699 passed, 0 failed** (lib 1582 incl. the
  new test + 15 integration binaries totalling 117 + 0 doc-tests).
- `cargo test -p velnor-runner`: **2209 passed, 0 failed, 4 ignored**.
- Contract suite (`crates/velnor-workflow-contract`, own workspace): **6/6** (2+4).
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean (exit 0).
- `cargo fmt --check`: clean. `actionlint -config-file .github/actionlint.yaml`: clean.

## 6. CI — PASS (terminal; zero new failures vs baseline)

Terminal state, head `38995b3b`: **9 SUCCESS / 42 SKIPPED / 4 FAILURE** (55 checks,
nothing in flight; no 30-min wait needed).
- **Policy SUCCESS** (run `35219190324`): took the CANDIDATE path (correct for a
  generator-src PR — pin closure differs), built the candidate from the PR head,
  and reports `PASS generated-tree: the tree matches the candidate render
  (d08854ef…), not the render at a6fa8d4a…: a generator change in flight; bump
  [generator] revision after merge`. Pinned-path concern does not apply, and the
  candidate-render match is an independent render-neutrality proof.
- Every executed GitHub-lane job SUCCESS: Planning, Docker/GitHub, Rust
  velnor-control/model/runner/workflow + production-topology (GitHub), DCO.
- The 4 failures are a STRICT SUBSET of merged-PR #929's 7 failures (same-event
  baseline), all pre-existing infra signature, none PR-caused:
  - `Docker · Docker / Velnor` + `Control / Prepare Cargo / prepare-cargo`:
    `Velnor rejected this job before workflow execution — operational store
    rejected the sanitized admission row; job failed closed before execution`
    (identical on #929; fails in ~15s pre-execution).
  - `ci-required` + `Control / Required`: fail-closed consequences of the above
    (also failing on #929 and on main HEAD).
  - #929 additionally failed Bun/Docs/OpenTofu Velnor-lane jobs that this PR's
    plan selection did not schedule — hence fewer failures here (4 < 7).
- Main Policy-red judged as briefed: main HEAD Policy FAILURE (push-event run
  `35212222146`) is `no candidate product … was published within 15 minutes` —
  the pre-R2m push-event candidate-rendezvous gap (main-push unit jobs skip so no
  candidate artifacts arrive). Different event/path from this PR's pull_request
  Policy SUCCESS. No shared gap signature; PR introduces no Policy regression.
  (Post-merge, main-push Policy will stay red until the campaign fixes main-push
  candidate publishing — pre-existing, out of scope for this PR.)

## Per-item scorecard

1. FIX CORRECTNESS — PASS. 2. REGRESSION TEST — PASS. 3. RENDER-NEUTRAL — PASS.
4. PIN+BASE — PASS. 5. GATES — PASS. 6. CI — PASS.

**VERDICT: MERGE-OK** — fix correct at od level with drift structurally removed,
regression test provably catches the bug class, render-neutral, fresh base,
all gates green, Policy SUCCESS with no new CI failures vs baseline.
Post-merge: normal pin-bump flow applies (Policy already emits the reminder).
Note for the R2m follow-up: the pinned generator at `d8138ef0` still carries the
bug, so the R2m pin-bump must point at a commit containing this fix (as
anticipated in r2m-verify §5).
