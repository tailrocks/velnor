# Behavior-to-capability ledger: Jackin PR #994 → Velnor generic capabilities

Goal: replace every valid reusable behavior in jackin-project/jackin#994 with
Velnor-generated output, migrate Jackin, retire the override copies.
PR is evidence, not implementation. No PR bytes are copied into Velnor.

## 1. Pinned evidence (revalidated 2026-09-17)

- Velnor main HEAD: `33688938297eee3933997dbbf368ee20fb1779d4` (local checkout = remote HEAD).
- Jackin main HEAD: `92f347ac39fbf0d6f9853168e2896a6c60522924` (= #994 base).
- PR #994: OPEN, branch `fix/restore-static-ci-after-992`,
  head `ab5b0c4e7517d5b7b96d26d4e96fe6ff623c54bc`, 63 files, +13408/−51.
- PR #992: MERGED 2026-09-16 (`fecda3c7`, clean-room regen, −13384 lines in
  .github-gen/.github). Jackin #993 then synced pin b9c3156c.
- #994 CI checks: pass (incl. Swift/Apple 20m55s, construct-required,
  docs-required). Policy: FAIL (generated-tree). DCO: fail. Merge state: BLOCKED.
- Rechecked during integration: still OPEN at `ab5b0c4e`/`92f347ac`,
  untouched since 2026-09-16T15:22:42Z. No Jackin migration started yet.
