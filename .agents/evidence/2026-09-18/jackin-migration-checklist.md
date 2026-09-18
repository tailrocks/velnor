# Jackin migration checklist: adopt Velnor-generated CI, retire PR #994 copies

Survey date: 2026-09-17. READ-ONLY survey; no file in either repo was modified.
Ledger: velnor2 `plans/2026-09-17-pr994-behavior-ledger.md` (L-rows below refer to it).

## 0. Pinned facts (verified this session)

- Jackin main HEAD: `0be3fcf95cd33c14fd3fe47af08026f17135a157` (#995; tree clean).
- Velnor worktree HEAD: `33688938297eee3933997dbbf368ee20fb1779d4` (= ledger pin).
- PR #994: OPEN, head `ab5b0c4e`, base `92f347ac`, branch `fix/restore-static-ci-after-992`,
  untouched since 2026-09-16T15:22:42Z, mergeState DIRTY (base moved to 0be3fcf9).
- Jackin main has **zero** of the 27 #994 mappings: 9 workflows only
  (ci-main, ci-policy, ci-pr, ci-unit-{bun,docker,rust,swift}, maintenance, nightly),
  no `.github/actions/`, no `.github-gen/sources/`, no `[[static_files]]` declares.
  All 27 mappings (12 workflows + 15 action files incl. 7-file JS runtime) exist
  ONLY in the PR. Migration from main is pure declaration + regen; there is
  nothing to delete from main except scripts noted under L6.
- #994 disposition: **close unmerged**. Its approach (static copies + pin
  downgrade to 541d8926 + hand-edited YAML/hashes, §1a) is superseded. Closing the
  PR retires all 27 copies at once. Never merge it, never pin 541d8926.
- Velnor slice code (1, 3/G, 4/D, 5/F, 6/E, H8a) is present in the velnor2 worktree
  ONLY as **uncommitted modifications** (+5366/−226 across 14 files, plus untracked
  `check_profiles.rs`, `docs_site.rs`, 3 test suites, 4 fixtures). B (prepared
  tools), H-remainder, A-prereq, C-reuse are absent here (spawned elsewhere).
  Migration pin = a future verified velnor main revision AFTER the slice commits
  land — not 33688938, not b9c3156c, never 541d8926.
- L1 bug is live in main: `ci-unit-swift.yml:104` runs on `ubuntu-26.04`.
- F2 target verified: scan-derived unit `rust-jackin-usage-ffi` exists
  (`.github/ci/project.toml:539`); main's swift row has no `depends_on`.

## 1. Config declarations to add (`.github-gen/velnor-workflow.toml`)

All in one migration commit, in this order (see §3 for why):

1. `[generator] revision` → new verified post-slice rev (L2).
2. Swift row: `depends_on = ["rust-jackin-usage-ffi"]` (F2; one line).
3. `[[check_profile]]` rows (G; L5, L7, L12):
   - desktop-cadence: schedule `41 4 * * 1`, macOS runner, tasks
     `desktop-merge`, `desktop-scheduled` (+ thresholds in task bodies).
   - hygiene: one profile per job group (16 PR jobs → group; schedule `23 11 * * *`);
     **task bodies must first be extracted from PR-inline shell into named mise/xtask
     tasks** — main has no fuzz/miri/mutants/dhat/deny/hakari tasks today.
   - reuse: `reuse lint` via named task; PR triggers are push/PR/dispatch with NO
     schedule — confirm generator supports event-triggered (non-scheduled) profiles
     (OPEN Q1).
4. `[docs]` (F; L6, L16): `site_url = "https://jackin.tailrocks.com"` (from
   docs/package.json lychee remap), `site_dir` = docs build output (`.output/public`),
   `build_commands` = `bun run scripts/gen-crate-pages.ts && vite build && bun run
   scripts/prerender-static.ts` (docs/package.json `build`), link/spell/verify
   commands from package.json (`check:repo-links`, `check:links` w/ docs/lychee.toml,
   spell), `schedule` for external-links lane, `docs_paths` filters.
