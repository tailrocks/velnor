# Exact combined checkout/CAS integration review — 81f6362d4b43a94e6524954d4f41b882695f3bec

Reviewer: `/root/g2_distribution_review`  
Effective settings: `gpt-5.6-luna`, reasoning `max`  
Review tree: `/private/tmp/g3-combined2-review-81f6362`, detached at exact
HEAD  
Prior bounded raw-store baseline: `63e8d61fc199acecbfb25819127ff8786bb11347`  
Boundary: read-only offline checkout-proof/CAS integration review. No live
authority, publication, remote write, Linux claim, or production release
approval.

## Verdict

**Pass for the scoped offline adapter and bounded raw-store integration.** The
adapter reads the producer's original-byte namespace through the reviewed
descriptor-relative store, holds the namespace lock for the reader lifetime,
and lets the proof verifier measure digest and length before JSON/archive
parsing. Strict DTOs remain private/typed and reject unknown fields; live
authority remains explicitly unavailable.

The prior `63e8d61` bounded raw-store result is retained separately: its
rejected-record lifecycle, quota, fault, and race contract remains the basis
for this adapter review.

## Exact verification

- HEAD `81f6362d4b43a94e6524954d4f41b882695f3bec`; parent
  `63e8d61fc199acecbfb25819127ff8786bb11347`; detached tree clean and
  `git diff --check` passed.
- Focused raw-store suite: `rtk cargo test --locked --all-features
  --package velnor-tools --test github_raw_store -- --nocapture`: **34
  passed**.
- Full `velnor-tools` package, default parallel test execution: **348 passed**.
- Full package, serial `-- --test-threads=1`: **348 passed**.
- `rtk cargo fmt --all -- --check`: passed.
- `rtk cargo clippy --locked --all-features --package velnor-tools
  --all-targets -- -D warnings`: passed.
- Adapter-specific production-store test
  `reviewed_raw_store_reader_feeds_offline_cas_measurement`: passed. It
  stores distinct safe and original bodies and confirms the adapter returns
  the original body.
- Inherited hostile raw-store harness, exact `81f6362` source by absolute
  path: **10/10** passed, including quota peaks, rejected-state accounting,
  pending-name binding, malformed retention, and source replacement.
- Independent adapter harness: **1/1** passed. It confirmed reader bytes are
  original bytes, a second store open blocks until the reader is dropped, a
  missing-object error releases the lock, safe-body storage refs are not
  accepted as original refs, and noncanonical refs fail closed.
- Repeated race checks: four inherited races, three repetitions each, all
  passed: concurrent different-payload same-ID publication; concurrent
  no-clobber publication; symlink/hardlink replacement verification; and
  concurrent orphan rejection.

## Contract evidence

- `checkout_proof.rs:446-454` implements `ImmutableCasReader` only for the
  producer-owned `RawObjectFileStore`.
- `github_raw_store.rs:333-367` accepts only lowercase 64-hex
  `sha256://` references, acquires the anchored namespace lock, validates the
  namespace, opens the original digest object descriptor-relatively, and
  returns a reader carrying the lock through drop.
- `checkout_proof.rs:524-603` checks the canonical storage URI, bounds the
  read, computes digest and byte length, and only then parses the subject,
  provider job/artifact, and archive. The CAS adapter therefore cannot mint a
  typed proof from caller-owned redacted/safe bytes.
- `checkout_proof.rs:242-305` uses strict `deny_unknown_fields` DTOs plus
  typed decimal/SHA/member-name values. `parse_strict_json` enforces bounded
  input/depth/collection size and complete consumption.
- `g0_binding_producer.rs:59-75,219-223` records checkout proof as
  `unavailable`; it does not turn API `head_sha` into checkout evidence.
- `live_authority.rs:123-143` defaults to `UnavailableCollector`, and
  `evidence_check.rs:932-949` can enter trusted-live mode only through an
  authenticated producer install. No producer is installed in this commit.

## Residual scope

The concrete raw-store adapter test covers original-body measurement, while
the full proof-path test uses the strict in-module memory CAS fixture; there
is no live collector or published checkout artifact in this review. The
reader intentionally serializes same-root store access until it is dropped;
the canonical verifier consumes each reader before opening the next one, and
the independent lifetime/error test verified release. Physical crash/disk-full
durability, Linux execution, API authority, external checkout provenance, and
release/install publication remain unverified and unclaimed.

No source edits, production wiring, live capture, publication, or gate
approval were made.
