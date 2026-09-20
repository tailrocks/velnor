# GitHub-first dual-lane status

Current gate: **G0 — inventory and execution setup**

Overall status: **in progress; no gate passed**.

Last source snapshot: `2026-09-19T16:18:44Z`, Velnor revision
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.

Fleet snapshots are timestamped external observations, not a fresh gate claim.
The inventory at `2026-09-19T16:34:19Z` recorded 32 `main` default branches
with branch/SHA rows and 78 baseline open-PR identities (75 ready, 3 drafts)
across 12 repositories, 23 generator configs, 22 nonempty workflow sets, and
10 empty/no-workflow sets. `G0/fleet/main-revisions.tsv` records 32 `main`
branch/SHA rows again at `2026-09-19T17:05:57Z`. The later PR reconciliation
snapshot generated at `2026-09-19T17:03:32Z` contains 77 current rows versus
78 baseline rows after `jackin-project/homebrew-tap#494` merged and
`tailrocks/velnor#953` changed head; it is not asserted fresh beyond that
timestamp. Baseline records remain under external
`G0/fleet/{inventory.md,configs.tsv,open-prs.tsv}`; current records are
`G0/fleet/{main-revisions.tsv,pr-checks.tsv,requirements.json,context-pages.json,main-verification.json,handoff.json}`.
This is inventory evidence only. It does not prove workload completeness,
required-check success, migration, or a gate exit.

A later read-only collector snapshot is external at
`G0/fleet/check-contexts-full.json`: it started at
`2026-09-19T19:15:10.439Z`, reconciled at `2026-09-19T19:28:02.940Z`, and
was serialized at `2026-09-19T19:29:58.430Z` (SHA-256
`1a51f8a276c912c6f03f3bcb749d1f5845b52c0551cdff6254c8e90c0c78901e`). It
contains 32 unique repositories, 76 stable open-PR rows (3 drafts, 0 bots,
0 forks), 1,268 observed main check runs, and 1,742 observed PR check runs.
These are timestamped observations pending independent review, source binding,
and required-context/App, workload, dependency, and run-identity enforcement;
they do not replace the historical 78-to-77 reconciliation or claim a
current-forever state. The historical Velnor check contract at `abe9ad82` is
not reusable against the later live default revision.

## Historical v4 external checkpoint — 2026-09-20 (superseded by v5)

No gate passed. Existing v3 authority records remain frozen and unapproved;
this v4 checkpoint is historical and superseded by the later v5 disposition.
V4 was approval-required and performed no mutation. Its six remaining
readiness classes are tracked in `PLAN.md`: resolve all placeholders;
execute the real old-parser `static_files` fixture; review B source/closure
and two byte-stable renders; prove writer freeze/lease/watchdog/ruleset
guards; capture the complete Main-B live census bound to B; and obtain owner
plus both independent approvals with cleanup proof.

| Checkpoint | Exact observation | Boundary |
| --- | --- | --- |
| Authority v4 | Historical external `G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v4.{md,json}`, observed `2026-09-20T00:27:10Z`; Markdown SHA-256 `53aac3ef7c2112d0428582a74e61c4da013d026e6fb17cdae419cd209f5895dd`, JSON SHA-256 `1bc1d64db969519e4339b64410aa9a87b53b5cf0e9933f81ecaa16a3593078ba` | Superseded proposal only; no source edit, dispatch, App/ruleset change, release, merge, host operation, or G1 authorization |
| `static_files` feasibility | External typed-publisher feasibility MD/JSON; hashes `8764712693e2883d05846de05a3c2137fb3d31e3ee97ab0ad129186f3ff960f4` / `01ad40d473aa26ae7e5e814ef7c3f93e854d682d2c359fa7588f0bba2b4943e9` | Transports source bytes only; no actual B admission or authority |
| Checker seam | Bounded source `ca18d01166681269b6eb5fce8d0f6175fc17aad4` closes the reusable child-matrix blocker; exact source checks report 258 tests plus build/fmt/clippy/diff pass | Live authenticated collector/API remains unwired; offline cases fail `offline-validation-only`; no G0 proof |
| Bootstrap owner checkpoint | Exact `b981f43e8dfd70b4c628d29b0e7e9dce679ce537`; generated workflows/state regenerated; owner reports 1,727/1,727 library plus actionlint/fmt pass | `G1/reviews/bootstrap-b981f43e-independent.md` rejects the source checkpoint for archive/API-tree, freshness, provenance, legacy, fixture, and image-digest gaps; full source suite is not G1 evidence |
| APT schema-2 | Exact source checkpoint `91bdf6cc1d0a5c429c5c01f17bf15dbb153c661b` | `G1/reviews/apt-83e7ab4-91bdf6c-independent.md` is **BLOCKED** for authority, verify-to-publish, extraction races, native handoff, and generated actionlint; no G2 delivery/publication |
| Corrected workload index | `G0/fleet/workload-contract-index-20260920T002955Z-corrected.json`, SHA-256 `2b3bf88b42f291a40dcc2d6eb65a489d72d2a5bdcf9ab094a69a43e01f750c9c`; exact disjoint union 32 | Source-derived only, `gate_status=not_evaluated`, no execution/job/provider/Velnor output |
| Native product review | Exact `a8d46536e7e11db0bbd5e207be802970362b751f` | Changes required; self-authored identity/archive gaps and Intel capability block remain; no G2 approval |
| Action scanner review | Exact `40ddcc02dde1ff07aff538ea2ca95da091379e17` | Changes required: Docker/action schema and real consumer/runner semantics remain incomplete |
| Hostile producer fixture | Exact `d60c0e2211b2830c64e7489d05b4cfe0bc77d65f` | ZIP-only assertion residual; source-only, no G1 approval |
| Scan candidate | Exact `820f6509fe8462265986bacf02e8e86eead26750` reports 1,760 library tests plus clippy/fmt/diff pass | `G1/scan-integrity/source-review-820f6509.md` rejects detector TOCTOU, SI-B3 fixed-point, journal/post-action recovery, typed recovery, generated-state/D19, and authority gaps; owner reactivated fixes; no G1 approval |

