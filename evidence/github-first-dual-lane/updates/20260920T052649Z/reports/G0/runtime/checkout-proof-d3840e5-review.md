# Exact offline checkout-proof review: d3840e5e

Status: **PASS as a bounded offline checkout-only verifier checkpoint;
REJECT for authoritative G0/live integration.**

The exact source adds hostile ZIP census checks to the 09d1d363 offline proof
module. It remains correctly disconnected from the live collector and
authenticated authority seam. The result cannot be treated as a signed
provider assertion, CAS authority, checkout proof for an arbitrary run, or
builder-consumption proof.

## Exact scope

- HEAD: d3840e5e6795a6f3f3228e50a53d768ef1941bd2
- Parent: 09d1d363652b020fe114d1108b0f511f56e3532d
- Earlier design baseline: dual-lane-evidence/G0/runtime/checkout-proof-design.md
- Detached worktree: /private/tmp/g3-combined2
- Remote branch tmp/g3-combined2 resolves to the exact HEAD.
- HEAD changes only checkout_proof.rs; g0_contract and live-authority wiring
  are untouched.
- No source edits, host checkout, dispatch, credentials, live capture,
  installation, or remote writes.

## Verification

- checkout_proof-focused tests: 8 passed, 314 filtered.
- Full package tests, serialized: 322 passed.
- Full package tests, default parallel run: 322 passed in this run.
- cargo fmt --all -- --check: passed.
- cargo clippy --locked --all-features --package velnor-tools --all-targets
  -- -D warnings: passed.
- git diff --check 09d..d384: passed.

The exercised module tests cover a positive checkout-only fixture, strict
unknown/duplicate subject fields, numeric and leading-zero ID rejection,
JSON depth/size limits, traversal rejection, duplicate-name insertion, and
CAS digest tamper rejection. They do not cover the central/local header,
hidden-local-entry, symlink, hardlink/overlap, or decompression-bomb cases
listed below.

## Sound boundaries

- Decimal provider IDs are strings only, reject signs, leading zeroes,
  non-digits, and u64 overflow. SHA-1/SHA-256 values are fixed-length
  lowercase hex. DTOs deny unknown fields.
- Proof JSON is measured before typed parsing, capped at 64 KiB, depth 16,
  object members 64, and array members 128. The strict serde pass rejects
  duplicate fields and trailing bytes. Subject digest and byte length are
  computed from the exact input bytes before parsing.
- Snapshot member names reject absolute paths, empty/dot/dot-dot components,
  backslashes, drive prefixes, NUL, and control bytes.
- The artifact URL is constrained to HTTPS api.github.com, an exact
  repository/artifact ZIP path, and no query or fragment. Provider archive
  digest and length are compared with the exact archive bytes.
- The census checks EOCD bounds, single-disk layout, central-entry count,
  central record bounds, central names, local-header offsets/signatures, and
  exact local-vs-central filename bytes. It rejects duplicate canonical names,
  traversal names, visible ZIP symlinks, per-member/total decoded-size
  declarations above limits, and a missing or directory target.
- VerifiedCheckoutProof fields and status construction are private. The only
  public fixture result is forced to CheckoutOnly; BuiltFrom is not emitted.
  The module is only exposed under github_acquisition::checkout_proof and is
  not called by live_authority, check_paths_live, or the CLI.

## Blocking findings before producer authority

### 1. CAS-before-parse is an optional helper, not an enforced verifier path

verify_checkout_proof_fixture accepts four caller-supplied byte slices and
parses them directly. read_original_from_cas independently checks an exact
sha256:// reference, reads a Vec, applies a caller-supplied max, and hashes
the returned bytes, but the fixture verifier does not require it. There is no
canonical store implementation, original/safe-byte reread, signature,
attestation, provider request ledger, or API pagination reconciliation.

Therefore provider/job/artifact assertions are self-consistency checks over
caller bytes, not external authority. This is safe only because the returned
status is checkout_only and no live adapter consumes it. A future producer
must route every subject/provider/archive byte through the approved CAS,
parse only the measured original bytes, and independently reconcile signer,
repository, run, attempt, job, check-run, and artifact identities.

The CAS helper itself receives an arbitrary ImmutableCasReader and reads the
entire returned Vec before applying max_bytes. It also trusts a caller-chosen
max_bytes. A hostile store can allocate beyond the proof limit before the
check. The producer store must enforce fixed per-object limits and
descriptor-relative/race-safe original-byte reads before any parser runs.

### 2. Central/local ZIP validation is incomplete

validate_zip_central_directory compares central and local names and validates
central offsets/signatures, but does not compare the remaining local/central
header fields (flags, compression method, CRC, compressed size,
uncompressed size, or relevant extra fields). ZipArchive itself uses central
metadata for sizes and reads local metadata mainly to find the data start.
Thus a crafted central/local metadata disagreement can pass this validator.

The validator walks only central-directory entries. It does not walk all
local-file records from the beginning of the archive, require the local
records to form exactly the central set, reject unreferenced hidden local
entries, or reject two central entries sharing/overlapping one local data
range. A hidden local traversal/symlink entry can therefore be outside the
census, and shared offsets are an archive-alias/hardlink-like ambiguity.
Reject hidden local records and overlapping/duplicate local offsets, or use a
strict archive format policy that proves those invariants.

### 3. Declared size caps do not cap actual decompression

census_archive checks file.size() and adds that declared central size to the
total before calling file.read_to_end. The read is unbounded. A malformed or
hostile compressed stream whose declared uncompressed size is under the limit
but whose actual output expands past it can cause memory growth before the
post-read length comparison. The total cap likewise covers declared lengths,
not actual bytes read. Use a bounded reader that stops at
MAX_ARCHIVE_MEMBER_BYTES + 1 and account actual bytes before accepting each
member.

### 4. Symlink coverage is visible-entry-only; hardlink coverage is absent

file.is_symlink rejects Unix-mode symlinks for entries surfaced by
ZipArchive. It does not cover hidden local entries omitted from the central
directory scan. ZIP has no portable hardlink API in this reader; duplicate
local offsets/overlapping compressed ranges are the relevant alias case and
are currently accepted. The source has no executable symlink, hidden-local,
hardlink/overlap, or bomb fixture.

### 5. Provider URL exactness has an authority ambiguity

descriptor_from_provider checks scheme, host_str, path, query, and fragment,
but does not reject URL userinfo or an explicit port. A URL such as
https://attacker@api.github.com/repos/.../zip or an explicit
api.github.com:443 authority can have the same host_str/path while not being
the one canonical API URL. Exact provider binding should reject username,
password, and non-empty port (or compare the complete canonical origin).

## Authority conclusion

The module is a useful fail-closed parser/census checkpoint. It correctly
returns checkout_only and is not wired to live authority, so no current G0
bypass was found. It is not ready to support an authenticated checkout-proof
producer until CAS-before-parse is mandatory, provider/signer/API facts are
independently captured, central/local and hidden-entry invariants are closed,
and actual decompression is bounded. Keep live checkout proof unavailable.
