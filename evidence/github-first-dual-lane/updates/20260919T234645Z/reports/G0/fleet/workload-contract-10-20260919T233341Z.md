# G0 source-derived workload supplement: disjoint ten repositories

Status: incomplete source contract. No workflow, build, helper, fixture, release,
install, dispatch, or publication command was executed. No source repository was
edited. The typed record is
[`workload-contract-10-20260919T233341Z.json`](./workload-contract-10-20260919T233341Z.json)
(SHA-256 `910d4db877c4ada0e241ec390546d080b0aed2e34dca1e879f6d6237a628a47b`).

## Integrity and scope

- Ten exact repositories were reread from committed source objects.
- Closing ref capture: `2026-09-19T23:33:41Z`; every `git ls-remote refs/heads/main`
  value equaled the reviewed local source HEAD at closing.
- `tailrocks/velnor` moved during collection: initial `1048337062…`, closing
  `d20d4d1d17590cca85b501d982cbaad70d42c641`. Its row is bound only to the closing
  `d20…` tree and is explicitly unstable across the capture window.
- Every row carries the committed tree listing digest, immutable config/scan
  blobs, workflow blobs, reusable workflow blobs, local action blobs, source unit
  roots, and explicit unit dependency edges.
- Historical `G0/fleet/workloads-full.json` and Rust-consumer rows are comparison
  inputs only. They cannot convert absent execution into pass.

| repository | closing `main` | tree entries | listing digest prefix | source units | expected IDs | committed workflows | reusable workflows |
|---|---|---:|---|---:|---:|---:|---:|
| `velnor` | `d20d4d1d` | 680 | `7328126aa6c0` | 17 | 21 | 14 | 6 |
| `parallax` | `6a12bf47` | 1881 | `ad86ce2f368c` | 23 | 25 | 8 | 3 |
| `tracing-request-level` | `f234158e` | 21 | `ade5aad63fbc` | 1 | 3 | 6 | 1 |
| `termrock` | `936982e6` | 1812 | `4bfec01c1c16` | 8 | 8 | 0 | 0 |
| `termpane` | `7602430f` | 94 | `198cb98e5386` | 3 | 4 | 6 | 1 |
| `tablerock` | `e2fe040c` | 1065 | `1cfeccd78531` | 11 | 12 | 7 | 2 |
| `schemalane` | `ab49e2dd` | 90 | `3f4d61fc9bf0` | 6 | 8 | 6 | 1 |
| `ruxel` | `3d340498` | 487 | `58be4997ef0a` | 7 | 9 | 7 | 2 |
| `pg-bigdecimal` | `7dc5267d` | 20 | `a84928d58c1f` | 1 | 2 | 6 | 1 |
| `holla` | `027dac6b` | 85 | `bcee4dfd8f91` | 1 | 4 | 6 | 1 |

The GitHub Actions API reported 15 Velnor workflows at capture, while the closing
source tree contains 14 workflow YAML files; the extra API object is not silently
treated as committed source. `termrock` has two active repository rulesets and a
200 branch-protection response whose `required_status_checks` is null; this does
not make its empty workflow tree green. Other endpoint 404s and empty ruleset
responses remain unknown policy, not proof of no policy.

## Source-derived obligations

- `velnor`: 17 committed units cover Bun, Docker, docs, OpenTofu, policy, and Rust.
  Source also requires runtime-products, package signer, preview publication, and
  Linux/Apple cross-target boundaries. Config uses providers `github-hosted` and
  `velnor`; automatic provider is hosted. Release is enabled in source but still
  requires immutable artifact, provenance, credential, environment, and tag-policy
  proof.
- `parallax`: 23 units cover Bun, two Docker image builds, fixture, 18 workspace
  Rust members, and policy. `docs/guide/quickstart.md` makes GreptimeDB, Turso,
  and OTLP behavior a product service obligation; image builds do not satisfy it.
  Rust source declares Apple/Linux targets. Release is explicitly disabled.
- `tracing-request-level`: one Rust unit. No `Cargo.lock` is committed, so locked
  dependency resolution is an expected unresolved obligation. Cargo metadata says
  `donbeave/tracing-request-level`, while the fleet repository is `tailrocks/...`;
  package/release identity must be resolved before G2.
- `termrock`: `.github-gen/NO_WORKFLOWS_REQUIRED.md` records analyzer failure, not
  no workload. Committed source independently yields five root Rust crates, the
  detached old-revision harness, detached contract-audit workspace, and Bun docs
  with WASM preview/catalog/contract/browser scripts. Deterministic
  `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), literal))` paths require the
  central static macro resolver; no copied historical YAML is accepted.
- `termpane`: policy, root Rust, and detached fuzz units; benches, heap profiling,
  and fixtures remain explicit source obligations.
- `tablerock`: eight Rust units plus Swift Package/Xcode units. `Package.swift`
  requires macOS 26 and arm64/x86_64 native work, while the committed Swift
  reusable workflow declares `ubuntu-24.04`; this is a source-visible routing
  defect/hold, not Swift coverage. `runners = "github"` excludes Velnor.
- `schemalane`: six Rust units plus a separate Postgres test-service obligation.
  `testcontainers-modules` and many `#[ignore = "requires Docker daemon"]` tests
  are committed source facts; generic `--no-tests pass` is not service coverage.
- `ruxel`: Docker image, policy, and five Rust units plus SSH/privileged/storage
  fixture and target-matrix obligations. The fixture installs `openssh-server`,
  exposes SSH 22, and the matrix names Postgres, systemd, iptables, multihost,
  ext4/two-tier/xfs storage. Image build alone is insufficient.
- `pg-bigdecimal`: one Rust unit plus package/release identity. Cargo metadata says
  `donbeave/pg-bigdecimal`, not the fleet `tailrocks/...` identity.
- `holla`: one Rust unit. Cargo/mise/docs declare Debian amd64/arm64 cargo-deb /
  zigbuild, Homebrew preview, and signed cross-repository `holla-apt` publication,
  but current source has no release-deb or publication workflow. The nine open PR
  API rows report stale base `fca7d0cc…`, not current `027dac6b…`; fresh tested-merge
  applicability is required.

## Provider, safety, and non-results

The JSON preserves provider configuration separately from eligibility proof. A
configured `velnor` label is not hosted/self-hosted capability evidence. Fork/Bot
trusted execution remains denied. Docker, SSH, Postgres, Swift/Xcode, WASM/browser,
cross-target, release, install, registry, digest, signature, and upgrade claims
remain unproven unless source-only obligations are named above.

`actual_jobs`, run IDs, conclusions, release assets, installs, and publication
records are `null`. The expected IDs and platform/provider rows are logical source
inputs for checker/collector work only. Missing workflows, stale generated state,
unmodeled services, invalid marker suppression, and identity mismatches remain
incomplete; none is downgraded to pass.
