# Independent G1 rereview: corrected real API check-suite supplement

Date: 2026-09-20
Reviewer: G1 independent evidence review

## Verdict

**APPROVE the corrected successor for bounded raw-derived fixture metadata and relationship use only.** The previous exact-metadata rejection is resolved. This remains read-only provider evidence; it is not current gate authority, execution proof, rollout evidence, or merge approval.

## Frozen identity and successor relation

Target: `G0/real-api-fixture-supplement-20260920T085311Z-corrected-20260920T090934Z/`

```text
manifest.json             fea65b61ff1668ff58762066b207ac1c9534452d328080f1f0680e7cb571447e
relationship-report.json  dbcc5a2d331463c12253c49203ac54ac7d8d0df1824333e6cc90c61f9118893d
README.md                 f940601df3c0c71ead0cf9e1f1797a8c766e70569389710c77299f0ffde6196c
```

This is intentionally metadata-only. `relationship-report.json.raw_source_root` points to the frozen original raw directory; the successor contains no copied raw tree. The successor's `raw_files` entries are exactly equal to the original manifest's entries. Original artifacts remain unchanged:

```text
original manifest         1146764830a0e6e31518dd05abd6c00e6627f554d6dbbc97a55d721facabe70a
original relationship     c1ab3d2eb6802be4e9c67ba07b3ef76e4deeb88757ac31b20a6689224bc69e12
original README           8fd79fe81ebd56ccf809a3546558ca7c66d1c868573ccc199b266f07773bcaae
raw source root           G0/real-api-fixture-supplement-20260920T085311Z/
```

The base real corpus identity is preserved (`manifest d2c66c…e2bb`, relationship report `28b88e…8445`); the quarantined preliminary 422 directory is excluded.

## Independent proof

1. Raw-source integrity

   - Successor manifest declares 72 raw entries and 18 unique GET request records; the original raw source contains exactly 72 files.
   - Recomputed hashes and byte counts through `raw_source_root`: 72 entries, missing `0`, hash/size mismatches `0`.
   - Original-versus-successor `raw_files` manifest comparison: exact match.
   - Reusing the frozen original raw bytes, all 18 response envelopes remain exact: response size is HTTP headers + four separator bytes + body; header prefixes and body suffixes compare byte-for-byte.

2. Corrected metadata and parser boundary

   - Independent line parser against every referenced raw `.http` file: records `18`, raw missing `0`, date mismatches `0`, status mismatches `0`, Link mismatches `0`, `has_next` mismatches `0`, method/status failures `0`.
   - Direct suite `velnor-check-suite-96108227551` now has `response_date_header=Sun, 20 Sep 2026 08:53:22 GMT` and `link_header=absent`, exactly matching raw headers. The earlier tab-shifted value and truncated `GM` date are gone.
   - Manifest and relationship-report request-record sets canonicalize to an exact match. All 18 records are structured objects with one consistent field set; metadata Link/date fields contain no embedded TSV tab contamination.
   - `metadata_revision=2`, `raw_bytes_preserved=true`, and the declared parser regression status is `passed`, covering independent missing Date/Link, spaces/tabs in values, exact `rel=next`, and structured-object emission. Output-level checks above independently confirm the corrected result.

3. Pagination and relationship scope

   - Velnor commit `df9fb272c025f76cc8711560209afcdfd6cc4e00`: list pages 1–3, total 3, suite IDs `96108219979`, `96108224766`, `96108227551`.
   - Homebrew commit `c501e90d014c207234ed94ea41f7a1c9b6ea0c7c` is represented by pages 1–4, total 4, suite IDs `96059348120`, `96059348227`, `96059348318`, `96059350728`.
   - Relationship graph has 7 direct suites, 4 provider App objects, and 7 edges; every edge suite/App ID resolves to the corresponding raw-derived object.
   - Original base Velnor/Homebrew check-run body hashes remain the recorded values. Velnor's three supplemental list suites equal the three original suite IDs. Homebrew's two newer IDs `96059348120` (Claude) and `96059348227` (DCO-2) remain explicit list-only additions.
   - Those queued Homebrew direct bodies still say `status=queued`, `conclusion=null`, `latest_check_runs_count=0`. No execution, jobs, artifacts, or backdated check-run success is inferred. The artifact/run-attempt limit remains untouched.

## Boundary

The corrected successor is suitable for checker/mapper regression and bounded relationship evidence. It makes no live-gate, execution, checkout, release, rollout, dispatch, or authority claim. Consumers must retain the successor's `raw_source_root` binding and both successor hashes; do not substitute the rejected original metadata or the excluded 422 capture.
