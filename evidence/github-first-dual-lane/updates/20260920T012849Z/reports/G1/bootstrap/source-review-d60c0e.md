# G1 hostile producer fixture source review: `d60c0e`

- Review date: 2026-09-20 Asia/Ho_Chi_Minh.
- Reviewed commit: `d60c0e2211b2830c64e7489d05b4cfe0bc77d65f` (`codex/g1-bootstrap-hostile-fixture`).
- Parent: `c010334e48a426d3b7a5e848ad334fd002da6406`.
- Worktree: `/private/tmp/g1-bootstrap-hostile-fixture`, clean before and after review.
- Scope: exact producer ZIP/member remediation, manifest schema/hash binding, and safe negative coverage. No source files were changed and no merge was performed.

## Verdict

**CHANGES REQUIRED for the source-only producer archive contract.** The previous archive finding is substantially fixed: the wrapper now requires two base-owned file names, exact file-set equality with only parent directories permitted, full consumption of both files, ZIP CRC/size checks, strict manifest schema validation, and manifest identity binding to trusted head/closure/run/binary observations. One residual contract gap remains: the exact-member checker accepts TAR as a valid producer archive, and the harness does not assert `format == "zip"`. This is not G1 approval. No hostile probe, Docker operation, network request, or Mac-host runtime was performed.

## Finding

### G1-HF-8 — Producer exact-member mode is not ZIP-only (medium)

`trusted-archive-check.py:225-238` deliberately accepts both TAR and ZIP, and `exact_member_census` at `:354-383` is format-agnostic. The producer wrapper invokes exact census at `trusted-harness.sh:300-309` but checks only schema, status, and `files == 2`; it never checks `.format == "zip"`. A safe TAR containing exactly `velnor-workflow` and `candidate-manifest.json` returned:

```json
{"schema":"velnor.bootstrap-archive-exact.v1","status":"valid","format":"tar","members":2,"files":2,"bytes":8}
```

The GitHub artifact service contract is a raw ZIP transport. A correctly independent service digest should prevent a TAR from matching the recorded artifact digest, but the source contract itself must fail closed on the required transport type. Add a ZIP-only producer mode or assert `format == "zip"` in the harness; retain TAR support for the clean source archive if needed.

## Remediation verification

- `trusted-contract.schema.json:21-99` now requires `manifest`, `producer.manifest_member`, and `producer.manifest_sha256`; `fixture.manifest_schema_sha256` is also required.
- `producer-manifest.schema.json:5-30` is strict (`additionalProperties: false`) and fixes schema/profile/features/platform/repository/run/revision/closure/binary digest types and values.
- `trusted-harness.sh:43-108` requires and validates the new member/schema/hash inputs. `:137-148` verifies the manifest schema path is canonical, under the trusted root, and exactly hashed.
- `trusted-harness.sh:168-251` compares every new handoff field to base-owned expected values. `:300-342` performs exact two-file census, hashes both members, extracts only the manifest, validates its strict schema, and binds its schema/profile/features/platform/repository/run/revision/closure/binary digest to trusted handoff values and the measured binary.
- The new `G1_*` values are consumed before candidate launch and are not included in the allow-listed container environment (`trusted-harness.sh:431-438`). Candidate output, fixture JSON, manifest contents, and probe output do not select the expected names, schema, or hashes. Their provenance still depends on the external acquire job populating the base-owned environment; this fixture contains no API acquisition and therefore cannot independently prove that caller boundary.

## Safe archive cases

The repository negative harness passed. In addition, direct disposable ZIP fixtures produced these checker results with `--exact-member velnor-workflow --exact-member candidate-manifest.json`:

| Case | Result |
| --- | --- |
| exact binary + manifest | accepted (`format=zip`, `files=2`) |
| extra file | rejected |
| missing manifest | rejected |
| duplicate member name | rejected |
| traversal member | rejected |
| symlink member | rejected |
| corrupted member CRC | rejected |
| declared member size above 256 MiB | rejected |
| TAR with the two expected files | **accepted — G1-HF-8** |

Both accepted ZIP members were independently hashed; the strict producer manifest schema accepted a valid disposable manifest. The negative harness also covered duplicate/float/boolean/extra/missing JSON, exact archive extra/missing/duplicate/CRC cases, forged manifest identity, unreadable output traversal, unexpected output, and the non-Linux early gate.

## Prior c010 finding status

The c010 exact member/manifest gap is closed for ZIP contents: extra/missing/duplicate names, unsafe paths, links, declared size, CRC, and manifest identity now fail closed. Raw service/archive digest equality remains correct (`trusted-harness.sh:107-108`, plus `trusted_file` at `:137-148`). The only remaining archive-contract issue found here is the missing ZIP-format assertion above.

## Static verification

- `rtk git diff --check c010334e48a426d3b7a5e848ad334fd002da6406..HEAD`: pass.
- `rtk bash -n` for all three fixture shell scripts: pass.
- `rtk python3 -B trusted-archive-check.py --help`: pass.
- `rtk jq empty` for fixture, handoff, and producer-manifest schemas: pass.
- `rtk rustfmt --check --edition 2024 probe.rs`: pass.
- `rtk bash trusted-harness-negative-tests.sh`: pass.

No hostile probe execution, Docker daemon operation, image pull/build/create/start, or network request was performed. No G1 claim follows from this report.
