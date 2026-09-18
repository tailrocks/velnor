# A0 consumer-research ledger: later-repo read-only inventory

Date: 2026-09-17. Scope: read-only (`gh api` + `git ls-remote` only; zero clones).
Mutation of these repos is FORBIDDEN until their gates (B2 velnor-apt, F jackin, G ChainArgos).
Authority: `plans/bastion-three-provider-ci/spec.md`; inputs from `evidence.md` §§1–7.
Audited SHAs below are revalidation inputs, never pins.

Live mains (all three agree between `git ls-remote HEAD` and `gh api .../git/ref/heads/main`):

| Repo | Live `main` SHA |
| --- | --- |
| `tailrocks/velnor-apt` | `d62820d47a3814e98c4e25512151b4af14e3c3e1` |
| `jackin-project/jackin` | `0be3fcf95cd33c14fd3fe47af08026f17135a157` |
| `ChainArgos/java-monorepo` | `38a8fb5777fd02fec8b3904d86387aba05940e9a` |

---

## 1. `tailrocks/velnor-apt`

### 1.1 Main SHA + tree listing

- Old observation: no audited SHA recorded for velnor-apt in evidence §1 (only the
  omission-notice blob); last live-refresh (§7) said only "APT omission notice still present".
- New evidence: live `main` = `d62820d4` (2026-09-16T15:30:26Z,
  `chore: sync velnor-workflow to b9c3156c (#238)`). Recursive tree at that SHA is
  complete (`truncated: false`, 33 entries): `.github-gen/` (omission notice +
  `velnor-workflow.toml`), `.github/` (`AGENTS.md`, `actionlint.yaml`, `ci/`,
  `workflows/ci-unit-docs.yml` only), `conf/distributions`, `config/fleet/`,
  `scripts/` (`package-update.sh`, `verify-release.sh`, tests), `package-state*.json`,
  `velnor.gpg`, root docs. Generator revision in `velnor-workflow.toml` = `b9c3156c…`.
- Consequence: B2 baseline is `d62820d4`, not an unknown ref. Tree is small enough
  that B2's atomic pin+tree promotion diff stays reviewable.

### 1.2 Omission-notice blob SHA

- Old observation: `NO_WORKFLOWS_REQUIRED.md` blob `3172bb883ec343a676d82c2594cb1a399191bf07`
  declaring APT publication primitives absent (evidence §5 "APT gap").
- New evidence: blob SHA at live `main` is byte-identical: `3172bb883ec343a676d82c2594cb1a399191bf07`
  (size 809, verified via tree entry AND direct `git/blobs` read). Content still says
  `apt-repository` is a descriptive profile label only, typed config produces
  `ci-unit-docs.yml` only, and lists the same omitted set (`ci.yml`, `ci-apt.yml`,
  `publish.yml`, `package-update.yml`, `package-updater.yml`, `renovate.yml`,
  composite `aggregate`/`cache-contract`/`run-gate`).
- Consequence: no drift. B1 (typed APT primitives) remains genuinely unstarted
  upstream; B2 must still land the first generated APT coverage before removing
  the notice. Do not remove the notice on a docs-only change.

### 1.3 Any APT primitive workflows present?

- Old observation: none — typed config produces `ci-unit-docs.yml` only.
- New evidence: `.github/workflows/` at live `main` contains exactly one file,
  `ci-unit-docs.yml` (a `workflow_call` docs unit). No `ci-apt`, `publish`,
  `package-update(r)`, or `renovate` workflow exists. `project.toml` confirms
  `files = ["ci-unit-docs.yml"]`, single `docs` unit, `release.enabled = false`.
- Consequence: unchanged. The B1 renamed-fixture + negative-test gate has a clean
  negative baseline: any APT workflow appearing before B1's reviewed primitives
  would be hand YAML and must be rejected.

---

## 2. `jackin-project/jackin`

