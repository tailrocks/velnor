# Independent acceptance-matrix audit

Status: **not approved; no gate verdict**. This is a read-only contract audit,
not a checker implementation approval or distribution approval.

Exact compiled hostile-fixture results for the later checkpoint
`2ba66b116dd5511f0b4f2a6856cfbed6bd290152` are recorded separately in
[`2ba66b1-fixture-review.md`](./2ba66b1-fixture-review.md). They expose G0
provider-key, blocker, ruleset-binding, workload-edge, PR-check inventory, and
G1 URL/host/log false greens.

## Review boundary

- Baseline source reviewed immutably: `dual-lane-checker`
  `017c92e0bf608d42675a0b8e495f0486c7296041` (`evidence_check.rs` and
  `evidence_live.rs`). Later `790a0805`/`8f976b52` author checkpoints are not
  accepted by this report; they need a separate clean exact-commit review.
- Records baseline: `dual-lane-records` `61d1433d407a4293477f5d4ee88ce11c7e417dd1`.
  `docs/ci/github-first-dual-lane/fleet.json` is a static 32-row seed with
  null/`unknown` fields, not a live v2 manifest/snapshot/records envelope.
  The current records checkout has no runnable v2 `manifest.json`,
  `current-snapshot.json`, or `records.json` conversion output.
- Contract sources: `velnor-github-first-dual-lane-goal.md:21-58,89-98,273-296`
  and records `SPEC.md:107-119,425-556`. Goal lines 21-58 fix the exact 32;
  G0 is inventory/setup, G2 is release/distribution; release/install is not a
  G0 prerequisite.
- Effective model retained: `gpt-5.6-luna`, max effort, as recorded by
  `dual-lane-evidence/G0/fleet/inventory.md` and `session.json`; orchestration
  is separately recorded as `gpt-6-astra`, low effort.

Prior detached exact-017 checks were: `rtk cargo test -p velnor-tools
--no-fail-fast` (216 passed), `rtk cargo fmt --all -- --check`, clippy with
`-D warnings`, and `git diff-tree --check` clean. These prove test/build
hygiene only, not this semantic matrix.

## Requirement → evidence → invariant/gap → negative fixture

