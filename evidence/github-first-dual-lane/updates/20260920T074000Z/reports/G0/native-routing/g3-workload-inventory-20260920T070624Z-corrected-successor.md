# G0 native-routing workload inventory — corrected successor

Created: 2026-09-20T07:06:24Z  
Status: `source_derived_not_execution`; `gate_status=not_evaluated`  
Scope: the same six repositories and immutable evidence as the predecessor.

This is the corrected successor to
`G0/native-routing/g3-workload-inventory-20260920T052347Z.md` (SHA-256
`840e32fa6dcaf977d65e51005e12da75a9f704303375eae35d848efcf8a42e41`). The
predecessor remains immutable. Independent review
`G1/reviews/g0-native-workload-inventory-840e32fa.md` (SHA-256
`3fdcb5d563046c35c7ef1af9b7869f985073761f386aab723b1417a25ea8581a`) approved
the source facts and required this count-scope correction.

## Corrected count

The authoritative inventory contains **62 expected workload IDs**. The
explicit active/generated subtotal is **54**, with **8 additional
marker-suppressed source obligations**:

| Repository | Source units | Additional source obligations | Expected IDs | Count scope |
|---|---:|---|---:|---|
| `tailrocks/holla` | 1 | Debian/Homebrew/cross-repository publication | 4 | active/generated |
| `tailrocks/parallax` | 23 | service exercise and release cross-target | 25 | active/generated |
| `tailrocks/ruxel` | 7 | disposable SSH fixture and target matrix | 9 | active/generated |
| `tailrocks/tablerock` | 11 | XCFramework producer/consumer | 12 | active/generated |
| `tailrocks/termpane` | 3 | benchmark/heap/fuzz fixture obligation | 4 | active/generated |
| `tailrocks/termrock` | 8 | `.github-gen/NO_WORKFLOWS_REQUIRED.md` suppresses generation | 8 | marker-suppressed, still required |
| **Total** | **53** |  | **62** | **54 active/generated + 8 marker-suppressed** |

The 8 Termrock IDs are not excluded work and are not successful execution.
The marker records an analyzer/workflow boundary; it does not erase the
workspace, docs/WASM/Playwright, contract-audit, embedded-font, or pinned
old-revision obligations. The complete six-row inventory must therefore never
be reported as “54 IDs” without the qualifier above.

## Source and capture binding

The six source rows, exact mappings, platform/architecture boundaries,
provider eligibility, dependency edges, native exclusions, and unknowns are
unchanged from the immutable predecessor. Independent review reverified all
source trees and **108 canonical/scan/workflow/reusable-workflow blob pins**.

| Repository | Default branch SHA | Source tree listing SHA-256 |
|---|---|---|
| `tailrocks/holla` | `027dac6be2070b7e836c1ca123861fcbade72003` | `bcee4dfd8f91f0ab0fe6922c50a6b81ddbe8d0b001cfa26aa7985c5191f047c5` |
| `tailrocks/parallax` | `6a12bf47a816b63e848b563aaa45ef9694159c79` | `ad86ce2f368c6b9ebeb36f253761687ee5af4464f01cc5f976cbf4432a7b862a` |
| `tailrocks/ruxel` | `3d34049820a6d4ce3707e574f407cbf4047ff932` | `58be4997ef0af7970038420f3e2bea87168521ad7c92e72ef405203edde4e7ba` |
| `tailrocks/tablerock` | `e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc` | `1cfeccd7853116dec3accc5fdb81a08a267aefd9111029e42ec206e7a54d068b` |
| `tailrocks/termpane` | `7602430f18852350cc3155e58bb96799234cbfe5` | `198cb98e5386929d02409568addb7f3e3b84aa9b622364058dabdca4ba86b93e` |
| `tailrocks/termrock` | `936982e60bce6d19cf33ae09b53545a955f1073d` | `4bfec01c1c1665756edeee19d0b2ed63c75064b4b670196d2da7727851baa0df` |

Bound capture values remain:

- canonical workload index SHA-256 `75000aca431f4e195a39f605eea846bc0ba6aed58e3bbc38e94e42009b12b269`;
- capture metadata SHA-256 `f8d073509f3c11471e878a596f48f312af57598f0e676493996f6a6e98c590bc`;
- capture analysis SHA-256 `439cd2deee3a45bafe7e23edcaab998610cf157d8e9431066e4fc2007f743a17`;
- workflow-source ledger SHA-256 `96b2ed5becf5028b40c57217cb0ec193a891566c8f1924cf66f7dd56b6356c97`;
- capture script SHA-256 `6d3ebd3a4e71d1af1cbc280d64c23b6ed167c9e931d169fa8b7e4cdca717757a`;
- identity reconciliation SHA-256 `62ff8bdff4e0d33e2edf856edacf3edf9db6900b3300337811add408ace93cac`;
- capture churn SHA-256 `b44e0a4e4e0dc0563bb96484fc8e3d36839e724a35a756ec8e0031b93060a00b`.

The source-owned generator pin recorded in all six configs is
`b9c3156cdb88e63c11b9e595a3e694b02238c09`. No later generator or consumer
workflow was substituted into this source-only inventory.

## Native Swift boundary (non-execution finding)

Tablerock remains a required native workload, not a Linux result. At source
SHA `e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc`, `native/Package.swift` declares
macOS 26; `native/App/project.yml` declares Xcode 26.6, macOS 26 deployment,
and arm64+x86_64 release output; the Xcode project consumes the
`tablerock_ffiFFI.xcframework`. `ci-main.yml` calls both native units through
`ci-unit-swift.yml`, whose captured blob
`334d182e2d935ebbba265c393b83b72ab53277f4` uses `runs-on: ubuntu-24.04`.

The architecture trace is a stale-generator diagnosis, not execution proof:
the old `b9c3156c` collapsed Swift renderer selected the lane default and did
not propagate native member requirements into the reusable job. Current Velnor
native-policy source has a generic typed native/portable split, but that source
fix is not silently attributed to the captured consumer. The correct next
operation is a reviewed source-pin/consumer regeneration after the applicable
G1/G2 gates. No hosted Swift/Xcode result is claimed here.

Parallax Darwin triples remain cross-build obligations unless source evidence
proves native execution. Tablerock x86_64 remains a build/archive obligation;
Intel macOS 27 execution is unavailable. No provider label, generated job,
source plan, or configured Velnor label is capability or execution proof.

## State boundary and validation

All 62 rows are logical/source obligations only. `not_run`,
`not_generated`, `marker_suppressed`, fixture, service, publication, native,
and unknown states are not pass/green states. No workflow was dispatched; no
host, artifact, publication, release, migration, or generated consumer was
changed.

Independent review performed the read-only tree, blob-pin, source mapping,
capture identity/churn, platform, native, and dependency checks. The only
successor change here is the explicit count scope: **62 total = 54
active/generated + 8 marker-suppressed required obligations**.