The v4-capture external main evidence (historical at its capture) reports d20
`d20d4d1d17590cca85b501d982cbaad70d42c641` routing Apple jobs to forbidden
`macos-26`; exact `xcode-27` is required. Runs `35475920678` (runtime) and
`35475920808` (preview) / `35475920826` (CI/Main) are historical facts, not
accepted evidence. This does not weaken the newest-actual-major policy or
permit an older fallback.

## Historical live reconciliation — 2026-09-20T01:28:35Z (superseded by 2026-09-20T02:54:52Z census)

This is historical, capture-bound evidence; the later `2026-09-20T02:54:52Z`
paginated census supersedes its “latest” status.
External `G0/fleet/push-checkpoint-current.json` (SHA-256
`d4987b227e79212df2abdf503c33a80daff225ed270ea445f5c657f938fd101a`) binds
this narrow snapshot. It is not a forever-current claim and does not close G0
or any later gate.

| Item | Captured fact | Boundary |
| --- | --- | --- |
| Main / PR965 | Live/origin `main` is `325719f1e05d3d46322c9fd3eeb9ad545e175638`, parent `e94b48406c4ed206fce2bbf39b788264e72cf39c`; merged PR965 source `6b48f8fff2f4943dbf79c21c3274caecdf77bdd5` produced it | Timestamped identity/merge observation only |
| PR963 | Open head `6ccf37486d255bbe3656f0066b0f1e5c84753903` on API base `e94b48406c4ed206fce2bbf39b788264e72cf39c`; mergeable `false`, state `dirty`, base stale by one main commit | No review/approval transfer; earlier `c440d4db` query is superseded only for this capture |
| Runtime / native | Runtime-products `35481089522` succeeded; macOS job `105998930189` succeeded technically on `macos-26` | Negative native-policy observation because exact `xcode-27` is required; no G1 proof |
| Preview / CI | Preview `35481089629` failed at identity job `105998913030`; no macOS preview jobs scheduled. CI/Main `35481089696` was `in_progress` with no conclusion | Failure/incomplete; no gate result |
| Authority v5 | Markdown/JSON pair remains approval-required with no mutation; external disposition rejects structural graph/schema/cycle/writer-freeze requirements | No authority approval or execution |
| Post-census packaging review `294de951` | Independent packaging review passed at cutoff `2026-09-20T01:28:49Z`: exact remote/ancestry, 66 checksums, 63 inventory entries, and no source paths | Packaging-only pass; later review observed `distribution/schema2-handoff-0712.md` changed at `01:34:02Z` and `skills-adapter/integration-map.md` at `01:31:01Z`; those observations are separate from the `01:28:35Z` census, with no attestation/current-equality/gate approval |
| Scan / checker | Scan `820f6509` is rejected despite 1,760 tests/clippy/fmt/diff; checker `ca18d011` closes the reusable child-matrix blocker with 258 tests/build/fmt/clippy | Scan residuals and live collector/API wiring remain; no G1/G0 proof |

## Paginated live census (capture: 2026-09-20T02:54:52Z)

External `G0/fleet/push-checkpoint-current.json` (SHA-256
`5326b47a65d35196816a62783dba60cf0afc58f6d258aae5a90688ca7fd865a0`) records
a fresh paginated read-only census. It binds exact main/PR/run/check pages and
does not use a newest-green shortcut. It is timestamped evidence only; no gate
passed. Task/dependency state remains in the separate task ledger below; this
section is only the paginated census.

