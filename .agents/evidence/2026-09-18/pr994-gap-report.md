Gap analysis complete. Read-only; no files modified (PR bytes staged to `/tmp/pr994gap` scratch only).

## Method / scope check

- PR #994 is a single commit `ab5b0c4e` on base `92f347ac`: **63 files, +13408/−51 — matches ledger §1 exactly.**
- All 27 `static_files` sources are **byte-identical** to their `.github` dests at head (verified by hash), so the ledger's "source + copy = 1 behavior" premise (§2) holds — including `ci-unit-swift` (source is a frozen copy of the hand-patched dest).
- Enumerated every behavior-bearing hunk: toml (pin + scan-exclude + 10 policy excludes + 27 mappings), state, 8 touched generated workflows, 12 static workflows, 8 actions + JS runtime. Covered items (with ledger cites) listed at the end; **3 contradictions + 14 gaps** below.

## Contradictions (ledger claims PR bytes refute)

**C1 — L3 "gh-cache purge on closed PRs" is wrong on triggers.** `sources/workflows/cache-cleanup.yml` is `workflow_dispatch`-only (L8–19) with a manual `ref` input; no closed-PR trigger exists anywhere in the PR. The "maintenance prunes merge-ref caches" half is pre-existing base behavior (`maintenance.yml` diff is pin-swap-only). The *loop shape* is covered (L3/H "no retry-without-progress"), the trigger description is not. Severity: moderate.

**C2 — L4 "bake/…" over-specifies.** No `buildx bake` invocation exists in `construct.yml`; builds run via `mise run construct-init-buildx/construct-build-platform/construct-push-platform` (L271/282/291). `docker-bake.hcl` appears only in paths-filter (L107/157/167). Severity: minor (wording).

**C3 — L11 validator "checks upstream customManagers sources" is false.** `renovate-validate.yml` (70 lines, read fully) has exactly 2 steps: checkout + mise-arch asset check. Zero customManagers-source checks exist (only the header comment describes them). Knock-on: L11's "upstream-content checks stay Jackin named tasks" plans to preserve checks that aren't in the copy. Severity: moderate.

## Gaps (PR behaviors the ledger never dispositions)

**G1 — `[scan] exclude += ".github-gen/sources/**"`** (`velnor-workflow.toml`; 0 ledger hits). Prevents the vendored runtime `package.json` from selecting a Node unit. Removal-with-sources is implied but never stated. Minor.

**G2 — `[policy] exclude_workflows` 10-entry exemption** (`velnor-workflow.toml`, "#965 interim" comment; 0 hits for the key, `965`, or asymmetry). Only oblique hook is §4 slice-8 "drop 10 excludes": no migration step, and the desktop-cadence/ci-unit-swift asymmetry is unexplained (12 static workflow mappings vs 10 excludes; `desktop-cadence.yml` has push/schedule/dispatch triggers yet is not exempt). Partial gap, moderate.

**G3 — Dead PR/push lane-branch arms beyond renovate** (L11 covers renovate only; same bug class, 4 more files):
- `cache-cleanup.yml` L30 runs-on + L36 `CACHE_REF` PR arm (triggers: dispatch only)
- `hygiene.yml` ×13 runs-on + L70 `CI_LANE` push arm (triggers: schedule+dispatch)
- `preview.yml` L288/L396 matrix `push` arms + publish-gate runs-on PR/push arms (triggers: workflow_run+dispatch)
- `release.yml` L44/L76/L698/L812 `pull_request` arms (triggers: push-tags+dispatch; push arms live). Moderate.

**G4 — download-ci-xtask env contract**: `CI_XTASK/CI_TOOLS_PATH/CI_TOOLS_HIT/CI_XTASK_HIT/CI_METADATA/CI_CARGO_FUZZ` + PATH/chmod, consumed at 9 call sites (`construct.yml` L515; `hygiene.yml` L625/628/1167; `preview.yml` L324/347/430/448/689). L17/F3/F4 cover acceptance flaws only; the contract dies-with-copies implicitly. Minor.

