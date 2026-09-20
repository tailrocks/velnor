# G1 hostile producer fixture source review: `55546e7d`

- Review date: 2026-09-20 Asia/Ho_Chi_Minh.
- Reviewed commit: `55546e7d64d77a3230c4e6ed52b12f433e3f36aa` (`codex/g1-bootstrap-hostile-fixture`).
- Parent: `d60c0e2211b2830c64e7489d05b4cfe0bc77d65f`.
- Worktree: `/private/tmp/g1-bootstrap-hostile-fixture`, clean before and after review.
- Scope: narrow producer-archive transport remediation. No source files were changed and no merge was performed.

## Verdict

**SOURCE DELTA APPROVED; no G1 approval or hosted-canary claim.** This commit closes the prior `G1-HF-8` source finding: the producer contract is now ZIP-only and the producer census result must explicitly report `format == "zip"`. Generic TAR support remains available only to the clean-source archive checker path. No new source finding was found in this delta.

## Remediation verified

- `fixture-contract.json:20-22` declares the base-owned producer transport as `zip`.
- `trusted-harness.sh:252-274` requires that contract field to equal `zip` before execution.
- `trusted-harness.sh:301-310` still runs the generic checker for the clean source archive, but the producer exact-member census now requires the exact schema, valid status, `format == "zip"`, and two files.
- `trusted-harness-negative-tests.sh:45-126` creates both an exact ZIP and an exact TAR. It requires the generic checker to accept the TAR as `format=tar`, then requires the producer ZIP predicate to reject that result. Existing extra, missing, duplicate-name, and CRC rejection cases remain covered.
- The commit does not add a TAR alias or fallback to producer execution.

## Independent safe archive checks

At the frozen commit, a disposable archive check returned:

```json
{"producer_tar_gate":"rejected","tar_generic":{"format":"tar","files":2,"status":"valid"},"zip":{"format":"zip","files":2,"status":"valid"}}
```

Thus an exact two-member TAR remains accepted by the generic checker but cannot satisfy the producer transport gate; an exact two-member ZIP satisfies it.

## Report provenance

The reviewer-owned `source-review-d60c0e.md` is separate and currently contains the original `G1-HF-8` finding, with no appended `55546e7d` remediation text. Current SHA-256: `758de5eb7e46f9a373074135c7c6b6f4661ac3238f823b3e7a73957c3b75571f` (5,540 bytes). The author’s separate note is `source-remediation-55546e7d.md`, SHA-256 `1e1bce4946a927000ee5943b9a163aff1416a9d46d52a61cf149a9b8873298e2` (921 bytes). The prior annotated-state hash recorded during review was `54bd08e3285756cc852091cb08b084aa15ad295303859329f292bc952022fea`; it is not the current reviewer report. No pre-annotation byte snapshot was retained locally, so byte-for-byte equality to an unannotated historical copy cannot be independently reconstructed; the current file content and finding are verified intact.

## Static verification

- `rtk bash -n` for `build.sh`, `trusted-harness.sh`, and `trusted-harness-negative-tests.sh`: pass.
- `rtk python3 -B trusted-archive-check.py --help`: pass.
- `rtk jq empty` for fixture, trusted-contract, and producer-manifest schemas: pass.
- `rtk rustfmt --check --edition 2024 probe.rs`: pass.
- `rtk git diff --check d60c0e2211b2830c64e7489d05b4cfe0bc77d65f..55546e7d64d77a3230c4e6ed52b12f433e3f36aa`: pass.
- `rtk git diff --check c010334e48a426d3b7a5e848ad334fd002da6406..55546e7d64d77a3230c4e6ed52b12f433e3f36aa`: pass.
- `rtk bash trusted-harness-negative-tests.sh`: pass.

No hostile probe execution, Docker/OrbStack/Velnor operation, image pull/build/create/start, network request, or Mac-host runtime was performed. External acquire provenance, the hosted Linux canary, and the broader G1 gate remain outstanding.
