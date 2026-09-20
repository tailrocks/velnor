# G0 workflow-source inventory: bounded independent review

Date: 2026-09-20

Scope: read-only review of `G0/workflow-source-inventory-20260920T102146Z`. This is a source-integrity review only. It is not G0/G2 approval, an execution result, an expected-workload projection, or a publication/install decision.

## Reviewed evidence

- Inventory root: `G0/workflow-source-inventory-20260920T102146Z`
- `manifest.json` SHA-256: `075423a0304521ac2ea74d654c92ba16ba417ac24bc57904e41b5e003b84c707`
- `workflow-objects.ndjson` SHA-256: `90a57d9c808c0a44da85e037403d00b44783352d6443bf59de2b449efb3bdcda`
- `owner-handoff.json` SHA-256: `7b7a7a6dd6f50a2aa313848ec1518bd6239cf213ea7c13630f7d860450c99bfd`
- Manifest source-index SHA-256 `43cb2affeca84e59379844bcef1d53d7ead2e1b9596c36bb3554ec012d7c72ca` independently matches `G0/partition-index-20260920T073424Z/index.json`.
- `shasum -a 256 -c SHA256SUMS`: all 57 listed artifact files `OK`; checksum coverage has zero missing and zero extra files (the checksum file itself is intentionally not listed).

## Independent checks

All of these checks completed without source edits:

1. The 32 inventory source revisions exactly match both the manifest's accepted-revision map and the canonical partition-index membership (`0` mismatches; `0` missing rows).
2. All 160 object identity fields have valid exact SHA/length form (`0` malformed). The full join `(repository, source_sha, path, git_blob_sha)` is unique (`0` duplicates); `(repository, source_sha, path)` is also unique.
3. All 116 local bodies were reread with `git cat-file blob` at the locator's exact commit/path. Commit, Git blob SHA, raw SHA-256, and byte length matched the recorded row (`116/116`, `0` bad).
4. All 44 GitHub-blob body archives were joined to their index rows, endpoint, source/blob identity, raw bytes, and byte length (`44/44`, `0` bad; no missing or extra archive files).
5. All four preserved API tree responses were checked against their recorded raw-response SHA, endpoint, source SHA, parsed response SHA, `truncated=false`, and entry count; tree paths were unique and typed (`4/4`, `0` bad). This is response evidence, not execution evidence.
6. The 44 API index rows join exactly to the corresponding source-object rows (`0` missing/mismatched joins).
7. Reference accounting is complete: `1,356` references total; `483` resolved (`446` local reusable workflows + `37` local actions), `873` unresolved. Every resolved identity points to an indexed accepted source object. Recursive identities are `74` edge occurrences / `37` unique workflow targets; all targets exist as workflow objects; cycle list and object cycle IDs are empty (`0`).

## Required downstream handling

- The manifest intentionally records `actual_jobs=null`, `checks=null`, `execution=false`, and `gate_input=false`. `uses-line-scan-v1` source rows cannot establish job/check existence, required-check applicability, run status, provider trust, or expected workload. A mapper must preserve this distinction and require independent collector evidence for execution claims.
- Ten of the 32 accepted repositories are explicitly `no_workflow_or_action_manifests_observed` (including `tailrocks/homebrew-velnor`, `tailrocks/termrock`, and the eight skills repositories). This is an observed source-inventory state, not proof of an empty workload or a successful exclusion. Keep the repository row and apply expected-workload policy separately.
- Seed reconciliation is explicit: `141/155` default workflow seed rows match accepted source/blob identity; `14` remain unmatched because their seed revision is the opening Velnor SHA `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, while the accepted revision is `d20d4d1d17590cca85b501d982cbaad70d42c641`. Do not alias, silently upgrade, or count those historical rows as current execution evidence.
- All `873` unresolved references remain represented and unresolved. This includes `871` unresolved external-action references (pinned refs retained) and two unresolved local-action paths with reason `local_target_not_in_exact_source_tree`. They must not be dropped, treated as successful dependency edges, or used to fabricate child jobs. The README says unresolved references carry a reason and ref; the 871 external rows carry refs but omit `reason`. Resolution remains fail-closed, but the schema/documentation should either classify that external reason or explicitly make it optional.
- Only four repositories have preserved API tree evidence; the other source bodies are proven through exact local Git objects. A consumer must not infer complete repository-tree absence from a null `tree_evidence` field, and must not infer execution or expected jobs from this source-only corpus.

## Verdict

Bounded source-byte, identity, join, reference-retention, and cycle checks: **clean**. The artifact is suitable as an immutable source-object input seam. It is **not** a G0 gate input and cannot support a green result until an independent collector binds current source/event/run/provider/check/artifact evidence and separately derives nonempty expected workloads. The unresolved-reference reason omission is a small contract/documentation defect; it does not create a green path because all such rows retain `resolution=unresolved`.