| Stage / requirement | Authoritative evidence | 017 status, missing invariant, or false-green path | Required negative fixture |
|---|---|---|---|
| G0 exact 32, unique, no extras | Goal `:21-58`; checker `evidence_check.rs:24-1205`; independent `G0/fleet/inventory.md` | Baseline checker hard-codes and compares the exact set. Preserve this in later revisions. `audit_ci`'s 28-row estate and `fleet.json` parsing are never substitutes. | Remove one row, duplicate one row, add an in-scope-looking substitute, and pass a 28-row helper result. All fail. |
| Live default branch, SHA, UTC freshness | Goal `:275-296`; snapshot structs `:312-376`; live collector `evidence_live.rs:66-129`; freshness `evidence_check.rs:3770-3791`; records `SPEC.md:109-119` | `fleet.json`'s branch/SHA is correctly only a seed. Live mode reconciles supplied facts; offline mode can still consume a stale caller snapshot. Snapshot has no per-endpoint completion ledger, and static `manifest.default_branch` is not independently authoritative. Require `snapshot.default_branch == live default`, live SHA, UTC observation, and fresh PR snapshot. | Stale default SHA, future/old timestamp, changed default branch, stale snapshot accepted offline, or static `main` value disagreeing with live. |
| Every open PR: draft/bot/fork, head/base, tested merge, merge-group, trust/applicability | Goal `:294`; exact G0 matrix `SPEC.md:517-523`; collector `evidence_live.rs:85-113` | `SnapshotPullRequest` only has number/state/head/base/optional merge SHA/URL/executions. No draft, author bot, fork, trust policy, applicability, required-check producer, or `merge_group_sha`. Collector queries runs by contributor `head_sha`, not the tested merge candidate. | Drop a draft/bot/fork PR; replace head/base; supply unrelated/absent merge SHA; make trust ineligible but report success; provide a merge-group run under a contributor SHA. |
| Required checks and producing Apps | Goal `:282,294`; manifest/snapshot checks `evidence_check.rs:1295-1300,1434-1451`; check collector `evidence_live.rs:524-582` | Manifest required contexts/apps are checked for shape, and snapshot ruleset contexts/apps are checked for shape, but 017 never asserts manifest contract == independently read ruleset contract. Collector selects the first matching check name and records no check-run ID, suite ID, repository, source SHA, workflow-run binding, or trusted job binding; `external_id` is accepted as a job ID. | Same context/app from an unrelated SHA/run; same name with wrong App; fabricated external ID/details URL; manifest-required App differs from live ruleset. |
| Complete workflow, reusable-action, scanner, generated-state, trigger inventory | Goal `:524-525`; `WorkflowObservation` `evidence_check.rs:358-364`; collector `evidence_live.rs:136-184` | Only path/revision/source SHA/URL/event exist; collector writes event `"unknown"`. No reusable action pins, scanner inventory, generated-state/config source, trigger set, or complete content graph. A single `workflow_path` in the manifest cannot prove the full generated graph. | Omit a workflow/action/scanner/generated-state node, replace an action revision, set event unknown, or let default-branch workflow metadata stand in for a PR workflow revision. |
| Nonempty workload/platform/architecture, provider eligibility, explicit exclusions | Goal `:526-527`; manifest `:221-306`; checks `:1267-1300,3479-3645` | Nonempty workload rows, exact workload→platform rows, supported targets, expected jobs, and required `github`/`velnor` keys are checked. There is no `justified_exclusions` field, no strict eligibility key set (unknown provider keys can be present), and no cross-check that each expected job's platform/architecture equals its workload row. | Empty workload/job matrix; duplicate/missing target; eligible third provider; expected job uses a different valid target than its workload; exclusion with no reason; exclusion reported as success. |
| G0 dependency/dependent-workload graph | Goal `:91`; records `SPEC.md:437-484,517-529` | Required typed graph fields are absent from `ManifestRepository`, `SnapshotDocument`, and `EvidenceRecord`. `ExpectedJobSpec.child_workflow` and `child_run_links` are not a graph. Require typed nodes/edges with stable edge ID, from/to kinds and IDs, relation, stage, required/applicability, source revision, UTC observation, evidence reference, and status. Required edges: workload→child, workload→release, workload→package, workload→required-check. | Missing edge; dangling node/edge; wrong source revision; workload→job target mismatch; child edge points at another run; duplicate authority; illegal cycle between producer/package/release; excluded workload marked success; required edge status omitted/unknown at an execution gate. |
| Generator/runtime/source/pin provenance | Goal `:279-283`; manifest `:229-243`; record comparison `evidence_check.rs:1945-2005` | Fields exist and record values are compared, but 017 does not recompute or independently bind the manifest source digest/revision to the reviewed bytes. `generator_artifact_digest`, runtime/source/image/config/generated/scan digests can be caller strings with valid shape. | Mutate generated plan while retaining its claimed digest; swap runtime/image/config/scan digests; use stale generator revision; use `github.sha`/moving main as source. |
| Run event/source/checkout and current revision | Goal `:284-287,294`; execution checks `evidence_check.rs:2112-2365`; collector `evidence_live.rs:255-347` | Checker checks event and SHA relationships, but collector sets `trigger_source_sha` and `actual_checkout_sha` both to run `head_sha`; Actions run `head_sha` is not checkout proof. Main accepts `workflow_dispatch` as sufficient coverage (`:1761-1823,2262-2289`), so a manual diagnostic can false-green absent a required push/PR association. Workflow revision is looked up from default-branch inventory, not run source. | Manual dispatch on current SHA with no required-check association; run head SHA differs from checkout; PR run on contributor SHA but claims merge checkout; stale workflow revision; queued/canceled/timed-out/skipped/failed run. |
| Provider, runner, host identity | Goal `:286`; host check `evidence_check.rs:2367-2437`; classifier `evidence_live.rs:585-595` | Provider is inferred from all job runner labels; GitHub/Velnor host identity is heuristic (`runner_name`, labels, runner ID/name). No trusted registration, host attestation, Mac/OrbStack/Docker endpoint, guest architecture, or actual image proof. G4/G5 require these capabilities, not merely a host ID. | Self-hosted label/name spoofed as hosted; Velnor label on GitHub host; wrong platform/architecture; OrbStack socket replaced by another Docker engine; host shell succeeds while container job did not. |
| Expected vs actual jobs; terminal success | Goal `:282,287,296`; checks `evidence_check.rs:2439-2575` | Required expected names and actual IDs/conclusions are compared; completed/success is required and duplicate actual IDs fail. Missing graph binding remains: expected job platform/arch is not cross-bound to workload row; duplicate job names can collapse in a set; no child job/check/log inventory. | Missing required job; extra failed job; duplicate ID; duplicate name with two IDs; valid-target but wrong workload platform/arch; `status=completed` with non-success conclusion; skipped/queued job. |
| Complete child graph and child logs | Goal `:287,296`; child structs `evidence_check.rs:433-450`; collector `evidence_live.rs:350-434`; handoff child policy | Parent ID, run identity, event, source SHA, provider, status, conclusion, URL are present, but child jobs/checks/logs/descendants are absent and recursion is absent. Child expectation is optional and self-declared in `ExpectedJobSpec`; no branch/producer identity. GitHub list-runs does not expose `parent_run_id`; collector drops unassociated children, then cannot prove required lineage. | Required child missing; child with wrong parent/source/event/branch/producer; failed child; child job failure hidden; child log absent; nested grandchild omitted; same-SHA unrelated run inferred as child; illegal cycle. |
| Explicit expected action logs and artifacts; complete pagination | Goal `:287,296`; records `SPEC.md:548-556`; `EvidenceRecord.logs` `evidence_check.rs:455-558`; log check `:2112-2242`; generic paginate `evidence_live.rs:598-630` | **Not implemented in 017.** No action artifact/log observation or expected artifact/log fields exist. `CanonicalArtifact` is release-manifest data, not Actions-run artifact evidence. `logs: Vec<String>` is opaque caller input; empty/non-HTTPS fails, but any unrelated HTTPS URL passes. No `/actions/runs/{id}/artifacts` or `/actions/jobs/{id}/logs` collection, no run/job binding, no content identity, no per-endpoint completeness. Generic pagination would request page 2 after exactly 100 items for endpoints it calls, but cannot cover endpoints it never calls; `rulesets.pages_complete=true` is hard-coded. | First artifact page=100, second page=1 omitted; page-2 403/429/500; 1000-page cap truncation; artifact from another run; missing log; empty log body/URL; unrelated HTTPS log URL; required child log absent; page-2 log omitted; expected artifact absent. Every fixture must fail independently of helper/overall-green. |
| Canonical release/tag/assets and acyclic parent digest | Goal `:288-290,309-310`; release structs/checks `evidence_check.rs:564-695,2754-3219` | G2+ correctly requires an external release document and computes its digest over canonical bytes with `manifest_sha256` outside the hashed manifest (`:568-571,2884-2893`), avoiding hash recursion. Source/tag/release ID, artifacts/components/targets and asset digest equality are checked. APT/Homebrew checks only nonempty repository/tap/suite/formula, version, and valid revision; no feed/index/signature/formula-content fetch or monotonic channel proof. Child/package records may reference the parent digest; never embed parent digest in bytes whose digest is the parent. | Component/target omitted; digest rebound; source/tag mismatch; wrong release ID/channel; APT revision points to unrelated content; Homebrew formula revision/version mismatch; stable/preview update deletes the other channel; parent digest embedded recursively. |
| Install/upgrade identity and functional result | Goal `:290,309`; install checks `evidence_check.rs:3221-3477` | Typed clean environment, clean install, same-channel upgrade, channel switch, predecessor identity, installed product/binaries/digests/paths, and functional result are structurally checked. Evidence is still caller-supplied strings; no raw command/result or independent package/feed observation is bound. | Missing/same-version predecessor; same-channel upgrade absent; channel switch same channel; installed binary/path/artifact mismatch; `functional_result != success`; unsupported service marked success; source checkout/PATH binary used instead of release asset. |
| Fresh snapshots; no newest/overall-green/helper/self-attested gate | Goal `:294,318`; live compare/freshness `evidence_check.rs:836-878,3647-3791`; records `SPEC.md:536-556` | Live mode compares default SHA, workflow/run/PR identities and freshness. Offline records can be stale unless live mode is invoked. `gate_status` is not trusted as authorization and 017 rejects blocker/next_action even if status says pass (`:2090-2108`), but later revisions must retain this check for G0 too. `audit_ci`, badges, display-green, newest run, and `github.sha` remain auxiliary/forbidden authority. | `gate_status=pass` plus blocker/next_action; newest successful run on stale SHA; overall green with skipped required job; one helper row/zero-pair census; stale snapshot; `audit_ci` clean while v2 fields are null. |
| Actual integration inputs, not flat seed conversion | Records `SPEC.md:107-119,500-505`; `G0/checker-v2-handoff.md`; current `docs/.../fleet.json` | Existing `fleet.json` is correctly marked static/unknown. No checked-in v2 conversion output or runnable adapter currently binds fleet rows to the checker’s strict schemas. A parser succeeding on 32 rows is not a G0 pass; null/unknown inventories, absent graph/access evidence, and outside-source records must fail closed. | Feed flat `fleet.json` with null rows into an adapter; omit one v2 document; fabricate non-null defaults; use a record outside the attested manifest/snapshot; let `audit_ci` output satisfy runtime evidence. |

