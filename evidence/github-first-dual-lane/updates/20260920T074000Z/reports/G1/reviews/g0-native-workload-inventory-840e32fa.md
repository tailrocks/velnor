# Independent G0 source-workload inventory review

Review target: `G0/native-routing/g3-workload-inventory-20260920T052347Z.md`

Target SHA-256: `840e32fa6dcaf977d65e51005e12da75a9f704303375eae35d848efcf8a42e41`

Review mode: read-only. No source, generated output, workflow, host, release,
G0/G3 gate, or GitHub state was changed. No workflow or test workload was
executed.

## Verdict

`APPROVE_SOURCE_ONLY_WITH_COUNT_SCOPE_CORRECTION`.

The six source checkouts, source-derived IDs, platform/provider boundaries,
native exclusion, dependency edges, and full source hashes are correct. The
report must state the count scope precisely:

- The authoritative six-row index contains **62 expected workload IDs**:
  holla 4 + parallax 25 + ruxel 9 + tablerock 12 + termpane 4 + termrock 8.
- **54** is the active/generated-workload subtotal when Termrock's 8
  marker-suppressed source obligations are excluded.
- Termrock's `.github-gen/NO_WORKFLOWS_REQUIRED.md` is an analyzer failure and
  source obligation boundary, not proof that those 8 obligations do not
  exist. The reviewed report itself records all 8 at lines 345-352 and says
  the six expected sets are nonempty at lines 380-388.

Therefore: approve the inventory as a 62-ID source-only inventory, or label it
`54 active/generated + 8 marker-suppressed`; reject an unqualified claim that
the complete six-row source inventory has only 54 IDs.

This is not execution approval. The report correctly keeps all logical,
not-run, not-generated, fixture, publication, service, and native states out
of green/pass semantics.

## Exact source identity and capture binding

The reviewed report binds the canonical index, raw capture, capture script,
generator revision, and hosted Apple evidence at lines 13-33. Recomputed
SHA-256 values match every recorded value:

| Evidence | Recomputed result |
|---|---|
| Reviewed report | `840e32fa6dcaf977d65e51005e12da75a9f704303375eae35d848efcf8a42e41` |
| Canonical index | `75000aca431f4e195a39f605eea846bc0ba6aed58e3bbc38e94e42009b12b269` |
| Capture metadata | `f8d073509f3c11471e878a596f48f312af57598f0e676493996f6a6e98c590bc` |
| Capture analysis | `439cd2deee3a45bafe7e23edcaab998610cf157d8e9431066e4fc2007f743a17` |
| Workflow-source ledger | `96b2ed5becf5028b40c57217cb0ec193a891566c8f1924cf66f7dd56b6356c97` |
| Capture script | `6d3ebd3a4e71d1af1cbc280d64c23b6ed167c9e931d169fa8b7e4cdca717757a` |
| Identity reconciliation | `62ff8bdff4e0d33e2edf856edacf3edf9db6900b3300337811add408ace93cac` |
| Capture churn | `b44e0a4e4e0dc0563bb96484fc8e3d36839e724a35a756ec8e0031b93060a00b` |

All six target rows have the same default SHA in normalized before, normalized
after, identity-before, identity-after, and the read-only checkout. The six
checkout working trees are clean.

| Repository | Source SHA | Tree entries | Tree-listing SHA-256 |
|---|---|---:|---|
| `tailrocks/holla` | `027dac6be2070b7e836c1ca123861fcbade72003` | 85 | `bcee4dfd8f91f0ab0fe6922c50a6b81ddbe8d0b001cfa26aa7985c5191f047c5` |
| `tailrocks/parallax` | `6a12bf47a816b63e848b563aaa45ef9694159c79` | 1881 | `ad86ce2f368c6b9ebeb36f253761687ee5af4464f01cc5f976cbf4432a7b862a` |
| `tailrocks/ruxel` | `3d34049820a6d4ce3707e574f407cbf4047ff932` | 487 | `58be4997ef0af7970038420f3e2bea87168521ad7c92e72ef405203edde4e7ba` |
| `tailrocks/tablerock` | `e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc` | 1065 | `1cfeccd7853116dec3accc5fdb81a08a267aefd9111029e42ec206e7a54d068b` |
| `tailrocks/termpane` | `7602430f18852350cc3155e58bb96799234cbfe5` | 94 | `198cb98e5386929d02409568addb7f3e3b84aa9b622364058dabdca4ba86b93e` |
| `tailrocks/termrock` | `936982e60bce6d19cf33ae09b53545a955f1073d` | 1812 | `4bfec01c1c1665756edeee19d0b2ed63c75064b4b670196d2da7727851baa0df` |

Recomputed sorted `git ls-tree -r --format='%(objectname) %(path)'` digests and
entry counts match the report's table at lines 38-48. All 108 recorded
canonical, scan, workflow, and reusable-workflow blob pins in the six rows
match `git rev-parse <source-sha>:<path>` exactly.

## Workload provenance and canonical fields

The canonical index's per-repository `expected_workload_ids` are source fields,
not run-derived fields. Counts are:

| Repository | Source units / marker obligations | Expected IDs |
|---|---:|---:|
| holla | 1 + 3 package/publication obligations | 4 |
| parallax | 23 + service + release obligations | 25 |
| ruxel | 7 + SSH fixture + target matrix | 9 |
| tablerock | 11 + XCFramework obligation | 12 |
| termpane | 3 + benchmark/heap fixture | 4 |
| termrock | 8 source obligations; no generated workflow | 8 |
| **total** |  | **62** |

