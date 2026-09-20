# Independent corrected G0 native-routing inventory review

Review target: `G0/native-routing/g3-workload-inventory-20260920T070624Z-corrected-successor.md`

Target SHA-256: `78727eaf79289bb5242fb3fac45937ba41b14a668599dec1a0643204f02edc0c`

Predecessor binding: `g3-workload-inventory-20260920T052347Z.md`, SHA-256
`840e32fa6dcaf977d65e51005e12da75a9f704303375eae35d848efcf8a42e41`.
The predecessor remains immutable. This review is read-only; no source,
generated output, workflow, host, release, G0/G3 gate, or GitHub state changed.

## Verdict

`APPROVE_CORRECTED_SOURCE_ONLY`.

The successor fixes the only prior defect: count scope. Its machine-backed
canonical index contains:

| Repository | Expected IDs | Non-marker IDs | Marker-suppressed IDs |
|---|---:|---:|---:|
| `tailrocks/holla` | 4 | 4 | 0 |
| `tailrocks/parallax` | 25 | 25 | 0 |
| `tailrocks/ruxel` | 9 | 9 | 0 |
| `tailrocks/tablerock` | 12 | 12 | 0 |
| `tailrocks/termpane` | 4 | 4 | 0 |
| `tailrocks/termrock` | 8 | 0 | 8 |
| **Total** | **62** | **54** | **8** |

This is not an execution or gate approval. The eight Termrock IDs remain
required source obligations; `logical_only_source_derived_no_workflow` is not
green. The canonical index reports `status=source_derived_index_only`,
`gate_status=not_evaluated`, and `attestation=none`.

## Machine and immutable-source checks

Read-only checks against
`G0/fleet/workload-contract-index-20260920T000017Z.json` produced:

- index SHA-256 `75000aca431f4e195a39f605eea846bc0ba6aed58e3bbc38e94e42009b12b269`;
- flattened expected workload states: 62 total, 54 excluding the
  `logical_only_source_derived_no_workflow` state, 8 in that state;
- blob inventory totals for the six rows: canonical 48, scan 17, workflow 34,
  reusable-workflow 9, total 108;
- exact source-tree listing counts/digests: holla 85/
  `bcee4dfd8f91f0ab0fe6922c50a6b81ddbe8d0b001cfa26aa7985c5191f047c5`,
  parallax 1881/`ad86ce2f368c6b9ebeb36f253761687ee5af4464f01cc5f976cbf4432a7b862a`,
  ruxel 487/`58be4997ef0af7970038420f3e2bea87168521ad7c92e72ef405203edde4e7ba`,
  tablerock 1065/`1cfeccd7853116dec3accc5fdb81a08a267aefd9111029e42ec206e7a54d068b`,
  termpane 94/`198cb98e5386929d02409568addb7f3e3b84aa9b622364058dabdca4ba86b93e`,
  termrock 1812/`4bfec01c1c1665756edeee19d0b2ed63c75064b4b670196d2da7727851baa0df`;
- a read-only pin walk over those exact trees: `pins=108`,
  `mismatches=0`;
- all six source checkouts had empty `git status --short` output.

The recorded default-branch SHAs and capture hashes remain bound by the
successor and prior review. Recomputed capture hashes match: metadata
`f8d073509f3c11471e878a596f48f312af57598f0e676493996f6a6e98c590bc`, analysis
`439cd2deee3a45bafe7e23edcaab998610cf157d8e9431066e4fc2007f743a17`,
workflow-source ledger `96b2ed5becf5028b40c57217cb0ec193a891566c8f1924cf66f7dd56b6356c97`,
capture script `6d3ebd3a4e71d1af1cbc280d64c23b6ed167c9e931d169fa8b7e4cdca717757a`,
identity reconciliation `62ff8bdff4e0d33e2edf856edacf3edf9db6900b3300337811add408ace93cac`,
and churn `b44e0a4e4e0dc0563bb96484fc8e3d36839e724a35a756ec8e0031b93060a00b`.

## Native routing boundary

The successor's Tablerock conclusion is correct and source-only:

- `native/Package.swift:10-12` requires macOS 26;
- `.github/ci/project.toml:157-179` retains both Swift/Xcode units and their
  native commands;
- the exact captured `.github/workflows/ci-unit-swift.yml` blob is
  `334d182e2d935ebbba265c393b83b72ab53277f4`, with `runs-on:
  ubuntu-24.04` at lines 100-105;
- `ci-main.yml` invokes both native units through that reusable workflow.

The stale-generator explanation is a diagnosis, not a run result. At the
recorded `b9c3156cdb88e63c11b9e595a3e694b02238c09`, the Swift scanner creates
ordinary `UnitKind::Swift` units without a native platform field
(`crates/velnor-workflow/src/scan/swift.rs:15-51,137-168`), while the captured
consumer is statically Ubuntu. Current Velnor native-policy code is not
silently attributed to that captured consumer. No hosted Swift/Xcode result,
provider capability, generated-output correctness, or migration completion is
claimed; regeneration remains a later gated operation.

The successor therefore correctly keeps Darwin triples, Tablerock x86_64, the
XCFramework producer/consumer edge, native IDs, and all not-run states as
obligations rather than pass evidence. Its only delta from the predecessor is
the explicit `62 total = 54 active/generated + 8 marker-suppressed required`
count scope.