5. `[release]` (D+E; L4, L8, L9, L10, L14, L19, L20) — **cardinality OPEN Q2**:
   config has a SINGLE `[release]` with one `kind`, but Jackin needs four
   release-side shapes: docker/construct (image, dockerfile
   `docker/construct/Dockerfile`, context, platforms), preview (rolling prerelease +
   `workflow_run` producer binding + modes validate/build/rehearse), release (tag
   publish + `[[release.credential]]` keychain setup/teardown pairs + 7-asset
   assembly + capsule signing), jackin-dev (version-bump + build×publish matrix).
   Declare per whatever multi-release shape the E/D slices finalize; fields available:
   `producer_workflow`, `producer_conclusion`, `modes`, `archive_members`,
   `archive_checksum`, `archive_retention_days`, `[[release.credential]]`
   (setup+teardown both required), docker `image`/`platforms`.
6. `[renovate]` (H8a; L11): enabled, reason, schedule `0 6 * * *`, dedicated PAT
   secret (not GITHUB_TOKEN, uppercase name), config `renovate.json`,
   validate=true, `lanes` = velnor default + dispatch override as needed.
   Upstream-content checks (customManagers versions.env, mise versions both archs)
   stay Jackin named tasks behind the generic validator shape.
7. Prepared-tools declares (B; L15, L17, L18): **BLOCKED** — no prepared-tools
   config exists in-tree yet. Nothing to declare until impl-handoff-wire lands.

## 2. Per-L-row checklist

| # | Source (PR-only) | Jackin-main change | Deletes | Status |
|---|---|---|---|---|
| L1 | ci-unit-swift static | none (pin bump only); regen flips ubuntu→macos-26 | PR copy dies with PR close | ready when slices land |
| L2 | pin 541d8926 | bump to new verified rev | — | ready when slices land |
| L3 | cache-cleanup | declarative retention (H-remainder) | — | BLOCKED on H-remainder; main maintenance.yml stays |
| L4 | construct | `[release]` kind=docker + platforms | — | ready when slices land (modulo Q2) |
| L5 | desktop-cadence | `[[check_profile]]` (tasks exist) | — | ready when slices land |
| L6 | docs | `[docs]` full contract | `scripts/ci/docs-lychee-contract.sh` (0 refs per ledger); `codebook-contract.sh`, `construct-result-contract.sh` referenced ONLY by PR copies → delete after verifying no main refs | ready when slices land |
| L7 | hygiene | `[[check_profile]]` rows + **extract 16 inline job bodies to named tasks first** | — | Jackin-side extraction work, then ready |
| L8 | jackin-dev | `[release]` modes + matrix | — | ready when slices land (modulo Q2) |
| L9 | preview | `[release]` workflow_run producer+revision binding, modes, rolling | — | ready when slices land (modulo Q2) |
| L10 | release | `[release]` + credential pairs | — | ready when slices land (modulo Q2) |
| L11 | renovate ×2 | `[renovate]` + lanes; validator stays generic | dead PR/push runs-on branches (PR-only) | ready when slices land |
| L12 | reuse-compliance | `[[check_profile]]` reuse-lint task | — | needs Q1 answer |
| L13 | aggregate-needs | none (generated ci-required) | PR copy | no-op |
| L14 | build-release-archive | `[release]` archive decls; zigbuild stays named task (`.github/scripts/zig-for-cargo-zigbuild` exists in main) | PR copy | ready when slices land |
| L15 | cache-cargo-registry | generated cache prep; lockfile discovery | — | BLOCKED on B |
| L16 | check-deployed-docs | `[docs] verify_commands` | PR copy | ready when slices land |
| L17/18 | download-ci-xtask/codebook | B handoff declares; **never preserve name+expiry acceptance or wrong-key save** | entire JS runtime (PR-only; `npm test` never run) | BLOCKED on B |
| L19 | sign-and-attest | `[release]` SBOM/prov/sign stages | PR copy | ready (D+E landed) |
| L20 | sign-capsule-manifest | orchestration via E; keep schema/verifiers | PR copy | ready when slices land |
| L21 | fakes/hand-edits | none (main is byte-identical b9c render) | — | H-remainder for real admission info |

