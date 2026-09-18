# B2 PREP — atomic velnor-apt promotion procedure (authored, NOT executed)

Date: 2026-09-17. Status: READ-ONLY prep. B2 gate not reached; zero writes to
`tailrocks/velnor-apt` until B1 + A2 preconditions below hold.
Inputs: `/tmp/a0-consumers.md` §1 (baseline), `/tmp/a2-gaps.md` G5 (mechanism
gap), `/tmp/b1-design.md` (primitive surface), work-plan STEP B2, spec §7.

## 0. Frozen baseline (re-verify read-only at B2 start, A0 method)

- velnor-apt `main` = `d62820d4` (`chore: sync velnor-workflow to b9c3156c`).
- Generator pin in `.github-gen/velnor-workflow.toml` = `b9c3156c…`; profile
  `apt-repository`; `files = ["ci-unit-docs.yml"]`; single `docs` unit.
- `.github/workflows/` = exactly `ci-unit-docs.yml`. Tree = 33 entries.
- Omission notice `.github-gen/NO_WORKFLOWS_REQUIRED.md` blob `3172bb88…`
  (809 bytes), omitted set: `ci.yml`, `ci-apt.yml`, `publish.yml`,
  `package-update.yml`, `package-updater.yml`, `renovate.yml`, composites
  (`aggregate`, `cache-contract`, `run-gate`).
- If live `main` ≠ `d62820d4` at B2 start: re-read tree + notice blob before
  proceeding (same `gh api` + `git ls-remote` method, no clones for audit).

## 1. Precondition checklist (ALL green before the promotion commit)

### P1. Generator product R published + attested (spec §3.2 steps 4–5)

- [ ] R is merged, reviewed source on `tailrocks/velnor` default branch
      (never a PR head, never a trusted-pin candidate).
- [ ] R built by the GENERATED trusted hosted producer only:
      `tailrocks/velnor/.github/workflows/ci-runtime-products.yml` @ R,
      owner-only, main-push/dispatch triggers, least-privilege perms.
- [ ] Content-addressed tag `velnor-workflow-runtime-v1-<closure16>`; tag-exists
      skip + pre-create re-check, never overwrite.
- [ ] G4 CLOSED: verify-then-create ordering — all platform assets +
      manifest + attestations + consumer-flow smoke verified BEFORE release
      exposure. No immutable-but-unverified release exists for R.
- [ ] G1 CLOSED: manifest carries source `revision`; release target SHA
      verified (not notes-text); binary `--revision` stamp consumed by
      verification, not decorative.
- [ ] G2 CLOSED: attestation pins `--signer-workflow` AND `--signer-ref`
      (or cert identity); producer dispatch constrained so a branch run
      cannot mint a trusted attestation.

### P2. B1 primitives complete (spec §7, all 11 capabilities)

- [ ] Typed contracts for: source repo + exact release identity, package,
      arch set (= exactly {amd64, arm64}), stable/preview suites (final
      grammars, no compat wrappers), signer fingerprint + opaque secret refs,
      release-coherence verification, repository assembly, publication
      records (stable + preview shapes), previous-version retention (typed
      fn, legacy strings gone), Pages single-writer deploy (last-publish
      no-rollback guard), channel-update tasks only where required.
- [ ] `verify-release.sh` logic behind the narrow typed contract
      (`apt.fetch`/`apt.verify`/`apt.publish`/`apt.pages-deploy`, fixed
      argv, config contributes validated scalars only); byte-parity proven
      against `test-verify-release.sh` fixtures, then script deleted.
- [ ] Renamed-fixture proof green (renamed package/repo/origin flow with
      zero name-keyed branches); `apt-repository` renders a real feed.
- [ ] B1 negative suite green (§4a oracle port + §4b typed-config cases):
      every rejection pre-mutation, trusted state intact.
- [ ] Generated APT workflow coverage from the new primitives already proven
      in-fixture: ownership inventory, local-ref resolution, structured
      policy, actionlint, fail-closed aggregation.

### P3. A2 promotion/consumer machinery closed