Audited SHA `92f347ac39fbf0d6f9853168e2896a6c60522924` (2026-09-16T15:22:10Z,
`chore: sync velnor-workflow to b9c3156c (#993)`) exists. Live `main` `0be3fcf9`
(2026-09-16T23:27:33Z, `fix: heal wiped cache mounts … (#995)`) is exactly
**1 commit ahead** (`status: ahead, ahead_by: 1`).

### 2.1 Nine workflows

- Old observation: 9 workflows (evidence §1).
- New evidence: identical 9-file set at BOTH audited and live SHAs —
  `ci-main.yml`, `ci-policy.yml`, `ci-pr.yml`, `ci-unit-bun.yml`,
  `ci-unit-docker.yml`, `ci-unit-rust.yml`, `ci-unit-swift.yml`,
  `maintenance.yml`, `nightly.yml`. The drift commit touches zero files under
  `.github/workflows/` (43 files changed, none there, none in `project.toml`).
- Consequence: F1's "9 workflows" ledger entry holds at live `main`. Workflow
  regeneration at F1 starts from a stable 9-file surface.

### 2.2 Forty-unit manifest re-read (counts by kind; IDs not enumerated)

- Old observation: 40 units: 36 Rust, 1 Bun, 1 Docker, 2 Swift; hosted-only
  defaults; full per-unit table unknown — do not invent IDs (evidence §§1, 4).
- New evidence: `.github/ci/project.toml` at audited SHA contains 40 `[[unit]]`
  entries: **36 rust, 1 bun, 1 docker, 2 swift** — exact match. Live-`main`
  `project.toml` is byte-identical (`diff` exit 0, same 40 / same kinds).
  `runners = "github"` (hosted-only defaults) confirmed in both manifests.
  The single docker-kind unit is rooted at `docker/construct` (plain image build).
  `.github-gen/velnor-workflow.toml` holds only 3 unit-override entries (typed
  deltas), not the inventory — the inventory lives in `project.toml`.
- Consequence: the 40-unit / 38-Linux + 2-Swift baseline (spec §9.1: 114
  three-provider executions + 2 genuine macOS units) revalidates exactly at live
  `main`. No coverage correction needed at A0.

### 2.3 Drift inside the 1-commit gap (config changed, tree NOT regenerated)

- Old observation: none — drift window did not exist at audit time.
- New evidence: the single commit `92f347ac → 0be3fcf9` modifies
  `.github-gen/velnor-workflow.toml` (`ruleset_external_status_checks` shrinks
  from `["DCO","construct-required","docs-link-check","docs-required","validate"]`
  to `["DCO"]`) plus the generator-state input hash — but does NOT touch
  `project.toml` or any workflow. Generated tree is therefore stale relative to
  its own config at live `main`.
- Consequence: live Policy `generated-tree` is expected to fail on this drift
  (same signature class as Velnor run `35129353335`). F1 must regenerate from
  the F-gate published pin rather than hand-syncing; no action before F.

### 2.4 `release.yml` state

- Old observation: desktop test read a deleted `release.yml` (run `35114867283`);
  spec §9.1 requires reconciling the obsolete assertion without restoring legacy
  YAML or enabling releases.
- New evidence: `.github/workflows/release.yml` is 404 at BOTH audited and live
  SHAs; recursive tree at live `main` shows no workflow of that name (only
  source-side `release_*.rs` modules, `release.toml`, docs). `project.toml`
  `[release]` is `enabled = false` (fail-closed) at both SHAs.
- Consequence: unchanged. The F1 release-reconciliation obligation stands as
  specified: fix the source assertion, keep releases disabled, never restore
  the file.

### 2.5 `docker-e2e` presence and execution state

- Old observation: default nextest excluded `dind_e2e`, `session_send_e2e`,
  `usage_broker_e2e`, `load_options_e2e`; spec §9.1 requires explicit
  `docker-e2e` execution with `e2e` enabled, same-source capsule ELF,
  `JACKIN_CAPSULE_BIN`, serial groups, 20-capsule fanout.