## 3. Ordering constraints

1. **Slices before pin.** Do not start the Jackin migration until slices
   1, 3, 4, 5, 6, H8a are committed to velnor main AND the no-disturbance
   re-render + policy-green verification pass at the new rev. Today they are
   uncommitted in one worktree — unpinnable.
2. **Pin + FFI edge are the safe first commit.** `revision` bump and the
   `depends_on` one-liner are independent of every other declaration; land them
   first so the L1 fix and F2 selection lock before the big adopt. (Verify
   `depends_on` parses at the new rev — supported, test-locked in slice 2.)
3. **B-gated rows migrate last or in a second commit** (L15, L17, L18):
   impl-handoff-wire must land + pin must advance again. Jackin has no prepared-tool
   behavior in main today, so this gates nothing else.
4. **H-remainder-gated** (L3 retention/cleanup bounds, L21 admission info):
   second commit with the next pin.
5. **Q2 gates all `[release]` declares** (L4, L8, L9, L10, L14, L19, L20):
   single-`[release]`/single-`kind` cannot express construct+preview+release+jackin-dev
   as four workflows. Resolve the multi-release shape before writing Jackin config.
6. **Jackin-side extraction precedes `[[check_profile]]` for hygiene** (L7):
   16 inline-shell job bodies → named tasks (mise or xtask). Desktop (L5) and
   reuse (L12) tasks already exist or are trivial.
7. **Close #994 only after** the migration renders green + policy passes at the new
   pin; the PR is the only copy of the inline bodies (hygiene/docs) being mined for
   task extraction until then. Do not merge it (base moved; approach superseded).

## 4. Product-owned remainders (stay in Jackin forever)

- Docker: `docker/construct/Dockerfile`, `versions.env`, VERSION immutable guard
  (`construct-assert-version-unpublished`), `construct-*` mise tasks.
- Desktop/macOS: all `desktop-*` task bodies, thresholds, entitlements, bundle
  contents, `desktop-sign-notarize`, `desktop-bootstrap-secrets`, signing identity.
- Docs: site address, `.output/public` mapping, `docs/lychee.toml`, package.json
  check commands, `gen-crate-pages`/`prerender-static` generators, spell config.
- Release: capsule schema (`crates/jackin-capsule`, `jackin-image/capsule_binary`),
  verifiers, homebrew tap `jackin-project/homebrew-tap` (`jackin-preview.rb`),
  signer ref `jackin-project/velnor-actions package-signer`, no-clobber policy,
  zigbuild invocation, dev-tool build specifics.
- Renovate: `renovate.json`, schedules, PAT credential, DCO, fleet trust facts,
  upstream-content named tasks.
- Thresholds/assertions for every check profile (never in generic code).

## 5. Open questions

- Q1: Can `[[check_profile]]` express event-triggered (push/PR/dispatch, no cron)
  checks like reuse-compliance, or is a schedule mandatory?
- Q2: What is the multi-release declaration shape (docker + preview + release +
  jackin-dev from one `[release]`/one `kind`)?
- Q3: `[docs]` Pages details: which repo/branch deploys, `site_dir`/`sitemap_path`
  exact values, external-link schedule — read from PR docs.yml at migration time.
- Q4: Do A-prereq (XCFramework prep) and C-reuse (result reuse/aggregation) change
  any Jackin declaration, or are they generator-internal? (No L-row maps to them
  except via F2-area overlap; confirm before final pin.)
- Q5: `codebook-contract.sh` / `construct-result-contract.sh` in `scripts/ci/`:
  confirm zero main-tree refs, then delete with migration (B/F replace them).
