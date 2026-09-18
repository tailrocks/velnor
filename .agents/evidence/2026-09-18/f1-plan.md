# F1 Execution Plan — Jackin migrated + routing/release/E2E repaired (bastion campaign)

Status: PLAN ONLY — F gate not reached, zero writes to `jackin-project/jackin`.
Authority: spec §9.1 (`plans/bastion-three-provider-ci/spec.md:319-329`);
checklist F1; work-plan F1 + §0.8 (`work-plan.md:43-45`);
inputs `/tmp/f1-e2e-probe.md`, `/tmp/f1-release-prep.md`,
`/tmp/f1-swift-prep.md`, `/tmp/f1-tools-probe.md`, `/tmp/a0-consumers.md` §2.
Baselines: jackin live main `0be3fcf9` (1 commit ahead of audited `92f347ac`);
9 workflows; 40 units (36 rust / 1 bun / 1 docker / 2 swift);
`project.toml` byte-identical across the drift gap.

Hard constraint (spec §9.1, last para): ONLY authorization, generic scope
entries where needed, and typed generator configuration are added.
No per-Jackin host architecture. No new daemon/VM/pool/script/copied workflow.

---

## Step 0 — Gate preconditions (verify, do not skip)

1. E gate signed (E1+E2 Velnor qualified) — F1 starts only after E.
2. F gate reached before ANY jackin write (all prep to date is read-only).
3. Re-read live jackin `main` SHA + `project.toml` + workflow set at execution
   start; if drifted from `0be3fcf9`, revalidate the §2 ledger deltas before
   proceeding (same `gh api` read-only method as A0).

## Step 1 — Generic fix FIRST: Swift→macOS routing (§0.8 path, before Jackin)

Spec: "Generic native routing to actual Apple Silicon macOS is fixed [...]
Generator/Velnor fix first, then Jackin continues."
This is a generic generator bug, so it travels the §0.8 path:

1. Fix in velnor3 `crates/velnor-workflow/src/primitives/ir.rs`
   `render_collapsed_kind_verify_job` (~:3936): mirror the trusted-Velnor
   pattern — `sample = github_members[0]`, `runs_on =
   runner_for_unit(RunnerMode::Github, sample)`. Sound: single-kind
   membership guaranteed by `render_kind_unit_workflow`. Non-Swift kinds
   resolve identically (fall-through to `runner_for`). No new config
   surface (`[workflow] macos_runner` exists, validated, Jackin declares
   `macos-26`); no command changes.
2. Add tests with the fix (prep §4): collapsed-path Apple routing test
   through `render_kind_unit_workflow(UnitKind::Swift)` — `verify-github`
   is `runs-on: macos-26` (custom) / `macos-15` (default), no
   `verify-velnor` job; plus non-Swift regression (rust/bun/docker
   collapsed files keep `runs-on: <github_runner>`). Home:
   `crates/velnor-workflow/tests/lane_pairing.rs` next to
   `both_allows_swift_with_explicit_github_jobs` (which pins structure,
   not `runs-on` — no existing test pins the buggy Ubuntu label).
3. Publish the fixed generator/Velnor package FIRST (tooling published
   before consumers repin, §0.8).
4. Requalify Velnor for that change: affected author/verifier pair re-runs
   the Velnor proofs (F2.4). No full campaign restart for this unrelated
   change; no unverified mixed-version state.
5. Only then continue with Jackin below.

Non-goals: no Velnor-lane Swift (`lane_supports_unit_kind` stays — Velnor
has no macOS fleet); no new runner labels; no `release.yml` restoration;
no release enabling (separate clause, Step 6).

## Step 2 — Authorization + scope + typed config (only additions)

1. Authorize the jackin repo (trust/access verify per §9.3 onboarding).
2. Add a declarative GitHub scope entry ONLY if necessary.
3. Typed generator config only: keep/confirm `.github-gen/velnor-workflow.toml`
   deltas (3 unit overrides; `macos_runner = "macos-26"`; the drift commit's
   `ruleset_external_status_checks = ["DCO"]` is already in config — regen
   absorbs it, never hand-sync). No per-Jackin host arch, no hand YAML.

## Step 3 — Published pin selection rule

- Select an ALREADY-PUBLISHED generator pin (never a local/unpublished
  revision), and it MUST be ≥ the Step-1 published fix (routing fix
  included) — else regen reproduces the Ubuntu Swift bug.