- New evidence (all identical at audited and live SHAs):
  - `.config/nextest.toml` defines `[profile.docker-e2e]` (filter = the 4 e2e
    binaries, serial `test-group docker-e2e`, `max-threads = 1`) and the default
    profile excludes exactly those 4 binaries. Revalidates the audit verbatim.
  - E2E sources exist: `crates/jackin/tests/{dind_e2e.rs + dind_e2e/,
    load_options_e2e.rs, session_send_e2e.rs, usage_broker_e2e.rs + usage_broker_e2e/}`.
  - `dind_e2e/common.rs` hard-requires `JACKIN_CAPSULE_BIN` pointing at an
    executable Linux `jackin-capsule` binary — the same-source-ELF contract.
  - Rust 1.97.1 (`rust-toolchain.toml`), Bun 1.3.14, Node 24.18.0 (`mise.toml`),
    `ubuntu-26.04` + `macos-26` runners — all match spec §9.1 historic tools.
  - BUT: zero of the 9 workflows mention `docker-e2e`, `e2e`, `nextest
    --profile`, or `capsule_bin`. Every Rust unit command is `mbx nextest run
    --locked --all-features …` with NO `--profile` flag, i.e. default profile =
    e2e excluded. The `docker-e2e` profile is defined but never invoked by CI.
  - Unmentioned 5th e2e file: `crates/jackin/tests/per_mount_isolation_e2e.rs`
    exists at the audited SHA yet appears in NEITHER nextest filter, so it runs
    under the default profile. Audit listed only 4 excluded binaries.
  - The literal "20-capsule" fanout constant was NOT located in the surveyed
    `dind_e2e*` support files (`fixtures.rs` mentions no capsule at all).
- Consequence: spec §9.1's E2E obligation is fully outstanding — F1 must add an
  explicit `docker-e2e` execution (same-source capsule build → `JACKIN_CAPSULE_BIN`
  → `docker-e2e` profile, serial group kept). Two F1 inputs: (a) disposition the
  5th e2e binary (`per_mount_isolation_e2e`: default-profile resident or
  `docker-e2e` member?) without silently dropping it from coverage; (b) locate
  the 20-capsule fanout in source during F1 (surveyed files only, not exhaustive).

### 2.6 Incidental: `.github/actions` absent (remote refs used)

- Old observation: none recorded for jackin.
- New evidence: `.github/actions/` is 404 at the audited SHA; `ci-unit-rust.yml`
  instead references `tailrocks/velnor/.github/actions/report-velnor-ci-outcomes@b9c3156c…`
  (remote, pinned). No broken local reference.
- Consequence: none for F1 beyond noting jackin's outcome-reporting already
  follows the remote-pin shape; ChainArgos (below) does not.

---

## 3. `ChainArgos/java-monorepo`

Audited SHA `235e479b150aeb949bc8a5190fba5b84f6303c80` (2026-09-16T12:16:31Z,
`docs(nominis): remove screenshot file references`) exists. Live `main`
`38a8fb5777fd02fec8b3904d86387aba05940e9a` (2026-09-16T20:31:28Z,
`docs(atlas): finalize fixture preparation`) is exactly **1 commit ahead**.

### 3.1 Eleven workflows

- Old observation: 11 workflows (evidence §1).
- New evidence: identical 11-file set at BOTH SHAs — `ci-main.yml`,
  `ci-policy.yml`, `ci-pr.yml`, `ci-unit-bun.yml`, `ci-unit-docker.yml`,
  `ci-unit-docs.yml`, `ci-unit-gradle.yml`, `ci-unit-node.yml`,
  `ci-unit-rust.yml`, `maintenance.yml`, `nightly.yml`.
- Consequence: G1's "11 workflows" ledger entry holds at live `main`.

### 3.2 Seventy-one-unit counts

