# G1 hostile producer fixture source review: `c010334e`

- Review date: 2026-09-20 Asia/Ho_Chi_Minh.
- Reviewed commit: `c010334e48a426d3b7a5e848ad334fd002da6406` (`codex/g1-bootstrap-hostile-fixture`).
- Parent: `de23afc17f3869d7d86c7acdb20935dbf917db74`.
- Review ancestry: normal remote branch, descended from the previously reviewed `119127c3b5127914aa192b13fdfb5fc87ada97db`.
- Worktree: `/private/tmp/g1-bootstrap-hostile-fixture`, detached review worktree; clean before and after review.
- Scope: source-only review of the hostile probe, trusted archive/schema checker, negative harness, and Linux-only trusted wrapper. No source files were changed and no merge was performed.

## Verdict

**CHANGES REQUIRED for source-only Linux-harness suitability.** The five blockers from `119127c3` are substantially remediated in the source, and the wrapper now binds the handoff to caller-supplied exact API observations. The producer ZIP contract is still not fail-closed: the checker validates safe archive shapes and hashes one binary member, but never enforces an exact member census or validates the required producer manifest. A ZIP with arbitrary extra members, or with the required manifest omitted, is accepted. This is not G1 approval. No hosted canary ran, and the probe, Docker, and Mac-host runtime were not executed.

## Findings

### G1-HF-6 — Producer ZIP has no exact member census or manifest validation (high)

The trusted handoff schema has only `producer.binary_member` and `binary.member` (`trusted-contract.schema.json:50-84`); it has no required manifest member, manifest digest, or manifest contract. `trusted-harness.sh:273-279` checks the producer archive summary and hashes one caller-selected binary member, but never supplies an expected set of archive members and never reads or validates a manifest. `trusted-archive-check.py:154-177` only rejects unsafe names/types/duplicates/limits; arbitrary safe regular members remain accepted.

Safe reproduction on this review host: a ZIP containing `binary` plus an unrelated `extra` file returned archive status 0 with `members:2`, and `--member-sha256 binary` returned status `valid`. Thus service-digest equality and binary-byte equality do not prevent extra metadata, missing required metadata, or a forged/malformed manifest from crossing the producer boundary. The approved isolation design requires the exact binary and manifest files and malformed-manifest rejection.

The service binding itself is correct: `trusted_file` re-hashes `G1_PRODUCER_ARCHIVE_PATH` (`trusted-harness.sh:130-140`), and input validation requires `G1_EXPECTED_PRODUCER_SERVICE_DIGEST == sha256:$G1_PRODUCER_ARCHIVE_SHA256` (`:100-101`). Keep that raw ZIP/service equality, then add a base-owned exact member allow-list (including the required manifest), reject missing/extra/ambiguous members, consume every admitted member with declared-size/CRC checks, and strictly validate the manifest's binary hash/profile/features/platform/closure fields. Do not let candidate manifest fields choose the allow-list.

### G1-HF-7 — API provenance is exact-compared but remains an unproved caller boundary (high integration gate)

The new inputs and comparisons are materially better: workflow/event, target/head repository names and IDs, run/job/artifact IDs, exact names, service digest, archive digest, binary member, and all handoff fixture/image hashes are required and compared (`trusted-harness.sh:43-103, 163-234`). This closes the prior `119127c3` shape-only check **if and only if** the acquire job supplies those values independently from trusted API/tree observations.

This fixture still receives every expected identity/digest as environment input and contains no API acquisition or provenance proof. It does not itself reject a fork (`G1_EXPECTED_HEAD_REPOSITORY` only has to equal `G1_SOURCE_REPOSITORY`, not the target repository), encode producer run-attempt/expiry/duplicate-artifact facts, or prove that the archive path/hash was not selected from candidate output. The base acquire/verify integration must enforce same-repository owner-only admission, successful exact run/job/artifact selection, non-expired/unique artifact state, clean-head closure, and independent download/re-hash before invoking this wrapper. Until that integration exists, these comparisons are not G1 provenance evidence.

## Prior `119127c3` blockers

- HF-1 producer identity: **source remediation present, conditional on base-owned expected inputs**. Exact IDs/names/service/archive path/hash are now compared; no API acquisition is present here.
- HF-2 H1–H9 assertions: **source remediation present**. The result parser requires all listed fields and asserts counts/statuses, H7 abuse outcomes, child pressure, cache masks, and contract-minting (`trusted-harness.sh:465-555`). The probe now emits cache presence/readability masks, including empty-directory readability (`probe.rs:365-390`).
- HF-3 output census: **source remediation present**. `check_tree` explicitly records traversal/stat errors, rejects links/special files/hardlinks, applies byte/entry bounds, and enforces allow/required sets (`trusted-archive-check.py:344-414`). The harness uses an empty allow-list and requires rejection (`trusted-harness.sh:558-570`).
- HF-4 cache visibility: **source remediation present, conditional on base-owned expected masks**. Count, presence mask, readability mask, and zero writes are required (`trusted-harness.sh:480-499, 547-554`). The image/mount integration must still provide the expected masks independently.
- HF-5 schema enforcement: **source remediation present for the checked schema**. Duplicate keys, escaped-equivalent keys, wrong integer/boolean/float values, missing/extra fields, trailing JSON, and oversized JSON fail closed through the checker. The validator is a deliberately narrow implementation of the hashed handoff schema, not a general JSON-Schema engine; that is acceptable only while the trusted schema remains fixed to its supported constructs.

## Safe verification

- `rtk git diff --check 119127c3b5127914aa192b13fdfb5fc87ada97db..HEAD`: pass.
- `rtk bash -n` for `build.sh`, `trusted-harness.sh`, and `trusted-harness-negative-tests.sh`: pass.
- `rtk python3 -B trusted-archive-check.py --help`: pass.
- `rtk jq empty` for `fixture-contract.json` and `trusted-contract.schema.json`: pass.
- `rtk rustfmt --check --edition 2024 probe.rs`: pass.
- `rtk bash trusted-harness-negative-tests.sh`: pass. It covered the non-Linux early gate, duplicate/float/boolean/extra/missing schema cases, unexpected output, and unreadable output subtree. It did not reach Docker because the wrapper rejects `RUNNER_OS=Darwin` first.
- Supplemental safe checker inputs for an escaped-equivalent duplicate key, trailing JSON, and a 4 MiB-plus handoff all returned nonzero as expected.
- Supplemental safe ZIP census reproduction demonstrated the HF-6 acceptance gap above.

No hostile probe execution, Docker daemon operation, image pull/build/create/start, network request, or Mac-host runtime was performed. No G1 claim follows from this report.