**G5 — Shared `homebrew-tap-publish` concurrency group** (`jackin-dev.yml` L379 + `preview.yml` L661): cross-workflow mutex. Slice-12 "mutexed immutable publish" covers jackin-dev-internal only. Minor-moderate.

**G6 — Preview lane routing**: dispatch default `github` (vs `velnor` in all 10 other dispatch inputs) + `lanes != 'velnor'` runs-on routing in publish-gate/publish-preview. L9/Q6 don't cite. Minor.

**G7 — desktop-cadence event split**: dispatch runs merge-only (`if: != schedule` L41; scheduled job schedule-only L69); `lanes` input wholly ignored (static macos-26). L5/Q6 cover macos-static only. Minor.

**G8 — Preview fail-open error semantics**: diff failure "falling open" (rebuild), missing old_sha falls open, formula-fetch failure warn-only (`preview.yml` L98–155). Safe direction, undispositioned. Minor.

**G9 — Cache-save gating policy**: main/dispatch/same-repo-PR gates (`cache-cargo-registry` save; docs/construct result publish; mise `cache_save: main-only`; mbx `save-on-workflow-dispatch`). B slice covers validation, not write-gating. Minor.

**G10 — Renovate repo-cache scheme**: per-run key `renovate-<os>-<run_id>` + prefix restore, `/tmp/renovate` paths (`renovate.yml` L31–40). L11 notes Velnor "cache exist"; copy's scheme unaddressed. Minor.

**G11 — `external-links` input** (`check-deployed-docs`, default true, never overridden by either caller → post-deploy gate checks externals). Trivial.

**G12 — Construct lane×platform matrix**: Velnor=amd64-only, arm64 via GitHub `ubuntu-24.04-arm` only (`construct.yml` L51); BuildKit `mirror.gcr.io` mirror; fork-safe Docker Hub login gating; `MISE_TASK_RUN_AUTO_INSTALL=false`. L4/D cover multi-arch/publish only. Minor.

**G13 — `Swatinem/rust-cache` in release builds** (`release.yml` L308/L407) alongside `cache-cargo-registry`: backend choice unaddressed by L14/L15 remainders. Minor.

**G14 — Cosmetic**: vestigial `writer` flag in reuse matrix (never read; jackin-dev's is live); capitalized lane names in reuse/preview/jackin-dev/release matrices (slice-12 lowercasing covers E-followup only). Trivial.

## Verified covered (no gap)

Pin-only files (`ci-pr`, `ci-unit-bun/docker`, `maintenance`) → L2; `command -v mold` L327 in `ci-unit-rust` → §1a(1); 403-fallback hunk → §1a(2); state hashes → §1a(3); swift runs-on + 4× e05aee6 → L1/F1; 82 `velnor-admission-closure` fakes → L21; aggregate-needs skip==ok + 2 call sites → L13/F11; 120s/14-tool/first-unexpired/lane-never-read/wrong-key-save → L17/F3/F4; codebook exact-name → L18; cosign/syft/attest → L19; capsule manifest → L20; 6/7 signer matrices, no-clobber+ZIP-conflict, `|| true` swallow → L9/L10/F10; CI-gate poll, tap poll/dispatch, xtask rolling publish → L9; 16 hygiene jobs incl. miri×3, deny/hack/shellcheck → L7; Pages×3, spell×2, codebook, perf audits → L6; version/assert/writer-gating/PR-skips → L8/slice-12; mise-arch validator half → L11; REUSE lint → L12; `npm test` never invoked and dist contains src logic → L17; L6 dead-filters claim verified against base (lychee-contract.sh exists, 0 refs; xtask/site-contract.sh referenced but absent); secrets/creds → L11 remainder.

Net: ledger's slice coverage is substantively right, but C1/C3 misdescribe two behaviors, G2/G3 leave the policy-exemption + dead-branch classes incompletely dispositioned, and G4–G13 are undispositioned details (mostly minor) a migration checklist should name.
