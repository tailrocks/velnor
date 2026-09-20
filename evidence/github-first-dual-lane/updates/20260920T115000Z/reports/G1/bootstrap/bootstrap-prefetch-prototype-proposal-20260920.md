# G1 bootstrap prefetch prototype — successor proposal

Date: 2026-09-20 (UTC/Asia/Ho_Chi_Minh)

This is a new owner proposal. It does not amend the frozen review or either
earlier proposal. Those files remain read-only:

`
bootstrap-image-build-contract-review-20260920.md
  SHA-256 6f5ce0d29e9bcfda5bc20115e9a4ab3ea687645031a8bfc9ddfde463eca277ce
bootstrap-image-build-contract-proposal-20260920.md
  SHA-256 d08a0468066d2bf2331a6417f0c930806b5a384fa4de81a753b36f505c2aadb5
bootstrap-image-build-contract-proposal-v2-readiness-review-20260920.md
  SHA-256 acdd5da1741ad81338c4f908c317256db7505508df569c75cb4f11a6279a6916
`

## Owned executable source

Implementation commit:

`
b24989d33c3cdae7b02c8b6479eb9d920c7591d7
`

`
tools/bootstrap_prefetch.py
  SHA-256 37f599fbedcae522e24f85fdacbc612d97701f23882de6c28a7380cd04593f29
tools/test_bootstrap_prefetch.py
  SHA-256 d9f7ddae8ec11db577eede89c02ba0e998d41315d73d6d722a1cff9f992b3628
`

The source is an executable proof prototype, not an image builder and not
publication code. It makes no Docker, registry, Actions-dispatch, or remote
write call.

`build_bundle` (tools/bootstrap_prefetch.py:514) consumes only the root
workspace manifest, allowlisted member manifests, `Cargo.lock`, and exactly
one toolchain input. It verifies the reviewed source head/tree when the input
is a Git checkout. It rejects path escape, source replacement, unreviewed
registry, floating Git selectors, unknown Git URL/revision, malformed target
paths, and workspace/lock member mismatch.

Candidate Rust, `build.rs`, tests, examples, generated files, workflows,
`.git`, and `.cargo/config*` are never copied. Every declared Cargo target is
rewritten to a base-owned `.prefetch-targets/*.rs` file. The generated files
are trusted empty stubs. The root/member manifest census is closed and the
Cargo lock census is exact.

`validate_bundle` (:751) enforces:

- strict schema keys;
- bare lowercase 64-hex digests;
- JSON numbers for all counts (quoted integers reject);
- canonical `bundle_sha256` preimage formed after removing the
  `bundle_sha256` field;
- exact file byte/size/hash census with no unlisted files or links;
- zero-byte trusted target stubs only;
- 10 workspace member manifests plus one root manifest;
- lock package count, registry checksum census, and the one reviewed Git
  source;
- dependency and toolchain input digests over explicit canonical records.

`git_census` (:1053) requires exactly one Cargo Git DB and one checkout for
the reviewed source set, binds DB/checkout commit and tree, and rejects extra
or stale repositories. The public URL/revision comes from the reviewed lock
source; a Cargo checkout's local `file://` remote is not treated as public
source proof.

`copy_cache_bounded` (:1103) refuses symlinks, special files, hardlinks,
pre-existing destinations, path overflows, file-count overflow, and byte
overflow. It is the only normative cache-copy primitive in this prototype.

`fetch_environment` (:963) builds an explicit no-ambient-credential
environment. It contains no GitHub/cloud/registry token, proxy, netrc, SSH
key, or inherited variable. `network_policy` (:996) records the reviewed
public host set but deliberately reports `enforced: false`.

## Exact current-source proof

The prototype was run against source
`3ed0023b038335d7b22dfa2758457e3808f777ee`, tree
`45be601efb57e8d9da424a07e9115beee93a1564`.

Observed current closure:

`
normal/build package identities: 115
Cargo.lock packages:             438
workspace path packages:         10
registry packages/checksums:      427
reviewed Git source:              termrock
reviewed Git revision:            5283c2acf9154d0cfcd37b1ffe821c00faf90ea2
`

The exact offline measurement used the producer manifest, Linux target,
`--edges normal,build`, `--locked`, `--offline`, and a sanitized
environment. The Cargo output contains one `termrock v0.11.0` line with the reviewed
revision prefix.

The generated source-free bundle contained 28 declared files, including 15
zero-byte trusted target stubs. `cargo metadata --locked --offline --no-deps`
against the generated root exited `0`; no source file or build script existed
in the bundle. A controlled `cargo fetch --locked --offline` against the same
bundle also exited `0` when pointed at the already hydrated local Cargo cache.
That is a local hydrated-cache control only, not a network fetch or builder
image proof.

Focused test command:

`
python3 -m unittest -v tools.test_bootstrap_prefetch
Ran 7 tests ... OK
`

The tests cover the actual 115-node closure, current source bundle, hostile
target path, hostile Git URL, hostile registry, quoted-count/self-hash
tampering, ambient credential rejection, extra Git DB rejection, stale
checkout rejection, symlink rejection, and cache quotas.

## Gates that remain unimplemented

### Host egress

The Python prototype cannot enforce host-level network egress. Its allowlist
is declarative only:

`
registry/index: index.crates.io
registry/archive: static.crates.io
Git:             github.com
`

The final refresh job must run inside a separately reviewed container or
firewall boundary that enforces those hosts, DNS/proxy behavior, and the exact
reviewed `termrock` URL/revision. Until that boundary exists, no networked
prefetch is accepted and no image digest is populated.

### Cargo compiler-probe gate

The current no-build environment intentionally sets `RUSTC=/bin/false` and
`CARGO_BUILD_RUSTC_WRAPPER=/bin/false`. Cargo 1.98 was measured to invoke
`rustc` metadata probes during `cargo fetch` itself ( `-vV` and a
`--print`/`--crate-name ___` target probe). Therefore this environment is a
safe negative gate, not the final runnable fetch recipe: `cargo fetch` exits
before network resolution if it is used unchanged.

Before any networked refresh, a base-owned, digest-pinned
`rustc-probe-gate` must allow only the exact Cargo version/target metadata
probe argument forms, delegate those probes to the pinned compiler, and
reject every compile, build-script, proc-macro, test, or arbitrary source
invocation. The gate itself must be outside the candidate lock-input bundle
and independently reviewed. A temporary local control gate allowing only
those observed probe forms made the source-free offline fetch exit `0`; this
does not prove the network gate or image.

## Disposition

This prototype closes the previously prose-only construction, schema/hash,
workspace/lock/Git census, trusted-stub, and bounded-copy gaps. It does not
approve G1, produce a builder/sandbox image, or justify any digest, manifest,
config, attestation, publication, Mac, or OrbStack operation.

Next required evidence, in order:

1. base-owned probe-gate source/recipe and digest;
2. fresh `CARGO_HOME` network fetch inside the reviewed egress boundary;
3. complete post-fetch Git/registry census and no-secret log;
4. offline image build and independent Linux manifest/config/provenance review;
5. hosted canary after source approval.

Until all five exist, keep builder and sandbox final image digests empty.