## API limitations and required handling

1. **Synthetic PR merges.** A PR object’s optional `merge_commit_sha` is not
   proof that the required PR run tested that candidate; it can be absent or be
   a server-generated merge result different from the run under review. The
   collector currently queries Actions runs by contributor `head_sha`. Require
   an exact `pull_request` merge checkout and separately typed `merge_group_sha`
   for queue runs, with source event and run IDs. Never substitute newest run or
   `github.sha`.
2. **Required-check Apps.** Check-runs can share a context name. The collector
   must bind check-run ID, App ID, repository, source SHA, suite/workflow run,
   job ID, event, status, and conclusion. A context plus App string or
   `external_id` is not an association proof.
3. **Child links.** GitHub workflow-run list data does not reliably expose the
   parent run ID. Accepted alternatives are a verified dispatch payload/context,
   a typed producer artifact/output, or same-run `workflow_call`. Missing parent
   edge is a blocker; SHA, event, path, or newest-run coincidence is not a link.
   Every accepted child then needs jobs, checks, logs/artifacts, and recursive
   descendant closure.
4. **Checkout/provider/host.** Run `head_sha`, runner labels, and runner name
   do not prove checkout contents, provider registration, Mac/OrbStack target,
   Docker endpoint, guest architecture, or image digest. Require explicit
   immutable checkout and capability attestations; fail closed when APIs cannot
   expose them.
