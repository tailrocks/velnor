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
| Bootstrap owner checkpoint | Latest exact source `bba2e758f91e7d40c295a2f034eaeff64ef2f09d`; owner reports 1,700 passed / 1 generated `ci-policy.yml` drift failure | Exact-head report `G1/bootstrap/source-review-bba2e758f91e7d40c295a2f034eaeff64ef2f09d.md`, SHA-256 `54ee1d5559021a5623c3b0a070de052c6b0138bbd7649180d0e19def02f27567`, requires changes for local-action/source-closure bypasses, policy test/clippy failures, and generated drift; no G1 approval |
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
| `2026-09-20T04:18:47Z` cutoff | Evidence ref `evidence/github-first-dual-lane-20260919T181508Z`, append commit `d25819f5b4a9f1e7724456693e9d8061182cdd1a`; `updates/20260920T041500Z/checkpoint.md` SHA-256 `0dce91c1a4c5228988dbe654556d5a7002acb27e76af9f1bb65ccf9f64a2eea8`; JSON SHA-256 `8246db11d4ca7477e80f1501fc8f5c2137ab69ce73e2dc0e86f464464361fc69` | 89 stable regular files / 5,000,169 source bytes (G0=74, G1=15), 70 JSON; source base `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`; attestation `none`, `gate_status=not-evaluated` | Independent packaging review **PASS** (report `G0/evidence-checkpoint-review/d25819f5-packaging-review.md`, SHA-256 `3831370b0c008fcf62588547ee10559e13066b68b09e2b32dd7a72c7c6cfce3e`); packaging-only historical result, no G0–G7 gate or attestation |
| `2026-09-20T03:58:55Z` v9 observation | Frozen proposal `G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v9.{md,json}`; Markdown SHA-256 `bfcafb4f706ac5c1a1ecccc7d8ba1d0d777f16e52bf4fa335a15085908e832ed`, JSON SHA-256 `e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615`; later independent audit report/results SHA-256 `853b28bee7e3475a898921f50a035b713eedd83c8403fd99c5900964944f227c` / `d010e465c84d8552453af3612e950bc41715ee19a5fadd63070b0774944ae88a` | Owner audit reports 33/33 contract checks passing, but the later independent audit records 22 checks: 18 pass, 3 fail, 1 unimplemented; release 14-leaf coverage, preimage/self-hash, product identity, fixture lineage, and transport defects remain; authority claim `False` | Later design-only independent rejection; v9 remains `successor_draft_external_blocked`. Frozen proposal only; no authority operation, source approval, execution, or gate |
| Raw-store source `5688af96857a0cbe3b6a0fa064fcfb8b31ac2d36` | `G0/checker-v2-review/5688af96-g3-raw-store-rereview.md`, SHA-256 `03cc0a51c0c0465d564735191680bb3cf62a65e4a0414081842f16b2061c7bff` | **Reject** for bounded-retention acceptance: exact-source adversaries observe retention-owned peaks above 128 MiB during ordinary publication and valid `.txn` recovery; pending-record identity race and fault-injected publication proof also remain | Bounded-store source review only. No production collector, Linux runtime, authoritative capture, or gate claim |
| Bootstrap transport source | Helper `00c752268fc0e9b19a45badaaea0934736bb47d5` is integrated in owner checkpoint `e2124475806a042313fc5a4e251f88246a6d16ab` (authored `2026-09-20T04:19:27Z`, committed `2026-09-20T04:23:46Z`); independent report `G1/bootstrap/source-review-e2124475806a042313fc5a4e251f88246a6d16ab.md`, SHA-256 `2030e55bd471148ca6473149867a517d4a63a16b83b635b6d5383f79037abdc2` | **CHANGES REQUIRED**: full library `1,700 passed / 1 failed` on generated `ci-policy.yml` drift; clippy, focused transport `4/4`, fmt, and diff pass separately. Review found Python `urllib` runtime `POST` and opaque publisher-action bypasses | Source checkpoint only; no G1 approval/publication. Exact report UTC review capture is not stated; security blockers remain |
| PR963 source review | Exact head `056362aadb738279924ef05597f9a014392f48bf`, parent `b1563dadb022bbf8eff6b699c09f4e4267c53d81`, API base `325719f1e05d3d46322c9fd3eeb9ad545e175638`; source review `G1/reviews/pr963-056-independent.md`, SHA-256 `d4d2e953a4b8dae0bf3a8e1d1e99c521ff1da7deeeb674b4b7b6883754d56ae3`; source authored/committed `2026-09-20T04:12:18Z` | Independent review approves source admission at this exact head; 5,448 workspace tests / 5 ignored, hosted run `35488557270`, tested merge `3d450612adb6c12be3b6b811348555807861c7c7`, exact 17 nonempty units, `excluded=[]` | Bounded source-only approval. Report is dated `2026-09-20` but does not state exact UTC review capture; no merge, pin adoption, rollout, or hosted G1 gate approval |
| Action scanner source `ab8a281277de084e8115653e0d022f805adc1496` | `G0/action-review/review-ab8a2812.md`, SHA-256 `f026e06327d7e866459983a5df99cfe59fc7f63a83f6bd26ec035fe5e2f03b9f` | Bounded source review passes: 1,898 serial tests, clippy, fmt, and diff; declared `fdeed` closure still blocks zero-diff generator verification | Source-only bounded pass. No merge, rollout, macOS/Docker operation, pin publication, or G0/G3 claim |
| `2026-09-20T05:32:10Z` cutoff | Evidence candidate `38269cf0ebe2c6c007f80fa78458fc63d40f76a5`; independent report `G0/evidence-checkpoint-review/38269cf0-packaging-review.md`, SHA-256 `46455c14b24bdb5c19b5b22eef98459bb0c33e3b4d066816ed579e3efaf4c7e5` | Packaging integrity **PASS**: append-only 55-file update, 51 selected source files, four controls, 1,018,389 selected source bytes; no path escape, deletion, symlink, or checksum mismatch | Packaging-only historical result; excluded v10 adversarial side tree remains excluded; `attestation=none`, `gate_status=not-evaluated`; no G0–G7 or v10 approval. |
| `2026-09-20` UTC; exact review time unstated, v10 | V10 plan Markdown/JSON SHA-256 `ab8513aebb6cf6657f0c88f5fd7af283c8b785d054cd64d0156be4a4bad5c12b` / `8e5ff3e61a1d25590c88a161f9e38db260b9fa1afeccfe1c956b1194b5b66050`; authority review `G1/bootstrap-transition/v10-independent-authority-transition-review-2026-09-20.md`, SHA-256 `b2596f1d06de42a6c59bcee8412adc66e06a363665a349f34065a2349d1454db`; adversarial `G1/bootstrap-transition/v10-independent-adversarial-audit-2026-09-20/{v10-independent-adversarial-report.md,v10-independent-adversarial-results.json}`, SHA-256 `d0ee3b1bb9a511db2dfb3845a45500ea288462e47cfad9c15ccdc84fad21c180` / `ea7fa62a685dd94bf7e86c42a65dd86e3ab371d415b6451af0689949e843c446` | V10 is **not approved**: owner 36/36 is proposal evidence only; independent adversarial audit reports 14/25 passed, 9 failed, 2 unimplemented, `authority_claim=false` | Design-only rejection; caller permissions/output transport, strict fixtures, DAG/lineage, typed Main-B implementation, provider trust/freeze, and live proof remain absent. No authority/source/release/gate claim. |
| `2026-09-20` UTC; exact review time unstated, v11 | Frozen v11 plan Markdown/JSON SHA-256 `2fe0a7e534299e0b7dc83102706d29d91fd07e422a795ef726c1dccd5d119dde` / `4c50c8e591924cbdbccb7ec489e8a8bbb20576d5f5fbb207c42e52837f6d42c6`; independent adversarial `G1/bootstrap-transition/v11-independent-adversarial-audit-2026-09-20/{v11-independent-adversarial-report.md,v11-independent-adversarial-results.json}`, SHA-256 `b877b8ad6ed9de30b0541ccdb99ebdaf061f35a46526709849540fed1f59a271` / `972a5435150ac27b077b9edf390a20eb4e4db849f11571a23d1fd7c071d6c5b0` | Independent design audit reports 19/19 synthetic hostile/positive checks passing and `authority_claim=false`; live/provider/typed implementation/native/generator/freeze blockers remain; v11 stays `successor_draft_external_blocked` and frozen pending broader independent review | Design-only evidence; synthetic positives are not live proof. No authority operation, source approval, execution, release, or gate. |
| `2026-09-20T05:37:42Z` source checkpoint | Raw-store source `024c5f612874c5ffa2dfd1520be4f0aa6edb9cd7`; independent report `G0/checker-v2-review/024c5f61-g3-raw-store-lifecycle-review.md`, SHA-256 `a289efec6fab7e89fe63777442df747fd46506c156802678aab985a3b6286099` | Scoped lifecycle **PASS**: 34 focused tests, 294 package tests, 10/10 hostile checks, fmt/clippy; identity-race, anchored recovery, pathname binding, and 128-MiB retention invariants covered | Local bounded contract only; report timing is unstated beyond source timestamp. Production collector/integration, live capture, physical crash/power-loss, Linux, publication, and G2 approval remain disabled/pending. |
| `2026-09-20T05:37:58Z` source checkpoint | APT source `f069789a87b20cd15869c5d9cca8fd96db547b2b`; independent report `G1/reviews/apt-f069789a-independent.md`, SHA-256 `b7917b92510ff0f1b11c41882d6de92b196db1df3cd35b8d4324aff3be0e93c8` | **BLOCKED**: generated live stable/preview paths omit required `--expect-signer`; `VALIDSIG` handling confuses signing-subkey and primary fingerprints; provider/native handoff gaps remain | Source-only review; no publication, install, runtime, merge, or G2 approval. |
| `2026-09-20T05:55:56Z` source checkpoint | Bootstrap source `bba2e758f91e7d40c295a2f034eaeff64ef2f09d`; no independent report for this exact head was present at this earlier checkpoint (later exact-head report is recorded below) | Owner reports full library `1,700 passed / 1 failed`, sole generated `ci-policy.yml` drift; this is source-owner evidence only | No G1 approval. Later exact-head review remains changes-required; generated fixed-point proof and structural repair are not complete. |
| `2026-09-20T05:57:19Z` source checkpoint | Native product source `578a3470f4871ada32916429099627823e861126`; no independent report for this exact head was present in the verified external evidence | Owner checkpoint remains pending independent review; Intel compatibility blocker and no publication/install evidence are recorded | Source-only; no G2 approval, publication, or platform-selection workaround. |
| `2026-09-20T05:57:57Z` | Recovery source `0e344790dfba48765d2c971f2dfe7938840d4649`; independent report `G0/transaction-recovery-review/0e344790dfba48765d2c971f2dfe7938840d4649-review.md`, SHA-256 `978d93e1b9738ec31608166bc21321eb3c8f2cb43b3589e17b5b8f09a030de67` | Bounded transaction/recovery **PASS**: 1,772 library tests, 2 CLI tests, fmt/clippy; cooperative-writer F1–F3, rollback, lock/root replacement, and journal-retention fixtures pass | No power-loss, post-final-check hostile-writer, hosted, collector, runtime, or G0 approval claim. |
| `2026-09-20T05:23:53Z`–`06:17:50Z` | Raw capture `G0/fleet/g0-current-workflow-ruleset-raw-capture-20260920T052347Z/`; report SHA-256 `0afc046c6ce508f7e789bbe98a5238dd202ff07aa697d08d01dc42669a81d10a`; capture metadata SHA-256 `f8d073509f3c11471e878a596f48f312af57598f0e676493996f6a6e98c590bc`; churn SHA-256 `b44e0a4e4e0dc0563bb96484fc8e3d36839e724a35a756ec8e0031b93060a00b` | Historical exact-32 raw observation: 32 repositories and 87 open PRs before/after; 2,095 requests (2,085 HTTP 200 / 10 HTTP 404), 1,304 workflow bodies, 3,865 check runs. Five PR records changed during capture, so normalized identity reconciliation is false; 35 merge SHAs are null and 10 workflow-directory 404s remain explicit unknowns | No jobs, artifacts, logs, build, install, dispatch, release, publication, or gate evidence. Pending paginated job/review/log collection; do not treat changed-ref observations as current. |
| `2026-09-20` UTC; exact review time unstated | Independent capture review `G0/fleet/g0-current-workflow-ruleset-raw-capture-20260920T052347Z-independent-review.md`, SHA-256 `d3aba1f96dbd88170a0737cd3537e2f26b65d16cf1b8133dafe94e627db3aa91` | Raw artifact integrity **PASS**, completeness **BLOCKED**: all 2,095 triplets/hashes, 438 pagination groups, 1,304 workflow blob SHAs, and 3,865 latest-filter check rows reconcile; five PR churn, 10 workflow 404s, latest-only checks, missing jobs/artifacts/logs, and partial historical auth binding remain | Integrity observation only. Check inventory is not all-run/current/G0 evidence; recapture changed refs, non-latest history, jobs/artifacts/logs, and sanitized auth provenance before acceptance. |
| `2026-09-20` UTC; exact review time unstated | Source-boundary review `a546f5973e33464a9b16f205c4602b0113999367`; report `G0/estate-scope-review/review-a546f5973e33464a9b16f205c4602b0113999367.md`, SHA-256 `5835f8603c5226a71c07fb95000779fb13cd117356437f635da4764b7b1bad9f` | Bounded source-boundary **PASS**: fixed 32-scope equality, pinned auxiliary-plan binding, and hostile scope cases fail closed; duplicate JSON-key hardening remains separate | No G0, live-fleet, hosted, runtime, or publication proof; source review only. |
| `2026-09-20` UTC; exact review time unstated | Strict-JSON delta `1d7a6459108a4f5e55cf153622892e1086639c2d`; report `G0/estate-scope-review/review-1d7a6459108a4f5e55cf153622892e1086639c2d.md`, SHA-256 `58528f6310872e3e5aff345cd4d5b63c7404ed5193c99b0aee9beb80fb09c3c0` | Bounded source delta **PASS**: recursive duplicate/escaped-key rejection, nested-array checks, trailing-token and malformed/depth fail-closed probes; 232 package tests and 5 focused CLI tests | Source-only hardening; no fixed-scope, live-fleet, hosted, runtime, or G0 approval claim. |
| `2026-09-20` UTC; exact review time unstated | Offline CAS/checkout review `81f6362d4b43a94e6524954d4f41b882695f3bec`; report `G0/checker-v2-review/81f6362-g3-checkout-cas-integration-review.md`, SHA-256 `c96b2da55beeac80413d396467cac527ad25764eab3266ee8064c00c1272cb74` | Scoped offline **PASS**: 34 raw-store tests, 348 package tests (parallel and serial), strict DTOs, adapter 1/1, inherited hostile 10/10, fmt/clippy | Offline adapter/store only; authenticated collector, live authority, production release, publication, Linux, crash, and G0 evidence remain absent. |
| `2026-09-20T05:55:56Z` source checkpoint; review date exact time unstated | Bootstrap source `bba2e758f91e7d40c295a2f034eaeff64ef2f09d`; exact-head report `G1/bootstrap/source-review-bba2e758f91e7d40c295a2f034eaeff64ef2f09d.md`, SHA-256 `54ee1d5559021a5623c3b0a070de052c6b0138bbd7649180d0e19def02f27567` | **CHANGES REQUIRED**: reachable `./` local-action paths and repository-controlled scripts outside the measured action directory bypass closure; 25 policy tests have 1 generated-marker failure, transport 4/4 passes, clippy has a too-many-lines failure, and generated fixed-point drift remains | No G1 approval; structural source-closure repair, generated regeneration, and fresh independent review remain required. |
| `2026-09-20` UTC; exact review time unstated | v11 frozen design inputs; authority review `G1/bootstrap-transition/v11-independent-authority-transition-review-2026-09-20.md`, SHA-256 `785cb9e9eaf2a64f84cdb072cebb085ede54bfa1d45b06edb47d6c1dc3f911c5`; Luna audit `G1/bootstrap-transition/v11-independent-adversarial-audit-2026-09-20-luna/v11-luna-independent-audit-report.md`, SHA-256 `7b0628135d2490b881b7c09898fb7bdef7514efc2e3d8eafd35ba1b8d1e69e66` | Authority review **not approved**; later Luna design audit records 24/38 pass, 9 fail, 5 unimplemented, `authority_claim=false` (do not combine with the earlier 19/19 subset) | Design-only contract evidence. No execution approval, source approval, live provider, authority mutation, merge, release, or gate. |
| `2026-09-20` UTC; exact review time unstated | Runtime source `5b61841dff1607c6dcb0d9ede9b93b1787bcddd6`; report `G1/reviews/runtime-stall-5b61841d.md`, SHA-256 `d825209c397740974eeba535fdcb7655071e8468ebd2bfbf89e256d9ed9d65b9` | **REJECTED**: 15 focused tests, clippy/check/fmt pass, but a passing cleanup test left an orphan `sleep 3600`; keeper-descendant lifecycle and absolute output-gate claims remain unproven | Source-only runtime review; no host payload, Velnor/Docker run, G4/G5 proof, or gate. |
| `2026-09-20` UTC; exact review time unstated | APT source `71a8a5391e5596c5c37a90376b0c505419dae196`; report `G1/reviews/apt-71a8a539-independent.md`, SHA-256 `1e443be31aa0f21bd2914a040d80c1b5543344685504de4f62c5c6cbef48e233` | **BLOCKED**: signer wiring and ten-field primary/subkey `VALIDSIG` binding pass, but live verifier still sends unsupported `gpgv --no-default-keyring`; provider/native authority remains unresolved | Source-only APT rereview; no publication, install, provider proof, merge, or G2 approval. |
| `2026-09-20` UTC; exact review time unstated | Native source `0dbc7de2377c0f0be6b26f195199726d443d982b`; report `G0/homebrew-contract/native-0dbc-exact-crosscheck.md`, SHA-256 `fa1f88dd0ecf50e042db0f9ede9c0cffb5ae5198385f210fdcb85db50e33f899` | Synthetic Homebrew consumer cross-check **PASS** for arm64 and Intel, including truncated/MH_OBJECT rejection | Synthetic contract only; no install, real release, provider provenance, publication, runtime, or G2 approval. |
| `2026-09-20T06:15:53Z`–`06:17:05Z` capture; review `06:26:05Z` | Eight-skills raw inventory review `G0/fleet/skills-raw-20260920T061548Z-independent-review.md`, SHA-256 `54d82f0f61967055c907d41c4d536ee250246d80d2144c2fcbe11ffecc1f5a13` | Independent raw inventory **PASS only**: eight unique repos/default SHAs and 226 raw files rehashed; all eight have zero workflow paths, 16 protection 404s, empty checks/statuses, and aggregate `pending` | Inventory only; missing workflows/checks/statuses are incomplete evidence, not N/A/success; no G0/G3 gate. |
| `2026-09-20` UTC; exact review time unstated; package assembly `06:35:20Z` | Evidence candidate `de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d`, parent `38269cf0ebe2c6c007f80fa78458fc63d40f76a5`; independent report `G0/evidence-checkpoint-review/de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d-packaging-review.md`, SHA-256 `919fafed1586e2ccfec9fa1a186a156674e764ec5a1a1790b3ed3ce03e7773b7` | Append-only tree **PASS**, byte-exact package integrity **FAIL**: 105 CRLF response blobs were normalized to LF, losing 2,860 bytes; the same 105 `SHA256SUMS` entries fail | Packaging failure only; prior packaging checkpoints remain historical. Repackage raw bytes and regenerate controls; no attestation, source approval, or gate. |
| `2026-09-20T06:52:33Z` correction cutoff; review date exact time unstated | Evidence candidate `1101eec44291c59992ead80a14c400bfb1bc7221`, exact parent `de326237e604ecd8847e6ee7cf5ecb82f1b7ba9d`; report `G0/evidence-checkpoint-review/1101eec44291c59992ead80a14c400bfb1bc7221-packaging-review.md`, SHA-256 `f752d6b4134e553096d4b7e7e1a32b75912950857fe6fc2271ea11b606d4e38d` | Correction-scope packaging **PASS only**: append-only 286-file tree, corrected 281-file Skills partition, raw-byte and 285-entry manifest reconciliation; de326's 105 CRLF/LF mismatches remain immutable historical failure | Packaging integrity only; `attestation=none`, `gate_status=not-evaluated`; no source admission, publication, release, or G0–G7 claim. |
| `2026-09-20T07:06:24Z` target capture; review date exact time unstated | Corrected native target `G0/native-routing/g3-workload-inventory-20260920T070624Z-corrected-successor.md`, SHA-256 `78727eaf79289bb5242fb3fac45937ba41b14a668599dec1a0643204f02edc0c`; independent review `G1/reviews/g0-native-workload-inventory-78727eaf.md`, SHA-256 `bbf58a942dc630c669952b94f49f53c19d8db5f82ce8a0c174562cd0e3f5af63` | Corrected source inventory **APPROVE_CORRECTED_SOURCE_ONLY**: 62 expected IDs = 54 non-marker IDs + 8 marker-suppressed required IDs; all six source checkouts clean and 108 pins match | Source-only; eight required IDs remain obligations. No hosted Swift/Xcode result, provider capability proof, generated-output proof, rollout, G0/G3 gate, or execution approval. |
| `2026-09-20T07:10:25.299308Z` | Corrected Skills contract `G0/skills-adapter/workload-contract-20260920T070005Z-corrected/contract.json`, SHA-256 `f0d85edb989a331495a5ff6f5dbed0ded55233fe1aab5547780dc796164767de`; README SHA-256 `bd6e68ac3f6640c8496fb54a8fad259c15b777286bcc0e515f90e280dd47bdf0`; successor changelog SHA-256 `60b0c69d25441270c10f14618421827c4b564cb56fe0cc80fa662deee07c12db`; predecessor SHA-256 `48bfaab27b6dc79a60433c7b298043c1969ee6037bfbdd548225cd10baf72bb5` preserved | Corrected source contract is `source_derived_not_execution` with `gate_status=incomplete`; all eight repositories share four logical workload categories, not four observed jobs | Source-contract disposition only; no helper/generated/workflow/Velnor execution, required checks, G0 proof, or G3 rollout. |
| `2026-09-20T07:19:23Z` source commit; independent review cutoff `2026-09-20T07:33:34Z` | Integrated estate strict-JSON source `3c7a732f26c77fa5c3e9cc58a2233c52440ce469`, clean local/remote branch; report `G0/estate-scope-review/review-3c7a732f26c77fa5c3e9cc58a2233c52440ce469.md`, SHA-256 `90a1a64094e58aec9a5eab762157218487e49d871f54bf76187bf130ae7d83c9` | Independent review **APPROVE bounded source integration**: all-targets 232 passed, `audit_ci` 90 passed, five CLI checks passed, clippy/fmt/diff passed, and the compiled config digest was `ef6fa934b7b0431579aad8c92e2c5386b48e49210055249113659a03099b1f79` | Source-only bounded approval; no G0 approval, live fleet reconciliation, publication, runtime, or gate claim. |
| `2026-09-20T07:25:00Z` source commit time | Historical bootstrap owner checkpoint at source `f5f166f70eb7547ea3f3eb3ba04160681ef01cda`; exact local focused `candidate_namespace_scan` test returned 3 passed, clippy and fmt passed; full library returned 1,702 passed / 1 failed on generated `ci-pr.yml` drift | Later exact review records source seams **PASS** and adoption **CHANGES REQUIRED**; generated pin/snapshot drift remains and the builder digest is empty/unassigned | Source-only; the generated drift is not a full-suite pass and no G1 approval, publication, or gate claim is made. |
| `2026-09-20T07:34:11Z` report capture; review date `2026-09-20`, exact time unstated | Rust-consumer v3 source-contract report `G0/rust-consumers/workload-dependency-inventory-20260920T064408Z/report-v3-20260920T073411Z.md`, SHA-256 `91b89afd01dcd838527b1737bd967957398da932756de097591315565968b5af`; review wrapper SHA-256 `8bc39e4ca56fa9dea331b8007f4ada51a1f377cfca75e50f85445ba2d5606da2` | Bounded source-contract review records the corrected ten paired Rust units and GitHub-only Swift/Ubuntu/no-Darwin wording; detached source snapshots and raw workflow-source provenance match the report | Source-contract/provenance only; any focused execution, runner, G0, or G3 review remains pending. v2 and earlier inventory reports remain historical. |
| Historical citation; original capture time unavailable | Prior source-owner row cited runtime source `d97c29b52c632d424f0231610dd7fde1b3f3850c` and mutable report `G0/hosted-config/report.md` as SHA-256 `de299defeb09f6cde4d149eb85a7ba502a8fabccb73fe2a9f957188e4e8de05b`. The searched immutable evidence ref contains only `evidence/github-first-dual-lane/G0/hosted-config/report.md` at commit `264315ff5f6778d62d297ffb3f4df34e1ef27a8f`, SHA-256 `efdee594f33cf365e6203a8349da9617853f6e443dd09444bcece032fb1bdf99`; it does not match `de299…` | The old bytes are not recoverable in the searched immutable refs. The prior d97 owner claims are therefore **unverifiable as that historical artifact** and are not transferred to the current mutable report | Lost historical citation; no source admission, independent review, hosted/live proof, G1, G4/G5, or gate claim. |
| `2026-09-20` UTC; current mutable report observed after citation audit; exact capture time unstated | Current mutable `G0/hosted-config/report.md`, SHA-256 `1ffa9a6a1a3cf6f186b789d6157700ae516ae8f11d5faf5fb56c701f4a71400b`; its later appended sections include the d97 owner checkpoint and subsequent security/runtime notes | Mutable owner observation only: the file states 21 focused stall tests and 1,790 library tests for d97, but these appended bytes require a new immutable evidence capture and independent review | Current report is not an attestation or gate result; no source admission, hosted/live proof, G1, G4/G5, or gate claim. |
| `2026-09-20T07:34:24Z` target capture; review date `2026-09-20`, exact time unstated | Partition index `G0/partition-index-20260920T073424Z/index.json`, SHA-256 `43cb2affeca84e59379844bcef1d53d7ead2e1b9596c36bb3554ec012d7c72ca`; independent report `G1/reviews/g0-partition-index-43cb-independent.md`, SHA-256 `6e5e95e01ceae0cc903575f9136e86c83249847ba412f4945210f000ed720b92` | **APPROVE source integration-bookkeeping only**: exact 32 names, disjoint 8/6/6/7/5 partitions, 194 expected IDs, `actual_jobs=null` for all rows, and 23 source-artifact/5 review hash joins | Source inventory only; canonical schema validation and execution remain incomplete. No G0, hosted, release, or gate approval. |
| `2026-09-20T07:40:00Z` source cutoff; evidence commit `07:56:10Z` | Evidence candidate `c391d96a3d1e3cc434f83c5481949f25bdbe49d6`; report `G0/evidence-checkpoint-review/c391d96a3d1e3cc434f83c5481949f25bdbe49d6-packaging-review.md`, SHA-256 `9ce387c543fda0f935d7cc71fd1d634c671c8a62ce23a1640263e6512fe3da9a` | Packaging integrity **PASS only**: 107 selected files / 3,401,260 source bytes, 110 checksum rows, and fresh-clone byte/hash checks pass; de326 byte-exact failure remains unchanged | `attestation=none`, `gate_status=not-evaluated`; packaging only, no source admission, execution, or G0–G7 claim. |
| `2026-09-20T07:57:34Z` review cutoff | Source integration `4ee8ab4b454fd674558d892511f38cf5365aba47`; report `G0/rust-scan-review/review-4ee8ab4b454fd674558d892511f38cf5365aba47.md`, SHA-256 `4ced3d90a5953c1a2455b7aff3f0ad8664ce97fb26e58b8a54a46fc9c9c7387d` | Independent review **APPROVE bounded scanner/source integration**: 1,924 tests across 20 suites, focused include parser 20, clippy/fmt/diff pass; generated check fails closed on stale `.github/ci/project.toml` and generator state | Scanner/source only; no generated-surface, native, G1, hosted, release, or gate approval. |
| `2026-09-20T15:24:52+07:00` read-only capture | Main delta map `G0/fleet/main-delta-map-20260920T152452+0700.json`, SHA-256 `e9766415bb8d317d569a97f1dfbb2876ef72938a7e43bee919ce40ab6b98bb34` | Main `58b5c8122bb7aa2c0431dfea64bd7daf50c55624` differs from integration base `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`; six shared paths require semantic replay and native/scanner/estate effects are separated | Read-only dependency map only; no merge, rebase, cherry-pick, pin, regeneration, or supersession decision. |
| `2026-09-20` UTC; exact review time unstated | Mapper source `6cdeae4701db29901a1250439f520d7c9b449cb3`; report `G0/checker-v2-review/6cdeae47-g3-combined-mapper-review.md`, SHA-256 `705972d2a983486708059487f11b259e81a940bb082cc04df447680e9fb8765c` | **REJECTED acceptance-ready**: 358 package tests, 12 focused mapper tests, fmt/clippy pass, but no captured positive through the full mapper, raw request/response provenance, trusted CAS/live binding, or caller-authored graph rejection | Offline mapper review only; producer adapter/live `verify_g0` remains unwired and no G0 claim follows. |
| `2026-09-20` UTC; exact review time unstated | Runtime source `43ba3b414f245bf3aa9176afce5bf97f2d1e5235`, parent `d97c29b52c632d424f0231610dd7fde1b3f3850c`; report `G1/reviews/runtime-stall-43ba3b4.md`, SHA-256 `057817bbefcfe6a0ffdac1cca483764d08cffe163ef5a91ccc340ce57e4b0e0d` | **REJECTED**: 24 focused and 1,793 library tests pass, but allowing `setsid`/`setpgid` escapes contradicts the whole-process-tree contract and permits detached-child false-green success | Source-only runtime review; no host payload, Velnor/Docker run, G4/G5 proof, or gate. |
| `2026-09-20` UTC; exact review time unstated | Native-policy integration `24e62277fafe0a2b63885d1bafed60f2e756daf`; report `G0/native-review/review-24e62277fafe0a2b63885d1bafed60f2e756daf.md`, SHA-256 `dd3fc7338ae73e6494cbef00cf1bc378e5b85164ff3b8507949db854e0cc6edb` | **CHANGES REQUIRED**: bounded native-policy/scanner checks pass, but full library is 1,810 passed / 2 failed on pinned runtime bytes and generated drift; four generated artifacts remain stale and legacy macOS-15 runtime code is not migrated | Source integration review only; no generated adoption, native publication, hosted run, G1, or G2 approval. |
| `2026-09-20` UTC; exact review time unstated | APT source `a34b4ef8cbc60ca0a8753cf8d0b724cfd4d99577`; report `G1/reviews/apt-a34b4ef-independent.md`, SHA-256 `e440c5dab6423937f0d3923b05524f960cc2ac9c9704d730451e286a8e9a6d49` | Owner-URL defect is closed for observed Organization/User API shapes; focused APT tests pass, but repository numeric identity, synthetic User fixture, native producer handoff, and attestation wiring remain open | Source-only APT review; no publication, install, canary, merge, or G2 approval. |
| `2026-09-20` UTC; exact review time unstated | Bootstrap source `f5f166f70eb7547ea3f3eb3ba04160681ef01cda`; report `G1/bootstrap/source-review-f5f166f70eb7547ea3f3eb3ba04160681ef01cda.md`, SHA-256 `dcd5bbca550b1df0ac55ff5f59965d42a2eba5f0119e91cd5114d30e54235751` | Source seams **PASS**, adoption **CHANGES REQUIRED**: transport fixtures 4/4 pass, but checked-in workflows remain stale against the declared generator pin and builder digest is empty/unassigned | Exact-source/offline review only; no generated pin adoption, image proof, candidate execution, hosted run, or G1 approval. |
| `2026-09-20T08:50:00Z` source cutoff; candidate commit `08:55:48Z`; review date `2026-09-20` | Evidence candidate `339527872a63d5e36ed172fee267d500fdde9510`; report `G0/evidence-checkpoint-review/339527872a63d5e36ed172fee267d500fdde9510-packaging-review.md`, SHA-256 `9ee9368085dd6648b36a0eb97ecac18d662031aa4be2c6782c682bdd76519a69` | Packaging integrity **PASS only**: 48 source files / 3,034,649 source bytes and 28 real-API raw files reconcile through 51 strict checksum rows in a fresh clone; full G0 raw bodies remain intentionally excluded | `attestation=none`, `gate_status=not-evaluated`; packaging-only evidence, no source admission, authority, execution, or G0–G7 claim. |
| `2026-09-20` UTC; exact review time unstated | Checker successor source `a185eb512f857c23ab4a14f980bae11f50c5b8a7`; report `G0/checker-v2-review/a185eb512f857c23ab4a14f980bae11f50c5b8a7-successor-review.md`, SHA-256 `8da9fc0019546b0fae1e9755220123751d88cb7b4ee55a638b9ed857a7969e06` | **CHANGES REQUIRED**: singular-body fallback is closed and `check_suites[]` pagination/count/link/duplicate checks pass; 260 tests/clippy/fmt pass, but page/per-page/cursor/filter queries are not bound to typed metadata/provider semantics | Exact offline checker review only; producer CAS/live adapter remains unwired, no live authority or G0 claim. |
| `2026-09-20` UTC; exact review time unstated | Raw-store source `d4777d9f431432165819f1178d5b288dae2a1455`; report `G0/checker-v2-review/d4777d9f-g3-raw-store-success-cleanup-review.md`, SHA-256 `9991accb7f5c1ae797d15e4b39618589584acb1793b1679c18cef74e41934075` | **REJECTED** strict race-safe retention: an observed transaction pathname disappearing after FD verification is treated as successful discard on `ENOENT`, losing failure evidence; quarantine-name disappearance has the same fail-open shape. Exact suite reports 362 passed / 1 ignored | Source/retention review only; no production collector, live capture, crash/power-loss, Linux, publication, or gate claim. |
| `2026-09-20` UTC; exact review time unstated | Native successor source `04b51d761c5a08ebd9626fe74bb2c6c3e66370e8`; report `G0/native-review/review-04b51d761c5a08ebd9626fe74bb2c6c3e66370e8.md`, SHA-256 `58aae889601fc2f2c4d8152bea3bb95bd1981cc955e13556ab9d8dc1f29772b2` | Scoped legacy runtime-product centralization **PASS**: producer follows shared `xcode-27` authority and focused native/scanner/platform checks pass; integration remains **CHANGES REQUIRED** for inherited S2 byte-pin and four generated-artifact drift | Source-only bounded review; no generated adoption, hosted/native publication, G1/G2, or gate approval. |
| `2026-09-20T08:53:18Z`–`08:54:00Z` supplement capture; review date `2026-09-20`, exact review time unstated | Real-API supplement `G0/real-api-fixture-supplement-20260920T085311Z/`; independent report `G1/reviews/g0-real-api-fixture-supplement-114676-independent.md`, SHA-256 `cc207ebf8ff0e0e43aeb1e7d520df773e7002016629e865f5af82f0c2a47da2d` | Raw/relationship checks are bounded (72 raw files, 18 requests, seven list pages, four Apps), but metadata is **REJECTED**: one duplicated request record has a truncated GMT header and a non-absent `link_header` where raw headers have none | `read_only_source_supplement_not_gate`; repair and rehash manifest/relationship metadata only. No jobs, artifacts, execution, authority, or gate claim; base corpus identity remains preserved. |
| `2026-09-20T09:47:00Z` source cutoff; candidate commit `2026-09-20T09:59:04Z`; review date `2026-09-20` | Evidence candidate `264315ff5f6778d62d297ffb3f4df34e1ef27a8f`; report `G0/evidence-checkpoint-review/264315ff5f6778d62d297ffb3f4df34e1ef27a8f-packaging-review.md`, SHA-256 `74b7beb6851e9b1fe52f0640251b535145c5309fccd857ba837d377eb97ec7a2` | Packaging integrity **PASS only**: fresh-clone readback reconciles 134 regular source files / 1,665,184 source bytes and 137 strict checksum rows; corrected metadata-only API successor is included and prior raw content is preserved | `attestation=none`, `gate_status=not-evaluated`; packaging-only evidence, no source admission, authority, execution, release, merge, or G0–G7 claim. |
| `2026-09-20` UTC; exact review time unstated | Checker successor `32100eba010a2db137db4e7df624d6af8b114de7`; report `G0/checker-v2-review/32100eba010a2db137db4e7df624d6af8b114de7-successor-review.md`, SHA-256 `34f1aca2d4d179bb787772561b71c77d2d6f79b7581cc07377930987c9f49dae` | **CHANGES REQUIRED** for full public-chain proof: endpoint/query contract and 262 tests/clippy/fmt pass, but typed provider rows remain synthetic; real App/suite/provider semantics and wrong-member tampering remain unproven | Offline checker review only; live collection, all-32 closure, current/recursive reconciliation, checkout authority, producer adapter, and G0 remain unavailable. |
| `2026-09-20` UTC; exact review time unstated | Raw-store source `6dd5cd28ddee3d8d3ecc87678eb421eb9ab2f9f0`; report `G0/checker-v2-review/6dd5cd28ddee3d8d3ecc87678eb421eb9ab2f9f0-g3-raw-store-refactor-review.md`, SHA-256 `2f1baf4700fa165d4e8a4bc8de82fdafbd86c84368997148b761dabc6c4666f4` | Bounded refactor **PASS**: focused store tests report 39 passed and package tests 371 passed / 1 ignored; full-target clippy exits 101 with 15 diagnostics in the mapper test file, none in the changed store file or new expectation type | Source-only bounded review; no integration, live capture, collector, G0/G3, publication, or gate claim. |
| `2026-09-20` UTC; exact review time unstated | Bootstrap source `3ed0023b038335d7b22dfa2758457e3808f777ee`; report `G1/bootstrap/source-review-3ed0023b038335d7b22dfa2758457e3808f777ee.md`, SHA-256 `1c379a17c677c70cb72469370993dcba3c963cf4e04ed4b339a96505b3810149` | **PARTIAL / CHANGES REQUIRED**: four transport fixtures, fmt, clippy, and offline generated checks pass, but generated snapshot/test drift, unbounded non-archive transfers, producer-path admission, and host/race/extraction proofs remain open | Exact source/offline review only; no generated adoption, image proof, candidate execution, hosted run, publication, or G1 approval. |
| `2026-09-20` UTC; exact review time unstated | Native source `a8fd18e61edd02fd0db741a7edf13ecafdf4b0e1`; report `G1/reviews/native-a8fd18e61edd02fd0db741a7edf13ecafdf4b0e1-independent.md`, SHA-256 `8379d2f3baee697029b766efe802de124ce490d2b8752f0e29972f1db049dc59` | Offline stable/preview census and partial/missing/duplicate hostile cases pass, but full library is 1,745 passed / 1 failed on generated drift and the native-product signer boundary remains unresolved | Scoped source/fixture review only; no install, publication, hosted run, G1, or gate approval. |
| `2026-09-20` UTC; exact review time unstated | Native source `f6cb27c4606103d0c879bbc60a6a060911ce7b91`; report `G0/native-review/review-f6cb27c4606103d0c879bbc60a6a060911ce7b91.md`, SHA-256 `4b203fb3379ca8c8776926c856bbd34f5ac46da607ef1f2643c6c427644d7dbc`; aggregate correction report `G1/integration/review-draft-regeneration-f6-aggregate-correction.md`, SHA-256 `d072b0fa00e85de9b922cbc768aaab622063f70a4fbea33951d1bc5e14791443` | Scoped source/test fixture review **PASS** and narrow aggregate metadata correction **PASS**; prior S2 byte-pin failure is resolved, but the full library remains 1,811 passed / 1 failed on generated drift and four generated paths remain stale | Metadata correction is not a rerender or adoption; no source authority, generated adoption, publication, G1, G2, or gate approval. |
| `2026-09-20T11:03:00Z` source cutoff; review date `2026-09-20` | Evidence candidate `31cdf59d45cc975ebf7244d1e1d111974f2f62b1`; report `G0/evidence-checkpoint-review/31cdf59d45cc975ebf7244d1e1d111974f2f62b1-packaging-review.md`, SHA-256 `6af6031067a13db28b3a3e7c965a0542bad57e6f9c1e8c646bbfdc2635e9f296` | Packaging integrity **PASS only**: 87 declared regular non-symlink source files / 3,131,231 source bytes reconcile 87/87; top-level 88/88 and nested 5/5 plus 57/57 checksum controls pass; missing requested `8fd6…` input is excluded | `attestation=none`, `gate_status=not-evaluated`; packaging-only evidence, no source admission, current collector, G0/G1, or gate claim. |
| `2026-09-20` UTC; exact review time unstated | Source integration `7df0481e3f38f9f662d25cd963c99d24bc32357b`; report `G1/integration/review-7df0481e3f38f9f662d25cd963c99d24bc32357b.md`, SHA-256 `bab4d4d23a3599cec5d0144e7f2d9bc29884d03467c08b002bb89198c54eed44` | **CHANGES REQUIRED**: 1,831 library tests, fmt/clippy, estate CLI, and diff checks pass, but fixed-point validation fails and four generated paths are stale (`actionlint.yaml`, `project.toml`, generator state, runtime-products); source requires `xcode-27` while checked-in runtime remains `macos-26` | Exact source review only; no generated adoption, hosted execution, G1, or gate approval. |
| `2026-09-20` UTC; exact review time unstated | Executable 4fa probe report `G1/bootstrap-transition/BOOTSTRAP-PATH-FEASIBILITY-EXECUTABLE-4FA-7DF-XCODE27-2026-09-20.md`, SHA-256 `2780e3f22efdbd2692033792f4a3fe42b6647f6e0a3ba8ee281876864a420b23`; paired JSON SHA-256 `c43041d5dd9b6cfe6c5718f85485d6aa1464a3e8675b3e2f4cddf5b4f789848b` | Published 4fa admits its baseline hosted labels but rejects the isolated `xcode-27` fixture as an undeclared provider selector; clean and fixture checks also expose generated drift | Read-only feasibility probe; no validator transition, candidate execution, hosted authority, G1, or gate claim. |
| `2026-09-20` UTC; exact review time unstated | Hostile transport report `G1/reviews/bootstrap-transport-hostile-fixtures-f2f1a3b0.md`, SHA-256 `4f6aed10d664730fcba212a3e2667ec29a9c01cc26410ff34291aefbcfb10850`; exact owner-tip review `G1/bootstrap/source-review-80ceab4b4a9f15f88e2ce016c6c3864d2de5a3e0.md`, SHA-256 `a17ceb67f4d44c7e9d116724620c9e6e49e46b406228b8ae280bcf3c8d39970b` | Owner tip records **5 hostile tests passed / 1 intentional red**: scanner and partial-path race pass, but same-name cross-job replacement remains unbound to the producer job; artifact ownership is not proven | Source/fixture evidence only; no G1 security, hosted, artifact authority, or gate claim. |
| `2026-09-20` UTC; exact review time unstated | Run-isolation decision `G1/reviews/bootstrap-artifact-binding-run-isolation-followup-independent-review-20260920.md`, SHA-256 `ecbf65c91d44ce6abcb0bc1053acc35d81653eb97105e317684c112c902692a7`; selected design SHA-256 `56bf72bca3ddf60503f567326c5a90399b562510130b784e75e0805e533e647f` | Local generator-only work is permitted before hosted proof, but acquisition remains hard-disabled: `provider_cross_run_binding_proof=Unknown` yields `ProviderProofPending / NotAuthorized`; no green or fallback path | Design/source boundary only; hosted cross-run binding, live collector terminality, G1, and authority remain unproven. |
| `2026-09-20` UTC; exact review time unstated | Native source `9908296d28d27e0d5b993d1e48ea7a96bc31db83`; review `G1/reviews/native-id-9908296d-independent.md`, SHA-256 `b1ea37e593a2b487444b9245822f8f6e37669f6906e7decdcd9f81a3f6551887`; reconciliation `G1/reviews/native-id-9908296d-reconciliation.md`, SHA-256 `d31052a65c0d3326aaefa1616755fe43f20b42a0dc61c87bff6e2a5f2b85db4d` | Native source renderer is a bounded **PASS** for canonical string `release_id`; reconciliation confirms checked-in generated stable runtime still has the numeric filter at `release.yml:4638` | Source/generated drift only; no generated adoption, package install, publication, producer authority, G2, or gate claim. |
| `2026-09-20` UTC; exact review time unstated | Native signer source `61cb7fdfbcac0eb18676f2d4c47189517f832b3a`; report `G1/reviews/native-signer-contract-61cb7fdf-independent.md`, SHA-256 `642ba39bb48dad69ee6b5f8275ef1bed6fbf0cf8818d74dd6e1a581beedd3141` | Source/render/local signer tests are bounded **PASS** only; no Linux/arm64 build was executed; generated caller/DAG remains unbound and preview cross-toolchain changes are source-only | Contract/source review only; cross-build caller clarification, hosted execution, install, publication, G1/G2, and gate remain pending. |
| `2026-09-20` UTC; exact review time unstated | Checker source `efdccb5d12a075c8adebd24e6da4ae895aab56e4`; report `G0/checker-v2-review/efdccb5d12a075c8adebd24e6da4ae895aab56e4-g3-checker-review.md`, SHA-256 `82ccff4164185c69536b99bfbaa5422c178cd588fcffe19688cbb5146cdf6738` | **CHANGES REQUIRED** for composition: bounded checker tests (46 focused, 264 package) and fmt/clippy pass, but the closed endpoint map rejects collector raw kinds and source/graph bindings can be orphaned or cross-endpoint; full collector→mapper→checker was not run | Offline checker review only; terminal/current collector evidence is unconfirmed, producer adapter/live authority remains unwired, and G0 is incomplete. |
| `2026-09-20` UTC; exact review time unstated | Generator source `1fc2c8c38a595cce1559de4bf0a29227348a1a7c`; immutable report `G0/hosted-config/source-review-1fc2c8c3.md`, SHA-256 `c4b1b1a234ed58304e1cedcf962176617178a9b70c512b9bbd4dcaed52c929f1` | Owner source run reports 1,836 library tests passed with the generated fixed-point test intentionally filtered; no generated outputs changed and drift remains | Source-only checkpoint; independent review is pending, so this is not an all-tests-green result or G1 approval. |
| `2026-09-20T12:03:46Z` owner-health observation | Prefetch source `e04ff8b1d15efd3ca97609d528e3a26c1fa14bb0`; owner-health `G1/integration/execution-health-owner-audit-20260920T120346Z.json`, SHA-256 `ac091d559189a6baebbdd96dea08d2ac9a7f2d785b44f352227b3742096d6bde` | Owner handoff reports 9 source-authentication tests; the exact immutable review report is not present in canonical evidence. Source branch was clean and remote-equal at observation | Owner-only evidence; independent review remains pending. No source approval, hosted proof, or G1 claim. |
| `2026-09-20` UTC; exact review time unstated | Homebrew source `86bc621bdeec8b408a854d902b002b807c93e975`; report `G1/reviews/homebrew-86bc621-independent.md`, SHA-256 `c759150d4ae9b8db88d6b73a6f760dcf0e4665ab4f40afddd5a4452245675a76` | Exact checkout, cleanup, focused checks, and real online audit pass; strict audit remains **BLOCKED** by inherited Homebrew credentials/config, temporary-parent containment, and symlink confinement | Source/consumer review only; no install, publication, dispatch, or G2 approval. |
| `2026-09-20T12:02:22Z` observed; correction JSON captured `12:02:44Z` | Collector correction `G1/dependency-graph-refresh-20260920T085949Z/delta-20260920T120244Z.json`, SHA-256 `2c17a6202187e5cf265b3ced5517840f67ca2ff3d3fcf0502a9bd731e5b19189`; collector `413189538fa03e069bb82a92f69d75a70beaf72d` | Timestamped correction records 180 original files, 705 refs (704 JSON + 1 `.txn`), and 180 SHA files | Collector was still active with no terminal full-coverage result; serial dependency traversal/dedup and collector→mapper→checker validation remain incomplete. These are category-qualified counts, not a total-coverage claim or G0 proof. |
| `2026-09-20` UTC; exact review time unstated | Hosted runner matrix `G0/distribution-review/hosted-runner-matrix-20260920T112021Z-successor-review.md`, SHA-256 `8f96159e1b6026921be0ba29bf21178bdf925969cede8e2b3409024fab573e60`; matrix SHA-256 `381ed3717b14e9e74943d1aa02fa314a9cc2fa858e6c5656e53ed0075332b4ce` | Public-source mapping validates 138/138 manifest entries, 29/29 labels, and 28/28 image documents; `xcode-27` is arm64-only and `ubuntu-slim` remains unknown | Capability-reference only: no entitlement, capacity, routing, or hosted execution proof. |
| `2026-09-20T11:59:11Z` read-only reconciliation | `G1/bootstrap-transition/AUTHORITY-PLAN-RECONCILIATION-9E5-4FA-2026-09-20.md`, SHA-256 `18d75a1b5a0e29b0d46da1810966fa07e30a4632c0636775691327af89370bb8` | No accepted executable authority-transition plan; v12 remains frozen/external-blocked with `authority_claim=false` and `execution_authorized=false`. Live main is `9e5c0eb215d4169578d6f064806e89fe4c793e85`, trusted pin `4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d`, current Mac publisher `macos-26`, and typed validator is absent; 4fa rejects `xcode-27` and PR-candidate fallback is forbidden | Authority/G1 execution remains unavailable; no authority mutation, source approval, merge, publication, or gate claim. |
| `2026-09-20` Asia/Ho_Chi_Minh; exact review time unstated | Canonical hostile-bootstrap review `G1/reviews/bootstrap-transport-hostile-owner-80ceab4b-independent.md`, SHA-256 `0c4d4b3b1f9c1773cd27d6dba61e5d7f07a17a518bb6cc349a838d55653d2095`; fixture `f2f1a3b0c45bc2d69e7a581719d29f66e6373a64`; owner source `80ceab4b4a9f15f88e2ce016c6c3864d2de5a3e0` | Fixture review is valid: 5 tests passed and 1 intentionally red cross-job provenance case | Offline fixture/source review only; no hosted execution, artifact authority, security attestation, or G1 approval. |

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
| G0-runtime | Offline CAS/checkout integration `81f6362d4b43a94e6524954d4f41b882695f3bec` passes its bounded adapter/store review; runtime `5b61841dff1607c6dcb0d9ede9b93b1787bcddd6` is rejected after an orphan keeper descendant survived a passing cleanup test | Live collector/production store remain absent; actual Mac remains deferred until G3, with no absolute output-gate, power-loss, Linux, or G0 proof |
| Native macOS version policy | GitHub-hosted Velnor macOS workloads must use the newest actual major/architecture; current verified macOS 27 arm64 label is exact `xcode-27`; macOS 27 Intel has no supported label and cannot fall back to `macos-26`/`macos-26-intel`/older/alias/skip | Amendment recorded; official-label research/PR and exact runner/image/SDK evidence pending; immutable action/image pins remain required |
| Runner protocol source | `actions/runner` revision `80bb1fb827fa44d489263061e71ef4adba7ad8cd` pinned for later work | Observed; no implementation here |
| G2 native package compile | Three required ARM64 macOS binaries compile/smoke at `abe9ad82`; nothing installed or published | Preliminary only; G2 remains pending |
| G2 product identity | Typed native-product contract `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98` remains rejected; descendant `0dbc7de2377c0f0be6b26f195199726d443d982b` has only a synthetic Homebrew consumer cross-check | G2-native-product remains blocked; no install, provider provenance, package acceptance, or publication |
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
| G0/G2 review checkpoint | Scan `29d6a9` is conditional source-only; checker `2d436be` parser-only; collector/store remain rejected; offline CAS `81f6362` and synthetic native `0dbc7` are bounded passes; APT `71a8a539` remains blocked | External exact reports; no approval, publication, or gate claim |
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
| G0-inventory | Historical raw capture `2026-09-20T05:23:53Z`–`06:17:50Z` covered all 32 repositories and 87 open PRs before/after; five PR identities changed, with 35 null merge SHAs and 10 workflow-directory 404s; independent integrity review passes but completeness is blocked | Inventory only; checks use `filter=latest`, normalized identity is false for this capture, and jobs/artifacts/logs/reviews remain pending; workload, dependency, execution, and gate evidence remain separate |
| G0-bootstrap | Later exact `7df0481e3f38f9f662d25cd963c99d24bc32357b` source review is **CHANGES REQUIRED**: 1,831 library tests plus fmt/clippy pass, but four generated paths drift and source `xcode-27` conflicts with checked-in `macos-26`; 4fa rejects the xcode fixture; hostile transport remains 5-pass/1-red | Regenerate/reconcile all four outputs, repair provider/runner contract and cross-job artifact ownership, then obtain fresh exact-head review; no G1 approval; older 3ed/f5 findings remain historical |
| G0-distribution | Assigned; result pending | Revalidate product/runtime discovery and both channels |
| G0-fleet | 32-row inventory complete; workload matrix/review pending | Build workload/platform/category matrix |
| G0-runtime | Exact runtime `43ba3b414f245bf3aa9176afce5bf97f2d1e5235` is **REJECTED** despite 24 focused/1,793 library tests: detached `setsid`/`setpgid` descendants remain outside cleanup, contradicting the whole-process-tree contract; d97 is historical parent context | Keep actual Mac deferred until G3; no authenticated collector, production raw store, live authority, absolute output-gate proof, power-loss proof, or G0 gate |
| G0-records | This source-doc checkpoint records the capture-bound rows below; latest packaging-only PASS is candidate `31cdf59d45cc975ebf7244d1e1d111974f2f62b1`; checker, bootstrap, native, and collector outcomes remain bounded and no gate is inferred | Preserve unknowns; packaging is not source admission, e212/4fa/7df and native/checker results do not close G0, current collector terminality is unconfirmed, and all execution gates remain pending |
| G0-checker | Successor `efdccb5d12a075c8adebd24e6da4ae895aab56e4` is **CHANGES REQUIRED**: bounded tests/clippy/fmt pass, but legitimate collector raw kinds and orphan/cross-endpoint source/graph bindings remain unresolved; full collector-to-mapper chain was not run | Add exact contracts and binding negatives, then run authenticated collector/current API with terminal child/log/artifact evidence; offline checker fixtures cannot close G0 |
| G0-reviewer | Independent review rejected initial checker/docs state | Review exact v2 commit and refreshed external evidence |
| G1-cache-semantics | Jobs/provider adversarial review rejected initial checker paths | Retain external findings; review v2 candidate without rewriting old evidence |
| G1-hosted-config | In progress; candidate remains unverified | Thread `01a0ba77-5222-7e63-97fa-553849b96d7b`; recheck clean exact commit/output |
| G1-review952 | `29d6a9caf64d625799efa1ad52c3bba0e4f52db2` is a conditional source-level pass only; `820f6509` and prior scan candidates remain rejected/blocked | Thread `01a0ba77-dd88-7ac2-9fb7-118f3c09d1af`; do not combine historical test counts or transfer review across heads |
| G1-run-operations | In progress; stale-runs evidence updated | Thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`; trace child outcomes |
| G1-runtime-product-audit | Evidence written; next publish verification pending | Same operations thread; old pin verified, candidate remains unpublished |
| G1-seed-pin | Source review written; pin adoption pending | Thread `01a0ba72-3925-7141-b1f7-5529a5cf6c98`; clean regeneration remains required |
| G1-scan-integrity | `29d6a9caf64d625799efa1ad52c3bba0e4f52db2` conditionally passes injected journal regression only; `820f6509` and prior exact candidates remain rejected | D19/generated-state, real filesystem-fault mapping, authority, and recovery-entrypoint gaps remain; no G1 approval |
| G2-native-packages | Corrected source inventory records 62 expected IDs = 54 non-marker + 8 marker-suppressed required; source `578a3470f4871ada32916429099627823e861126` retains Intel compatibility blocker, and install/publication remain unproven | Thread `01a0ba7a-328e-7282-943e-5b54c2ac209d`; source-only inventory does not provide install, provider provenance, platform-selection workaround, or G2 approval |
| G2-homebrew-contract | Consumer `6520aad7bd66d53349e040508146956e9c4f0c1e` rejects producer/consumer handoff against `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98` | Thread `01a0ba80-8408-7380-8ac2-b743eb4494a5`; fix shared checksum/schema contract before provider/client proof; no G2 approval |
| G2-native-product | Native `9908296d` source renderer is bounded **PASS** for canonical string release IDs, but reconciliation records stale generated numeric filtering; signer `61cb7fdf` is a pure contract/source **PASS** while the generated caller/DAG remains unbound | `/root/g2_native_packages`; regenerate/adopt checked-in outputs and wire/review the signer caller, then prove native identity; no install/publication or G2 approval |
| G2-preview-publication | Planned; blocked until G1 | `/root/g1_run_operations`, thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`, worktree `dual-lane-preview-publication`; reviewer `/root/g2_distribution_review` |
| G2-distribution-review | APT `a34b4ef8cbc60ca0a8753cf8d0b724cfd4d99577` closes observed Organization/User owner URL shapes, but repository numeric identity, synthetic User fixture, native producer handoff, and attestation wiring remain open | Thread `01a0ba81-1af6-7f11-9f24-3ff115b8f314`; no publication approval |
| latest_macos_policy | Amendment assigned; exact official label research/PR pending | Isolated AGENTS-rule/research task; exact current arm64 label `xcode-27`, future newest labels supersede; macOS 27 Intel has no fallback; reviewer `/root/g2_homebrew_contract` |
| g3-skills-adapter | Corrected source contract binds exact eight repos/default SHAs to four logical workload categories, not four observed jobs; no workflows/checks/execution and status remains incomplete | Thread `01a0ba7b-0152-7a70-8d18-c2387f1c9469`; corrected contract is source-only, central scanner and G3 rollout remain pending |
| g3-native-routing | Read-only evidence written; generator policy amendment must use newest actual hosted macOS major and explicit incompatibility failure; rollout blocked until G2 | Thread `01a0ba7b-32f1-7af1-a25f-4cde73f1f075`; native routing report only; native/package contract review `/root/g2_homebrew_contract` |
| g3-action-roles | Read-only evidence written; G3 contract incomplete | Thread `01a0ba7b-57a5-7623-bd92-d0666a02b96e`; reusable publisher/runtime gaps remain |
| g3-rust-consumers | Exact `4ee8ab4b454fd674558d892511f38cf5365aba47` scanner integration is independently approved bounded source-only: 1,924 tests/20 suites and clippy/fmt/diff pass; generated project/state check remains failed | Thread `01a0ba7d-bb3f-78c3-84c8-eb0b2d75e5d0`; no generated adoption, execution, or G3 rollout |
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
7. Authority transition remains unapproved. V7 is historical; the
   `2026-09-20T11:59:11Z` reconciliation (`G1/bootstrap-transition/
   AUTHORITY-PLAN-RECONCILIATION-9E5-4FA-2026-09-20.md`, SHA-256
   `18d75a1b5a0e29b0d46da1810966fa07e30a4632c0636775691327af89370bb8`)
   records v12 as frozen/external-blocked with both authority and execution
   false. The trusted `4fa` path rejects `xcode-27` and candidate fallback is
   forbidden; explicit user authority remains required before any transition.

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
