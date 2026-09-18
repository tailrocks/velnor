# A1: PR-912 branch-RED diagnosis (docs/bastion-final-plan @ 40ff66ec)

Target: CI/PR run 35167995756 (Control/Planning exit 5) + policy run 35167993091
(FAIL generated-tree). No edits, no commits. Evidence below is reproduced
locally, not inferred.

State note: the shared checkout advanced 40ff66ec -> 89773efc (a2-consumer-negatives
merge, unmerged paths) while diagnosing. All commit-pinned results (closures, jq
repro, manifest scans, both renders) are against the 40ff66ec tree and match the CI
failure bytes exactly.

## F1: Control/Planning exit 5 — jq `null cannot be matched` at manifest.json:19

ROOT CAUSE: our merged A2 change 65beb2ed (revision accept-filter). Not pre-existing,
not a platform-key/cache issue.

Chain:
- CI/PR runs the PR's own setup action (`Run ./.github/actions/setup-velnor-workflow`,
  local path = PR tree), with `rev: 7341ef4b` (tree pin). Closure resolves to
  9f236b40635970a422bf41a55f1529510e42f7113883c53683d24d088f79dad3; the Actions
  cache HITs and the download step skips, so the cached manifest is verified by
  the "Verify runtime product" step with the NEW filter, which 65beb2ed extended
  with `(.revision | test("^[0-9a-f]{40}$"))`.
- The live release `velnor-workflow-runtime-v1-9f236b40635970a4` (published
  2026-09-16T22:37Z from main, pre-A2) has NO `revision` field. `.revision` is
  null, `null | test(..)` throws, jq exits 5. jq reports the error at input end
  (manifest.json:19 = closing brace) because the null comes from a MISSING field,
  which is why it looks like a products/platform failure. It is not:
  `.products["Linux-X64"].binary` is present and valid
  (`faba239c…`; platform lookup alone exits 0).
- Corroboration: run 35162700625 (23:31Z, pre-A2) passed Planning far enough to
  reach the Rust jobs (operational_store x3); A2 merged ~00:26Z; every run since
  fails in runtime setup. Policy's own setup step succeeds because
  pull_request_target runs BASE's action (old filter, no revision clause).

Blast radius, measured: ALL 14 live `velnor-workflow-runtime-v1-*` releases lack
`revision` (scanned every manifest via `gh release download` + `jq -r
'.revision // "MISSING"'`). The new filter rejects every existing product.
Repro, byte-exact (same message, same `:19`, exit 5):

    jq -e --arg closure 9f236b40… --arg platform Linux-X64 \
      '.closure == $closure and (.revision | test("^[0-9a-f]{40}$")) and …' \
      manifest.json   # -> exit 5, CI-identical error
    # same filter minus the revision clause -> exit 0

