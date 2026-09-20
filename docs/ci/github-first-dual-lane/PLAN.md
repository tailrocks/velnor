# GitHub-first dual-lane plan

Current gate: `G0` (inventory and execution setup). Input revision for this
initial wave: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`. No task may claim a
gate exit without durable evidence under the external ledger root
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/`.

## Durable gate graph

```text
G0 inventory/setup
 ├─> G1 hosted recovery
 │    └─> G2 preview/stable APT + Homebrew delivery
 │         └─> G3 complete hosted fleet
 │              └─> G4 actual macOS/OrbStack pilot
 │                   └─> G5 packaged pilot repaired and passed
 │                        └─> G6 dual-provider fleet
 │                             └─> G7 independent audit
 └─> G0 evidence/checker/reviewer tasks (parallel, no gate bypass)

G4 <────────────── repair loop ──────────────> G5
G6 may reopen G4/G5 when a fleet workload exposes a runtime defect.
```

The graph is a dependency graph, not a success claim. Read-only discovery can
start early; operational actions obey the arrows.

This source graph is the durable contract. Mutable task ownership, revisions,
invalidations, and amendments live at the stable external session path
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/session.json`.
The current read-only G0 workload/platform projection is
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/workload-matrix.json`;
it covers all 32 manifest rows and does not claim generated G3 coverage or
execution success.

## Gate exit contracts

| Gate | Depends on | Exit condition | Required evidence owner |
| --- | --- | --- | --- |
| G0 | — | Exact 32 unique rows, live inventory, settings, access/dependency gaps | `/root`; G0 inventory + records |
| G1 | G0 | Hosted-only recovery PR and resulting main execution pass | G1 hosted-config/run-operations |
| G2 | G1 | Real preview/stable package delivery and clean install/upgrade checks | G0 distribution + G2 package owner |
| G3 | G2 | All 32 hosted migration PR/main revisions pass | G0 fleet + migration owners |
| G4 | G3 | Actual host identity/routing/workload comparison and defects | G0 runtime |
| G5 | G4 | Released installed pilot passes full checklist | G0 runtime + release owner |
| G6 | G5 | Both lanes pass for eligible fleet workloads and resulting mains | Fleet migration owners |
| G7 | G6 | Fresh revision/PR audit and deterministic checker + independent review | G0 checker + G0 reviewer |

## Mandatory pull-request merge gate

No recovery, migration, release, or generated-output pull request may merge
until this complete gate passes for its exact candidate SHA. The owner and
independent reviewer must read all paginated reviews, issue comments, inline
threads, bot comments, requested changes, and feedback added after any fix.

1. Inspect the actual code/config/generated-output diff and the relevant tests;
   do not rely on review labels, summaries, or a green subset.
2. Fix every valid finding, including test and documentation findings. Rerun
   affected checks and inspect the final diff.
3. Record each rejected suggestion and its evidence in the external ledger.
4. Re-read the complete paginated review/comment/thread/bot/requested-change
   set after every fix or new feedback event. Verify required CI and the final
   candidate SHA.
5. Stop if any feedback or required result is unread, unverified, or
   actionable. Merge only with a complete disposition and final main-SHA
   record.

The review snapshot must bind exact candidate head/base SHAs and retain each
review/comment/thread commit ID (or explicit no-commit value), author, state,
timestamp, and stale/current disposition. Paginated `reviewThreads` resolution
and outdated state are required; an incomplete page fetch blocks merge.