- [ ] G5 CLOSED by this procedure's mechanism (§2): one command produces
      pin+metadata+full-tree; atomic-promotion test proves pin/tree
      consistency (policy already fails stale mainline).
- [ ] G3 CLOSED: Velnor provisioner resolves the generator pin from the
      PRODUCT repo (`tailrocks/velnor`), never the consuming repo. Verify
      the promoted tree carries no consumer-repo lookup path.
- [ ] G6/G7: executable consumer negative suite + cold-consumer proof green
      (missing/bad-digest/bad-manifest/untrusted-signer/lookup-confusion/
      PR-checksum each reject on executed fixtures, not string asserts).
- [ ] G8/G9: closure covers runner/target mapping (or rebuild-on-label-change
      proven); path-dep completeness test guards future escape.

### P4. Promotion inputs staged (still read-only)

- [ ] Verified product binary for R acquired through the FULL consumer path
      (C1–C9: product repo, full 64-hex closure, source revision, platform
      key, accept filter, manifest+binary digests recomputed, signer
      workflow+ref, `--closure` self-report tripwire). Zero `cargo` fallback.
- [ ] Full regenerated velnor-apt tree rendered by that binary into a
      DISPOSABLE scratch dir (never in place first); render diff vs
      `d62820d4` reviewed file by file.

## 2. The ONE-commit shape (G5 mechanism)

One command, one commit, no follow-ups. Conceptually:

`promote --repo tailrocks/velnor-apt --base d62820d4 --rev R
--product <verified R bundle> --out <scratch> --commit`

1. Acquire + verify product R per P4 (fail closed on ANY check).
2. Render the ENTIRE tree with R into scratch; byte-compare against a second
   render (determinism proof).
3. Stage exactly the set below as ONE signed commit (`git commit -s`; DCO is
   a required external check on velnor-apt). Push as a PR, never direct
   to `main`; `ci-required` + DCO must pass.

Commit contents — pin/product metadata + full tree, nothing else:

- `.github-gen/velnor-workflow.toml`: `revision = R` (+ any product-metadata
  fields R's schema requires: closure/manifest locator — carried VERBATIM
  from the verified product, never hand-edited); `files = [...]` expanded to
  the full generated set; profile stays `apt-repository` (now a REAL feed).
  G3 check: confirm `[generator]` repository/pin semantics resolve R from
  the product repo after this change (baseline file names the consumer repo
  — do not let that become a consumer-repo lookup).
- `.github/workflows/`: full regenerated set (retained `ci-unit-docs.yml`
  re-rendered at R + every B1 APT workflow: publish path, APT CI,
  channel-update tasks, renovate/maintenance as R renders them). Exact file
  names are R's output — do NOT pre-declare them from the old omission list
  (its `ci.yml` predates current `ci-main`/`ci-pr`/`ci-policy` shapes).
- `.github/actions/`: generated composites R renders (must cover the
  `aggregate`/`cache-contract`/`run-gate` class the notice omits).
- `.github/ci/`: regenerated `project.toml` + generator state (APT units
  typed, docs unit retained).
- `.github/`: regenerated `AGENTS.md`/docs-link targets as R renders.
- DELETED: `.github-gen/NO_WORKFLOWS_REQUIRED.md` (only per §3).
- UPDATED: `README.md` + any doc R does not own but the new flow invalidates
  (only per §3). No source/config/secret/schema drift in the same commit.

## 3. Omission-notice + stale-doc removal criteria (real coverage ONLY)

DELETE the notice in the promotion commit iff EVERY omitted class has a
generated replacement IN THE SAME COMMIT (mechanical check, not judgment):