Why this is an architecture trap, not just a bad commit:
- Products are immutable and content-addressed ("never overwrites"); the producer
  builds HEAD's closure only, on main push. An old closure's product can NEVER be
  rebuilt (old closures don't recur on new main) and pre-revision binaries…
- …actually DO stamp `--revision` (flag dates to 5475a22e, Sep 16; verified in the
  old binary's strings: "Print the source commit this binary was built from").
  Only the MANIFESTS lack the field. So the binding step would work if manifests
  named the right build commit — but 12/14 products are branch-built (per f79ab248
  notes) from commits that may no longer resolve, and naming them requires
  per-release archaeology plus running each old binary to confirm its stamp.
- Therefore the consumer+pin advance must be atomic: a consumer that REQUIRES
  revision needs the pin to resolve to a WITH-revision product. No such product
  exists anywhere (new producer unmerged; old producer doesn't write revision),
  so the combined A2 branch can never go green as assembled — chicken-and-egg.

PROPOSED SOURCE FIX (ranked):
1. Split the landing (process + source, no shim, matches the proven PR-914 flow):
   a. Land producer-only first (manifest gains `revision`; old consumers ignore
      unknown fields — old filter never mentions revision, old PATH step only
      checks `--closure`: verified safe). Main push builds the new-closure
      product WITH revision.
   b. Land consumer-require + pin bump to (a)'s commit together: Planning
      resolves the new closure whose product exists with revision; policy takes
      the candidate path (now functional because Planning publishes the
      candidate artifact). This also needs the 65beb2ed commit split (it mixes
      producer + consumer + G10).
2. Immediate unblock alternative: make the consumer filter revision-tolerant for
   legacy manifests (`has("revision")` guard + conditional `--revision` binding),
   removable once no live pin resolves to a pre-revision product. A genuine
   finishable migration, but a shim the project rules disfavor; prefer (1).
3. Rejected: delete+rebuild old releases (impossible — producer builds HEAD
   only, old closures unreproducible); closure-version bump (same reason, worse:
   invalidates ALL old products at once); manifest backfill surgery on 14
   releases (violates never-overwrite, needs per-release build-commit proof,
   AND requires a cache-key-prefix v3->v4 bump since caches hold stale
   manifests — note Verify-step/cache poisoning as adjacent risk either way).

Minor adjacent finding (not causal): the cache-verify filter omits the
`.products[$platform].asset == $asset` check the download filter has.

## F2: Policy FAIL generated-tree (8 files) — candidate exception never engaged

ROOT CAUSE: generator changed + tree regenerated, pin NOT bumped. The tree no
longer equals the pin render, and the candidate exception is keyed on
pin-vs-base closure difference, so with pin == base it early-exits by design
(log line 782: "pin 7341ef4b… shares the base closure; the Stage-0 validator
renders" -> VELNOR_WORKFLOW_CANDIDATE_MANIFEST empty). Two independent locks:
  (a) Acquire step early-exit (pin == BASE_PIN == 7341ef4b, both closures equal).
  (b) Even without (a): no candidate artifact exists — it is published by the
      `github-rust-velnor-workflow` CI/PR unit job (`candidate_publish: true`,
      needs plan==success), which SKIPPED because Planning failed (F1).

Three-way comparison, all reproduced locally (HEAD debug binary + pin worktree
binary, `policy` render flags `--plain --force --default-branch main`):
- committed tree vs CANDIDATE (HEAD-generator) render: ZERO diffs (fixed point).
- committed tree vs PIN (7341ef4b-generator) render: exactly the 8 policy-listed
  files: `.github/ci/.github-actions-generator-state`, `.github/ci/project.toml`,
  `ci-main.yml`, `ci-pr.yml`, `ci-runtime-products.yml`, `ci-unit-rust.yml`,
  `preview.yml`, `release.yml`.
- So the exception WOULD pass if engaged (tree == candidate render); it fails
  only for lack of a bound candidate. Validator-side mechanics confirm: with no
  manifest and no env-slot binary, `render_with_candidate` tries only current_exe
  (base product, release closure 9f236b40…) against wanted HEAD candidate closure
  4bd2c9d2… (debug/tui profile) — never equal by construction.
- Closure drift pin->HEAD is real, not cosmetic: release 9f236b40… -> d0ea78ef…,
  candidate 98e0ebd5… -> 4bd2c9d2… (963-line closure-input diff). Publisher would
  NOT skip; acquire would NOT early-exit — once the pin is bumped.

Established flow (PR #914, merged 22:35Z, "prove the candidate path live"):
generator PR bumps `[generator] revision` to its own commit + regenerates; CI
publishes the candidate; policy renders with it. This branch skipped the bump.

PROPOSED SOURCE FIX: bump `[generator] revision` to the generator-change commit
and regenerate (mechanical: edit `.github-gen/velnor-workflow.toml`, run the
generator, commit). BLOCKED on F1: a bumped pin resolves to closure d0ea78ef…,
whose product does not exist ("no runtime product … built after merge"), and
post-A2 no producer can build a PR-closure product (producer main-only +
consumer `--source-ref main` rejects branch builds — the pre-A2 escape hatch
that built 12/14 live releases is deliberately closed). So F2's fix rides the
F1 landing split above: after producer-first lands on main, bump pin to that
main commit (product exists, main-attested, revision-carrying) and the candidate
path greens the consumer PR.

## Recommended campaign sequence

1. Split 65beb2ed: producer-half PR (revision write + producer proofs) -> merge;
   main push publishes first revision-carrying product.
2. Consumer-half PR: accept-filter + `--revision` binding + Velnor provisioner
   pin (f79ab248 halves accordingly) WITH pin bump to (1) + regen -> candidate
   path carries policy; Planning acquires the (1) product.
3. Then rebase docs/bastion-final-plan onto post-A2 main; its tree regenerates
   under the new pin; operational_store x3 (currently masked — pipeline fails
   before the Rust jobs) resurfaces for A3+.

## Evidence map (all under /tmp, reusable)

- /tmp/a1-ci-log.txt, /tmp/a1-policy-log.txt, /tmp/a1-policy-full.txt (gh fetches)
- /tmp/a1-manifest/manifest.json (+ per-tag subdirs): live 9f236b40… manifest
- /tmp/a1-render-head (== tree, 0 diffs), /tmp/a1-render-pin (8 diffs, as listed)
- jq repro one-liner in F1 section above.