- Old observation: 71 units: 37 Gradle, 17 Rust, 11 Docker, 4 Bun, 1 Node,
  1 Docs; Velnor-only defaults; per-unit table in evidence §3 (authoritative
  for IDs — not re-enumerated here).
- New evidence: `.github/ci/project.toml` at audited SHA contains 71 `[[unit]]`
  entries: **37 gradle, 17 rust, 11 docker, 4 bun, 1 node, 1 docs** — exact
  match. Live-`main` manifest is byte-identical (same 71 / same kinds).
  `runners = "velnor"`, `automatic = "velnor"` (Velnor-only defaults) confirmed.
  `.github-gen/velnor-workflow.toml` is 15 lines with zero unit overrides.
- Consequence: the 71-unit / 213-execution baseline (spec §9.2) revalidates
  exactly. No coverage correction at A0.

### 3.3 Drift commit is docs-only

- Old observation: none — drift window did not exist at audit time.
- New evidence: the single commit `235e479b → 38a8fb57` touches only
  `docs/product/research/atlas-redesign/**`, `scripts/check-atlas-preparation.py`
  (added), and `scripts/check-docs-ignore.txt`. Zero CI files: workflows,
  `project.toml`, `velnor-workflow.toml` all byte-identical across the gap.
- Consequence: no CI drift. G1 starts from the audited CI surface plus unrelated
  docs/script additions (the new `scripts/check-*` files fall under existing
  docs-unit coverage, not new units).

### 3.4 Generator-pin provenance gap (flag, not consumer drift)

- Old observation: audited generator pin `1279c4f92c97b75dc4cc627f122e119f8a5eae16`
  (evidence §1).
- New evidence: ChainArgos `.github-gen/velnor-workflow.toml` contains NO
  `revision` key (15-line file: `[generator] repository` + `[workflow]` only),
  and workflow headers carry no pin comment. The pin's on-repo provenance could
  not be confirmed from the consumer side.
- Consequence: A0 velnor-lane follow-up — re-anchor the `1279c4f9` pin from the
  producer side (releases/attestations) rather than the consumer tree. Not a
  consumer defect; G1 repins to the then-published product regardless.

### 3.5 Missing `.github/actions` (defect persists at live main)

- Old observation: inspected tree lacked `.github/actions/` despite references
  to `report-velnor-ci-outcomes` (run `35094895601`, evidence §5).
- New evidence: `.github/actions/` is 404 at BOTH audited and live SHAs, while
  all six unit workflows (`ci-unit-{bun,docker,docs,gradle,node,rust}.yml`)
  contain `uses: ./.github/actions/report-velnor-ci-outcomes` (local path).
  Every unit workflow therefore references a nonexistent local action at live
  `main`. (Contrast jackin §2.6, which pins the remote action.)
- Consequence: the G1 "missing generated local actions are repaired FIRST"
  obligation (spec §9.2, checklist G1) stands unmitigated. The defect is
  load-bearing for all 71 units, not a single workflow.

### 3.6 `ansible-configs` reference paths (spec §6.1) at live main

- Old observation: 9 reference paths under `ansible-configs/` per spec §6.1 table;
  pin to audited SHA at execution start, re-resolve against live `main`, record drift.