| Omitted (notice text) | Real-coverage bar |
|---|---|
| `publish.yml` | Generated publish workflow: typed fetch→verify→assemble→sign→record→single-writer Pages deploy; last-publish guard present |
| `ci-apt.yml` / `ci.yml` | Generated CI/required-result workflows covering the APT surface + retained docs unit; fail-closed aggregation |
| `package-update.yml` / `package-updater.yml` | Generated channel-update coverage (or B1 proof the index IS the state and the task was deleted by design — then this row is N/A by B1 record, not by silence) |
| `renovate.yml` | Generated renovate config/coverage |
| composites (`aggregate`, `cache-contract`, `run-gate`) | Each referenced composite exists locally generated (or remote-pinned, jackin-style) AND every `uses:` resolves — zero dangling refs |

- If ANY row fails: KEEP the notice untouched, abort B2. Partial promotion
  (pin bump without full coverage, or coverage without notice removal) is
  forbidden — it recreates the stale-pin / false-omission states.
- Stale direct-install docs: `README.md` "How it is built" links
  `.github/workflows/publish.yml`, which is 404 at baseline — that link MUST
  resolve post-commit (mechanical `link-check`). Rewrite ONLY the install/
  upgrade/maintainer sections the generated flow actually changes; per spec
  §7 the key-setup section MUST gain fingerprint authentication against a
  separately trusted reference (never blind-trust the download-URL key) and
  repository-scoped `Signed-By`. Docs-only touch-ups ride in the same commit
  only if they are invalidated by the new flow — no drive-by edits.

## 4. Post-commit proof commands (run against the promotion commit)

Run from a clean checkout of the promotion HEAD. `<PROMO>` = promotion SHA,
`<BASE>` = `d62820d4` (or the re-read baseline SHA).

```bash
# 0. exact commit shape: one commit, signed, DCO
git log --format='%H %G? %s' -1 <PROMO>          # exactly one commit on the PR
git show --stat --oneline <PROMO>               # pin + tree + notice + docs only
git verify-commit <PROMO> 2>/dev/null || git log -1 --format=%B <PROMO> | grep -q Signed-off-by

# 1. regeneration exactness: re-render with verified R, expect ZERO diff
velnor-workflow closure --rev=<R>               # record closure; compare to manifest
# re-render full tree into scratch with the verified R binary, then:
diff -r <scratch-render> . && git status --porcelain  # both empty

# 2. ownership + sole-ownership + reference checks
git status --porcelain                           # no unexpected/untracked files
# every generated file carries the generator header; no hand YAML:
grep -rL "Generated by velnor-workflow" .github/workflows/ .github/actions/ && exit 1
# every local uses: resolves:
grep -rhoE '\./\.github/actions/[A-Za-z0-9_./-]+' .github/workflows/ | sort -u \
  | while read -r a; do test -d "$a" || { echo "DANGLING: $a"; exit 1; }; done

# 3. structured policy clean (same invocation shape as velnor ci-policy)
velnor-workflow policy \
  --workflow-root "$PWD" \
  --head-sha <PROMO> \
  --base-sha <BASE> \
  --candidate-manifest "" \
  --ruleset-contexts "$RULESET_CONTEXTS"

# 4. actionlint clean (pinned, as in velnor ci-policy)
mise exec actionlint@1.7.12 -- actionlint

# 5. release-verifier coverage on the new tree (work-plan B2.5)
# B1 negative suite re-run against the promoted config + typed-verifier
# tests address the new workflows; record logs as evidence.
cargo test -p velnor-workflow                   # generator gates stay green
cargo clippy -p velnor-workflow -- -D warnings
cargo fmt --check
```

Evidence bundle for the B2 gate: publication record for R (tag, manifest,
attestation verify logs for ALL platforms, smoke log), the atomic pin/tree
diff (`git show <PROMO>`), §3 row-by-row coverage proof, and the logs of
every command in §4. Verifier: ownership/APT verifier.

## 5. Explicit non-goals / abort conditions

- No velnor-apt branch, PR, or commit exists before P1–P4 are ALL green.
- No hand YAML in the promotion commit — every workflow/action/manifest byte
  comes from verified R; any hand fix means R is wrong → fix generator, new R.
- No `latest`/branch-following selectors; no `dpkg -i`/sideload anywhere.
- Any B1 capability deferred → B2 aborts (notice stays, per A0 C-apt-1).
