# Exact secure raw-store re-review — `3e3aa993c0f0b648a3e07c1d90913f13a14c8a2b`

Reviewer: `/root/g2_distribution_review`  
Effective settings: `gpt-5.6-luna`, reasoning `max`  
Review tree: clean `/private/tmp/g3-raw-store` (`codex/g3-raw-store`)  
Compared with: `8526294628568799f0ebbe5ee444697c1405d053`  
Boundary: read-only exact-source review; no producer, host, Docker, remote, or production-wiring edits.

## Verdict

**Reject for production integration and authoritative capture.** The isolated
macOS module materially closes the 852 namespace/orphan gaps and its hostile
fixtures pass. It is not the production `RawObjectStore`: the module is never
declared/imported by the binary, its exact `RawObject`/`RawObjectRef` fields do
not match the production acquisition types, and the live CLI still selects the
legacy path-based store. Linux compilation/runtime/syscall behavior is also
unavailable here. These are false-green risks, not publication approval.

## Exact verification

- `git rev-parse HEAD`: `3e3aa993c0f0b648a3e07c1d90913f13a14c8a2b`.
- `git status --short --branch`: clean, remote equal.
- `cargo test --locked --all-features --package velnor-tools --test github_raw_store -- --nocapture`: **16 passed**.
- `cargo test --locked --all-features --package velnor-tools -- --nocapture`: **276 passed**.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked --profile test --all-targets --all-features --package velnor-tools -- -D warnings`: passed.
- `git diff --check`: passed.
- Focused named fixtures passed: original-byte provenance, complete/incomplete
  `.txn` recovery, private temporary cleanup, root/`refs` namespace replacement,
  object/sidecar/original hardlinks, same-ID concurrent collision, symlink and
  hardlink replacement race, regular temporary replacement race, and FIFO
  sidecar nonblocking refusal.
- Host: Darwin arm64 (`aarch64-apple-darwin`, Rust 1.98.1). Installed Linux
  targets exist, but `x86_64-linux-gnu-gcc` is absent.
- `cargo check --locked --all-features --package velnor-tools --target
  x86_64-unknown-linux-gnu`: **unavailable**; `openssl-sys` fails before crate
  compilation because `x86_64-linux-gnu-gcc` is missing. No Linux claim is made.

## Improvements over 852

1. `store_unix` hashes `object.original_bytes` and `object.bytes` itself and
   publishes both CAS objects (`src/github_raw_store.rs:129-215`). The focused
   provenance test confirms the original bytes/digest/length are distinct from
   the redacted object and that a forged original object fails verification.
2. `NamespaceAnchor` retains root and child descriptors, records device/inode/
   mode identities, and checks both descriptors and named paths before/after
   operations (`:382-455`). The namespace-replacement fixture rejects a moved
   and recreated `refs` directory and rejects a new store until the original
   namespace is restored.
3. Root `flock` plus a process-local inode registry serializes setup,
   publication, and recovery (`:1258-1351`). The 16-way different-payload,
   same-raw-ID fixture yields one bundle and one sidecar; no extra safe or
   original object remains.
4. Durable `<raw-id>.txn` intent is written before either object. Constructor
   recovery completes a bundle when both objects are present, otherwise drops
   the intent and sweeps unreferenced objects (`:836-940`). Tests cover a
   complete journal, an incomplete journal with the safe object removed, and
   stale private temporary names.
5. Descriptor-relative `O_NOFOLLOW|O_NONBLOCK`, regular/single-link checks, and
   macOS `renameatx_np(RENAME_EXCL)` reject symlink, hardlink, and FIFO entries;
   final object and sidecar bytes are reread through the opened descriptor.

## Remaining findings

### R1 — exact module is incompatible with production acquisition API (P0)

The focused suite defines a different local API in
`tests/github_raw_store.rs:8-45`: `RawObject.original_bytes` and
`RawObjectRef.original_storage_ref`. The production types instead define only
`RawObject.original_sha256`/`original_byte_length` at
`src/github_acquisition.rs:343-355`, and `RawObjectRef` has no
`original_storage_ref` at `:375-392`. The new module references the test-only
fields at `src/github_raw_store.rs:132,148,192` and `:166,225-226,313`.

Therefore the 16 passing tests prove a stand-in contract, not production
compilation or collector behavior. `cargo check` passes only because
`github_raw_store.rs` is not in the binary module graph. This is the primary
false-green path. The production contract must be migrated coherently so exact
pre-redaction bytes reach this store; digest fields alone cannot satisfy this
module's claimed `original_bytes` boundary.

### R2 — production collector still uses legacy path-based CAS (P0)

`src/github_acquisition.rs:29-50` wires `live_transport` and `live_cli`; the
new `github_raw_store.rs` has no production `mod`/import. The live CLI imports
`super::live_transport::RawObjectFileStore` at
`src/github_acquisition/live_cli.rs:8-10` and constructs it at `:78` and
`:121`. That old implementation remains in
`src/github_transport.rs:286-456` and has path-based `create_dir_all`/open/read
semantics, no namespace anchor, no durable transaction journal, and no original
CAS object. Secure-store production wiring, removal of the duplicate, and a
consumer-level test are still required; this review does not approve them.

### R3 — cleanup unlink remains pathname-TOCTOU (P1/P2)

`remove_private_named` and `remove_exact_file` validate an opened descriptor,
then call pathname `unlinkat` after the final `fstat` (`:1061-1121`). A
same-identity local writer can replace the name between that check and unlink,
so the cleanup can delete the replacement. `reconcile_temporary` is weaker:
it first checks a private mode, then reopens the name through
`remove_private_named(..., None)` (`:1123-1136`), where no private-mode check is
performed. The existing regular-file race test exercises publication-time
`Drop`, not this recovery-time gap or the post-check swap. A replacement
fixture must prove attacker-owned entries are never unlinked; otherwise leave
uncertain names for bounded operator reconciliation.

### R4 — recovery leaves hostile/unknown temporary names silently

`reconcile_temporary` removes only private, single-link regular entries with
mode `0400`/`0600`; FIFO, symlink, hardlink, non-private, and unknown names can
remain without a constructor error (`:1123-1137`). Object sweep similarly
ignores non-digest, non-temp names (`:992-1018`). This avoids destructive
cleanup, but it is not a bounded debris guarantee. Add explicit fixture and
policy for FIFO/hardlink/symlink/oversized/unknown temp entries: fail closed or
retain a bounded, typed quarantine record.

### R5 — Linux primitive and recovery behavior unproven (P1 for dual-platform claim)

The source selects Linux `renameat2(RENAME_NOREPLACE)` at
`:1222-1231`, but this host could not compile the crate for Linux because of
the missing cross compiler. No Linux `renameat2` support/error behavior,
directory fsync behavior, inode identity semantics, FIFO behavior, or journal
recovery was observed. Run the same hostile suite on a real Linux runner;
retain explicit unsupported/fail-closed behavior for other Unix targets.

## Acceptance constraints

1. Reconcile the store with the production acquisition API; do not retain a
   test-only `original_bytes`/`original_storage_ref` schema. Bind exact source
   bytes before masking and verify the returned reference through the real
   collector.
2. Wire this module into the live collector, delete/retire the duplicate
   path-based store under the no-legacy rule, and run the live consumer tests.
3. Close or explicitly accept the cleanup pathname race with a safe namespace
   transaction/quarantine policy; cover recovery-time private replacement,
   hardlink, FIFO, symlink, and unknown-name fixtures.
4. Execute publication and recovery on real macOS and Linux. Linux remains
   unproven in this review.
5. Preserve the passing namespace-anchor, concurrent collision, original-byte,
   transaction-recovery, sidecar/object hardlink, FIFO, and replacement-race
   fixtures. Production wiring remains a separate required gate.

No source changes, live capture, host/Docker edits, or publication approval.
