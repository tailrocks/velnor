# PR #962 investigation report — `codex/github-first-hosted-g1-security-3ae`

Verdict: **SELECTIVE-PORT (scope: exactly `ea9686f0`, rebased + regen) — then CLOSE unmerged, do not merge.**
Recommended PR action: **close unmerged** (mergeable is CONFLICTING anyway); land `ea9686f0` as a fresh rebased commit if wanted.

## 1. Pins (verified 2026-09-21, after `git fetch origin --prune`)

- SRC (tip) `43ba3b414f245bf3aa9176afce5bf97f2d1e5235` — MATCHES expected `43ba3b41`. (`ls-remote` head ref = same SHA; `pull/962/head` = same.)
- MAIN `0dbcdb25f95881d8d76d79d91b518e8174c11b69` (`fix(workflow): aggregate docker release unit fan-out behind release-verified (#1049)`) — same as "was 0dbcdb25".
- MB `1048337062ea625fada1b4f7c07f2feed75f60c7` (`fix: make generated workflow rendering reproducible`) — shared with #960 as provisional said.
- PR: OPEN, not draft, `mergeable: CONFLICTING`, `mergeStateStatus: DIRTY`, `reviewDecision: ""`, labels none, closingIssues none. Created 2026-09-19T22:22:44Z (= frozen PR key ✓), updated 2026-09-20T08:01:06Z. 25 files +6194/-1352 (matches).
- 20 commits MB..SRC (18 non-merge + 2 merges). `git cherry main SRC MB` = all 18 `+` (zero patch-id carry — but 5 have verbatim re-lands under new SHAs, see §4).