- Jackin main has moved past the PR base: now `0be3fcf9` (#995, cache
  mounts). Migration targets current main per §7 (merge, don't recreate).
- No-disturbance proof (slices 1,2,G,F,D,E,H8a merged): Jackin base
  rendered with the pristine HEAD generator vs the current tree differ in
  exactly one workflow line — `ci-unit-swift.yml`: `runs-on:
  ubuntu-26.04` → `macos-26` (slice 1's fix) — plus ownership-state
  hashes. G/F/D/E/H8a contribute zero bytes to the current consumer.
- Full-slice disturbance triage (A/B/C/H + verifier fixes merged;
  pristine-33688938 render vs current-tree render of Jackin main
  `0be3fcf9`, every hunk attributed): (1) Swift units stay on the default
  executor but flip runtime provisioning setup-action → plan-artifact
  download (A: `render_unit_runtime` now keys on
  `platform.requires_apple()` instead of `kind == Swift`; scan sees no
  Apple evidence for either Jackin Swift unit, so both go portable until
  the Q4 `capabilities` declarations land — the migration MUST include
  them); (2) `actionlint.yaml` drops `macos-26` (same cause; restored by
  the declarations); (3) `project.toml` detected gains `rust-ffi:<name>`
  (A scan evidence, improvement); (4) additive-only `apple_executor` /
  `unit_dependencies` / `unit_admission` caller inputs + `Record unit
  dependencies` step on every unit workflow (A/H, inert); (5)
  `maintenance.yml` cache-cleanup rewrite with bounded deletes and no
  undeletable retry loops (H); (6) ownership-state hashes. No other
  semantic change. A `refresh_swift_executor_note` contract note now
  names any Swift unit on the default executor with the capabilities
  remedy, so the Q4 obligation is loud at generation time.

### 1a. Policy failure root cause (reproduced locally)

Policy run 35114917830 fails `generated-tree`: checked-in
`.github/ci/.github-actions-generator-state`, `ci-main.yml`, `ci-policy.yml`,
`ci-unit-rust.yml` differ from the render at the declared pin.

Reproduction: worktree velnor@541d8926 + `velnor-workflow --output` against
jackin@ab5b0c4e → same 4 files differ, byte-identical hunks.

Verified base: Jackin base (92f347ac) is a BYTE-IDENTICAL b9c3156c render
(full-tree `diff -rq` empty; b9c generator built and executed locally).

`#994` method (proven, no generator run): mechanical pin-swap b9c3156c →
541d8926737213542156ee32e420010817f8a323 (older ancestor) in generated files,
PLUS hand-added content, PLUS hand-updated state hashes. Drift vs 541d8926
has three components:

1. ci-unit-rust.yml: kept b9c content (`command -v mold` probe from `9466fc71
   stop mold from replacing /usr/bin/ld globally`); 541d renders `ln -sf mold`.
2. ci-main.yml / ci-policy.yml: HAND-ADDED ruleset-API 403 fallback to
   DECLARED_RULESET_CONTEXTS. The hunk matches velnor `a70d5bdb` ("fall back
   to declared ruleset contexts on API 403"), which is in NEITHER pin and
   postdates the PR (a70: Sep 17 05:08 +0700; PR: Sep 16 15:22 UTC) — #994
   hand-wrote or forward-ported it; base does not contain it.
3. generator-state: hashes hand-updated to the inconsistent tree (state alone
   would pass; policy re-renders at the pin, which fails).

Migration rule (§7): never adopt pin 541d8926. Pin the new verified revision
built from current main. The downgrade also forfeits the mold and 403 fixes.

### 1b. Commit ordering (all ancestors of velnor HEAD 33688938)

oldest → newest: `e05aee6d` (docs rev 21) … `541d8926` (#899) →
`9466fc71` (mold fix) … `b9c3156c` (docs rev 27) → (+16 crate commits) → HEAD.

## 2. Static-mapping inventory (27 mappings; source + generated copy = 1 behavior)

Workflows (12): cache-cleanup, ci-unit-swift, construct, desktop-cadence,
docs, hygiene, jackin-dev, preview, release, renovate, renovate-validate,
reuse-compliance.
Actions (15 files): aggregate-needs, build-release-archive,
cache-cargo-registry, check-deployed-docs, download-ci-xtask (+ 7-file JS
runtime: src/dist/package/package-lock/test/.gitignore), download-codebook,
sign-and-attest-archive, sign-capsule-manifest.

Each row below: source · observed behavior · requirement · Velnor support
(with code evidence) · disposition · proposed change · Jackin remainder ·
verification · removal status.

| # | Source | Behavior → requirement | Velnor support + evidence | Disposition | Change / remainder / verify / removal |
| --- | --- | --- | --- | --- | --- |
| L1 | sources/workflows/ci-unit-swift.yml → .github/workflows/ci-unit-swift.yml | Frozen generated Swift unit file, hand-patched `runs-on: macos-26`; embeds stale `setup-velnor-workflow@e05aee6` + `VELNOR_WORKFLOW_POLICY_REVISION=e05aee6` (4 refs) | BUG at HEAD, now fixed: collapsed kind path used lane-blind `runner_for` (ir.rs:3950) while per-unit path used `runner_for_unit` (ir.rs:5499); existing test covered only the unused path | existing capability needing correction | Slice 1 (done, in review): collapsed jobs use `runner_for_unit`; test `collapsed_swift_kind_lands_on_macos_while_rust_stays_on_linux`; Jackin drops the mapping + e05aee6 refs after pinning fixed rev; verified by unit test + Jackin re-render (macos-26, others ubuntu) |
| L2 | velnor-workflow.toml `[generator] revision = 541d8926` | Pin downgrade vs base b9c3156c | N/A (consumer declares pin; policy renders at pin) | obsolete/incorrect, remove with evidence | Migrate Jackin to new verified rev; §1a is the evidence; verify policy green |
| L3 | sources/workflows/cache-cleanup.yml | gh-cache purge on closed PRs; maintenance prunes merge-ref caches | release.rs:2439 prune/retention + maintenance cache-plan cover the shape | existing-needing-extension (H: bounded, owned-scope cleanup; no undeletable retry loops) | H slice: declarative retention; verify no retry-without-progress; removal after H lands |
| L4 | sources/workflows/construct.yml (539 lines) | Result-reuse, paths-filter, build/push digest matrix, manifest publish + PR rehearsal, ci-audit; bake/push-by-digest/imagetools; VERSION immutable guard | scan/docker.rs local-build only; release.rs has imagetools assembly (both-arch required), reconcile, retention, attest; NO construct/compare mode (F8), no cosign/SBOM in native lanes, no docker logout | new capability via extension (D) | Slice impl-docker-publish: multi-arch, BuildKit caches, digest transport/verify, single publisher, verified manifest, SBOM/prov, resume; Jackin keeps Dockerfile/contents/VERSION guard policy; drop copy |
| L5 | sources/workflows/desktop-cadence.yml (93 lines) | macOS merge/scheduled mise-task checks | No scheduled-check primitive (nightly=dispatch+signal only) | new capability (G) | Slice impl-check-profiles: cadence/platform/task/timeout/required-advisory profiles; Jackin keeps task bodies + thresholds; drop copy |
| L6 | sources/workflows/docs.yml (826 lines) | build, repo-link/link/built-site/codebook/spell lanes, Pages ×3 deploy, deployed checks, perf audit | scan/docs.rs markdownlint-only; release kind=pages exists; no spell/linkcheck/post-verify in generator | new capability (F) | Slice impl-docs-compose: compose + local-vs-scheduled-external split + bounded-retry deploy + post-verify; Jackin keeps site addresses, mappings, lychee config, generators; drop copy. DEAD (0 refs / missing filter files): docs-lychee-contract.sh, docs-*-contract.sh filters → obsolete, delete |
| L7 | sources/workflows/hygiene.yml (1260 lines, 16 jobs) | cache report, deny/hack/shellcheck/fuzz, macos smoke, benches, dhat, cold-start+frame-timing, rust-analyzer, build-time ratchet, beta clippy, coverage, miri×3, mutants, hakari, dylint, DinD e2e, health snapshots | rust-policy covers deny/audit only; no check-profile primitive | new capability (G) | Slice impl-check-profiles (same as L5); Jackin keeps assertions/thresholds/tool tasks; drop copy |
| L8 | sources/workflows/jackin-dev.yml (401 lines) | Version-bump validation + version/assert + matrix build (target×lane) + publish (PR skips build/publish); cargo-timings + tool artifacts | E slice covers modes/bindings/matrix; NOT covered: second release-pipeline file (canonical pin forces `release`→release.yml, mod.rs) + version-policy jobs (`version_bump_units` only records into project.toml, enforces nothing) | existing-needing-extension (E-followup) | E-followup per revised Q2/Q2b: shape-first publisher mapping (preview-kind vs new kind) + per-row identity + generic PR-only version-GATE job running a Jackin named task (selection version_bump_units narrowing is NOT enforcement); see decisions memo challenge verdicts; Jackin keeps dev-tool build specifics + version-task body; drop copy |
| L9 | sources/workflows/preview.yml (732 lines) | workflow_run+dispatch triggers, source diff vs release/formula, archives 4+2 targets, CI-gate poll, 6-asset manifest assembly, external signer ×6, rolling prerelease replace, tap poll | 6 release kinds + rolling preview + reconcile + signer template exist; NO workflow_run trigger (F6); rolling concurrency present (preview group) | existing-needing-extension (E) | E slice: event bindings (workflow_run revision/producer), rehearsal-no-main-wait, rolling rules; Jackin keeps formula/tap/capsule specifics; drop copy |
| L10 | sources/workflows/release.yml (871 lines) | tag+validate, serialization, version/published gates, nextest, 4+2 archives, macOS menu-bar (build/verify/sign-notarize, keychain trap cleanup, sidecars AFTER cleanup, symbols), 7-asset assembly, signer ×7, no-clobber publish + ZIP conflict check | Full-scope validation (F7 false), reconcile (F10 supported part), signer/attest exist; capsule/macOS-sign/homebrew specifics absent (correctly) | supported + Jackin remainder + small extension (E: credential setup/teardown pairing incl. timeout/cancel restore) | E slice for event/mode gaps; Jackin keeps capsule schema, entitlements, bundle contents, verifiers, signing identity; drop copy; no-clobber/conflict semantics already enforced by reconcile |
| L11 | sources/workflows/renovate.yml + renovate-validate.yml | Writer: schedule + dispatch(velnor/github/both lanes choice) on velnor labels, github only via manual dispatch; validator: push(main)+PR checks upstream customManagers sources + mise versions for both archs | renovate.rs writer + validator + cache exist; fail-closed gates verified live (reason → declares → trusted-label → velnor-lane). Copy's PR/push runs-on branches are DEAD (no such triggers) | existing-needing-extension (H) | H slice: (a) decouple writer execution from CI unit lanes — runners=github repos currently rejected, forcing a doubled CI surface via runners=both; (b) declarative dispatch lane override; (c) drop dead PR/push branches; (d) validate = generic trigger/shape, upstream-content checks stay Jackin named tasks; schedules/creds/targets/DCO + fleet trust facts stay Jackin declarations; drop both mappings |
| L12 | sources/workflows/reuse-compliance.yml (50 lines) | REUSE lint gate | No REUSE primitive in generator (grep empty) | covered by new capability (G) | impl-check-profiles: REUSE lint as a required tool-check profile (`reuse lint` via named task); Jackin keeps compliance content; drop copy |
| L13 | sources/actions/aggregate-needs | Needs aggregation with skip==ok | Weaker than generated required gate (selected+admitted must be success; F11) | obsolete/incorrect | Delete; generated ci-required replaces it; — |
| L14 | sources/actions/build-release-archive | zigbuild deterministic multi-target archives + sidecars | Release archive assembly exists (tarball/deb); deterministic metadata ≠ reproducible builds (document) | existing-needing-extension (E: declared archive contents, checksums, manifests, retention) | E slice; zigbuild invocation stays a Jackin named task; drop copy |
| L15 | sources/actions/cache-cargo-registry | Registry cache (offline-verify, online-retry) with hardcoded fuzz/lints/toolchain paths | cache.rs/snapshot/mise cover the shape; hardcoded paths violate scan-discovery | supported shape, copy incorrect | Use generated cache prep; B slice verifies lockfile discovery without hardcoded paths; drop copy |
| L16 | sources/actions/check-deployed-docs | Post-deploy docs verification | Only pages-verify job exists; no URL/health post-check | new capability (F) | impl-docs-compose post-verify stage; drop copy; URLs stay Jackin config |
| L17 | sources/actions/download-ci-xtask + 7-file JS runtime | Prepared-tool fetch: cache + first-unexpired exact-name + 120s wait + 14-tool completeness + cargo fallback; flaws: name+expiry acceptance (F3), wrong-key save (F4), lane input never read | runtime_products (owner-only, attest+smoke, never-overwrite) + strict acceptance (F3/F4 false for Velnor) | new capability via extension (B: prepared-tool handoff with exact-run prefer + producer/manifest/byte/ABI/trust validation + bounded taxonomy) | B slice; npm test never run → obsolete; drop copy + entire JS runtime; never preserve its acceptance semantics |
| L18 | sources/actions/download-codebook | Exact-name codebook artifact fetch | Same as L17 | same (B) | B slice; drop copy |
| L19 | sources/actions/sign-and-attest-archive | cosign + syft SBOM + attestations | Attest v4.2.2 + signer template exist; no cosign/SBOM step in native lanes | existing-needing-extension (D/E: signing/SBOM/attestation stages + verification) | docker-publish + E slices; signing identity/permissions stay Jackin config; drop copy |
| L20 | sources/actions/sign-capsule-manifest | jq + cosign capsule manifest signing (verified by jackin-image Rekor+SAN) | Capsule schema correctly absent from Velnor | Jackin-owned remainder + generic orchestration | Velnor orchestrates production/verification/signing; Jackin keeps schema, metadata, verifiers; drop copy; keep verifier in Jackin |
| L21 | misc | admission-closure fake never-triggered steps; hand-edited generated files (fallback hunk, pin swaps) | Velnor emits no fake steps; policy regenerates at pin | obsolete/incorrect | Delete fakes; generate real admission info (H slice); never hand-edit generated YAML; —; TBD |

## 3. Failure candidates (§5) — reproduce/disprove

| # | Candidate | Status | Evidence |
| --- | --- | --- | --- |
| F1 | Swift runtime/policy pins differ from generator pin | REPRODUCED | ci-unit-swift.yml source: 4× e05aee6 vs toml pin 541d8926 vs base pin b9c3156c; structural: frozen copies can't track the pin. Fix = generate, don't copy (L1). |
| F2 | FFI changes may not select Swift consumer | REPRODUCED then CLOSED as already-supported | Without declaration: FFI touch → 10 Rust units, `swift_matrix=[]` (gap real). Chain proven: xtask `FFI_CRATE_DIR="crates/jackin-usage-ffi"` → boltffi → `target/xcframework/JackinUsage.xcframework` → Swift binaryTarget; ci_task rebuilds it in-job. With `depends_on=["rust-jackin-usage-ffi"]` on the Swift row: FFI touch → 11 units incl. `swift-package-native`, `swift_matrix` non-empty. Caller keeps `needs: [plan, policy]` — selection coupling only, no forced serialization (correct: Swift rebuilds inputs in-job). Jackin remainder: declare the edge at migration. Locked by test `affected_closure_follows_cross_kind_depends_on_edges` (runtime.rs). No Velnor change needed. |
| F3 | Prepared xtask/Codebook/Docs/Construct reuse by name+expiry | COPY FLAW (Velnor already strict) | Stream G: Velnor acceptance requires manifest bindings + pre-exec digest match + full-closure equality + self-report tripwire (lib.rs:3890-3902, policy.rs:1462-1530); name+expiry is discovery only (lib.rs:3873). The weak reuse lives in Jackin copies (download-ci-xtask runtime index.js:76 first-unexpired exact-name, no producer/SHA/attest; codebook same). Migration: replace copies with Velnor verified handoff; do not preserve name+expiry acceptance. |
| F4 | Fallback xtask bytes cached under different contract | COPY FLAW (Velnor already strict) | Stream G: Velnor restore prefixes stay in compat class (snapshot.rs:184-198), save gate excludes exact hits (ir.rs:1724-1736), rustup exact key has no restore-keys. The wrong-key save lives in Jackin copy (download-ci-xtask action.yml:135 saves fallback under requested key; lane input never read). Migration: drop the copy behavior. |
| F5 | Digests omit decision logic / disagree with selection | FALSE for Velnor | Stream G: compat digest includes recipe/commands, toolchain, inputs (snapshot.rs:51-116, ir.rs:1000-1028); closure footer binds version+features+profile (closure.rs:108-121). No action unless a concrete disagreeing digest is produced. |
| F6 | Preview workflow_run provider vs assembly artifact-name mismatch | ABSENT in Velnor (no workflow_run trigger rendered) | Stream G/E: generator emits no `on: workflow_run`; binding model is signer-workflow checks + workflow_call source-ref admission. The mismatch is a Jackin-copy construction. Slice needed (area E): release/preview event bindings incl. workflow_run source-revision/producer binding — design from stream-E evidence, not from the copy's shape. |
| F7 | Default release validation skips substantive builds | FALSE for Velnor | Stream G: all release kinds compose full-scope unit jobs (release.rs:1900,2008,2371); CI_SCOPE full per unit (release.rs:1849-1854). The skipping validation is a copy construction. Migration: use release-kind validation. |
| F8 | Construct comparison lacks arch publisher | ABSENT in Velnor (no construct/compare mode) | Stream G: no construct mode in generator; image assembly requires both arch digests (release.rs:1229). Slice needed (area D): docker-publish capability (multi-arch, digest transport, single publisher, verified manifest, SBOM/provenance, resume) from stream-E evidence. |
| F9 | Preview signing capabilities vs job permissions | FALSE for Velnor | Stream G: attest permissions patched in (release.rs:1669-1672,1137,774-781). Copy flaw only. |
| F10 | Release repair swallows upload failures / weak conflict check | ABSENT in Velnor (no repair step; reconcile fails loud) | Stream G/E: existing-release path fails loudly on missing asset/digest mismatch (release.rs:1368); reconcile present (absent→create, coherent→noop, different→conflict-fail). Valid reconcile behavior already supported; the swallowing is a copy flaw to drop. |
| F11 | Aggregation accepts unexpected skips | COPY FLAW (Velnor already strict) | Stream G: plan/policy must be success (ir.rs:3673-3676); selected+admitted must be success (ir.rs:2997-3014); unselected fails on cancelled/failure; empty = explicit planned no-work (runtime.rs:1392-1397). The skip==ok lives in Jackin's aggregate-needs copy. Migration: generated required gate replaces the copy. |
| F12 | URL inputs interpolated into shell | FALSE for Velnor | Stream G: no URL-typed inputs; shell reached via env + quoted expansion or allowlist; curl URLs from repo-pinned microvm/pins.json. Adversarial tests still required (§6) for the new-surface inputs added by later slices. |
| F13 | Duplicated bootstrap / serial / long polling in copies | FALSE for Velnor | Stream G: single shared bootstrap (lib.rs:72-86,4413); parallel matrices fail-fast:false; callers need only plan/policy (ir.rs:3341); concurrency only on shared mutable resources; sole bounded poll is a real cross-run dep with fail-closed deadline (lib.rs:3860-3884). Copy inefficiencies are not requirements. |

## 4. Slices (§4 A–H coverage)

| Slice | Area | Content | Status |
| --- | --- | --- | --- |
| 1 | A (platform routing) | Collapsed kind verify jobs use `runner_for_unit`; Swift→macos on GitHub lane; Rust control stays Linux | DONE + independently verified sound: 488 lib tests pass, clippy/fmt clean, Jackin re-render (macos-26 Swift, ubuntu others) |
| 2 | A+C (FFI→Swift selection) | No Velnor change: declared cross-kind `depends_on` already selects + orders correctly | DONE (already-supported): live Jackin experiment + new closure test; Jackin remainder = one-line declaration at migration |
| 3 | G (check profiles) | Composable `[[check_profile]]` (schedule/lane/tools/tasks/deps/timeout/artifacts/thresholds/required-advisory) → `scheduled-checks` primitive + `nightly.yml` | MERGED + gated (561+ lib tests green incl. `scheduled_check_profiles` suite, clippy/fmt clean, genericity law holds). Covers L5, L7, L12 |
| 4 | D (docker publish) | `kind = "docker"` (image/dockerfile/context/platforms), native per-arch publishers + verified index, SBOM/provenance, `release verify-digests` | MERGED + gated: render pin recomputed to `920ae6e4` (= implementer's hash, byte-identical port), self-surface regen shows D-only diff (perms/provenance+sbom/platform-set check). Covers L4, partly L19. Pin later moved `920ae6e4` → `5aa5e076` by the D-1 fix (GHCR_IMAGE now YAML-quoted at all 7 sites); preview pin unmoved |
| 5 | F (docs pipeline) | `[docs]` contract + `docs-site` primitive → `docs.yml` (build, source/built-site link checks, spelling, Pages deploy w/ bounded retry, post-verify, scheduled external links) | MERGED + gated (incl. `docs_site_pipeline` suite). Covers L6, L16 |
| 6 | E (release events/modes) | Producer/mode/archive/credential bindings: `workflow_run` producer+revision binding, `validate/build/rehearse` dispatch modes (publish never dispatched), declared archive members/checksum/retention, credential setup+always-teardown, `resolve-mode/resolve-source/admit-producer/assemble-manifest` runtime | MERGED + gated: default output byte-identical (no regen needed, all legacy pins unmoved). Covers L8 modes/bindings/matrix, L9, L10, L14, L19 remainder, L20 orchestration; L8 second-file + version-policy deferred to E-followup (decisions memo Q2/Q2b) |
| 7 | B (prepared-tool handoff) | Exact-run prefer + producer/manifest/byte/ABI/trust validation + bounded taxonomy; lockfile discovery | MERGED + gated (phase 1 core + phase 2 wiring via 3-way merge, 31 conflicts; 676 lib tests green, clippy/fmt clean, self-surface regen byte-identical). Covers L15, L17, L18 |
| 8 | H (renovate/policy/maintenance) | Declarative schedules/creds, drop 10 excludes + fake steps, real admission info, bounded cleanup | MERGED + gated (H8a + remainder via 3-way merge; multi-schedule/targets/credentials/author/allowances, `unit_dependencies`/`unit_admission` admission info replacing fake steps). Covers L3, L11, L21 |
| 9 | A (platform/prereq model) | Capability-based placement (OS/arch/SDK independent of provider names), prerequisite producers, FFI→Swift XCFramework prep | MERGED + gated: `platform.rs` (`PlatformRequirement`, capabilities/products/prerequisites/env/mbx), Apple-executor split, `[[units]]` row-over-scan merge. Supersedes the §5 deferral note below: Linux-Swift is now expressible via capabilities, macOS only where declared/detected. Q4 migration declares recorded in decisions memo |
| 10 | C (selection/reuse/aggregation) | Unified input/dependency model, successful-result reuse validation, planner-expected aggregation | MERGED + gated (`reuse.rs`, closure tests). No new config keys (Q4: no Jackin declaration change) |
| 11 | G-followup (file-level `events`) | `events` as `scheduled-checks` declare-row arg (push/pull_request/workflow_dispatch); shared cron + declared set; cron-less files allowed only with `events` (validated in `select_profiles`, "one file, one trigger set"); PR-only-cancel on evented files; lanes stay per-profile | RENDERER VERIFIED (6/6 integration + 20/20 lib, incl. 2 new e2e + 9 new unit; ownership + logic independently reviewed). Config follow-through queued: `validate_check_profile_row` must allow missing `schedule` (select_profiles owns the file-level rule) + coverage-pre-check error label must name the file. Covers revised Q1 |
| 12 | E-followup (versioned-tool kind + version gate) | C6 CONFIRMED by trigger/job-graph diff vs ab5b0c4 jackin-dev.yml: NEITHER release (tag-driven) nor preview (rolling) fits; new KIND `versioned-tool` inside `release` family (gate+version+assert+matrix build+mutexed immutable publish), kind-conditional pin exemption, per-row name/prefix uniqueness, PR-only `validate-version` gate running declared named tasks | IMPLEMENTED + independently verified (77 release lib + 65 config lib + 7 e2e incl. writer regression; pin/name/5-job/M-1..M-4 hunks reviewed vs copy). Accepted: declare-only kind (no `[release]`-section support — migration uses declare rows); lane names lowercase (`velnor`, copy used `Velnor` — internal 2-day artifacts, glob-consumed); native-JSON runner configs (no fromJSON); build cells cold (no cache steps — perf follow-up, correct cold). Integration-owner hardening: single-writer upload gating (velnor-preferred `writer` flag + attest/upload gates — copy behavior E-2 missed). Covers revised Q2/Q2b; unblocks Jackin `[release]`-family declares |

## 5. Design notes / deferred items

- H8a renovate-lane decoupling (CHALLENGE MEMO, implemented below):
  generic? Yes — `[renovate] lanes = velnor|github|both` (default velnor)
  declares writer execution independent of CI unit lanes; no consumer names,
  labels, or secrets enter the generator (fleet facts stay declarations).
  Already supported? No — proven live: `runners=github` is rejected
  (config/mod.rs:1673), forcing a doubled CI surface via runners=both just to
  run a scheduled writer. Preserves behavior? Yes — velnor-only renders
  byte-identical output (locked by test); `both` reproduces the copy's LIVE
  behavior (velnor default + manual dispatch override to github) while dropping
  its DEAD PR/push runs-on branches (copy has no such triggers); writer stays
  gated to schedule/dispatch-on-default-branch (trusted events only).
  Bug class removed? Yes — lane-conflation: one `[workflow] runners` knob
  governing both CI units and the maintenance writer; plus silent fleet-down
  queueing (declared github allowance is now explicit instead of a frozen
  expression). Validator already github-lane; untouched.

- Capability-model refinement (distinguish Xcode/XCFramework-needing Swift from
  Linux-capable `swift build`): correctly DEFERRED with evidence. The generator
  has no Swift toolchain provisioning (no swiftly/swiftenv/mise-swift install
  path; grep negative) — Swift jobs rely on the image shipping Swift, which
  only macOS runners do. Kind-wide Swift→macOS is therefore necessary for
  correctness today, and both Jackin Swift units are Apple-bound (XCFramework
  binaryTarget; Xcode prototype). Revisit only when a consumer proves a need
  for Linux-Swift alongside a toolchain story. Slice 1 stands as the complete
  A-routing fix for this migration.
- Fuzz/bench/perf/alloc/coverage/Miri/mutation/beta/rust-analyzer/build-time/
  dylint/DinD-E2E/health-trend (area G), docs pipelines (F), Docker/release/
  preview/signing (D–E), Renovate/maintenance/policy (H): pending streams E/F.
- Mr. Boxington integration (§4A): ALREADY SUPPORTED, no slice needed.
  Evidence: pinned `jdx/mr-boxington-action` v1.3.0 + tool version 1.11.1
  (lib.rs:123,261-262), hosted store budget step (lib.rs:155-161),
  per-unit cargo routing through `render_tool_provisioning` with
  `cargo_cmd = "mbx"` (release.rs:956-961), `mbx_generation_bound` config
  (config/mod.rs:131), scanner `mbx_output_cache_justification` gates
  (scan/rust.rs:592). Generic configuration preserved; nothing Jackin-copy
  specific. A-prereq slice must not regress this routing.

## 6. Removal log

(nothing removed yet; #994 still OPEN, no Jackin migration started)

## 6a. Independent gap re-audit (2026-09-17, PR bytes vs ledger)

Read-only agent enumerated every behavior-bearing hunk of `ab5b0c4e`
(63 files, +13408/−51 confirmed) and hashed all 27 static sources
byte-identical to their dests. Ledger coverage substantively confirmed
(§53-item verified-covered list in the agent report); corrections below.

Row corrections (ledger wording the PR bytes refute):

- C1 → L3: `cache-cleanup.yml` is `workflow_dispatch`-only with a manual
  `ref` input; NO closed-PR trigger exists. The gh-purge half is
  dispatch-only; the merge-ref-cache pruning half is pre-existing base
  behavior (base `maintenance.yml` diff is pin-swap-only). L3 disposition
  stands (H bounded cleanup, no retry-without-progress); trigger text fixed.
- C2 → L4: no `buildx bake` invocation exists in `construct.yml` (builds
  run via `mise run construct-init-buildx/construct-build-platform/
  construct-push-platform`); `docker-bake.hcl` appears only in
  paths-filters. Wording fixed; D-slice disposition stands.
- C3 → L11: `renovate-validate.yml` (70 lines) has exactly 2 steps
  (checkout + mise-arch asset check); ZERO customManagers-source checks
  exist — only the header comment describes them. L11's "upstream-content
  checks stay Jackin named tasks" is revised: the copy preserves nothing
  here; the validator remainder is the mise-arch asset check only.

New dispositions (PR behaviors the ledger never named):

| # | Behavior | Disposition |
| --- | --- | --- |
| G1 | `[scan] exclude += ".github-gen/sources/**"` (keeps vendored runtime `package.json` from selecting a Node unit) | obsolete-with-sources: dies with the vendored runtime; no Velnor change |
| G2 | `[policy] exclude_workflows` 10-entry "#965 interim" exemption (12 static workflow mappings vs 10 excludes; `desktop-cadence.yml` has push/schedule/dispatch yet is not exempt) | obsolete: slice-8 drops all 10; replacement workflows conform (no exclusions). Migration step: verify zero excludes post-regen |
| G3 | Dead PR/push lane-branch arms beyond renovate (cache-cleanup PR arm, hygiene push arm + 13 runs-on, preview push-matrix arms, release `pull_request` arms — none in triggers) | obsolete/incorrect, delete (extends L11 dead-branch verdict to 4 more files; same bug class) |
| G4 | `download-ci-xtask` env contract (`CI_XTASK/CI_TOOLS_PATH/CI_XTASK_HIT/…`, 9 call sites) | obsolete-with-copy: dies with L17; prepared-tool handoff defines its own contract |
| G5 | Shared `homebrew-tap-publish` concurrency group across `jackin-dev.yml` + `preview.yml` | Jackin remainder: cross-workflow mutex is product deployment serialization; E-followup covers per-file single-writer gating only. Migration: keep ONE product-owned group if both publishers survive, else drop |
| G6 | Preview dispatch default `github` (vs `velnor` in all 10 other dispatch inputs) + `lanes != 'velnor'` routing | obsolete: Q6 `lanes_input` supersedes with per-family defaults; no `github`-default special case |
| G7 | `desktop-cadence` event split (dispatch = merge-only job, schedule = scheduled-only job; `lanes` input wholly ignored, static macos-26) | covered by G-followup `events` (one file, one trigger set) + per-profile lanes; static-macos + ignored-input shape obsolete |
| G8 | Preview fail-open error semantics (diff failure → rebuild; missing old_sha → rebuild; formula-fetch failure warn-only) | safe-direction copy choice; Velnor renders fail-closed gates by default. Jackin remainder ONLY if product requires rebuild-on-diff-failure: express as an explicit named-task policy, not a copy |
| G9 | Cache-save write gating (main/dispatch/same-repo-PR gates on registry save, result publish, mise `cache_save: main-only`, mbx save-on-dispatch) | covered: B-slice save gate + `publish_guard` (default-branch-only) + per-candidate head-branch check (F-1 fix). Copy's ad-hoc gates obsolete |
| G10 | Renovate per-run repo cache (`renovate-<os>-<run_id>` key + prefix restore, `/tmp/renovate` paths) | obsolete shape: H renovate `cache = true` renders the generic cache contract; copy's key scheme dies with it |
| G11 | `external-links` input on `check-deployed-docs` (default true, never overridden → post-deploy gate checks externals) | covered by F `verify_commands` (Jackin configures which deployed checks run); input plumbing obsolete |
| G12 | Construct lane×platform matrix (Velnor = amd64-only, arm64 via GitHub `ubuntu-24.04-arm`; BuildKit `mirror.gcr.io`; fork-safe login gating; `MISE_TASK_RUN_AUTO_INSTALL=false`) | platform routing: D-slice native per-arch publishers + A capability placement supersede the hardcoded matrix; mirror/login/task-env are Jackin Dockerfile/task config (consumer-owned, retained) |
| G13 | `Swatinem/rust-cache` in release builds alongside `cache-cargo-registry` | Jackin remainder: backend choice is consumer-owned cache config; Velnor neither mandates nor forbids it |
| G14 | Vestigial `writer` flag in reuse matrix (never read); capitalized lane names in matrices | obsolete: E-followup redefines `writer` as live single-writer gating; lane names lowercase (internal 2-day artifacts) |

## 7. Independent-verifier dispositions (`/tmp/verify-merged-report.md`)

Slices 1–2, G, F, D, E, H8a re-verified by an independent read-only agent:
gates green, genericity law holds, no consumer names, no PR-shape copying.
Findings and dispositions:

| # | Finding | Disposition |
| --- | --- | --- |
| F-1 (high) | docs result-reuse accepts by artifact name+expiry alone; digest covers `docs_paths` only, so a command-only change skips checks green; same-repo PRs can publish reuse artifacts | FIXED + independently verified: trusted-producer rule (`publish_guard` = default branch only; per-candidate `head_branch` API check) + receipt-based byte verification of restored results. docs suites green (8 e2e + 12 lib). Note: this was NEW-code weakness in `docs_site.rs`, not a contradiction of F3 (which covers Velnor's cache/compat acceptance, still strict) |
| F-2 (medium) | same hole allows stale-site deploy on default push (branch-protection bypass shape) | FIXED with F-1 (same trust rule + receipt; verified same suites) |
| D-1 (medium-low) | `release.image` reached YAML unquoted at 3 new + 4 pre-existing sites | FIXED + gated: `valid_docker_image` OCI-charset validator in config + `yaml_scalar` at all 7 render sites; pin `920ae6e4`→`5aa5e076` (release.yml only); regression test `valid_docker_image_accepts_references_and_rejects_injection` |
| E-1 (medium) | `binary`/`targets` interpolated raw into `assemble-manifest --subjects` shell (suffix-only target check admits `"; …` payloads) | FIXED + gated: `validate_release_naming` mirrors runtime `valid_package`/`valid_binary`/`valid_target` alphabets (runtime fns promoted to `pub(crate)`, single source); regression test with the verifier's exact payload |
| E-2 (medium-low) | tarball bindings on `docker\|crates\|pages\|homebrew\|apt` render success-with-omission | FIXED + gated: `validate_release_binding_kind` usage error (bindings require `rust-binary`/`native`); renderer omission branch kept as second layer |
| H8a-1 (low-medium) | `fromJSON('…')` single-quote breakout via `velnor_labels` | FIXED + gated: `json_string` escapes `'` as `\u0027`; regression test `renovate_writer_both_lanes_escapes_quotes_inside_from_json` |
| — | `resolve-mode` tests assert status, never stdout mode tokens | COVERED: `resolve_mode_resolves_each_event` asserts per-event mode tokens; M-6 adds wrong-name/missing-expected refusal text (`resolve_mode_workflow_run_admits_only_the_trusted_producer`) |

## 8. Matrix re-audit dispositions (`/tmp/mx-full.md`, read-only, 8 fixture renders + 25 runtime probes)

| # | Finding | Disposition |
| --- | --- | --- |
| M-1 (medium) | feature-branch dispatch rehearsal builds nothing (`trusted_release_runner_gate` dispatch arm requires ref==main; run goes green) | FIXED by E-2, integration-verified: rehearse arm passes off main while publish stays tag-gated (`release.rs` rehearse-arm comments + tests `the stable/rolling build must rehearse a feature dispatch`); E-2 suites green (77 release-lib + 65 config-lib + 7 e2e incl. writer regression) |
| M-2 (medium) | native identity lane without modes publishes OCI version index on dispatch-on-tag, orphaning an index tag-push admission then refuses (bricks release flow; self-surface shape affected) | FIXED by E-2, integration-verified: `{gate}`/`{mode_gate}` non-dispatch gating on image-platform/image jobs; same E-2 suites green |
| M-3 (medium) | native preview without producer publishes on dispatch-to-main (missing event check that tarball preview has) | FIXED by E-2, integration-verified: event check ported to native identity+publish (`if: push && ref==branch` on the native publish job); same E-2 suites green |
| M-4 (medium) | binary + native-non-debian publish lacks tag-immutability re-verification (`gh --verify-tag` is existence-only; moved-tag TOCTOU) | FIXED by E-2, integration-verified: ls-remote+commit re-check ported to all publish lanes (4 `ls-remote` sites); same E-2 suites green |
| M-5 (low) | curl retry/time-bound non-uniformity (mold: retry w/o timeouts; guest kernel: timeouts w/o retry). All sites fail closed (`--fail` + sha check) — robustness only | FIXED by integration owner: canonical `CURL_DOWNLOAD_FLAGS` (fail+resume+retry-inside-bounds) shared by mold download and guest-kernel script; deliberate exceptions documented in-tree (producer-outcome `--retry 0` under Rust loop, docs live-probe under shell loop); 3 regression tests |
| M-6 (low) | `resolve-mode` workflow_run arm admits any producer name (latent; currently unreachable from renders, violates the fn's own contract) | FIXED by integration owner: arm requires `--expected` with exact-name admission mirroring `admit-producer` (missing/wrong name refuses); 3 tests incl. the auditor's `EVIL` probe |
| hint: tag shapes | partly confirmed: odd tags fail closed (probed), moved-tag TOCTOU real (M-4) | covered by M-4 |
| hint: curl retry | confirmed as uniformity gap, refuted as safety hole | covered by M-5 |

## 9. Followup-question dispositions (Q6/Q7/Q8, integration owner)

| # | Requirement | Disposition |
| --- | --- | --- |
| Q7 | release tag filter is hardcoded `v*`; Jackin release tags need a narrower shape | DONE: declared `tag_pattern` (config `[release]` + declare arg), default `v*`; base/docker renderers share one trigger block; validator rejects empty/whitespace/multi-line; 4 tests incl. default byte-identity |
| Q8 | docker publisher can only log in to GHCR; Jackin construct publishes to Docker Hub | DONE: docker-only registry-auth triple (`registry` + `registry_username_secret` + `registry_password_secret`, all-or-nothing), shared `docker_login_step`, GHCR automatic-token default byte-identical (3× login count test); secret-name reuse of `validate_secret_name`; non-docker kinds refuse; 6 tests |
| Q6 | preview/docs/scheduled/maintenance render static github runs-on under dual lanes; no dispatch lane selection | DONE: shared `Args::flag` + `admit_lanes_input` (Both + labels + no-runner-group gate) + `lanes_runs_on`/`lanes_dispatch_inputs`/`lanes_input_entry` (no `both` option); preview/docs/checks/maintenance honor `lanes_input` with lanes-first inputs; preview refuses guest/macos/pinned-cell routing; checks enforce per-file lane homogeneity with macos always static; renovate migrated byte-identically (dispatch inputs + `json_string`); 2 shared + 7 preview + 3 docs + 7 checks + 4 maintenance tests |
| pins | legacy + identity `release.yml` pinned digests predate E-2's M-1..M-4 bytes | REBASED after byte-audit: dumped renders contain exactly E-2's intended gates (rehearse arm, non-dispatch image gates, native event gate, ls-remote re-check) over byte-identical Q7/Q8 refactors (verified per call site); preview/maintenance/signer pins held |

## 10. Landing log (Velnor PR #917, candidate path per #914)

- Policy evaluates new-schema configs only via the candidate product:
  self-surface `revision` names the generator commit; the base validator
  cannot parse new keys, so `[release] image_package`/`dockerfile`
  self-surface use moves to a post-merge follow-up. Runtime product for
  the generator closure is dispatched per #914 precedent (`release ...
  for the proof PR's generator`); two superseded closure releases are
  orphaned by later fix commits (immutable, content-addressed, unused).
- CI-found, fixed in-PR: (a) `migration_contract` plan probe inherited
  the CI job's repo-relative `VELNOR_SELECTION_FILE` (local/CI split —
  local runs never set it); test now scrubs it. (b) Q1 law-probe
  `include_str!` is a genuine new compile input the scanner truthfully
  traces into the `rust-velnor-workflow` mbx key; pinned
  pre-parameterization key rebased after byte-audit (sole delta is the
  probe file, already covered by the `tests/**` glob). (c) 75
  markdownlint errors across the new plans memos (table style/counts,
  list blanks/markers). (d) Migration-found generator bug:
  multi-word `scheduled-checks` `name` rendered
  `run-name: "Name words" · ${{...}}` (quoted part + trailing content =
  invalid YAML, aborts generation); the whole composed value is now
  quoted like the versioned-tool renderer, locked by a regression test
  verified to fail on the old composition. Only `check_profiles.rs`
  composed user input mid-line (`ir.rs`/`release.rs` sites use fixed
  vocabularies).
- Velnor-lane `operational_store` rejections (`Docker`/`Documentation`/
  `OpenTofu`/`Prepare Cargo` on Velnor) are PRE-EXISTING INFRA, not this
  branch: identical rejections on unrelated PR #916 (base-identical
  admission rows) and a Velnor failure on merged #914 (which landed with
  `ci-required` red). No Velnor-side fix in scope.
- Migration-flag triage (draft report): flag 2 (push-trigger widening)
  DISSOLVES — the desktop-cadence copy has no push trigger, so the
  faithful mapping is schedule+dispatch only (drop `events=["push"]`);
  main-only push triggering is a Jackin product decision, not a schema
  gap. Flag 3 (renovate `lanes="github"`, validator `--strict`,
  dropped mise allowance) and flag 4 (anonymous tool-download rate
  limits) are Jackin migration-review items, not Velnor bugs.
