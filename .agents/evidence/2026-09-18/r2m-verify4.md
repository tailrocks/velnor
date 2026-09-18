# R2m round-4 verification: tailrocks/velnor#930 @ 7cd66e78 (bridge-pin update)

## VERDICT: MERGE-OK

Policy is GREEN via the bridged s2 path + candidate exception (mechanism
proven line-by-line from logs below), 17/17 github-hosted legs pass, and
every remaining red is the excusable velnor-admission baseline with zero
skips and zero github failures. All three MERGE-OK criteria hold.

Caveat (predicted, not blocking): merging lands main in an all-legs-skip
red state until a follow-up pin-bump (proven by replay in §6). This is the
flip's own nature — the Policy report itself says "a generator change in
flight; bump [generator] revision after merge" — and main is mergeable
behind it exactly as #932/#933 were behind their environmental reds.

- Heads verified: `origin/feat/r2m-flip` =
  `7cd66e78c0068f8630f8b6ab272d39be78ad0fdb`;
  `origin/main` = `027f4753638ae8f3432ed3802e9a38a53a196504`
  (#933 merge; re-fetched at report time).
- Chain: 17d4867b → f7282e31 (merge of main 027f4753) → 7cd66e78
  (pin+regen). No other commits.
- Worktrees: `/tmp/r2m-v4` (7cd66e78, detached; regen + gates),
  `/tmp/r2m-402` (40206d9f, detached; pin-binary replay),
  `/tmp/r4-replay` (merge replay, scratch).
- Read-only except `/tmp`; nothing pushed, merged, or amended.

## 1. UPDATE INTEGRITY — PASS

- Ancestry: `merge-base --is-ancestor 027f4753 7cd66e78` → YES.
- `gh pr view 930`: `mergeable: MERGEABLE` (mergeState BLOCKED = red
  environmental checks, not conflicts), head = 7cd66e78.
- Pin: `.github-gen/velnor-workflow.toml:9` =
  `revision = "027f4753638ae8f3432ed3802e9a38a53a196504"`,
  byte-identical to `git rev-parse origin/main` (full SHA, exact).
- Product for 027f4753 triple-linked to
  `velnor-workflow-runtime-v1-64fdd0bf7a8a5800` (closure, not commit):
  1. publisher run https://github.com/tailrocks/velnor/actions/runs/35234663531
     (headSha 027f4753, success) resolved
     `TAG: velnor-workflow-runtime-v1-64fdd0bf7a8a5800` from 027f4753's
     tree, found the release, and SKIPPED Build + Publish
     (`Resolve runtime closure` success, downstream skipped);
  2. locally recomputed closure over 027f4753 (`git ls-tree -r` over the
     six `CLOSURE_PATHS` + `closure-version:1/features:/profile:release`
     footer) = `64fdd0bf7a8a5800e014bb303e983c5fbf5c460b27cbaa9eff550482d7ef22a0`
     — matches the tag prefix AND release `manifest.json` `.closure`
     byte for byte (manifest `.revision = 40206d9f…`, profile release,
     features empty);
  3. zero diff on all closure paths 40206d9f→027f4753 (#933 touched only
     the pin literal + renders), so 027f4753's closure == 40206d9f's by
     construction; same digest recomputed over 40206d9f.
- Merge f7282e31: parents exactly 17d4867b + 027f4753. One file
  conflicted (generator-state); resolution = `--theirs`
  (byte-identical to main's copy: `git diff 027f4753 f7282e31 -- <state>`
  empty; `git diff 17d4867b f7282e31` touches ONLY that file). Full
  replay (`git merge --no-commit` + theirs + `git write-tree`)
  reproduces the recorded tree `73e46549…` exactly → zero hand edits.
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on 7cd66e78
  and on f7282e31; PR `DCO` check `pass`.

## 2. REGEN — PASS

Fresh worktree @7cd66e78: `cargo build -p velnor-workflow` ok;
`--plain --force` → exit 0, "Generated 21 files", `git status` clean;
`--plain --dry-run` → "Dry-run: 0 files would change", exit 0.

Diff 17d4867b→7cd66e78 (13 files, 94+/94-) fully classified, 188/188:
- pin repoint 40206d9f→027f4753: 164 non-state lines, 82 minus / 82 plus,
  every minus carries the OLD pin, every plus the NEW pin, zero
  cross-direction (toml pin, rev/POLICY_REVISION/EXPECTED_REVISION/
  artifact names);
- generator-state digests: 24 lines (config + 11 regen'd outputs);
- `crates/` delta: empty. The #933 merge contributed only the state
  file, which regen then overwrote — net is pin repoint + regen only.
- 164 + 24 = 188. Zero unclassified.

Schema-2 markers intact: `schema = 2`, `trust = "trusted-only"`,
`providers/automatic_providers/default_dispatch_providers` keys, 11/11
`SELECTION_UNITS: ${{ inputs.selected_unit_ids }}` (zero JSON-channel
remains), 35+35 caller `selected_unit_ids: ${{ needs.plan.outputs.unit_ids }}`
passthroughs, plan `unit_ids` outputs (ci-pr.yml:46, ci-main.yml:47).

## 3. GATES — PASS (all rerun in /tmp/r2m-v4, all green)

- `cargo test -p velnor-workflow`: **1709 passed, 0 failed** (identical
  to round 3; no source changes since).
- `cargo test -p velnor-runner`: **2209 passed, 0 failed, 4 ignored**.
- Contract suite (manifest-path; not a workspace member): **6/6** (2+4).
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean.
- `cargo fmt --check`: clean. `actionlint`: clean.

## 4. CI — PASS (Policy GREEN; remainder environmental-only)

Runs at head 7cd66e78, both terminal, no wait needed: CI/PR
https://github.com/tailrocks/velnor/actions/runs/35235701293
(74 jobs: 18 success, 7 failure, 49 skipped) and Policy
https://github.com/tailrocks/velnor/actions/runs/35235698059
(1 job: success). `gh pr checks`: Policy `pass` (10m0s), DCO `pass`,
7 fails = the environmental set below, rest pass/skip.

(a) POLICY SUCCESS — mechanism proven from the log
(`/tmp/r4-policy.log`, job 105250899487):
- Base is the bridge: setup checks out the base action at `027f475`
  (#933), resolves `CLOSURE:
  64fdd0bf7a8a5800e014bb303e983c5fbf5c460b27cbaa9eff550482d7ef22a0`
  (matches §1 recompute + manifest), verifies manifest + binary
  self-report.
- Bridge dispatch engaged: Acquire's `--plain --check` with the base
  binary renders the schema-2 tree WITHOUT the round-3
  `unknown field 'trust'` hard error (the s1 parser at 40206d9f still
  has `deny_unknown_fields` + `CONFIG_SCHEMA = 1` + `DeclaredTree::read`
  first — verified in source — so a reached check/render proves
  `s2::dispatch::run_if_s2` routed to the s2 pipeline via its raw-TOML
  `schema == 2` peek). The check reports:
  `error: generated files differ: .github/actionlint.yaml,
  .github/ci/project.toml, .github/workflows/ci-main.yml,
  ci-pr.yml, ci-unit-{bun,docker,docs,opentofu,rust}.yml,
  release.yml, .github-actions-generator-state` (11 files — the flip's
  emitter delta), then:
  `pin 027f4753… shares the base closure but the tree differs from its
  render; falling through to the candidate path`.
- Candidate path taken and bound: PR run published exactly
  `velnor-workflow-candidate-ae2681738c338f05-Linux-X64` (40,475,283 B)
  + the pinned runtime artifact; the name matches local
  `closure --rev=7cd66e78 --candidate`
  (`ae2681738c338f052d2c3e4d677a9b9a694224126a6b1b9788cec6fbd3e759cb`)
  byte for byte (same digest as round 3 — no source changes since).
  Enforce runs with `VELNOR_WORKFLOW_PINNED_BINARY` +
  `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` set (acquire `exit 1`s on any
  digest/manifest/self-report mismatch, so reaching Enforce proves all
  three passed).
- Enforce report, 11/11 PASS, 0 failed:
  `pin-declared` (revision = 027f4753…),
  `pin-reachable` (ancestor of 7cd66e78),
  `pin-monotonic` (descends from base validator 40206d9f),
  `generated-tree`: "the tree matches the candidate render
  (ae2681738c338f05…59cb), not the render of velnor-workflow at
  027f4753…: a generator change in flight; bump [generator] revision
  after merge",
  plus pull-request-target, entrypoint-privileges, trusted-runners,
  action-pins, workflow-structure, required-checks.

(b) 17/17 github-hosted SUCCESS (run URL above): all four non-rust legs
(bun-velnor, docker, docs, opentofu "GitHub · hosted") + all 13 rust
legs (incl. velnor-workflow, velnor-runner, contract, policy,
production-topology). 18th success = Planning.

(c) Velnor-side + prepare-cargo ran-and-failed-environmentally (NOT
skips): all 5 carry conclusion `failure` with
`##[group]Velnor rejected job (operational_store)` /
`operational store rejected the sanitized admission row; job failed
closed before execution` (jobs 105251002540/105251002829/105251002906/
105251004396 + prepare-cargo 105251004165, all within 14:45:39–52Z).
Same pre-bastion condition as every prior round.

(d) ci-required + Control/Required — environmental-baseline (excusable):
- ci-required fails fail-closed and correctly:
  `verdict binds plan digest 8563e4d2f58b08d6` (same digest as round 3),
  `expected CI job velnor-bun-velnor did not pass: failure`. Trigger is
  the admission failure, not a skip and not a PR defect. Zero
  `was skipped`/`was cancelled` lines.
- Control/Required is a pure mirror (`exit 1` only).
- Baseline: this PR's red set (4 velnor + prepare-cargo + ci-required +
  Control/Required; Policy + 17 github + DCO green) is now IDENTICAL to
  the set #932 merged with — the round-3 delta (Policy red) is gone.
- Zero skips among expected results, zero github-hosted failures.

## 5. POST-MERGE PREDICTION (proven by replay, not inferred)

Post-merge tree == 7cd66e78's tree (027f4753 is already an ancestor, so
the merge is a fast-forward or a `--no-ff` with an identical tree):
pin 027f4753, schema 2, flip sources.

- In-CI Policy: PIN-CHECK FAIL → CANDIDATE-TIMEOUT → red. Merged
  ci-main.yml: setup rev = BASE_PIN = 027f4753 (lines 66/154/162), so
  pin_closure == base_closure (both 64fdd0bf) and the `--check` fast
  path applies. Replay with the locally built 40206d9f binary over
  /tmp/r2m-v4: **exit 1**, `generated files differ:` + the SAME 11
  files as the live Policy Acquire log — the flip's emitter changes are
  in neither pin. Check fails → candidate path → polls
  `actions/workflows/ci-pr.yml/runs?head_sha=$HEAD_SHA&event=pull_request`
  (ci-main.yml:194) for the push SHA → no PR runs on push → 15-min
  timeout (line 214–216) → in-CI Policy FAILS.
- Callers: SKIP. All 35 `needs.policy.result == 'success'` gates in
  ci-main.yml (PR callers have 0 — why PR legs execute) + prepare-cargo
  skip on the red policy job.
- ci-required: FAILS on the policy prerequisite; Control/Required
  mirrors. Net: Planning green; Policy, ci-required, Control/Required
  red; every unit leg skipped.
- Delta vs current main: main @027f4753 TODAY is Policy-green with
  executing legs (CI/Main run
  https://github.com/tailrocks/velnor/actions/runs/35234664102: 19
  success = Policy + Planning + 17 github, 7 environmental failures, 50
  skipped — #933 fixed the stale pin). The merge REGRESSES this shape
  to all-skip red until the follow-up pin-bump PR (pin = merge HEAD),
  whose `--check` will match and go green (its standalone Policy runs a
  bridge validator that dispatches schema-2 trees).
- Rollback path (unchanged): single `git revert` of the merge restores
  the s1 config/tree and the old pin cleanly.

## Per-item scorecard

1. UPDATE INTEGRITY — PASS (ancestor ✓, MERGEABLE ✓, pin exact ✓,
   product triple-linked to 027f4753's closure ✓, merge = mechanical +
   theirs, tree replayed ✓, DCO ✓).
2. REGEN — PASS (force → 21 files clean; dry-run 0; 188/188 lines
   pin-only; schema-2 markers intact).
3. GATES — PASS (1709 + 2209 + 6, clippy/fmt/actionlint clean).
4. CI — PASS: Policy SUCCESS via bridge s2 dispatch + bound candidate
   ae2681738c338f05 (all 11 rules, log lines cited); 17/17 github
   green; velnor/admission + rollups = excusable environmental baseline
   (same set #932 merged with); zero skips, zero github fails.
5. POST-MERGE — main all-skip red (pin-check exit 1 replayed, 11 files;
   head_sha-keyed candidate wait must time out on push); recovery = one
   follow-up pin-bump; rollback clean.

**VERDICT: MERGE-OK** — the HOLD-3 skew is resolved by the bridge (#933
put a schema-2-dispatching validator on base; the flip's candidate
renders the tree byte-identically). Merge, then immediately land the
pin-bump follow-up to restore green main. Do not merge anything else
between the two without re-verifying the pin.