This is an execution prerequisite, not a gate-success claim. The authoritative
procedure and command boundary are in
[`RUNBOOK.md`](./RUNBOOK.md#mandatory-pr-merge-preflight).

## Checkpoint and publication discipline

Commit and push coherent WIP or ready checkpoints regularly at safe handoff,
review, and substantive-change boundaries. Every source commit must use DCO
signoff plus `Co-authored-by: Codex <codex@openai.com>`. Push only the normal
task branch; never force-push. A checkpoint record must bind the branch, local
and remote SHAs, clean/dirty status, validation, review disposition, and open
blockers.

Keep source records separate from live evidence. Operational session state,
review/comment/thread snapshots, raw logs, mutable ledgers, and final ledgers
stay external and are linked by path/SHA/digest. Label WIP explicitly. A commit
or push is not review approval, merge, publication, or gate success; merge and
other remote mutations remain separately authorized.

Use small verified logical commits and push regularly at safe checkpoints.
Reuse one branch per workstream. Create an isolated branch only for concrete
conflicting ownership or independent review; do not create a branch per fix.
An existing integration branch is valid when it combines already-approved
sources under one recorded owner and review boundary.

## Native macOS version amendment

Every Velnor workload on a GitHub-hosted macOS runner must use the newest
actual supported major and architecture available at dispatch. The current
verified arm64 mapping is macOS 27 via the exact hosted label `xcode-27`; that
label is an observed mapping, not a ceiling. A future newest actual major and
its exact label supersede it. macOS 27 Intel has no supported hosted label in
the current evidence; it must fail explicitly, never downgrade to
`macos-26`, `macos-26-intel`, another older major, or a lagging
`macos-latest` alias. Capture the resolved label, host/image identity,
Xcode/Swift, SDK, deployment target, and architecture. This amendment does
not loosen immutable action, container-image, release-asset, or digest pins;
pin updates require reviewed immutable identities.

The isolated `latest_macos_policy` task owns the AGENTS rule and official
runner-label research/PR. `g3-native-routing` owns generator policy, and
`g2_homebrew_contract` independently reviews the resulting native/package
contract. These assignments do not authorize a gate exit or change the
G0→G1→G2→G3→G4/G5 sequencing.

## Current candidate-bound checkpoint

These are external, revision-bound observations captured around
`2026-09-19T22:47:37Z`; they do not advance a gate.
A separate 2026-09-20 reconciliation observed live `main` at
`1048337062ea625fada1b4f7c07f2feed75f60c7`, parent `b5a4b4af`; it reports
generator-rendering reproducibility only. The rows below remain timestamped
`b5`-bound observations, not current-main proof. See external
`G1/bootstrap-transition/VALIDATOR-ONLY-DESIGN-2026-09-20.md`.

| Candidate | Observed state | Boundary/blocker |
| --- | --- | --- |
| PR957, source `9e06`, revision `53` | Source-only approval in `G0/native-review/review-pr957-92387e88.md`; 1,888 source tests plus fmt/clippy/check pass | Merge/live not ready; D19 unpublished; Policy `35473052923` candidate acquisition failed; separate typed-validator dependency unresolved |
| PR960, head `2c810f1b46ce8eddb5906fd4bdcc8ae23e78ed40`, base `b5a4b4afaa6ca807927cacc03659b570a895dd5c` | Open; Policy observed in progress; no review decision or merge | Candidate checks/review remain incomplete |
| PR962, head `94b43578cad9720e569780d18dc966370ed47c11`, base `b5a4b4afaa6ca807927cacc03659b570a895dd5c` | Open; required and Velnor-workflow hosted failures observed | Not merge-ready; no gate evidence |
| PR961 historical path | Open head `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303` on base `b5a4b4afaa6ca807927cacc03659b570a895dd5c`; its history contains unsigned `857`, and DCO is `action_required` | Historical record only; not repaired, approved, merged, or a gate result |
| PR963 signed replacement (historical b5 snapshot) | Open head `fb78d85d464fd5082e5c161922afd7942380fabc` on the same observed base; external comparison records the tree-equivalent signed replacement with redundant `857` excluded | Historical only; hosted checks were observed successful, but no disposition transfers to a later head |
| PR963 prior query (superseded snapshot) | Exact API tuple observed at that earlier capture: head `c440d4db3fd59a9e4abd396d7a75e670c4f3d862`, base `1048337062ea625fada1b4f7c07f2feed75f60c7` | Fresh exact-head review/approval was not observed; the historical `0c1ec75753cf9f8044a3a2ff01c2d144e9c59132` review does not transfer. The later live reconciliation below supersedes this query for its own capture only. |

The selected G2 design is a separately typed validator-only product/publisher,
owned by `/root/g0_inventory` and reviewed by `/root/g0_reviewer`. It must not
reuse a three-platform runtime or add a platform-selection workaround.
Publication precedes the separate PR957 pin operation. Secure CAS/sourcegraph
and collector/checker binding remain unresolved; no helper output closes G0.
The scan `38345852` reports five full-suite failures, but exact-parent baseline
attribution remains pending and must not be called a baseline failure.

## Historical v4 external checkpoint — 2026-09-20 (superseded by v5)

This is a historical source-document checkpoint over external evidence. It
does not alter the gate graph, authorize mutation, or claim a gate exit. The
later v5 pair supersedes this v4 proposal; v3 remains frozen and unapproved,
and v4 was **approval-required with no mutation performed** (observed
`2026-09-20T00:27:10Z`; Markdown SHA-256
`53aac3ef7c2112d0428582a74e61c4da013d026e6fb17cdae419cd209f5895dd`, JSON
SHA-256
`1bc1d64db969519e4339b64410aa9a87b53b5cf0e9933f81ecaa16a3593078ba`). The
v4 readiness summary has six unresolved classes, not six approvals; consult
the later v5 disposition below for the superseding structural result:

1. Resolve every App/operator/watchdog/integration/source/PR/tree/lease and
   ruleset placeholder.
2. Run the real old-parser `static_files` fixture and retain parser,
   fixed-point, policy, byte, and raw-current-tree outcomes.
3. Review the source-owned B publisher/generator, recompute its closure and
   graph, and prove two independent byte-identical renders.
4. Prove disposable-writer freeze, signed lease/watchdog recovery, ruleset
   hashes, and merge guards.
5. Capture the complete live Main-B run/job/check/artifact/release/native
   census and bind it to B.
6. Obtain owner approval plus independent `g0_reviewer` and
   `authority_transition_review` approval, then prove preflight cleanup.

The measured `static_files` result is transport-only: external
`G1/bootstrap-transition/TYPED-PUBLISHER-INPUT-FEASIBILITY-2026-09-20.{md,json}`
(SHA-256 `8764712693e2883d05846de05a3c2137fb3d31e3ee97ab0ad129186f3ff960f4`
and `01ad40d473aa26ae7e5e814ef7c3f93e854d682d2c359fa7588f0bba2b4943e9`). It
does not admit actual B source, an authority, or a gate. The old raw checker
failure remains a failure.

The v4 current-main observation is d20
`d20d4d1d17590cca85b501d982cbaad70d42c641`; its Apple route uses forbidden
`macos-26`, while exact `xcode-27` is required. Runtime `35475920678`, Preview
`35475920808`, and CI/Main `35475920826` are historical outcomes, not accepted
evidence or a reason to weaken the newest-actual-major rule.

## Historical live reconciliation snapshot — 2026-09-20T01:28:35Z (superseded by 2026-09-20T02:54:52Z census)

This is a historical, capture-bound reconciliation. The later `2026-09-20T02:54:52Z`
paginated census supersedes its “latest” status; this snapshot remains for
chronology only.
External `G0/fleet/push-checkpoint-current.json` (SHA-256
`d4987b227e79212df2abdf503c33a80daff225ed270ea445f5c657f938fd101a`) binds
the following narrow capture. It is not a forever-current claim and does not
close G0 or any later gate.

| Snapshot item | Captured fact | Boundary |
| --- | --- | --- |
| Main / PR965 | Live API and origin `main` are `325719f1e05d3d46322c9fd3eeb9ad545e175638`, parent `e94b48406c4ed206fce2bbf39b788264e72cf39c`; merged PR965 source `6b48f8fff2f4943dbf79c21c3274caecdf77bdd5` produced that merge | Capture-bound identity only; no broader gate conclusion |
| PR963 | Open head `6ccf37486d255bbe3656f0066b0f1e5c84753903`, API base `e94b48406c4ed206fce2bbf39b788264e72cf39c`; mergeable `false`, state `dirty`; base is stale by one main commit (`325719f1`) | This snapshot supersedes the earlier `c440d4db` query only for its timestamp; no review/approval transfer or merge instruction |
| Runtime / native | Runtime-products run `35481089522` completed successfully; macOS job `105998930189` also technically succeeded on label `macos-26` | Negative native-policy result: `macos-26` violates the exact `xcode-27` requirement; no G1 proof |
| Preview / CI | Preview `35481089629` failed at Resolve preview identity job `105998913030`; no macOS preview jobs scheduled. CI/Main `35481089696` was still `in_progress` with no conclusion | Failure/incomplete observations; no gate result |
| Authority v5 | Markdown/JSON pair is approval-required with no mutation; external disposition rejects structural graph/schema/cycle/writer-freeze requirements | No authority approval or execution; retain v5 as a rejected proposal record |
| Post-census packaging review | External evidence ref `294de951` passed packaging-only review at cutoff `2026-09-20T01:28:49Z`: exact remote/ancestry, 66 checksums, 63 inventory entries, and no source paths. The later review observed `distribution/schema2-handoff-0712.md` changed at `01:34:02Z` and `skills-adapter/integration-map.md` at `01:31:01Z`; those are not fields in the `01:28:35Z` census | Packaging integrity only; no attestation, current-equality, or gate approval |
| Scan / checker | Scan `820f6509fe8462265986bacf02e8e86eead26750` reports 1,760 tests, clippy, fmt, and diff pass, but is rejected for detector TOCTOU, SI-B3 fixed-point proof, journal/post-action recovery, typed recovery, stale generated state/D19, and missing authority. Checker `ca18d01166681269b6eb5fce8d0f6175fc17aad4` closes the reusable child-matrix blocker with 258 tests/build/fmt/clippy, while its authenticated live collector remains unwired | Bounded source-review results only; no G1 or G0 proof |

## Paginated live census (capture: 2026-09-20T02:54:52Z)

External `G0/fleet/push-checkpoint-current.json` (SHA-256
`5326b47a65d35196816a62783dba60cf0afc58f6d258aae5a90688ca7fd865a0`) records
a fresh paginated read-only census. It binds exact main/PR/run/check pages and
does not use a newest-green shortcut. It is timestamped evidence, not a G0 or
later-gate result. Task/dependency state remains in the separate task ledger
below; this section is only the paginated census.

| Checkpoint | Exact revision/report | Disposition and dependency boundary |
| --- | --- | --- |
| Live main and PR heads | Main `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, parent `325719f1e05d3d46322c9fd3eeb9ad545e175638`; PR957 `92387e88c32f933a9061b819256e535662655cb2`/base `b5a4b4afaa6ca807927cacc03659b570a895dd5c`; PR960 `c8f7a2b353f9d2d6a60d3ad8bb2dc6299a108ceb`/base `1048337062ea625fada1b4f7c07f2feed75f60c7`; PR962 `ea9686f0eb522e402441ecd461bd1f458b731d06`/base `1048337062ea625fada1b4f7c07f2feed75f60c7`; PR963 `f46fe7c2c3ca7635c44eab21cc6bf616709c7a50`/base `325719f1e05d3d46322c9fd3eeb9ad545e175638` | Exact identity snapshot only. PR963 is open, mergeable `true`, state `blocked`; no review, merge, or gate approval transfers from older heads. |
| Main runs | Runtime `35484968618`: completed/success, five child jobs successful; macOS child `106009574341` used forbidden `macos-26`. Preview `35484968732`: failed at identity job `106009550596`, five children skipped. CI `35484968706`: `in_progress`; Control/Planning succeeded, Policy remained in progress | Runtime success is negative under exact `xcode-27`; preview/CI are failure/incomplete. No gate result. |
| Records docs | Source checkpoint `06edf31faa2c855a7eff4d7821903f296b8adef1` | Bounded docs checkpoint accepted by the focused records disposition; no source implementation or gate approval. |
| G1 scan | Source `29d6a9caf64d625799efa1ad52c3bba0e4f52db2`; report `G1/scan-integrity/source-review-29d6a9.md`, SHA-256 `0bb1e2ba64dc8c452f76161fbba6ea4a0d4376caefbc0fe63395f97a1fbdd732` | Conditional source-level pass for injected post-capture journal regression only; D19/generated state, real filesystem-fault mapping, authority, and recovery-entrypoint gaps remain. No G1 approval. |
| G1 bootstrap | Frozen review source `7d409afdd61a87080be4439d29563313169537`; report SHA-256 `180fb0e150f2bffabe162fbc115a5ca1b425ccdc316e47da96e9349bc62eaa0c`; owner later advanced remote to `07c35f83d1a7353331c910d1d3b728aba1dc7add`, unreviewed at the `02:54:52Z` capture | Changes required; schema-1/legacy transport and uploader-census gaps, six clippy errors, and no hosted proof. A later exact-head review at `03:00:49Z` reports **CHANGES REQUIRED** (report SHA-256 `9cf552e8ca59fc920bbb70a692a4958300c4a27028a907258afbd8503a6c2d07`); do not backdate that disposition into this census; no review transfers. |
| G2 Homebrew | Consumer `6520aad7bd66d53349e040508146956e9c4f0c1e`, producer comparison `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98`; report SHA-256 `dfd870b2188fd64ca1c39386c478f645cf101f1f0d6321e01c2763d8e009a547` | Rejected: producer path-bearing checksum sidecar cannot pass verifier/consumer; fixture masks mismatch; canonical schema, generated wiring, preview, and real provider/client proof remain. No G2 approval/publication. |
| G2 native product | Source `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98`; report SHA-256 `cb27b9b38eed2c94f7091e606b5271e43f132f98dc17022ad132f3845afe7a20` | Rejected: sidecar, canonical component schema, APT parent binding, preview assets, idempotency, duplicate-component, and generated-workflow gaps. No G2 approval. |
| G0 checker | Source `2d436be96b24a6a02cc047604c7899eaea4a16f9`; report SHA-256 `089b0d9155dd4c3e244684ef6576b55ba6022b84e8c773014729c044aed2b241` | Bounded parser migration pass: 253 tests/clippy/build/fmt/diff and removed local CAS/evidence-root path. Producer adapter/live `verify_g0` remains unwired; parser-only, no G0 approval. |
| G0 collector | Source `0882aac85b62d3962049a8dc5094f25ca5187c20`; report SHA-256 `91cc5333b392c8838f51d94b7b49dd84a6f933c22c147cd3bcc4083fa8736e1a` | Rejected for authoritative live G0: documented job URL mismatch, attempt binding, duplicate/foreign identity, checkout proof, and hostile-fixture gaps. No authenticated collector installed. |
| G0 raw store | Source `0d7713cd15cefb554fda5954e2d3320968aa31e7`; report SHA-256 `bc6114c279882c5e7675e0116781618e4807d873639c3a22b4ccc6af69f01282` | Rejected for bounded retention and authoritative capture: pending quarantine bytes/partial records exceed logical quota; module is not production-wired; Linux and race proof incomplete. |
| Authority v7 | `AUTHORITY-CHANGE-PLAN-2026-09-20-v7.{md,json}`; Markdown SHA-256 `7339996f7c3d76fe4e750cb9036b474beb7e0d20ae7b2b7e7a9f0787cd81b72e`; JSON SHA-256 `1a1cc175b36611f6ce175f4c82b6f6ce51f36f74bfb22a5d8e1abb9cc5647826` | Frozen proposal: approval required, no mutation, and no authority approval. Keep Tree-A/Main-B/Tree-B sequencing unexecuted. |
| Authority freeze choice | Feasibility report observed `2026-09-20T02:36:32Z`; Markdown SHA-256 `9735c1b2184f32f3b6b1cd1fefe98045dbb23403f937cd65692d2bcf54b06725`, JSON SHA-256 `29fa711c78ffd8c323acaf6e19653e4adbad87d1658b4d27c9116d669bab36b9` | No provider-only freeze excludes the current admin. User must explicitly choose and authorize any consent-only, provider-gated, existing-updater, or external-root path before execution; none is selected. |

## Later capture-bound external checkpoints

These records postdate the `2026-09-20T02:54:52Z` census. They are separate
immutable/source-review observations, not a replacement current-state claim;
unknowns and gate boundaries remain explicit.

| Capture | Artifact and verified identity | Observed result | Boundary |
| --- | --- | --- | --- |
| `2026-09-20T03:57:17Z`–`04:00:39Z` | `G0/fleet/live-default-branches-open-prs-20260920T035717Z-pr-reread-final.json`, SHA-256 `1c844f5c09a6d9d6a5c24f1b36c410a7190eb63a2bc6d2893268e469d9839dee`; report SHA-256 `4134cb7050a1a2a1699cc6784a6797133182c14929e13e5d1527094a6745c780`; raw `manifest.json`, SHA-256 `88d150e78af354645eac07d31cede2163cb80e75a2ff86e9ef78efa46d3ed30a` | Exact 32-repository scope; 32/32 initial and final metadata/ref reads; 32/32 initial and final PR-page passes; 87 initial and 87 final PR records; 64 retained raw response bodies (32 initial + 32 final), with no API/access/pagination errors or normalized churn | Inventory/reconciliation only. No workload, build, install, dispatch, release, publication, or G0/G3 claim. |
| `2026-09-20T04:18:47Z` cutoff | Evidence ref `evidence/github-first-dual-lane-20260919T181508Z`, append commit `d25819f5b4a9f1e7724456693e9d8061182cdd1a`; `updates/20260920T041500Z/checkpoint.md` SHA-256 `0dce91c1a4c5228988dbe654556d5a7002acb27e76af9f1bb65ccf9f64a2eea8`; JSON SHA-256 `8246db11d4ca7477e80f1501fc8f5c2137ab69ce73e2dc0e86f464464361fc69` | 89 stable regular files / 5,000,169 source bytes (G0=74, G1=15), 70 JSON; source base `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`; attestation `none`, `gate_status=not-evaluated` | Independent packaging review **PASS** (report `G0/evidence-checkpoint-review/d25819f5-packaging-review.md`, SHA-256 `3831370b0c008fcf62588547ee10559e13066b68b09e2b32dd7a72c7c6cfce3e`); packaging-only historical result, no G0–G7 gate or attestation. |
| `2026-09-20T03:58:55Z` v9 observation | Frozen proposal `G1/bootstrap-transition/AUTHORITY-CHANGE-PLAN-2026-09-20-v9.{md,json}`; Markdown SHA-256 `bfcafb4f706ac5c1a1ecccc7d8ba1d0d777f16e52bf4fa335a15085908e832ed`, JSON SHA-256 `e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615`; later independent audit report/results SHA-256 `853b28bee7e3475a898921f50a035b713eedd83c8403fd99c5900964944f227c` / `d010e465c84d8552453af3612e950bc41715ee19a5fadd63070b0774944ae88a` | Owner audit reports 33/33 contract checks passing, but the later independent audit records 22 checks: 18 pass, 3 fail, 1 unimplemented; release 14-leaf coverage, preimage/self-hash, product identity, fixture lineage, and transport defects remain; authority claim `False` | Later design-only independent rejection; v9 remains `successor_draft_external_blocked`. Frozen proposal only; no authority operation, source approval, execution, or gate. |
| Raw-store source `5688af96857a0cbe3b6a0fa064fcfb8b31ac2d36` | `G0/checker-v2-review/5688af96-g3-raw-store-rereview.md`, SHA-256 `03cc0a51c0c0465d564735191680bb3cf62a65e4a0414081842f16b2061c7bff` | **Reject** for bounded-retention acceptance: exact-source adversaries observe retention-owned peaks above 128 MiB during ordinary publication and valid `.txn` recovery; pending-record identity race and fault-injected publication proof also remain | Bounded-store source review only. No production collector, Linux runtime, authoritative capture, or gate claim. |
| Bootstrap transport source | Helper `00c752268fc0e9b19a45badaaea0934736bb47d5` is integrated in owner checkpoint `e2124475806a042313fc5a4e251f88246a6d16ab` (authored `2026-09-20T04:19:27Z`, committed `2026-09-20T04:23:46Z`); independent report `G1/bootstrap/source-review-e2124475806a042313fc5a4e251f88246a6d16ab.md`, SHA-256 `2030e55bd471148ca6473149867a517d4a63a16b83b635b6d5383f79037abdc2` | **CHANGES REQUIRED**: full library `1,700 passed / 1 failed` on generated `ci-policy.yml` drift; clippy, focused transport `4/4`, fmt, and diff pass separately. Review found Python `urllib` runtime `POST` and opaque publisher-action bypasses | Source checkpoint only; no G1 approval/publication. Exact report UTC review capture is not stated; security blockers remain. |
| PR963 source review | Exact head `056362aadb738279924ef05597f9a014392f48bf`, parent `b1563dadb022bbf8eff6b699c09f4e4267c53d81`, API base `325719f1e05d3d46322c9fd3eeb9ad545e175638`; source review `G1/reviews/pr963-056-independent.md`, SHA-256 `d4d2e953a4b8dae0bf3a8e1d1e99c521ff1da7deeeb674b4b7b6883754d56ae3`; source authored/committed `2026-09-20T04:12:18Z` | Independent review approves source admission at this exact head; 5,448 workspace tests / 5 ignored, hosted run `35488557270`, tested merge `3d450612adb6c12be3b6b811348555807861c7c7`, exact 17 nonempty units, `excluded=[]` | Bounded source-only approval. Report is dated `2026-09-20` but does not state exact UTC review capture; no merge, pin adoption, rollout, or hosted G1 gate approval. |
| Action scanner source `ab8a281277de084e8115653e0d022f805adc1496` | `G0/action-review/review-ab8a2812.md`, SHA-256 `f026e06327d7e866459983a5df99cfe59fc7f63a83f6bd26ec035fe5e2f03b9f` | Bounded source review passes: 1,898 serial tests, clippy, fmt, and diff; declared `fdeed` closure still blocks zero-diff generator verification | Source-only bounded pass. No merge, rollout, macOS/Docker operation, pin publication, or G0/G3 claim. |

The checker seam remains fail-closed. Harness result
`G0/checker-v2-review/a2bca6e-public-cli-harness/27eb-harness-results.json`
(SHA-256 `a4e7d7f29289b16438602872867295a84cc39c5ad398b974bb0ef6bd192bead1`)
is for exact source `27eb094ccd545b642206ea3d52336e0ac74d6abd`; live
self-authored use exits because authenticated collector/current-API
reconciliation is unwired, and offline cases are explicitly
`offline-validation-only`. The related bounded source review is
`a2bca6e767aa038818a2ffc991401609118600f3`; its 250-test/build/fmt/clippy
result is not producer integration or G0 proof.

The latest bootstrap owner checkpoint is exact
`b981f43e8dfd70b4c628d29b0e7e9dce679ce537`: generated workflows/state were
regenerated and the owner reports 1,727/1,727 library tests plus actionlint and
format checks passing. Exact review
`G1/reviews/bootstrap-b981f43e-independent.md` rejects it as a G1 source
checkpoint for independent archive/API-tree, attempt/freshness,
action/upload-provenance, legacy-path, transport-fixture, and runtime-image
digest gaps. A full source-suite result is not G1 evidence. Older
abbreviations such as `7c` or `5cdf` are not the current owner revision.

Other bounded source checkpoints remain incomplete: APT
`91bdf6cc1d0a5c429c5c01f17bf15dbb153c661b` is **blocked** by
`G1/reviews/apt-83e7ab4-91bdf6c-independent.md` (provider/source authority,
verify-to-publish binding, extraction races, absent native handoff, and
generated actionlint); corrected
source workload index `G0/fleet/workload-contract-index-20260920T002955Z-corrected.json`
(SHA-256 `2b3bf88b42f291a40dcc2d6eb65a489d72d2a5bdcf9ab094a69a43e01f750c9c`)
has an exact disjoint 32-row union but is source-only with
`gate_status=not_evaluated`; native product review `a8d46536e7e11db0bbd5e207be802970362b751f`
requires changes and grants no G2 approval; action scanner review
`40ddcc02dde1ff07aff538ea2ca95da091379e17` requires changes; hostile fixture
review `d60c0e2211b2830c64e7489d05b4cfe0bc77d65f` still has a ZIP-only
format assertion residual; and scan candidate
`0a15fd06e002f57dca546d5c041754f1ec433508` is **rejected** by
`G1/scan-integrity/source-review-0a15fd06.md`: its 1,756-test report still
fails the exact D19 check on an incompatible symlink closure and retains
authority/detector/rollback/hostile-fixture gaps. None is a gate result.

## G0 acceptance-matrix handoff

The exact user acceptance matrix is canonical in
[`SPEC.md`](./SPEC.md#exact-g0-acceptance-matrix) and is mirrored in the
external session record. Its baseline is Velnor
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`; later integration
`12cc87b629802c294da9840325cb21087c020df6` and every candidate SHA are
separate current evidence. The fixed scope is the exact 32-row goal manifest,
not the stale 28-row `audit_ci` estate.

G0 requires fresh, independently bound default/PR/workflow/run/provider,
workload, dependency/access, source/revision/digest, and child-job evidence.
The static manifest's `default_branch` and `default_branch_sha` are only seed
claims: each must reconcile to an independent live/default-branch snapshot and
UTC observation. The dependency record is a typed graph, not only IDs or child
links, and includes workload→child, workload→required-check, workload→release,
and workload→package edges with relation, stage, applicability, provenance,
and unknown/blocker status.
Missing, stale, queued, canceled, timed-out, skipped, failed, manual-only,
wrong-provider/source, empty-expected, stale-SHA, missing-repository, absent
child-log, or artifact-mismatch evidence fails closed. `N/A` is exclusion-only.
Release/tag/feed/tap/install identity and functional results are stage-aware
G2+ evidence, not a G0 prerequisite. `audit_ci` and `lane_compare` are
diagnostic helpers only; neither can close G0 or G7.

G0 inventories the dependency/dependent-workload graph and leaves execution
unknown where not observed. G2+ proves applicable release/package execution and
functional results; it may not downgrade a required applicable dependency to
`N/A`. The typed graph shape and neutral example are canonical in
[`SPEC.md`](./SPEC.md#evidence-schema-and-checker-contract); the separate
checker schema and examples must adopt them before authoritative use.

The external fleet evidence has 32 populated `main` branch/SHA rows at
`2026-09-19T16:34:19Z` and again in `G0/fleet/main-revisions.tsv` at
`2026-09-19T17:05:57Z`. PR identity snapshots are 78 baseline rows at
`2026-09-19T16:34:19Z` and 77 later current rows in `requirements.json`
generated at `2026-09-19T17:03:32Z`; neither is asserted fresh beyond its
timestamp. The committed scope inventory still has 32/32 null workload,
required-check/App, platform/architecture, and provider-eligibility fields,
31/32 access rows unknown. The external
`G0/fleet/dependencies-and-access.json` artifact was observed at
`2026-09-19T19:21:38Z` (SHA-256
`f58da9d4ea2bc32ba8867cbb4897997bc48c6e4228a14ed4ec0710c056f67f60`): its
32/32 scope and 22 workflow-bearing rows are present, with 15 source-bound
edges, but graph coverage is explicitly partial and gapped. Source coverage
is 26/32 exact local checkouts, 1/32 Velnor checkout at the wrong pin, 4/32
report-only, and 1/32 inventory-only. This is incomplete inventory evidence,
not a validated full graph or reason to invent values.

The `17:03:32Z`–`17:05:57Z` refresh set is external under the logical evidence
root: `G0/fleet/{requirements.json,main-verification.json,pr-checks.tsv,
context-pages.json,handoff.json,main-revisions.tsv}`. It reconciles the 78-row
baseline to 77 later PR heads, records `jackin-project/homebrew-tap#494` as
merged and `tailrocks/velnor#953` as head-changed, and rechecked head
stability at `2026-09-19T17:02:04Z`. Its PR/check/workflow coverage is for 12
PR-bearing repositories, not all 32 repositories; these files are not a
fleet-wide required-check or workflow proof.

A later all-32 read-only collector is external at
`G0/fleet/check-contexts-full.json`, spanning
`2026-09-19T19:15:10.439Z`–`2026-09-19T19:28:02.940Z` and serialized at
`2026-09-19T19:29:58.430Z`. It reports 32 repositories and 76 open-PR rows,
with 1,268 observed main check runs and 1,742 observed PR check runs. It is a
timestamped input pending independent review, source binding, required
context/App semantics, workload/dependency graph, and complete run/child
verification; it does not make the earlier 78/77 snapshots current forever or
close G0. The historical Velnor check contract is not reused when its `abe9`
source revision differs from the collector's live default revision.

`G0/check-contract/check-contract.json` is Velnor-only (current main plus
PR948/952/953/954); the 78-row `open-prs.tsv` identity list has no per-PR
check/App/run rows for the other 31 repositories. Do not treat that contract as
fleet-wide coverage. Collection outputs are assigned externally:
`check-contexts-full.json` (all 32 repository rows with observed PR/main
records; required-context/App semantics remain pending) to
`/root/g3_distribution_consumers`, `dependencies-and-access.json` to
`/root/g0_runtime`, and `workloads-full.json` to `/root/g0_inventory`.
The check-context artifact now exists as the later timestamped snapshot above;
independent review, source binding, and semantic required-context/App proof
remain pending. `/root/g0_records` imports only validated
source-SHA/UTC/explicit-unknown outputs; each path and exact producer
invocation remains pending.

`fleet.json` remains a flat nullable scope inventory. Its static count/uniqueness
check is not a G0 result, and `gate_status: "pending"` is not success. The
conversion boundary is explicit: enrich the inventory into the checker-owned
`config/github-first-dual-lane/manifest.json`, collect the independent live
`evidence/current-snapshot.json`, and normalize bound execution/package records
into `evidence/records.json`. These paths are outside this source tree and are
not yet verified or populated here. `/root/g0_checker` must publish the strict
schema conversion and exact working invocation; no guessed command, synthesized
fact, or self-attested record can satisfy G0.

The conversion is strict and canonical: no CLI or serde aliases, flat-fleet
coercion, merged release/install fallback, or conflict-precedence shim is
permitted. Producers emit one representation and reject duplicates/conflicts;
missing facts remain incomplete. The checker owner must publish the exact
canonical command after schema review; only that command is authoritative.

The bounded external follow-ups are `/root/g0_fleet` in a new isolated tree
for baseline scope reconciliation, `/root/g1_run_operations` in a new
isolated tree for lane census/step/artifact-log red/green proof,
`/root/g1_cache_semantics` as independent lane review, and `/root/g0_checker`
in the existing checker tree for authoritative v2 correction. Exact unknown
worktree paths remain unknown until observed. Current checker and independent
review links, rejected candidates, and incomplete statuses remain external;
none is a gate result.

## Bounded initial task queue

Every task has one owner, one input revision, explicit evidence, and a separate
reviewer. Unknown thread/worktree metadata stays `unknown` until observed.

| ID | Repository/component | Owner | Dependencies | Owned files/worktree | Acceptance commands | Evidence output | Reviewer |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `G0-inventory` | Fixed 32-repository fleet | `/root/g0_inventory` | none | External ledger inventory only; worktree `unknown` | `[observed]` successor `03:57:17Z`–`04:00:39Z` paginated inventory has 32/32 initial/final metadata/ref reads, 87 initial/final PR records, and 64 retained raw bodies; type dependency/dependent-workload edges remain separate | `G0/fleet/live-default-branches-open-prs-20260920T035717Z-pr-reread-final.json` plus raw manifest | `/root/g0-reviewer` |
| `G0-bootstrap` | Velnor generator/runtime bootstrap | `/root/g0_bootstrap` | `G0-inventory` findings as needed | Generator worktree `unknown`; no records in source | `[changes required]` owner ref `b981f43e8dfd70b4c628d29b0e7e9dce679ce537` remains rejected; frozen follow-up `7d409afdd61a87080be4439d29563313169537` is also changes-required; owner transport helper `00c752268fc0e9b19a45badaaea0934736bb47d5` is integrated in `e2124475806a042313fc5a4e251f88246a6d16ab`, whose independent review records `1,700 passed / 1 failed` on generated drift plus runtime publisher bypasses | `G0/bootstrap.json` with source/artifact/output identities | `/root/g0-reviewer` |
| `G0-distribution` | Velnor, `velnor-apt`, `homebrew-velnor` | `/root/g0_distribution` | `G0-inventory` | Distribution investigation worktree `unknown`; external evidence only | `[pending]` release discovery/feed/formula inventory | `G0/distribution.json` and access gaps | `/root/g0-reviewer` |
| `G0-fleet` | Fleet categories/workload matrix | `/root/g0_fleet` | `G0-inventory` | Fleet worktree `unknown`; source edits prohibited in this wave | `[observed]` read-only 32-row workload/platform projection; exact emitted scanner IDs remain partial | External `G0/workload-matrix.json` plus fleet refresh files | `/root/g0-reviewer` |
| `G0-runtime` | macOS/OrbStack capability analysis | `/root/g0_runtime` | `G0-inventory` | Runtime investigation worktree `unknown`; no live host mutation | `[rejected/changes required]` collector `0882aac85b62d3962049a8dc5094f25ca5187c20` is not authoritative live G0; raw-store `5688af96857a0cbe3b6a0fa064fcfb8b31ac2d36` is rejected for quota peaks and remains test-oriented; legacy production wiring and Linux proof remain absent | `G0/runtime-capabilities.json` plus external collector/raw-store reviews | `/root/g0-reviewer` |
| `G0-records` | Canonical execution records | `/root/g0_records` | none | `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-records`; this directory's five docs only | `[verified]` RTK/version/git/model metadata; evidence package `d25819f5b4a9f1e7724456693e9d8061182cdd1a` has independent packaging-only PASS; `[pending]` checker schema validation and all execution gates | These five source docs; external session ownership amendments and immutable evidence package | `/root/g0-reviewer` |
| `G0-checker` | Deterministic evidence checker | `/root/g0_checker` | `G0-records` schema | `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-checker`; checker-owned code/tests | `[bounded]` `ca18d01166681269b6eb5fce8d0f6175fc17aad4` closes the reusable child-matrix blocker; `2d436be96b24a6a02cc047604c7899eaea4a16f9` removes local CAS/evidence-root and preserves parser/live fail-closed boundaries; producer adapter/live `verify_g0` remains unwired | External checker review and hostile fixtures | `/root/g0-reviewer` |
| `G0-reviewer` | Independent G0 records/evidence review | `/root/g0_reviewer` | all initial outputs | Review-only worktree `unknown`; no author approval | `[pending]` fresh read of source docs and external raw evidence | Independent findings and disposition | `/root` |
| `G1-cache-semantics` | Hosted cache compatibility | `/root/g1_cache_semantics` (Luna/max) | `G0-inventory`, `G0-bootstrap` | Thread `01a0ba76-f725-7022-9cfa-f28456ab67b2`; external findings | `[observed]` stale fixture and PR953 cache-contract diagnosis; refresh pending | `G0/cache-semantics/findings.md` | `/root/g0-reviewer` |
| `G1-hosted-config` | Hosted-first generator policy | `/root/g1_hosted_config` (Luna/max) | `G0-inventory`, `G0-bootstrap` | Thread `01a0ba77-5222-7e63-97fa-553849b96d7b`; worktree `hosted→g1_hosted_config` | `[in progress]` typed config/regeneration/policy checks | G1 candidate/source/output identity | `/root/g0-reviewer` |
| `G1-review952` | PR #952 and stacked recovery work | `/root/g1_review_952` (Luna/max) | `G0-inventory`, `G1-hosted-config` | Thread `01a0ba77-dd88-7ac2-9fb7-118f3c09d1af`; worktree unknown | `[pending]` PR #952/#953/#954 review and candidate checks | PR disposition and post-merge requirement | `/root/g0-reviewer` |
| `G1-run-operations` | Existing failed run/child graph | `/root/g1_run_operations` (Luna/max) | `G0-inventory` | Thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`; owns external `G0/stale-runs.json` | `[observed]` failed-run/child reconciliation in progress | `G1/run-operations.json`, `G0/stale-runs.json` | `/root/g0-reviewer` |
| `G1-runtime-product-audit` | Published runtime product and promotion sequence | `/root/g1_run_operations` (Luna/max) | `G0-bootstrap`, `G1-seed-pin` | Same thread; external audit only | `[observed]` old pin/release verified; current main and unpublished candidate distinguished | `G1/runtime-product-audit/{runtime-product-audit.json,PROMOTION.md}` | `/root/g0-reviewer` |
| `G1-seed-pin` | Generator seed/pin reuse | `/root/g0_inventory` (Luna/max) | `G0-bootstrap`, `G0-inventory` | Thread `01a0ba72-3925-7141-b1f7-5529a5cf6c98`; worktree `generator→g0_inventory` | `[observed]` exact seed/pin review; clean pin adoption/regeneration pending | `G1/reviews/seed-pin.md` | `/root/g0-reviewer` |
| `G1-scan-integrity` | Generated-output/source scan integrity | `/root/g0_inventory` | `G0-bootstrap`, `G1-hosted-config` | Worktree `dual-lane-scan-integrity`; source edits gated/reviewed | `[conditional source pass; G1 rejected]` `29d6a9caf64d625799efa1ad52c3bba0e4f52db2` proves injected post-capture journal preservation only; D19/generated-state, real filesystem-fault mapping, authority, and recovery-entrypoint gaps remain after earlier rejected candidates | External `G1/scan-integrity/{source-review-29d6a9.md,source-review-820f6509.md}` plus prior rejected reports | `/root/g1_review_952` |
| `G2-native-packages` | Native package/Homebrew prerequisites | `/root/g2_native_packages` (Luna/max) | `G1`, `G0-distribution` | Thread `01a0ba7a-328e-7282-943e-5b54c2ac209d`; worktree `dual-lane-native-packages` | `[observed]` three ARM64 macOS binaries compile/smoke only; worker now also owns product/manifest contract; install/publication pending | `G0/native-packages/findings.md` | `/root/g0-reviewer` |
| `G2-homebrew-contract` | Homebrew consumer/producer contract | `/root/g2_homebrew_contract` (Luna/max) | `G1`, `G0-distribution`, `G2-native-packages` | Thread `01a0ba80-8408-7380-8ac2-b743eb4494a5`; worktree `dual-lane-homebrew` | `[rejected]` consumer `6520aad7bd66d53349e040508146956e9c4f0c1e` rejects the producer `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98` path-bearing checksum sidecar and cross-lane handoff; no provider/client proof | `G2/homebrew-contract/findings.md` plus external `G0/distribution-review/homebrew-6520aad-review.md` | `/root/g2_distribution_review` |
| `G2-native-product` | Product binary/component identity and authoritative package manifest | `/root/g2_native_packages` (Luna/max) | `G1`, `G0-distribution` | Thread `01a0ba7a-328e-7282-943e-5b54c2ac209d`; worktree `dual-lane-native-product` | `[rejected]` exact `2f7d5fbae420d00d8105d4e8cbe0fed78d761b98` has sidecar/schema/parent-binding/preview/idempotency/duplicate-component/generated-wiring blockers; no G2 approval | `G2/native-product.json` plus external `G0/native-review/2f7d5fba-g3-native-product-rereview.md` | `/root/g2_distribution_review` |
| `G2-typed-validator-product-publisher` | Separately typed validator-only product/publisher | `/root/g0_inventory` | `G0-inventory`, `G0-distribution`; publication before separate PR957 pin | External design context; no three-platform runtime reuse or platform-selection workaround | `[pending]` typed validator/product/publisher design and independent review | External `G2/native-product.json` and session graph | `/root/g0_reviewer` |
| `G2-preview-publication` | Immutable preview publisher/version/monotonic channel | `/root/g1_run_operations` (Luna/max) | `G1`, `G2-native-product`, `G0-distribution` | Thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`; worktree `dual-lane-preview-publication` | `[blocked]` design/source work may prepare, but no real publication before G1 | `G2/preview-publication.json` | `/root/g2_distribution_review` |
| `G2-distribution-review` | Independent APT/Homebrew product contract review | `/root/g2_distribution_review` (Luna/max) | `G2-native-product`, `G0-distribution` | Thread `01a0ba81-1af6-7f11-9f24-3ff115b8f314`; review worktree unknown | `[observed]` package identity/publication/install blockers recorded | `G0/distribution-review/report.md` | `/root/g0-reviewer` |
| `g3-skills-adapter` | Eight skills repositories: `tailrocks/tailrocks-typescript-skills`, `tailrocks/tailrocks-skill-authoring-skills`, `tailrocks/tailrocks-rust-skills`, `tailrocks/tailrocks-roadmap-skills`, `tailrocks/tailrocks-pull-request-skills`, `tailrocks/tailrocks-open-source-skills`, `tailrocks/tailrocks-macos-skills`, `tailrocks/tailrocks-code-quality-skills` | `/root/g3_skills_adapter` (Luna/max) | `G0-inventory`; operationally G3 depends on G2 | Thread `01a0ba7b-0152-7a70-8d18-c2387f1c9469`; external read-only source inspection | `[observed]` catalog/frontmatter/template inventory; central scanner fix required | `G0/skills-adapter/report.md`; no G3 rollout | `/root/g3_distribution_consumers` |
| `latest_macos_policy` | GitHub-hosted macOS version/label policy and official label research | `/root/latest_macos_policy` | `G0-inventory`; rollout remains sequenced through G3/G4 | Isolated AGENTS-rule/research worktree; exact path/thread external | `[pending]` official label/availability research and revision-bound policy PR | `G0/native-macos-policy/` plus review/PR identity | `/root/g2_homebrew_contract` |
| `g3-native-routing` | Native Apple capability/routing: `tailrocks/tablerock`, `tailrocks/parallax-telemetry-playground`, `jackin-project/jackin` | `/root/g3_native_routing` (Luna/max) | `G0-inventory`; operationally G3 depends on G2 | Thread `01a0ba7b-32f1-7af1-a25f-4cde73f1f075`; worktree `dual-lane-native-routing` | `[observed]` actual routing gaps recorded; generator policy must honor newest actual major (current verified arm64 label `xcode-27` for macOS 27; future labels supersede), reject macOS 27 Intel without an exact label, and never fall back/skip; no rollout before G2 | `G0/native-routing/report.md`; no G3 rollout | `/root/g3_native_review`; native/package contract `/root/g2_homebrew_contract` |
| `g3-action-roles` | Action/role images: `jackin-project/jackin-role-action`, `jackin-project/jackin-the-architect`, `jackin-project/jackin-sentinel` | `/root/g3_action_roles` (Luna/max) | `G0-inventory`; operationally G3 depends on G2 | Thread `01a0ba7b-57a5-7623-bd92-d0666a02b96e`; worktree `dual-lane-action-scanner` | `[bounded-pass]` exact source `ab8a281277de084e8115653e0d022f805adc1496` passes 1,898 serial tests/clippy/fmt/diff; generated `fdeed` closure still blocks zero-diff verification | `G0/action-review/review-ab8a2812.md`; no G3 rollout | `/root/g0_runtime` |
| `g3-rust-consumers` | Eight Rust/product consumers: `tailrocks/parallax`, `tailrocks/tracing-request-level`, `tailrocks/termrock`, `tailrocks/termpane`, `tailrocks/schemalane`, `tailrocks/ruxel`, `tailrocks/pg-bigdecimal`, `tailrocks/holla` | `/root/g3_rust_consumers` (Luna/max) | `G0-inventory`; operationally G3 depends on G2 | Thread `01a0ba7d-bb3f-78c3-84c8-eb0b2d75e5d0`; worktree `dual-lane-rust-scan` for central fix | `[observed]` manifests, tests, publishing, dependency-closure gaps recorded; termrock fix pending | `G0/rust-consumers/{report.md,inventory.tsv}`; no G3 rollout | `/root/g0-reviewer` |
| `g3-distribution-consumers` | Six non-Velnor feeds/taps: `tailrocks/homebrew-tablerock`, `tailrocks/homebrew-ruxel`, `tailrocks/homebrew-parallax`, `tailrocks/homebrew-holla`, `tailrocks/holla-apt`, `jackin-project/homebrew-tap` | `/root/g3_distribution_consumers` (Luna/max) | `G0-inventory`, `G0-distribution`; operationally G3 depends on G2 | Thread `01a0ba7d-e457-7723-81ba-1f7ed038212c`; external read-only audit | `[observed]` formula/feed/signature/update and clean-client gaps recorded; G3 blocked | `G0/distribution-consumers/{report.md,consumer-inventory.json}`; no G3 rollout | `/root/g0-reviewer` |
| `g3-native-review` | Independent native-routing review | `/root/g3_native_review` (Luna/max) | `g3-native-routing` | Thread `01a0ba8a-5bef-7d71-9a17-1d082a4f122a`; review worktree unknown | `[pending]` fresh review of native-routing evidence | `G0/native-routing/review.md` | `/root/g0-reviewer` |

The added G1/G2 rows are bounded follow-up tasks. Agent/thread metadata is
recorded in the external session ledger only after actual assignment and
turn-context verification; any remaining unknown is explicit.

The `g3-*` rows are early, read-only category audits. They may prepare findings
before G3, but cannot operate the fleet or merge migration changes before G2
exits. Their exact scoped repository lists are fixed above; compact records live
in the named external G0 subdirectories.

The checker workstream's contract is
`docs/ci/github-first-dual-lane/evidence-schema.md` in its worktree. The
canonical manifest fields are `schema_version: 1` and `manifest_id`; this
manifest uses those names only. Preparation rows remain nullable and fail G0
until live workload evidence replaces the unknowns.

## Ownership and mutation rules

1. `/root` is integration/publication owner. It serializes generator pin
   adoption, release/tag/feed/tap mutation, shared ledger amendments, merges,
   and final snapshots.
2. `/root/g0_records` owns only the five files in this directory plus the
   explicitly assigned external session record. No source code, generated
   `.github` output, or checker implementation is edited here.
3. `/root/g0_checker` owns checker code/tests in its separate worktree. It
   consumes this manifest schema and cannot rewrite evidence to make it pass.
4. Investigators write compact records outside source. Raw logs remain outside
   both trees; links, hashes, and concise findings enter the immutable ledger.
5. Every evidence row identifies source SHA, event semantics, actual checkout
   SHA, provider, runner/host, expected/actual jobs, and reviewer. Missing data
   remains an explicit blocker.

## Phase work after G0

### G1 — hosted recovery

Repair generator/provider configuration, bootstrap, sidecars, cache semantics,
triggers, aggregate failure propagation, source pins, permissions, checkout
depth, Docker/Buildx setup, required-check transition, and stale-run analysis.
Prove exact candidate PR checks and resulting main CI on hosted runners.

### G2 — release and distribution

Implement the typed product/runtime discovery and channel contract, preview then
stable publication, signed APT metadata for amd64/arm64, Homebrew stable/preview
formulas and CI, clean-client install/upgrade/switch tests, and retry-safe
singular publication. Keep runtime artifacts out of application discovery.

### G3 — hosted fleet

Inventory then migrate by dependency-aware waves. Extend generic scanner and
primitives for missing categories; preserve native Apple checks and package,
feed, skill, action, and image behavior. Apply the newest-actual-major hosted
macOS policy (current verified arm64 mapping: exact label `xcode-27` for macOS
27; future newest labels supersede it) to every applicable Velnor workload;
macOS 27 Intel has no supported label and cannot fall back to `macos-26`,
`macos-26-intel`, `macos-15`, lagging aliases, skips, or other older labels.
Incompatible constraints must fail explicitly. For every row prove
deterministic regeneration, migration PR, full workload hosted run, reviewed
merge, and main.

### G4/G5 — actual host and repair loop

Install the published package on the authorized Mac, record identity and
OrbStack/Docker data, prove routing, trust, capacity, nested Docker isolation,
cache, cancellation, recovery, connectivity, observability, parity, and
packaged lifecycle. Hosted macOS Velnor jobs still use the newest actual
supported major and architecture (current verified arm64 mapping: exact label
`xcode-27` for macOS 27; future newest labels supersede it) and explicitly fail
on incompatible constraints; macOS 27 Intel cannot fall back to `macos-26`,
`macos-26-intel`, or an older label. The actual pilot host is recorded
separately. For
every defect create reproduction/fix/regression/review
and rerun the affected hosted and Velnor canaries, then republish and reinstall.

### G6/G7 — dual fleet and final audit

Generate both lanes from one typed provider model, run both on the same source
object and logical workload, merge only after checks/review, prove resulting
main, preserve native-only and singular publishing, reconcile all open PRs,
then refresh default tips and invoke the independent checker/reviewer.
