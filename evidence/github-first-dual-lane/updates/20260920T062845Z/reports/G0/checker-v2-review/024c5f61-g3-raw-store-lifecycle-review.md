# Exact raw-store lifecycle review — 024c5f612874c5ffa2dfd1520be4f0aa6edb9cd7

Reviewer: `/root/g2_distribution_review`  
Effective settings: `gpt-5.6-luna`, reasoning `max`  
Review tree: `/private/tmp/g3-raw-store`, branch `codex/g3-raw-store`  
Compared with: `b90a8e7715677bc22447a50409e06c23e3c7ea52`  
Boundary: isolated, read-only bounded raw-store review. No production wiring,
publication/install claim, remote write, Linux claim, physical crash, or
disk-full claim.

## Verdict

**Pass for the scoped bounded lifecycle contract.** The successor removes all
four bounded-store blockers independently found at b90:

1. identity-race output is a typed, validated, quota-accounted rejected
   terminal record and survives restart;
2. fresh materialization and pending recovery share the post-rename
   descriptor/manifest verification;
3. pending pathname is derived exactly from `manifest.record_name`;
4. normal publication and valid transaction recovery stay below the 128 MiB
   retention peak.

This is not approval of physical crash/power-loss or filesystem-full behavior,
Linux execution, collector/production integration, or G2 publication/install.

## Exact verification

- HEAD `024c5f612874c5ffa2dfd1520be4f0aa6edb9cd7`; parent
  `b90a8e7715677bc22447a50409e06c23e3c7ea52`.
- Detached tree was remote-equal, clean, and `git diff --check` passed.
- Focused suite: `rtk cargo test --locked --all-features --package
  velnor-tools --test github_raw_store -- --nocapture`: **34 passed**.
- Full package: `rtk cargo test --locked --all-features --package velnor-tools`:
  **294 passed**.
- `rtk cargo fmt --all -- --check`: passed.
- `rtk cargo clippy --locked --all-features --package velnor-tools
  --all-targets -- -D warnings`: passed.
- Exact focused coverage includes quota-before-move, concurrent orphan
  rejection, complete staged reopen, valid/symlink replacement during pending
  recovery, manifest fault boundaries, fresh materialization replacement,
  retention disk accounting, and source replacement identity races
  (`tests/github_raw_store.rs:1246-2000`).

## Independent hostile checks

Harness `/private/tmp/raw-store-adversarial-0d` imported the exact source by
absolute path. Its **10/10** tests passed after changing only stale b90
expectations to successor invariants:

- two 60 MiB pending quarantines consumed 125,830,494 logical bytes, below
  128 MiB;
- normal publication temporary peak measured 104,869,888 bytes;
- valid transaction recovery peak measured 117,460,992 bytes;
- quota rejection left the orphan and oversized transaction in place (no
  pre-admission move);
- malformed retention and same-content source replacement failed closed while
  preserving the hostile entries;
- generated rejected records reopened successfully;
- a numeric pending alias unrelated to its manifest name failed closed and
  remained inspectable;
- corrupt rejected-manifest content failed before public orphan reclaim.

The first run against the exact commit, before stale assertions were updated,
reported the two peaks above and the expected successor deltas: logical bytes
125,830,494, normal peak 104,869,888, recovery peak 117,460,992; the only
failures were assertions encoding b90 behavior.

## Source contract evidence

- `reconcile_pending_retained_records` and `materialize_retained_record` both
  call `verify_published_retained_record` after no-clobber rename
  (`src/github_raw_store.rs:1385-1486, 1922-2049`). The helper reopens the
  published directory, compares held and published directory identities,
  manifest identity/bytes, and manifest record name.
- `preserve_rejected_retained_record` uses a bounded generated name. The
  parser/read path is explicit (`:1449-1510`, `:2240-2340`); rejected records
  are replay-validated against their source qdir and accounted for both record
  and manifest allocation in `retention_usage` (`:2574-2650`). Duplicate source
  keys and malformed/corrupt content fail closed before public namespace
  reconciliation.
- `read_pending_retained_record` derives `retained_pending_name` from the
  authenticated manifest name and requires byte-for-byte pathname equality
  (`:2312-2329`).

## Residual scope constraints

The six deterministic manifest fault hooks test bounded return paths, not
actual process termination, power loss, filesystem exhaustion, or journal
recovery after a physical crash. Tests ran on the current macOS arm64 host;
Linux build/runtime behavior remains unverified. No production collector,
host/Docker, release, package, feed, install, or publication approval is
implied.

No source changes, remote writes, publication, or gate approval were made.
