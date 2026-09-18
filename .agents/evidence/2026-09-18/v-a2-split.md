# V-A2: independent review of PR #916 (producer-first split)

Date: 2026-09-17. Inputs: /tmp/a1-pr912-red.md, /tmp/a2-split.md.
Reviewer scratch worktree: /tmp/v-a2-worktree @ f4d46b3b (detached, PR head).
No repo edits made. Verdict: **CERTIFIED (safe to merge)**.

## Scope: PRODUCER-ONLY confirmed

- Base: merge-base HEAD..origin/main = 33688938 (PR #914 merge), as author claims.
- Commits: 51e635af (generator change + regen, 3 files) then f4d46b3b
  (pin bump + regen, 13 files, 94+/94- symmetric). Both Signed-off-by (DCO
  check SUCCESS on the PR).
- Only Rust file changed: crates/velnor-workflow/src/primitives/runtime_products.rs.
- Consumer sources byte-identical to base (git diff --quiet): both setup-action
  copies (.github/actions + .github-gen/sources), lib.rs, primitives/release.rs.
- MANIFEST_ACCEPT_FILTER const identical to base (line 74 -> 76, same string,
  no revision clause); producer evaluates it 2x (assemble + smoke test,
  count-asserted by test).
- Zero consumer accept-filter / --revision / provisioner changes: no
  `.revision` read or `--revision` probe anywhere in the setup action;
  `--source-ref` appears only 2x in the producer smoke test (count-asserted).
- All 11 non-producer workflow deltas: 0 non-pin lines (pure
  7341ef4b->51e635af substitution, verified per file).
- Producer render (ci-runtime-products.yml) contains exactly: default-branch
  guard as first closure step, --revision proofs in build + assemble +
  smoke test, `revision: $revision` in the manifest program, manifest
  read-back check, verify-before-create ordering (assemble < attest < smoke <
  single `gh release create`, no `gh release download`, no `skipped` output),
  never-overwrite re-check immediately before create, HEAD_SHA in assemble env.
- New tests: guard-first, guard accept/reject matrix executed in bash
  (main accept; trunk/tag/empty reject), ordering test, source-ref pin x2 +
  trunk-follow, shell-parse of all 8 step bodies, pinned-bytes digest updated.
  The HEAD_SHA env test removes the enabling condition of the 754c1cd6/65beb2ed
  composition bug class for this step.

## Old consumers ignore the new field (reviewer-owned proof, /tmp/v-a2-oldconsumer.sh)

- Base-33688938 setup action: no `.revision` read, no `--revision` probe;
  sole binary probe is `--closure` (line 177).
- Both old filters (download with asset check + cache-verify without),
  extracted textually from the base action, accept a manifest assembled with
  the branch's exact jq program (revision field present): 2 filters x 3
  platforms, all exit 0. ALL PASS.

## PR-914 flow correctness

- Pin file at PR head = 51e635af = the PR's own generator commit (self-bump).
- f4d46b3b touches pin file + regen only; generator `--check` green, so the
  committed tree equals the pin render (fixed point at the new pin).

## PR CI status (recorded, matches author's documented expectation)

- Control / Planning FAILURE (run 35169868299): exact self-describing error,
  exit 1: "no runtime product for revision 51e635af... (closure
  1bfbdc50fa00fdfd); the mainline runtime-product publisher builds it after
  merge". Red by construction: product builds on main push after merge.
- Policy FAILURE (run 35169868123): "no same-repository PR run published
  candidate velnor-workflow-candidate-7dcabc83ea2c2a6b-Linux-X64" (no
  candidate artifact; CI/PR unit job skipped behind Planning). Expected.
- All other jobs SKIPPED; DCO SUCCESS; PR state OPEN, mergeable MERGEABLE.
- Landing needs review + override, as the author states; then the main push
  publishes the first revision-carrying product for the consumer-half PR.

## Gates rerun in scratch worktree (all observed, all green)

- cargo test -p velnor-workflow --locked --all-features: 546 passed
  (491 lib + 2 + 6 + 5 + 9 + 33), 0 failed.
- cargo clippy (test profile, all-targets, all-features, -D warnings): exit 0.
- cargo fmt -p velnor-workflow -- --check: exit 0.
- actionlint (whole tree): exit 0, no findings.
- generator --plain --dry-run: "0 files would change"; --plain --check:
  "Generated files are current".

## Verdict

CERTIFIED (safe to merge). Scope is producer-only with proofs; old consumers
provably unaffected; red CI is the documented by-construction state.