5. **Pagination/completeness.** Use endpoint-specific page records for PRs,
   checks, rulesets, runs, jobs, artifacts, and logs. A page of exactly 100
   mandates page 2. Any page API error, malformed body, permission failure, or
   cap truncation invalidates the collection. An aggregate `page_count` and a
   hard-coded `pages_complete=true` are insufficient.

## Stage applicability (do not fake future gates)

| Stage | Must be required now | Must not be required/credited yet |
|---|---|---|
| G0 | Exact scope; independent live default/PR/workflow/ruleset/access inventory; workload/platform/provider/exclusion data; typed dependency graph; generator/runtime pin identity; unknown/blocker status. | Release/tag/feed/tap/install success. G0 inventory unknowns are not passes, and a flat helper/`fleet.json` row is not a gate. |
| G1 | GitHub-hosted applicable execution on current main and every current open PR; event/source/checkout/provider/host; exact expected jobs/check Apps; terminal success; complete child/log graph. | Release/install or Velnor-lane success. Explicit provider exclusion is not executed success. |
| G2 (and later stages carrying G2) | On applicable main records, canonical producer manifest, release/tag/assets, APT/Homebrew projections, clean install, same-channel upgrade, channel switch, installed identity, and functional result. PR records do not need release/install objects. | Do not require release/install when invoking G0/G1. For a manifest marked `Applicable`, do not allow a caller to downgrade to N/A with a free-text justification; N/A must be an explicit exclusion with authoritative reason and never success. |
| G4/G5 | Both eligible lanes plus real macOS/OrbStack/container capability and packaged operation where applicable. | Host ID/runner label alone is not Mac or Velnor proof. |
| G6/G7 | Single producer digest parity across lanes; G7 live reconciliation and independent reviewer. | No self-attested reviewer/helper gate, no release digest from a second publisher, and no overall-green shortcut. |

## Independent review conclusion

The immutable 017 baseline is **not acceptance-ready**. The largest direct
G0/G2 blockers are absent typed dependency graph; missing PR trust/merge-group
and workflow/action/scanner inventory; missing manifest↔ruleset App binding;
heuristic checkout/provider/host identity; optional/non-recursive child graph;
and completely absent paginated Action artifact/log evidence. The release
manifest’s external digest is acyclic and structurally useful, but current
APT/Homebrew projection checks do not prove external feed/tap content or
monotonic channel retention. `Applicable` release/install can be downgraded to
N/A in 017. No current records or helper output changes this conclusion.

Required adversarial suite before any checker approval: exact-scope substitute;
stale/default-SHA and stale-PR snapshot; draft/bot/fork trust omission;
manifest/ruleset App mismatch; unknown provider; workload/job target mismatch;
missing/dangling/wrong-source/cyclic dependency edge; manual-only run;
checkout/provider/host spoof; missing/failed/duplicate job; missing/wrong
child and recursive child log; artifact/log page-2 omission/API error/cap
truncation/unrelated URL; canonical component/digest/source/tag mismatch;
APT/Homebrew parent/revision/channel mismatch; missing/same-version upgrade;
unsupported service; `Applicable`→N/A downgrade; and `pass` with blocker or
next action. Each must fail independently; passing unit tests alone is not
evidence of semantic coverage.
