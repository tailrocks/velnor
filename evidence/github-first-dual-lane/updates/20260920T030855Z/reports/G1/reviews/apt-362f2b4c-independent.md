# APT hardening review: 362f2b4c

Date: 2026-09-20

Scope: exact detached source review. No source edits, release, install,
dispatch, runtime publication, Velnor, MacDocker, or merge.

Snapshot: `/private/tmp/velnor-apt362-review`, detached and clean at
`362f2b4c806059ab5658da40b47b7bca9f781b2a`. Remote verification showed
`origin/dual-lane-apt-schema2` at `c3df8229eaa194738f960a4750830cb1de3953d3`,
with 362 as an ancestor. The owner worktree was dirty/advancing and was not
used. The later owner tip e58f632 adds the scratch hardening described in the
delta report [apt-e58f632-independent.md](apt-e58f632-independent.md).

## Verdict

**BLOCKED.** 362 closes schema-2 incoming snapshot consumption, descriptor-
relative fetch writes/renames, and Rust descriptor-bound archive extraction.
It does not close provider/release/source authority, native producer handoff,
legacy fixed-sentinel byte identity, or predictable scratch materialization.
The generated APT workflow actionlint gate passes at this exact tip.

## Findings

1. **Provider/release/source authority remains self-declared.**

   `parse_discovery_selection` checks positive IDs, canonical-looking URLs,
   state, and shape (`crates/velnor-workflow/src/apt.rs:2250-2426`), while
   `run_fetch_selection` only calls
   `gh api repos/{source}/releases/assets/{id}` and checks size
   (`apt.rs:2615-2715`). No live release object is fetched or compared to the
   selection's release ID/tag/repository/asset metadata. The renderer runs a
   repository-owned discovery script and attests only `.deb` subjects
   (`s2/primitives/release.rs:4289-4307`), not selection/manifest/subordinate
   records or provider metadata.

2. **Legacy verify-to-publish identity is not byte-bound.**

   No-selection verification arms the fixed `verified\\n` marker
   (`apt.rs:1199-1235`). `IncomingSnapshot::capture` accepts that fixed marker
   without hashing legacy record, manifest, sidecar, or candidate bytes
   (`apt.rs:4412-4454,4502-4508`), then stable publication stages captured
   candidate bytes (`apt.rs:5355-5365`). A post-verify valid same-name
   candidate replacement remains publishable in the legacy path. Schema-2
   selection mode does bind selected asset digests and validate the captured
   inventory.

3. **Scratch materialization still has a predictable pathname boundary.**

   362 creates scratch directories relative to a held temp parent, but names
   them predictably from PID plus process-local sequence
   (`apt.rs:2950-2984`) and writes `scratch/input` by pathname
   (`apt.rs:1463-1471`). A same-UID observer can target the known scratch name
   or race replacement before the later external reader opens it. The control
   and archive destinations themselves are descriptor-bound; this finding is
   limited to the materialized input scratch path. e58f632 fixes it with UUID
   roots and a held scratch FD.

4. **Native producer-to-APT handoff remains absent.**

   The checked-in native producer names `tailrocks/velnor-apt` as consumer
   (`.github-gen/velnor-workflow.toml:65-78`), while the native release says it
   does not push or dispatch the APT publisher (`.github/workflows/release.yml:
   4381-4383`). No trusted producer-bound workflow/selection attestation is
   emitted by the APT renderer.

## Fixed / verified at 362

- `IncomingSnapshot` reads incoming entries through one held no-follow
  directory descriptor and publication consumes owned bytes
  (`apt.rs:4407-4484`). Schema-2 selection snapshots validate selected bytes
  and inventory.
- Fetch uses held directory `openat` writes and descriptor-relative `renameat`
  (`apt.rs:1316-1426,2619-2740`), closing the prior parent/rename race.
- Unix data/control extraction validates tar entries and creates every member
  relative to held destination/parent descriptors (`apt.rs:3039-3210,
  3311-3422`); hostile symlink/hardlink parent and destination tests pass.
- Generated APT actionlint and explicit ShellCheck pass; shell extraction also
  passes `bash -n`.

## Tests / checks

```text
apt::tests                                      76 passed, 0 failed
s2::runtime                                     60 passed, 0 failed
s2::config::tests::apt_                           2 passed, 0 failed
release::tests::a_declared_apt                   4 passed, 0 failed
cargo build -p velnor-workflow                  passed
generated APT actionlint + ShellCheck           exit 0
extracted generated run blocks | bash -n        exit 0
git diff-tree --check                            passed
```

The exact 362 APT source hash is
`fd271eb4613093c096bf178a99c12a9bfd9f5355adf3e76bbef18d49b33043a8`; the
generated workflow hash is
`1db28cd921acaec895b02768ea40226404d14a55bc977af556458cf192b6d5f7`.
Clippy remains the known 12 non-APT baseline diagnostics recorded at
`G0/distribution/apt-schema2-followup-4c4b7a7-independent.md:143-148`.

No publication approval is granted.