| Item | Exact observation | Boundary/status |
| --- | --- | --- |
| Main / PR identities | Main `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, parent `325719f1e05d3d46322c9fd3eeb9ad545e175638`; PR957 `92387e88c32f933a9061b819256e535662655cb2`/base `b5a4b4afaa6ca807927cacc03659b570a895dd5c`; PR960 `c8f7a2b353f9d2d6a60d3ad8bb2dc6299a108ceb`/base `1048337062ea625fada1b4f7c07f2feed75f60c7`; PR962 `ea9686f0eb522e402441ecd461bd1f458b731d06`/base `1048337062ea625fada1b4f7c07f2feed75f60c7`; PR963 `f46fe7c2c3ca7635c44eab21cc6bf616709c7a50`/base `325719f1e05d3d46322c9fd3eeb9ad545e175638` | Snapshot identity only. PR963 open, mergeable `true`, state `blocked`; no review/merge/gate approval transfer |
| Main execution | Runtime `35484968618` success with 5/5 child jobs; macOS job `106009574341` used forbidden `macos-26`. Preview `35484968732` failed identity job `106009550596`, remaining children skipped. CI `35484968706` remained in progress with Policy in progress | Negative/incomplete; no G1/G0 result |
| Records docs | `06edf31faa2c855a7eff4d7821903f296b8adef1` | Focused docs checkpoint accepted; docs-only, no implementation/gate approval |
| Scan review | `29d6a9caf64d625799efa1ad52c3bba0e4f52db2`; `G1/scan-integrity/source-review-29d6a9.md`, SHA-256 `0bb1e2ba64dc8c452f76161fbba6ea4a0d4376caefbc0fe63395f97a1fbdd732` | Conditional source-level pass for injected journal regression; no G1 approval; D19/generated/authority/recovery gaps remain |
| Bootstrap review | Frozen `7d409afdd61a87080be4439d29563313169537`; report SHA-256 `180fb0e150f2bffabe162fbc115a5ca1b425ccdc316e47da96e9349bc62eaa0c`; owner later moved to `07c35f83d1a7353331c910d1d3b728aba1dc7add`, unreviewed at the `02:54:52Z` capture | Changes required on frozen review; a later exact-head review at `03:00:49Z` reports **CHANGES REQUIRED** (report SHA-256 `9cf552e8ca59fc920bbb70a692a4958300c4a27028a907258afbd8503a6c2d07`); do not backdate that disposition into this capture; no disposition transfer |
| Homebrew / native | Consumer `6520aad7bd66d53349e040508146956e9c4f0c1e`, producer/native `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98`; report hashes `dfd870b2188fd64ca1c39386c478f645cf101f1f0d6321e01c2763d8e009a547` / `cb27b9b38eed2c94f7091e606b5271e43f132f98dc17022ad132f3845afe7a20` | Both rejected/changes required: sidecar and canonical schema/parent/preview/generated wiring blockers; no G2 approval/publication |
| Checker / collector / store | Checker `2d436be96b24a6a02cc047604c7899eaea4a16f9`, report SHA-256 `089b0d9155dd4c3e244684ef6576b55ba6022b84e8c773014729c044aed2b241`; collector `0882aac85b62d3962049a8dc5094f25ca5187c20`, report SHA-256 `91cc5333b392c8838f51d94b7b49dd84a6f933c22c147cd3bcc4083fa8736e1a`; store `0d7713cd15cefb554fda5954e2d3320968aa31e7`, report SHA-256 `bc6114c279882c5e7675e0116781618e4807d873639c3a22b4ccc6af69f01282` | Checker parser migration bounded-pass only; collector rejected for authoritative live G0; store rejected for bounded retention; producer/live wiring absent |
| Authority | v7 Markdown SHA-256 `7339996f7c3d76fe4e750cb9036b474beb7e0d20ae7b2b7e7a9f0787cd81b72e`, JSON SHA-256 `1a1cc175b36611f6ce175f4c82b6f6ce51f36f74bfb22a5d8e1abb9cc5647826` | Frozen proposal, approval required, no mutation, no authority approval |
| Freeze choice | Feasibility observed `2026-09-20T02:36:32Z`; report hashes `9735c1b2184f32f3b6b1cd1fefe98045dbb23403f937cd65692d2bcf54b06725` / `29fa711c78ffd8c323acaf6e19653e4adbad87d1658b4d27c9116d669bab36b9` | No provider-only admin freeze. User must explicitly choose/authorize a freeze path; none selected or executed |

## Later capture-bound external checkpoints

These records postdate the `2026-09-20T02:54:52Z` census. They are separate
immutable/source-review observations, not a replacement current-state claim;
unknowns and gate boundaries remain explicit.

| Capture | Artifact and verified identity | Observed result | Boundary |
| --- | --- | --- | --- |
| `2026-09-20T03:57:17Z`–`04:00:39Z` | `G0/fleet/live-default-branches-open-prs-20260920T035717Z-pr-reread-final.json`, SHA-256 `1c844f5c09a6d9d6a5c24f1b36c410a7190eb63a2bc6d2893268e469d9839dee`; report SHA-256 `4134cb7050a1a2a1699cc6784a6797133182c14929e13e5d1527094a6745c780`; raw `manifest.json`, SHA-256 `88d150e78af354645eac07d31cede2163cb80e75a2ff86e9ef78efa46d3ed30a` | Exact 32-repository scope; 32/32 initial and final metadata/ref reads; 32/32 initial and final PR-page passes; 87 initial and 87 final PR records; 64 retained raw response bodies (32 initial + 32 final), with no API/access/pagination errors or normalized churn | Inventory/reconciliation only. No workload, build, install, dispatch, release, publication, or G0/G3 claim |
| `2026-09-20T04:18:47Z` cutoff | Evidence ref `evidence/github-first-dual-lane-20260919T181508Z`, append commit `d25819f5b4a9f1e7724456693e9d8061182cdd1a`; `updates/20260920T041500Z/checkpoint.md` SHA-256 `0dce91c1a4c5228988dbe654556d5a7002acb27e76af9f1bb65ccf9f64a2eea8`; JSON SHA-256 `8246db11d4ca7477e80f1501fc8f5c2137ab69ce73e2dc0e86f464464361fc69` | 89 stable regular files / 5,000,169 source bytes (G0=74, G1=15), 70 JSON; source base `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`; attestation `none`, `gate_status=not-evaluated` | Immutable packaging captured; independent packaging review remains pending. No G0–G7 gate or attestation |
| `2026-09-20T03:58:55Z` v9 observation | Frozen proposal `G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v9.{md,json}`; Markdown SHA-256 `bfcafb4f706ac5c1a1ecccc7d8ba1d0d777f16e52bf4fa335a15085908e832ed`, JSON SHA-256 `e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615`; audit report/results SHA-256 `1e920b0186c050d78757919ca39dc71ffb686f4de97e7b2e3cf77cbb83d02f0f` / `4ddeb3272b5456f8e790e925c4dba4a71ba4436eee38af0910bbc660fc196ca1` | Owner audit reports 33/33 contract checks passing; v9 remains `successor_draft_external_blocked`, with target-generator/source fixed point, real provider identity, actor-5 freeze, and exact `xcode-27` proof absent | Independent reviews pending. Frozen proposal only; no authority operation, source approval, execution, or gate |
| Raw-store source `5688af96857a0cbe3b6a0fa064fcfb8b31ac2d36` | `G0/checker-v2-review/5688af96-g3-raw-store-rereview.md`, SHA-256 `03cc0a51c0c0465d564735191680bb3cf62a65e4a0414081842f16b2061c7bff` | **Reject** for bounded-retention acceptance: exact-source adversaries observe retention-owned peaks above 128 MiB during ordinary publication and valid `.txn` recovery; pending-record identity race and fault-injected publication proof also remain | Bounded-store source review only. No production collector, Linux runtime, authoritative capture, or gate claim |
| Bootstrap transport source | Helper `00c752268fc0e9b19a45badaaea0934736bb47d5` is integrated in owner checkpoint `e2124475806a042313fc5a4e251f88246a6d16ab`; owner reports transport 4/4, library/tests/clippy, fmt, and diff checks | Direct remote reconciliation later confirmed `e212447...` equality; generated-state drift remains unresolved and independent security review is running | Source checkpoint only; no G1 approval or publication |
| Action scanner source `ab8a281277de084e8115653e0d022f805adc1496` | `G0/action-review/review-ab8a2812.md`, SHA-256 `f026e06327d7e866459983a5df99cfe59fc7f63a83f6bd26ec035fe5e2f03b9f` | Bounded source review passes: 1,898 serial tests, clippy, fmt, and diff; declared `fdeed` closure still blocks zero-diff generator verification | Source-only bounded pass. No merge, rollout, macOS/Docker operation, pin publication, or G0/G3 claim |

## User native-version amendment

The amended policy is explicit: every Velnor workload on a GitHub-hosted macOS
runner uses the newest actual supported major and architecture available at
dispatch. The current verified arm64 mapping is macOS 27 via exact hosted label
`xcode-27`; this is an observed mapping, not a ceiling. A future newest actual
major and exact label supersede it. macOS 27 Intel has no supported hosted
label in current evidence; it must fail explicitly, never downgrade to
`macos-26`, `macos-26-intel`, an older major, or a lagging `macos-latest`
alias. Evidence must bind the resolved label, host/image identity, Xcode/Swift,
SDK, deployment target, and architecture. An incompatible native constraint
fails explicitly; it is not skipped or silently rerouted.

This does not weaken immutable action, container-image, release-asset, or
digest pins. Pin updates require reviewed immutable identities. The isolated
`latest_macos_policy` task owns the AGENTS rule and official-label research/PR;
`g3-native-routing` owns generator policy; and `g2_homebrew_contract`
independently reviews the native/package contract. These ownership records do
not approve a source change or alter G0→G1→G2→G3→G4/G5 sequencing. Exact label
research and policy PR evidence remain external and pending.

## Current candidate-bound checkpoint

External candidate observations around `2026-09-19T22:47:37Z` do not advance
G0 or any later gate:
A separate 2026-09-20 reconciliation observed live `main` at
`1048337062ea625fada1b4f7c07f2feed75f60c7`, parent `b5a4b4af`; it reports
generator-rendering reproducibility only. The candidate rows below remain
timestamped `b5`-bound observations, not current-main proof. See external
`G1/bootstrap-transition/VALIDATOR-ONLY-DESIGN-2026-09-20.md`.

- PR957 source `9e06`, revision `53`, has source-only approval in
  `G0/native-review/review-pr957-92387e88.md` with 1,888 source tests plus
  fmt/clippy/check pass. Merge/live is not ready: D19 is unpublished, Policy
  run `35473052923` failed candidate acquisition, and the separately typed
  validator dependency remains unresolved.
- PR960 is open at head
  `2c810f1b46ce8eddb5906fd4bdcc8ae23e78ed40` on base
  `b5a4b4afaa6ca807927cacc03659b570a895dd5c`; Policy was in progress at
  capture and no review decision or merge exists. PR962 is open at head
  `94b43578cad9720e569780d18dc966370ed47c11` on the same base; required and
  Velnor-workflow hosted failures were observed. These are not approvals.
- PR961 remains the historical open path at head
  `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303` on base `b5a4b4af`; its history
  contains unsigned `857` and DCO is `action_required`. It is not repaired or
  approved. PR963's `fb78d85d464fd5082e5c161922afd7942380fabc` signed
  replacement is also historical to the `b5` snapshot; external comparison
  records the tree-equivalent replacement with `857` excluded. A prior
  read-only query reported head
  `c440d4db3fd59a9e4abd396d7a75e670c4f3d862` on API base
  `1048337062ea625fada1b4f7c07f2feed75f60c7`; no fresh exact-head review or
  approval was observed. The independent `0c1ec75753cf9f8044a3a2ff01c2d144e9c59132`
  review does not transfer to `c440d4db`. Later remote main
  `e94b48406c4ed206fce2bbf39b788264e72cf39c` was a separate fresh census at
  that time; the later `325719f1` reconciliation is recorded above. d20 is a
  historical main observation. Force/override remains forbidden.
- The selected product boundary is a separately typed validator-only
  product/publisher owned by `/root/g0_inventory` and reviewed by
  `/root/g0_reviewer`; no three-platform runtime reuse or platform-selection
  workaround is allowed. Publication precedes separate PR957 pin adoption.
- Secure CAS/sourcegraph and collector/checker binding remain unresolved. The
  scan `38345852` reports five full-suite failures, but exact-parent baseline
  attribution is pending; this status does not call them baseline failures.

## Completed

- Read the full authoritative goal and applicable root rules.
- Confirmed the records worktree is clean at the initial Velnor revision.
- Confirmed the fixed manifest is represented exactly once in `fleet.json`.
- Verified RTK `0.49.0` and Codex CLI `0.155.0` locally.
- Verified root settings: `gpt-6-astra`/low orchestration and
  `gpt-5.6-luna`/max agents. This records agent turn context is Luna/max.
- Captured the stable external session/ruleset location in `SPEC.md`; the
  mutable registry hash is intentionally checkpoint-owned, not source-pinned.
- Recorded G0 distribution findings externally: publication remains blocked on
  G1; Homebrew contract and native-package work are coordinated.
- Recorded G0 bootstrap finding: candidate bootstrap has no Plan/unit/Velnor
  dependency; source edits wait for inventory seed/pin handoff.
- Recorded hosted/config progress: candidate commit
  `a38e459c7c88fa1c6e9a646a7513b2eecac292b8` selects hosted for automatic and
  default dispatch; generated final outputs and release-provider repair remain
  pending. PR954 is at `f16592ea165ced141bf0bb1c43466a95d7df8b2e`.
- Recorded failed-run operations externally: confirmed stale runs
  `35452270126` and `35445034780` were force-canceled; PR954 run
  `35454970877` has hosted work progressing while Velnor is queued; PR953 run
  `35453601367` has a cache-contract failure. No rules changed.
- The committed `fleet.json` is a static 32-row scope manifest, not the live
  enriched checker input. Its 32 workload-ID, required-check/App,
  platform/architecture, and provider-eligibility fields are null; 31 access
  rows are unknown; dependency/access structure is not embedded. External
  `main-revisions.tsv` has 32 timestamped branch/SHA rows, while current PR
  and check semantics remain snapshot-bound and incomplete.
- The current read-only workload projection is external at
  `G0/workload-matrix.json`: all 32 rows align to the latest main-revision
  snapshot, observed responsibilities are separated from unsupported/native/
  trust obligations, and missing or unexecuted work remains a blocker. This is
  not G3 generated coverage.
- Native-routing report found Tablerock/playground Apple workloads incorrectly
  routed to Ubuntu; Jackin macOS routing is available. Central shape-based
  scanner work is isolated and cannot roll out before G2.
- Early read-only category audits are now durable externally: skills at
  `G0/skills-adapter/report.md`, action/roles at `G0/action-roles/findings.md`,
  Rust consumers plus inventory at `G0/rust-consumers/{report.md,inventory.tsv}`,
  distribution consumers plus inventory at
  `G0/distribution-consumers/{report.md,consumer-inventory.json}`, and the
  independent distribution review at `G0/distribution-review/report.md`.
  They record blockers and missing proof; none is a gate pass.
- G1 seed/pin chronology is external at `G1/reviews/seed-pin.md`: 1858 tests
  belong to exact PR head `a5c1c0bd` before regeneration, while 1736 tests,
  fmt, and clippy belong to integrated source `12cc87b`; these counts are not
  combined. `G1/reviews/bootstrap-hosted.md` is a checkpoint only.
- G1 runtime-product audit is external at
  `G1/runtime-product-audit/{runtime-product-audit.json,PROMOTION.md}`:
  published old pin `fdeed261` resolves to closure `81ba31f`/release
  `391347842`; current main `abe9ad82` publishes distinct closure `63cea86`;
  local `12cc87b` needs unpublished closure `1cbf31a`. Promotion order is
  source admission, merged-main publication, immutable verification, then pin
  adoption/regeneration. This is not a G1 pass.

These are documentation/setup facts only. They do not establish hosted CI,
package delivery, fleet migration, Mac operation, or any gate exit.

## Current evidence

| Item | Observation | Evidence/status |
| --- | --- | --- |
| Source revision | `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` | Observed locally |
| Velnor workflow config | generator pin `fdeed261bd2247a38db6922a7726cd45d3d6f31e`; schema 2 | Observed in source; revalidate live |
| Config SHA-256 | `9911f537d1621a265ec6037475d8d5f8bf16bf7d918440cb4dd9a0120f3acd54` | Observed locally |
| Generated state SHA-256 | `2643fad3e4943262ceeb47b888b451119998c2ea14114dd86b27cc29a2647646` | Observed locally |
| Current ruleset | `19573071`, `DCO`, `ci-required`, `Policy`, active | External baseline; no change claimed |
| Evidence root | `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/` | Policy; live records pending |
| Session registry | Stable external `session.json` path; current hash recorded per checkpoint | Actual Luna/max worker threads, evidence paths, graph amendments, and no gate pass |
| Final ledger | Immutable artifact/evidence ref outside source | Pending |
| External fleet branch snapshot | 32 `main` rows with nonempty SHAs at `2026-09-19T16:34:19Z`; `main-revisions.tsv` repeats 32 rows at `2026-09-19T17:05:57Z` | External read-only observations; not a fresh current-state or G0 pass |
| External PR snapshot | 78 baseline identities at `2026-09-19T16:34:19Z`; 77 current rows in `requirements.json` generated `2026-09-19T17:03:32Z`; heads rechecked `2026-09-19T17:02:04Z` | Historical/current snapshot distinction preserved; no fresh claim |
| Later all-32 check-context snapshot | `G0/fleet/check-contexts-full.json`, `2026-09-19T19:15:10Z`–`19:28:02Z`; 32 repositories, 76 PR rows, 1,268 main and 1,742 PR observed check runs | Timestamped collector evidence; source binding, independent review, required-context/App semantics, workloads, dependencies, and child/run proof remain pending |
| Committed fleet semantic fields | 32/32 expected workloads null; 32/32 required check/App fields null; 32/32 platform/architecture null; 32/32 provider eligibility null; 31/32 access rows unknown | Explicit G0 incompleteness; do not fill nulls or treat static branch rows as enriched evidence |
| Check-contract scope | `G0/check-contract/check-contract.json` covers only Velnor current main and PR948/952/953/954; historical `open-prs.tsv` has 78 identities and the later collector has 76 PR rows | Neither scope proves all-32 required check/App/run coverage; Velnor contract is not reusable at a mismatched live SHA |
| Collection handoff | `/root/g3_distribution_consumers` → `G0/fleet/check-contexts-full.json`; `/root/g0_runtime` → `G0/fleet/dependencies-and-access.json`; `/root/g0_inventory` → `G0/fleet/workloads-full.json` | Check-context and dependency/access artifacts exist as timestamped external snapshots; graph coverage is partial/gapped, workload binding and checker envelope remain pending; records agent ingests only validated source-SHA/UTC/explicit-unknown outputs |
| PR952 source/integration chronology | Exact PR head `a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637` had 1858 tests before regeneration; integrated source `12cc87b629802c294da9840325cb21087c020df` has 1736 tests, fmt, and clippy pass; generated snapshot failure remains until regeneration | Separate observations; not a gate pass |
| PR954 current head | `f16592ea165ced141bf0bb1c43466a95d7df8b2e` | Observed; current run still pending/partial |
| PR953 cache result | Run `35453601367` failed cache contract | Observed; cache diagnosis reopened |
| G0-runtime | Collector `0882aac85b62d3962049a8dc5094f25ca5187c20` remains rejected; successor raw-store `5688af96857a0cbe3b6a0fa064fcfb8b31ac2d36` has exact report `03cc0a51c0c0465d564735191680bb3cf62a65e4a0414081842f16b2061c7bff` | Collector is rejected for authoritative live G0; successor store is rejected for bounded-retention quota peaks and remains test-oriented; actual Mac remains deferred until G3 |
| Native macOS version policy | GitHub-hosted Velnor macOS workloads must use the newest actual major/architecture; current verified macOS 27 arm64 label is exact `xcode-27`; macOS 27 Intel has no supported label and cannot fall back to `macos-26`/`macos-26-intel`/older/alias/skip | Amendment recorded; official-label research/PR and exact runner/image/SDK evidence pending; immutable action/image pins remain required |
| Runner protocol source | `actions/runner` revision `80bb1fb827fa44d489263061e71ef4adba7ad8cd` pinned for later work | Observed; no implementation here |
| G2 native package compile | Three required ARM64 macOS binaries compile/smoke at `abe9ad82`; nothing installed or published | Preliminary only; G2 remains pending |
| G2 product identity | Typed native-product contract exists at `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98`, but independent review rejects its sidecar, canonical schema, APT parent binding, preview, idempotency, duplicate-component, and generated-wiring behavior | G2-native-product remains blocked; no package acceptance |
| G1 scan integrity | `29d6a9caf64d625799efa1ad52c3bba0e4f52db2` conditionally passes injected journal regression only; `820f6509` and earlier candidates remain rejected | External `G1/scan-integrity/source-review-29d6a9.md` and exact prior reports; D19/generated-state, real filesystem-fault mapping, authority, and recovery-entrypoint gaps remain; no G1 approval |
| Early category audits | Skills, action/roles, Rust consumers, distribution consumers, and independent distribution review reports | Read-only evidence written; scanner/publication/native proof gaps remain |
| G1 runtime-product audit | Old pin/release, current-main distinction, and candidate closure/promotion sequence | External evidence written; no candidate publication or pin adoption |
| G0 workload matrix | 32 unique rows aligned to current main revisions; observed duties, native/unsupported/trust obligations, and missing execution retained | External `G0/workload-matrix.json`; inventory projection only |
| G0 dependency/access artifact | `G0/fleet/dependencies-and-access.json` observed `2026-09-19T19:21:38Z`; 32/32 scope, 22 workflow-bearing rows, 15 source-bound edges; source coverage 26 exact local, 1 wrong-pin Velnor, 4 report-only, 1 inventory-only | SHA-256 `f58da9d4ea2bc32ba8867cbb4897997bc48c6e4228a14ed4ec0710c056f67f60`; graph coverage partial/explicitly gapped; not full validation |
| G0 checker review | `2d436be96b24a6a02cc047604c7899eaea4a16f9` boundedly removes local CAS/evidence-root and preserves parser/live fail-closed behavior | Producer adapter/live `verify_g0` remains unwired; no checker completion or gate proof |
| G0 acceptance matrix | Exact 32/no-extras scope, live default/PR/workflow/run/provider/workload/dependency/source/digest/child evidence, and fail-closed stale/missing/manual-only rules | Canonical [`SPEC.md` matrix](./SPEC.md#exact-g0-acceptance-matrix), external `session.json`, checker and independent-review reports; unknown/incomplete |
| G0 dependency graph schema | Typed workload→child, required-check, release, and package edges with relation/stage/applicability/provenance/status; G0 inventories, G2+ proves applicable execution | SPEC neutral example and external checker handoff; checker schema migration unimplemented/unknown |
| G0 manifest identity | Static `fleet.json` `default_branch`/SHA is a seed claim only; each row must reconcile to independent live/default snapshot and UTC | Mismatch, missing snapshot, or stale SHA fails closed; no current reconciliation claim added here |
| G0 input conversion boundary | `fleet.json` is flat nullable scope inventory only; checker expects enriched `config/.../manifest.json`, independent `evidence/current-snapshot.json`, and bound `evidence/records.json` | Paths/schema/producer and exact invocation remain pending in `/root/g0_checker`; count validation and `pending` statuses are not G0 success |
| G0 baseline-versus-current | Independent findings baseline is `abe9ad82`; integration `12cc87b6` and later candidate SHAs remain separate | External `session.json`; no baseline result promoted to current or gate evidence |
| G0 helper boundary | `audit_ci` is auxiliary; `lane_compare` pair/census/step/artifact-log behavior is diagnostic and has recorded false-green paths | External checker review and lane assignment; complete paginated artifact/log proof pending |
| G0/G2 review checkpoint | Scan `29d6a9` is conditional source-only; checker `2d436be` parser-only; collector `0882aac` and store `0d7713` rejected; Homebrew `6520aad` and native product `2f7d5f` rejected | External exact reports; no approval, publication, or gate claim |
| Baseline test chronology | Clean `abe9ad82` `velnor-tools` test reported 207 passed in 11.42s; later 206-pass/one-failure output was contaminated by concurrent parent edits | External `session.json`; preserve both observations, do not call the full baseline green or assume flakiness |

## Model and runtime evidence

| Scope | Model/effort | Verification |
| --- | --- | --- |
| Root session `01a0ba6f-f806-7d31-9abb-c828b3dc9e4e` | `gpt-6-astra` / `low` | External `session.json` and local turn logs |
| Records agent `01a0ba74-0f82-7800-9937-fb9d22de7e3d` | `gpt-5.6-luna` / `max` | Local turn logs |
| Configured default agents | `gpt-5.6-luna` / `max` | Local config |

Verified host metadata: macOS `27.0`, build `26A428`, `arm64`.
This is the orchestrator machine metadata, not G4/G5 actual Velnor pilot
evidence. OrbStack, Docker server, Velnor package identity, and job image are
unknown.

## Initial task state

| Task | State | Next action |
| --- | --- | --- |
| G0-inventory | Successor read-only inventory captured 32/32 initial/final metadata/ref reads, 87 initial/final PR records, and 64 retained raw bodies at `2026-09-20T03:57:17Z`–`04:00:39Z` | Inventory/reconciliation only; workload, dependency, execution, and gate evidence remain separate |
| G0-bootstrap | Owner checkpoint `b981f43e8dfd70b4c628d29b0e7e9dce679ce537` rejected; frozen follow-up `7d409afdd61a87080be4439d29563313169537` changes-required; helper `00c752268fc0e9b19a45badaaea0934736bb47d5` integrated in `e2124475806a042313fc5a4e251f88246a6d16ab`; v9 remains external-blocked and independent review/security work is pending | Generated drift, exact source/output fixed point, provider identity, freeze, hosted-proof, archive/API-tree, freshness, provenance, fixture, and image-digest gaps remain; no G1 approval transfer |
| G0-distribution | Assigned; result pending | Revalidate product/runtime discovery and both channels |
| G0-fleet | 32-row inventory complete; workload matrix/review pending | Build workload/platform/category matrix |
| G0-runtime | Collector `0882aac85b62d3962049a8dc5094f25ca5187c20` rejected for authoritative live G0; raw-store `5688af96857a0cbe3b6a0fa064fcfb8b31ac2d36` rejected for bounded retention after exact quota-peak adversaries | Keep actual Mac deferred until G3; no authenticated collector, production raw store, or authoritative raw capture |
| G0-records | Docs commit `92477f806bc780cb1925d3cebacfa98dfb4a400e` remains prior accepted checkpoint; external append package `d25819f5b4a9f1e7724456693e9d8061182cdd1a` is captured | Preserve unknowns; packaging review and independent v9/action/security reviews remain pending; no gate |
| G0-checker | Bounded `ca18d01166681269b6eb5fce8d0f6175fc17aad4` closes the reusable child-matrix blocker; `2d436be96b24a6a02cc047604c7899eaea4a16f9` removes local CAS/evidence-root and remains parser-only | Wire producer adapter/live `verify_g0`, authenticated collector/current API, and exact artifact/run bindings; offline fixtures cannot close G0 |
| G0-reviewer | Independent review rejected initial checker/docs state | Review exact v2 commit and refreshed external evidence |
| G1-cache-semantics | Jobs/provider adversarial review rejected initial checker paths | Retain external findings; review v2 candidate without rewriting old evidence |
| G1-hosted-config | In progress; candidate remains unverified | Thread `01a0ba77-5222-7e63-97fa-553849b96d7b`; recheck clean exact commit/output |
| G1-review952 | `29d6a9caf64d625799efa1ad52c3bba0e4f52db2` is a conditional source-level pass only; `820f6509` and prior scan candidates remain rejected/blocked | Thread `01a0ba77-dd88-7ac2-9fb7-118f3c09d1af`; do not combine historical test counts or transfer review across heads |
| G1-run-operations | In progress; stale-runs evidence updated | Thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`; trace child outcomes |
| G1-runtime-product-audit | Evidence written; next publish verification pending | Same operations thread; old pin verified, candidate remains unpublished |
| G1-seed-pin | Source review written; pin adoption pending | Thread `01a0ba72-3925-7141-b1f7-5529a5cf6c98`; clean regeneration remains required |
| G1-scan-integrity | `29d6a9caf64d625799efa1ad52c3bba0e4f52db2` conditionally passes injected journal regression only; `820f6509` and prior exact candidates remain rejected | D19/generated-state, real filesystem-fault mapping, authority, and recovery-entrypoint gaps remain; no G1 approval |
| G2-native-packages | Compile/smoke observed; now owns product/manifest contract | Thread `01a0ba7a-328e-7282-943e-5b54c2ac209d`; no install/publication claim |
| G2-homebrew-contract | Consumer `6520aad7bd66d53349e040508146956e9c4f0c1e` rejects producer/consumer handoff against `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98` | Thread `01a0ba80-8408-7380-8ac2-b743eb4494a5`; fix shared checksum/schema contract before provider/client proof; no G2 approval |
| G2-native-product | Exact `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98` rejected by independent rereview | `/root/g2_native_packages`; fix sidecar, canonical schema, APT parent binding, preview assets, idempotency, duplicate components, and generated wiring |
| G2-preview-publication | Planned; blocked until G1 | `/root/g1_run_operations`, thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`, worktree `dual-lane-preview-publication`; reviewer `/root/g2_distribution_review` |
| G2-distribution-review | Typed review pending; initial checker hostile G2 suite failed all nine mutations on old b3b6 | Thread `01a0ba81-1af6-7f11-9f24-3ff115b8f314`; no publication approval |
| latest_macos_policy | Amendment assigned; exact official label research/PR pending | Isolated AGENTS-rule/research task; exact current arm64 label `xcode-27`, future newest labels supersede; macOS 27 Intel has no fallback; reviewer `/root/g2_homebrew_contract` |
| g3-skills-adapter | Read-only evidence written; central scanner fix required | Thread `01a0ba7b-0152-7a70-8d18-c2387f1c9469`; external report only, no rollout |
| g3-native-routing | Read-only evidence written; generator policy amendment must use newest actual hosted macOS major and explicit incompatibility failure; rollout blocked until G2 | Thread `01a0ba7b-32f1-7af1-a25f-4cde73f1f075`; native routing report only; native/package contract review `/root/g2_homebrew_contract` |
| g3-action-roles | Read-only evidence written; G3 contract incomplete | Thread `01a0ba7b-57a5-7623-bd92-d0666a02b96e`; reusable publisher/runtime gaps remain |
| g3-rust-consumers | Read-only evidence written; scanner/release gaps recorded | Thread `01a0ba7d-bb3f-78c3-84c8-eb0b2d75e5d0`; termrock central fix pending |
| g3-distribution-consumers | Read-only evidence written; G3 blocked/incomplete | Thread `01a0ba7d-e457-7723-81ba-1f7ed038212c`; native install/feed proof missing |
| g3-native-review | Independent review active | Thread `01a0ba8a-5bef-7d71-9a17-1d082a4f122a`; result not observed |

Actual thread IDs are recorded for assigned follow-up/category workers above;
remaining unknown worktrees or result states are deliberate. No task result is
inferred from assignment.

## Blockers and access gaps

1. Live workload/check/migration evidence for the fleet is not yet attached;
   inventory rows are not acceptance proof.
2. Generator/bootstrap/distribution investigations remain incomplete; the
   external category audits expose missing typed scanner/publication/runtime
   contracts.
3. The initial checker implementation is semantically rejected; v2 architecture,
   authoritative scope/live bindings, and hostile-fixture rerun are pending.
4. No hosted recovery, package publication/install, fleet migration, actual
   Mac/OrbStack pilot, dual-provider run, or final audit is proven.
5. Required-check transition and App binding remain unchanged/unknown beyond
   the recorded Velnor ruleset snapshot.
6. Native macOS policy still lacks official-label research/PR and exact
   runner/image/SDK evidence. Any incompatible native constraint must fail
   explicitly; no older macOS fallback, skip, or silent reroute is allowed.
7. Authority v7 remains frozen and unapproved. The provider-feasibility result
   shows no provider-only freeze against the current admin; an explicit user
   choice and authority for a consent-only, provider-gated, existing-updater,
   or external-root path is required before any transition operation.

Read-only early audits may continue before G2, but no `g3-*` task can claim a
G3 migration or authorize operational rollout. The current runtime audit also
reports a direct host-socket bypass, per-repository capacity defaults, and
incomplete cancellation targets; these are G4/G5 findings pending the required
G3 barrier.

## Exact next actions

1. Keep the 32-row workload matrix and current fleet refresh linked from the
   external ledger; do not turn observed duties into generated coverage.
2. Require the checker v2 architecture and authoritative evidence bindings;
   rerun the nine hostile G2 mutations plus G0/G1 adversarial fixtures.
3. Refresh live GitHub inventory and attach source/run/check evidence.
4. Reconcile bootstrap, distribution, fleet, runtime, and failed-run findings.
5. Complete `latest_macos_policy` official-label research/PR and the
   `g3-native-routing` generator-policy review; preserve immutable action/image
   pins and explicit incompatible-constraint failures.
6. Start G1 only after G0 exit evidence is complete and independently reviewed.

## Checkpoint rule

At each gate, before/after publication or merge, and after any central fix,
record an external checkpoint with UTC time, source revisions, task states,
running jobs, and invalidated evidence. A changed default tip, PR head, release
commit, or generator/runtime pin invalidates affected evidence.