Oldest-remaining: `codex/g1-current-generated-recovery` (#955), `codex/latest-macos-policy` (#957), `codex/g1-preview-amd64-dirty-identity` (#960), `codex/github-first-g3-integration` (#961) — all ABSENT from `ls-remote` heads and local tracking refs (pruned). Pull refs `refs/pull/{955,957,960,961}/head` still resolve (normal GitHub retention, not live branches). No reincarnation names among the 22 live remote heads. Live heads for parent's new-ref triage: `codex/{activation-foundation, ci-performance-campaign, ci-performance-next, github-first-g3-integration-signed, github-first-hosted-g1-security-3ae, rolling-preview-legacy-migration-20260920, schedule-actions-read, velnor-legacy-rolling-tag-repair-20260920}`, `fix/{apple-mise-tool-closure, ci-validation-contract, composable-regen-phases, desktop-candidate-evidence, native-product-closure, product-receipts, rust-cache-hit-bootstrap, s2-nested-bun-watch-scoping}`, `main`, `migfix3`, `red-main/velnor-pin-bump`, `refactor/holla-parity`, `rollout/cargo-bin-missing-flag`.

## 2. PR record (read fully)

- Body describes ONLY the pre-`ea9686f0` state (hosted recovery lanes, arm64 routing, preflight, regen + verification claims). It does NOT mention `ea9686f0` or the 6-commit runtime-containment stack pushed later — body is stale vs tip. The "Bootstrap note" correctly predicts the CI failure (old pinned renderer cannot parse typed `verification_providers`; "makes no G1 completion claim").
- Discussion: 1 bot comment (Codex review summary, reviewed `94b4357`). No human comments.
- Reviews: 1 bot COMMENTED review (no findings in summary). No approvals, no human reviews.
- Inline threads (2, both `chatgpt-codex-connector[bot]`, P1, unreplied):
  1. `s2/primitives/release.rs:70` @tip — unconditional `uname -m == aarch64` probe breaks x86 self-hosted cross-toolchain configs. ADDRESSED at tip: probe gated on `release_provider(config) == ProviderId::GithubHosted` at all 4 call sites (lines ~1382/1798/3073/4116, via `2fad9a2d`'s `enabled` param).
  2. `release.rs:2426` @`94b43578` — fixed `ubuntu-24.04-arm` bypasses `[workflow.selectors.github-hosted]`. ADDRESSED at tip: `ARM64_HOSTED_RUNS_ON` has ZERO uses at tip; all arm64 placement flows through configurable `arm64_runs_on` (`hosted_arm64_selector_runs_on`). — Both moot: the native direction itself is contradicted by main (§4).
- Linked issues/specs: none.

## 3. CI state (re-verified on CURRENT head; Policy logs read)

- Tip `43ba3b41`: exactly 2 check-runs — Policy FAILURE, DCO SUCCESS. No matrix on tip.
- CORRECTION to provisional "matrix never ran": the matrix DID run on earlier heads (`94b43578`: 70 check-runs; `ea9686f0`: CI/PR run `35477443890`). All hosted lanes SUCCESS except `rust-velnor-workflow` (+ dependent `ci-required`, `Control/Required`). Velnor lanes SKIPPED.
- Root cause of ALL red (single, procedural — NOT a content rejection): `verification_providers = ["github-hosted"]` (`.github-gen/velnor-workflow.toml:74/75`) is unparseable by the pinned old renderer (`0dc79895`): `unknown field verification_providers` → `rust-velnor-workflow` lane fails at `--check` regen → no candidate product published → Policy falls to candidate path → `no candidate product velnor-workflow-candidate-7bd39c46… was published within 15 minutes` → exit 1 (6 identical Policy timeouts, incl. run `35498407618`/job `106045609022`). No content test failure was EVER observed on this PR.
- The 6 runtime commits (`f90515f3`..`43ba3b41`) triggered ONLY `pull_request_target` Policy runs, no `pull_request` CI/PR runs — consistent with the branch becoming unmergeable (DIRTY now; `pull_request` needs the `962/merge` ref, `pull_request_target` doesn't). Their code never ran in CI at all.
- Policy-red is therefore a bootstrap chicken-and-egg the PR body discloses — procedural, not a direction veto. (Moot now: main parses the field.)

## 4. Delta evaluation vs CURRENT main (per commit, grouped)

### SUPERSEDED — re-landed on main under new SHAs (5 commits, do not port)

| Branch | Main re-land (later date, same subject) | Evidence |
|---|---|---|
| `a38e459c` ci: select hosted recovery lanes | `aa435f5d` | Same intent on evolved base + regen; main toml has `automatic_providers = ["github-hosted"]`; `default_dispatch_providers` key no longer exists on main (moot) |
| `1c5eb2aa` feat: make release verification lanes explicit | `75a1f31b` (code) + `3b1b1e1d` (toml) | release.rs hunks IDENTICAL modulo 1 drifted hash `-` line (407 lines each); config `+92` both; mod.rs 12-vs-22 only because main's `apply_release` was already fallible. `3b1b1e1d` message: "Port the deferred … flip from 1c5eb2aa (see phase-1 porting note)" — deliberate phase-1 port |
| `d70a88a2` bind hosted producer admission to source identity | `1d7fcc00` | Identical stats (67/18/155/504) minus dropped fixture `+2`; fixture works on main without (evolved via `dc150d2f`) |
| `3aecc6ed` complete hosted producer admission graph | `6aef43d9` | Sorted-line diff of full patches = EMPTY (byte-identical content) |

### SUPERSEDED — replaced by main's later re-architecture (1 commit)

- `525fc9e0` bind hosted tarball preview target: main's preview-publish rewrite uses atomic `gh release delete` + `gh release create preview --target "$COMMIT"` (identity commit) with version-guard rails — no `gh release edit` path remains in the producer flow. The branch's edit/create string-replace fix targets a deleted mechanism.

### CONTRADICTED by main design/tests — do not port (4 + 6 commits)

Native group (`73ecac5f`, `242eab75`, `423c2792`, `2fad9a2d`):
- Main cross-builds aarch64 release binaries/debs on x86 and TESTS it: `cross_linker_env` (`aarch64-linux-gnu-gcc` env, asserted at release.rs:6955/7038), `Install cross C toolchain` step (asserted present :6864/6943/9692), main `release.yml` = ALL `runs-on: ubuntu-24.04`.
- Branch REMOVES that step and asserts ABSENCE (`!preview.contains("libc6-dev-arm64-cross")`, :8580). Direct test-level contradiction.
- `2fad9a2d`'s `validate_release_arm64_selector` ERRORS on any hosted+aarch64 config lacking the new `arm64_runs_on` key — a breaking config change; main has 0 hits for `arm64_runs_on` and a comment stating the pinned policy runtime cannot approve novel `matrix.runner` values. Main routes aarch64 natively ONLY in guest-agent/image lanes (pre-MB #841 + #1028/R2), deliberately.

Runtime-containment stack (`f90515f3`, `c70428c9`, `5b61841d`, `225c800a`, `d97c29b5`, `43ba3b41`; all symbols 0 hits on main):
- Main merged the opposite design as #985 (`c832191f`, new `exec.rs`, both runtimes delegate to `exec::run_command`). Module doc: "silence must never fail a command by itself" + test `silent_past_stall_warn_still_succeeds` vs branch test `silent_command_past_stall_limit_fails_naming_the_stall` — direct semantic veto.
- Observable behaviors covered on main: own process group per child, SIGTERM→grace→SIGKILL→bounded reap, pump threads, descendant termination (`descendant_killed_with_group`, `term_ignoring_descendant_killed_with_group`, `closed_streams_live_child_bounded_by_wall`). Branch's s2 `run_command` anchor no longer exists on main (only `exec.rs` defines `fn run_command`). `rustix` dep independently adopted by main. Nothing separable to port.

### NOVEL — the single port candidate (1 commit)

- `ea9686f0` fix(generator): honor declared release verification providers (+181/-5, one file). Main's `declared_spec`/`declared_preview_spec` still hardcode `verification_providers: None` (main release.rs:331/:465 — the exact lines this commit changes); `declared_verification_providers`, `validate_declared_verification_providers`, `config_with_release_spec` all 0 hits on main. All port anchors present: `provider::{parse_provider_set, require_non_empty, require_subset}` ✓, `ReleaseSpec.verification_providers: Option<ProviderSet>` ✓ (mod.rs:1051), `release_verification_providers()` reads `config.release.verification_providers` ✓ (so `config_with_release_spec` plugs in), Release/Preview `render` bodies preserve the insertion shape ✓ (release.rs:172-230). Comes with fail-closed tests. PORT SCOPE: rebase this commit's semantic hunks (schema keys + parse/validate + render-config threading + tests) onto current main + regen; verify `declared_release_verification_lanes_are_exact` passes. FLAG at port time: validation requires subset of `[workflow] providers` (both lanes), not of `[release] verification_providers` (hosted-only per `3b1b1e1d`) — a declared stanza could name `velnor` while the release contract is hosted-only; parent decides whether to tighten.

### REJECT as stale/unproven (1 commit)

- `9ae363b3` gate every hosted preview consumer: release.rs consumer gates use the pre-mode `admitted`-only form while main's gating is mode/event-aware (`mode == 'publish'`, workflow_run-qualified); `identity_debian_needs`/`validate_admitted_run_identity`/`source-run-id` novel (0 hits) but marginal hardening on an already-strict admission check, no failing test on main, and main's publish flow moved on. Not recommended.

### Generated (not portable by definition)

- `94b43578` (+ state/project/ci-main/ci-pr/nightly + `2fad9a2d` state line): preview.yml +2010/release.yml −926 render the branch's OLD publish architecture; main went the opposite way (`3b1b1e1d`: −957/−942 by removing velnor jobs; atomic publish). State pins differ (generator 53 vs 69). Never hand-port: regen from source after any port.

### Merges — clean

- `b62c868e` (^2=`b5a4b4af`), `da521868` (^2=`10483370`=MB): pure main-syncs. Net file list == non-merge-commit file union (no merge-only files); no evil content.

### Rejected-family overlap (`b-cases`, `native`, `skills`, `transport`)

- None claimed by this verdict; spot checks confirm none: `fix/native-product-closure` (different domain: product inputs/selection, platform/swift) carries ZERO of the 18 patch-ids; branch has no `skills.rs`/b-case content; "transport" appears only inside the contradicted runtime stack (and #985-`product_transport` is the thing that contradicts it, not an overlap). No blob/patch-id samples indicate family membership.

## 5. Subsumption vs later-queued branches (both directions: none)

- #963 (`056362aa`): `cherry` = all 18 `+`; file overlap only incidental (state/project/Cargo/s2/mod/scan — no release.rs/runtime.rs).
- #978 (`970a6dd5`), #979 (`9bbf4a4e`), #980 (`ab2f12fa`): all `+`; zero hits for all four novel-symbol probes (`declared_verification_providers`, `arm64_runs_on`, `run_command_with_stall_guard_to`, `validate_admitted_run_identity`) on all three tips.
- Trio (b56–b58): no live refs (only merged-port commits reference b56, e.g. #1047). Nothing to compare.

## 6. Operational use / tags / bundle

- Tags on tip: none. `branch -r --contains`: own branch only. Branch-name refs in main `.github/`/`docs/`: 0.
- `/tmp/velnor-recovery-20260921.bundle`: `verify` = ok; tip `43ba3b41…` present EXACTLY as `refs/remotes/origin/codex/github-first-hosted-g1-security-3ae`. Covered — no per-PR evidence bundle needed, no main-bundle gap.

## 7. Bottom line

5 commits already on main via deliberate re-lands, 1 superseded by re-architecture, 10 contradicted by main's tested designs (#985 exec.rs; cross-built aarch64 releases), 1 stale/unproven, generated output unportable — except ONE clean gap-fix (`ea9686f0`, declared-stanza `verification_providers`, with tests, all anchors present). Nothing else on this branch should survive. Close the PR unmerged; optionally land `ea9686f0` rebased (note the §4 subset-validation flag).