The generated source-unit IDs were independently counted in exact
`.github/ci/project.toml` files: holla 1, parallax 23, ruxel 7, tablerock 11,
termpane 3. Each declared root is source-backed. Termrock has no project TOML
because its exact marker says static analysis failed on the non-literal
`include_str!` pattern; its eight mappings are still source-backed by the
workspace, detached old-revision harness, contract-audit package, and docs
`package.json`. This is why Termrock must be represented as a blocked/source
obligation state, not silently dropped.

The source-only state boundary is correct at report lines 9-11, 52-62, and
378-401: configured providers and labels are not capability evidence; target
triples are build obligations unless a workflow proves native execution; and
no current run/artifact/publication/service/native result is inferred.

## Platform and provider review

### Tablerock native Swift/Xcode boundary

The key routing claim is confirmed:

- Exact `native/Package.swift:8-12` declares `.macOS(.v26)`.
- Exact `native/App/project.yml:7-10,19` requires XcodeGen 2.46, Xcode 26.6,
  and macOS deployment 26.0; lines 25-27 require arm64 and x86_64 release
  output with `ONLY_ACTIVE_ARCH: NO`; app/tests are macOS targets.
- The project consumes the cargo-produced
  `target/xcframework/tablerock_ffiFFI.xcframework` at lines 61-72 and
  152-167.
- Exact `ci-main.yml:702-736` calls both native units into
  `ci-unit-swift.yml`; exact reusable workflow lines 100-105 use
  `runs-on: ubuntu-24.04`.

Thus the Ubuntu Swift job is an observed source routing defect and cannot
produce a Swift/Xcode/native green result. This does not mean all Swift is
Apple-only: a portable SwiftPM package with no Apple platform/API/linkage
contract may be Linux-valid. This exact Tablerock package is not such a
package. The report correctly preserves native IDs and the XCFramework
producer/consumer edge rather than masking them as Linux work.

The linked native-policy evidence says the current hosted offer is exact
`xcode-27` arm64/macOS 27, with no Intel macOS 27 label. The source's
`x86_64` remains a build/archive obligation; it cannot be used to claim Intel
execution. This is capability evidence only, not a generated-output or hosted
test approval. The report's recorded generator pin `b9c3156...` remains the
actual source pin under review; later scanner/generator fixes are not silently
substituted into this inventory.

### Other repositories

- Parallax's `aarch64/x86_64-apple-darwin` triples are cross-build targets only.
  `docs/guide/quickstart.md:32-51` independently establishes the GreptimeDB,
  OTLP `4317/4318`, GraphQL/UI `4000`, and Turso service contract. Docker image
  builds do not satisfy that service exercise.
- Ruxel's exact `tools/fixtures/docker/Dockerfile:1-13`, `gate.sh:17-24`, and
  `parity-matrix.json` establish a disposable SSH/privilege and storage,
  PostgreSQL, systemd, and multihost obligation. Image build is not fixture or
  privileged-host proof.
- Termpane's exact Cargo/bench/fuzz sources declare Criterion, dhat, proptest,
  benchmark, heap, and fuzz obligations. The source plan cannot report those
  as executed.
- Holla's Cargo metadata and `docs/debian-apt-repo.md` establish Debian
  amd64/arm64, Homebrew preview, signed `holla-apt`, and cross-repository
  publication edges. Exact closing source has no matching release workflow;
  publication remains unrun.
- Termrock's exact marker suppresses generated workflows after analyzer failure.
  Workspace crates, docs/WASM/Playwright scripts, contract audit, embedded
  fonts, and the pinned old-revision git dependency remain source obligations.

## Dependency-edge review

Exact generated project declarations match the report:

- Parallax: analysis → model/proto/semconv; API → analysis/evidence/metadata/
  storage/test-support; CLI → evidence/metadata/model/redaction/semconv/server/
  spool/storage; evidence → analysis/model/redaction; Greptime → ingest/model/
  proto/semconv/storage; ingest → model/proto/semconv; MCP → evidence; metadata
  → model/redaction/semconv/storage; proto → semconv; sentry-proxy → ingest;
  server → analysis/API/evidence/Greptime/ingest/metadata/proto/semconv/spool/
  storage/test-support; storage → model/proto/semconv; test-support → metadata/
  model/proto/semconv/storage; xtask → API.
- Ruxel: agent → core/proto; CLI → core/proto; spec-extract → core.
- Tablerock: CLI → core/engine/files/persistence/tools/tui; engine → core;
  FFI → core/engine/files/persistence/tools; files/persistence/TUI → core.
- Termpane: fuzz → root termpane. Holla has no generated unit dependency edge.
- Termrock's source edges are separate from generated reusable workflows:
  docs package → WASM/catalog/contract/Playwright checks; oldrev harness →
  immutable old Termrock revision; contract audit → catalog; docs → catalog-web.

The report also retains workflow-to-reusable edges and package/service/native
producer-consumer edges. No unsupported provider, service, privilege, native,
publication, or artifact edge is promoted to a result.

## Exact read-only checks

Commands used:

1. `shasum -a 256` over the reviewed report and every bound input listed above.
2. `/usr/bin/git rev-parse HEAD`, `git status --porcelain`, and sorted
   `git ls-tree` digest recomputation for all six exact source checkouts.
3. `git rev-parse <source-sha>:<path>` for all 108 canonical/scan/workflow/
   reusable pins.
4. `jq` comparison of six target default SHAs across identity-before,
   identity-after, normalized-before, normalized-after, and source snapshots.
5. Read-only source inspection of exact Swift, Xcode, workflow, package,
   fixture, benchmark, docs, marker, and dependency files named above.

No compile, test, fixture launch, network dispatch, publication, or merge
operation was performed. The capture metadata itself is
`raw_observation_only_not_gate`, and the canonical index remains
`gate_status=not_evaluated`, `attestation=none`.

