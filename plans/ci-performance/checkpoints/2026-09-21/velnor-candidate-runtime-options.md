# Candidate runtime distribution: parent diagnosis

Observed source: Velnor `ce75f7c3` / current integration.

The generated `ci-runtime-products.yml` accepts workflow_dispatch but its first
step rejects every ref except refs/heads/main. It produces Linux-X64, Linux-ARM64
and macOS-ARM64 release products, verifies closure/revision/digests and attests
binaries plus manifest before release publication. The consumer verification
requires this workflow and source-ref refs/heads/main.

The PR generator unit separately publishes a Linux-X64 debug candidate after
checks. Its current artifact is 42,196,133 bytes for 4f70; the pinned release
runtime artifact is 8,416,041 bytes. These are transport observations, not a
controlled compression/build comparison. Jackin also needs a macOS runtime.

Root cause: runtime build/verification is coupled to default-branch release
publication, while development candidate distribution is coupled to one Rust
unit/platform and a PR policy consumer. Typed consumer declarations cannot
currently select a verified multi-platform development candidate.

Alternatives to independently challenge before implementation:

1. Merge reviewed generator first, publish current main products, then migrate
   consumers. This uses existing trusted products but does not meet the explicit
   pre-release candidate requirement; the known prospective-main policy cycle
   must also be repaired before such a merge can be considered valid.
2. Extend the existing PR candidate producer to native platform products with
   typed immutable producer/run/attempt/source/profile/digest contracts. Bind
   consumers to a successful reviewed producer and matching source/configuration;
   preserve absence/expiry failure and exact native platform checks. Avoid
   duplicating generator validation across architecture legs without proof.
3. Separate existing runtime build/verify stages from release publication. A
   trusted branch/manual candidate mode can emit verified artifacts/receipts
   without creating a release. Default-branch publication remains its own gate.
   This reuses the existing platform matrix and provenance verification, but
   needs an explicit candidate trust contract; do not relax production
   source-ref verification to accept arbitrary branch artifacts.

No runtime workflow dispatched, no release created, no trust checks weakened.
This is an implementation dependency/design record, not an external blocker
or completed optimization iteration. Independent review remains required.