- Record pin + source identity in evidence. Tooling-published-before-repin
  (§0.8) is the ordering proof.

## Step 4 — Regen + ownership/policy/lint proofs

1. Regenerate the full tree from the Step-3 pin (covers the §2.3 staleness:
   config changed without regen at live main; live Policy `generated-tree`
   is expected to fail on exactly this drift class).
2. Expected diff: `ci-unit-swift.yml` `verify-github` → `runs-on: macos-26`,
   everything else structurally identical (plus the DCO-checks regen).
3. Proofs, all green: generator-ownership (every `.github/workflows/*`
   generator-owned, zero hand YAML); Policy `generated-tree` passes against
   the new pin; actionlint clean (allowlist already covers `macos_runner`
   for Swift); Policy + Planning `success` (run-3 baseline state preserved).
4. Cache keys separate automatically (`runner.os`/`runner.arch`); Policy
   trusted-runners/action-pins unaffected (hosted label, no new actions).

## Step 5 — 40-unit ledger (38 Linux×3 + 2 Apple Swift + E2E/controls)

- Baseline: 40 `[[unit]]` in `project.toml` (36 rust, 1 bun, 1 docker,
  2 swift), `runners = "github"` hosted-only — revalidated byte-identical
  audited→live (§2.2). 9 workflows identical at both SHAs (§2.1).
- Count rule: 38 Linux-oriented units × 3 providers = 114 executions;
  2 Swift units are genuine hosted macOS (`macos-26`) executions.
  NEVER 120 Linux — Swift-on-Linux counting is forbidden.
- Same-job chain preserved: `mise run swift-package-native-ci` =
  `desktop-xcframework` → `swift build` → `swift test --parallel` on ONE
  macOS-26 (Apple Silicon) runner (xtask `require_macos` is a
  compile-time host check; `target/xcframework` is not shared across
  jobs). Prototype unit's raw `swift build/test` lands on macOS via the
  same shared `verify-github` job.
- E2E/control jobs are ADDITIONAL and explicit (Step 7), counted separately
  ("before explicit additional E2E/control jobs").
- If any unit count/coverage differs at execution-start re-read, record a
  coverage-equivalent correction — never silent.

## Step 6 — `release.yml` reconciliation (source-side, releases stay disabled)

Per `/tmp/f1-release-prep.md` (live-main design; lands at F1, not before):

1. Rewrite `release_workflow_invokes_canonical_mise_tasks`
   (`crates/jackin-xtask/src/desktop/tests.rs:223-245`, same test name) as
   a POLICY-DRIVEN test keyed on generator-owned
   `.github/ci/project.toml` `[release].enabled` (fail-closed: missing key
   fails the test, never defaults enabled):
   - Disabled branch (current, must pass): assert `release.yml` does NOT
     exist + anti-rot pin — `mise.toml` defines all four canonical tasks
     (`desktop-build`, `desktop-verify`, `desktop-sign-notarize`,
     `desktop-release-state`) each delegating 1:1 to `cargo xtask
     desktop …` (reuse `task_block`/`assert_subsequence`).
   - Enabled branch (future, dead path today): current body verbatim
     (four `mise run …` present, four raw restatements absent).
2. Forbidden: restoring `release.yml` (contradicts #992 + generator
   ownership); setting `[release] enabled = true`; deleting the test;
   touching `preview.yml`; any workflow regen for this change.
3. Verify: full xtask suite green (303 incl. rewritten test); negative
   check — stub `release.yml` must FAIL the disabled branch (proves the
   absence assertion is live).

## Step 7 — Tool re-resolution (no silent substitution)

Per `/tmp/f1-tools-probe.md` — all five historic tools GO, no registry
removal:

| Historic | Resolution |
|---|---|
| Rust 1.97.1 | GO (both linux arches 200). Repo already ahead: `rust-toolchain.toml` = 1.98.1 = current stable. Campaign pins 1.97.1 exactly or follows 1.98.1 — recorded, never silent. |
| Bun 1.3.14 | GO (live signed asset; latest 1.4.2). Note repo drift (`mise.toml` 1.4.0 → latest 1.4.2) as SEPARATE tested-update, not campaign smuggling. |
| Node 24.18.0 | GO (dist 200 + index.json). v24 line tip 24.21.0 / overall 26.9.0 noted; change only as tested-update. |
| ubuntu-26.04 | GO (PREVIEW) — listed in runner-images + docs, but preview; GA is 24.04. Flag in record; fallback `ubuntu-24.04` if preview breaks. |
| macos-26 | GO (GA; `macos-latest` → 26 arm64). Use directly. |