- New evidence: all 9 paths exist at live `main` `38a8fb57`, and ALL are
  byte-identical to the audited SHA `235e479b` (zero drift):

  | Path | Live blob SHA⁸ | Audited→live |
  | --- | --- | --- |
  | `ansible-configs/install-base.yml` | `6c1e2ecf` | SAME (direct per-file SHA match) |
  | `ansible-configs/install-docker.yml` | `85b9a2c1` | SAME (direct per-file SHA match) |
  | `ansible-configs/install-docker-selene.yml` | `b30a027e` | SAME (direct per-file SHA match) |
  | `ansible-configs/hosts.ini` | `cc0eda31` | SAME (live dir listing + §3.3 drift proof) |
  | `ansible-configs/requirements.yaml` | `e8053df3` | SAME (live dir listing + §3.3 drift proof) |
  | `ansible-configs/README.md` | `9de32f6b` | SAME (live dir listing + §3.3 drift proof) |
  | `ansible-configs/docs/upgrade-debian.md` | `7f887ad9` | SAME (live dir listing + §3.3 drift proof) |
  | `ansible-configs/update-packages.yml` | `f3dd8199` | SAME (live dir listing + §3.3 drift proof) |
  | `ansible-configs/upgrade-debian.yml` | `6680c698` | SAME (live dir listing + §3.3 drift proof) |

  Drift proof: the only commit in `235e479b → 38a8fb57` contains 22 files, 0
  under `ansible-configs/` (`compare` API), so every path above is unchanged
  since the audited SHA. Reference permalink root for C1:
  `https://github.com/ChainArgos/java-monorepo/tree/38a8fb5777fd02fec8b3904d86387aba05940e9a/ansible-configs`.
- Consequence: C1 provisions from live-`main` content that is identical to the
  audited content — no drift to reconcile. Pin `38a8fb57` (or the C1-day live
  SHA after one re-read) as the §6.1 source.

---

## Correction ledger (append to A0 ledger)

| # | Old observation | New evidence | Consequence |
| --- | --- | --- | --- |
| C-apt-1 | Omission blob `3172bb88…`, APT primitives absent | Byte-identical blob at `d62820d4`; 1 workflow (`ci-unit-docs.yml`) | B1/B2 scope unchanged; no premature notice removal |
| C-jack-1 | 9 workflows, 40 units (36R/1B/1D/2S), hosted-only | Identical at audited AND live `0be3fcf9` | F1 ledger holds; no coverage correction |
| C-jack-2 | (no drift window at audit) | 1-commit gap changed gen config (external checks → `[DCO]`) without regen | Expect live `generated-tree` failure; F1 regenerates from F-gate pin |
| C-jack-3 | `release.yml` deleted, releases disabled | 404 at both SHAs; `[release] enabled=false` | F1 reconciles assertion; no restore, no enable |
| C-jack-4 | 4 e2e binaries excluded by default; `docker-e2e` must be explicitly run | Profile defined + serial, never invoked by any workflow; capsule-BIN contract in source; tools match §9.1 | §9.1 E2E obligation fully outstanding for F1 |
| C-jack-5 | 4 e2e binaries named | 5th file `per_mount_isolation_e2e.rs` runs under default profile (in no filter) | F1 must disposition it without dropping coverage |
| C-ca-1 | 11 workflows, 71 units (37G/17R/11D/4B/1N/1Docs), Velnor-only | Identical at audited AND live `38a8fb57` | G1 ledger holds; no coverage correction |
| C-ca-2 | (no drift window at audit) | 1-commit gap is docs/scripts-only; CI files byte-identical | No CI drift; G1 starts from audited CI surface |
| C-ca-3 | Pin `1279c4f9` | No `revision` key in consumer `velnor-workflow.toml` | A0 velnor-lane must re-anchor pin producer-side |
| C-ca-4 | `.github/actions/` missing despite local refs | Still 404 at live; all 6 unit workflows use broken local `uses:` | G1 repairs local actions FIRST (load-bearing, all units) |
| C-ca-5 | 9 ansible-configs paths (spec §6.1) | All 9 exist at live `38a8fb57`, all SAME as audited (0/22 drift files under `ansible-configs/`) | C1 pins live content = audited content; no drift |

Method note: every SHA, count, and file-existence claim above comes from a live
`gh api` read in this session (`/tmp/jackin-*.toml`, `/tmp/ca-*.toml`,
`/tmp/jackin-rust.yml`, `/tmp/jackin-fixtures.rs` hold the fetched bytes for
verifier re-checks). No clones, no writes, no consumer mutation.