Rules: a different Linux userspace is never silently forced;
Apple-only mise tools are never installed on Linux. Every version delta
from historic is an explicit, separately-tested update in the
tool-resolution record.

## Step 8 — Explicit `docker-e2e` (capsule ELF + serial + 20-fanout, zero weakening)

Current state (§2.5): `[profile.docker-e2e]` defined (4 binaries, serial
`test-group docker-e2e`, `max-threads = 1`) but INVOKED BY ZERO workflows —
all Rust units run default profile (e2e excluded). F1 adds the explicit
execution (one top-level job per work-plan F1.7):

1. Build the capsule Linux ELF FROM THE TESTED SOURCE in-job: install
   `zig` + `cargo-zigbuild` (+ `rustup target add`), run
   `cargo run --bin build-jackin-capsule -- --export`, eval
   `JACKIN_CAPSULE_BIN` into env. Same checkout (`workspace_root` walk),
   `cargo zigbuild -p jackin-capsule --target <arch>-linux-gnu.2.17`.
   NO preview-release substitute (contract panic text forbids it).
2. Run `docker-e2e` profile with `e2e` enabled. Gating contract
   (`dind_e2e/common.rs`): `JACKIN_CAPSULE_BIN` set + is-file +
   executable bit + ELF magic; `docker info` (honoring `DOCKER_HOST`);
   `docker buildx version`; `script(1)` on PATH. Prove in the job:
   Docker, Buildx, Compose where used, `script(1)` PTY, nested privileged
   DinD, Java Testcontainers, TLS/no-proxy behavior, temporary relay
   socket/file mounts.
3. Serial groups preserved: `docker-e2e` group `max-threads = 1`
   (`.config/nextest.toml:35,67-69`) + in-test `e2e_serial_lock()`.
4. 20-fanout exercised via the EXISTING suite — no new fanout code:
   `usage_broker_desktop_and_twenty_docker_capsules_make_one_provider_call`
   (`usage_broker_e2e/docker.rs:201-202` → `assert_desktop_capsule_singleflight(20)`;
   20× `python:3.14-alpine` capsules, barrier-synced, single provider
   call). Note: NO `dind_e2e*` source contains the fanout (zero `20`
   occurrences); host-process `CLIENTS = 20` (`usage_broker_e2e.rs:26`)
   is the sibling singleflight proof.
5. `per_mount_isolation_e2e.rs` disposition: STAYS in `default` (current
   placement correct — hermetic: `ScriptedRunner`, `NoOpDocker`, no
   daemon/ELF/PTY/serial need; `_e2e` suffix is a misnomer). Optional
   rename to drop the suffix. FORBIDDEN: moving it to `docker-e2e`
   (needless serialization + Docker host) or excluding it from default
   without adding to `docker-e2e` (silent coverage drop).
6. ZERO fixture weakening: no timeout inflation, no fanout reduction, no
   quarantine-ledger gaming (unquarantined flake fails review), no
   `|| true` lanes to make green.

## Step 9 — Generic-bug §0.8 path (standing rule during F1)

If ANY generic bug surfaces during Steps 2–8: stop Jackin work → fix/publish
the generator or Velnor package → requalify Velnor via the affected
author/verifier pair re-running the Velnor proofs → then continue Jackin.
Tooling published before repin; no repeated full campaign restart for
unrelated changes; no unverified mixed-version state. (Step 1 is the first
instantiation of this path.)

## Done criteria + evidence bundle

Done when checklist F1 holds: E gate signed first; authorization + scope +
typed config only; published pin ≥ Step-1 fix; tree regenerated + owned +
policy/lint clean; 40-unit ledger (114 + 2 macOS, never 120 Linux);
same-job XCFramework→Swift on macOS-26; tools re-resolved without silent
substitution; `release.yml` reconciled without restore/enable; explicit
`docker-e2e` with same-source ELF + serial + 20-fanout + zero weakening;
generic bugs via §0.8. Evidence: migrated tree + ledger, routing/release
diffs, explicit E2E proof, tool-resolution record.
Verifier: Jackin/platform/E2E verifier.
