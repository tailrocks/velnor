# Delivered source history and evidence

The current target implementation is preserved. This session's full Git
history through e90b353, including all source edits and the 1,346-file
collected-evidence archive, is embedded in work.bundle.part-*.

Recover from this repository (its existing history satisfies prerequisites):

```sh
cat plans/ci-performance/delivery-2026-09-21-root/work.bundle.part-* > /tmp/velnor-work.bundle
git bundle verify /tmp/velnor-work.bundle
git fetch /tmp/velnor-work.bundle HEAD
git show FETCH_HEAD:plans/ci-performance/handoff/README.md
```

Verify the concatenated bundle SHA256 using manifest.json. The fetched
commit contains editable source and evidence parts, not only a textual patch.
Use a detached worktree or deliberate merge to inspect it without replacing
the current target tree. Both histories are preserved; conflicting WIP is
not silently declared integrated. This session is stopped by user request.

---

# Final delivery and agent handoff

Work stopped at the user's request after committing and pushing to
`refactor/holla-parity`. This is a recoverable WIP delivery, not evidence that
the original CI reliability/performance acceptance criteria passed.

## Source state

- Velnor integration starts from 9bbf4a4e and includes the pending
  concurrency-name, Windows environment-name and policy-identity edits.
  Those final edits were interrupted by the agent service usage limit and
  are not fully verified. The typed Rust phase integration still does not
  compile; its incomplete model remains in the source tree.
- Jackin 251cbd63 refreshes the exact published 4fa7a3a8 generator scan state.
  Exact pinned regeneration check passed. The 15 desktop task tests passed
  with Rust 1.97.1. Earlier policy failure 35528422285 was solely stale scan
  identity; successor policy/CI results were not yet verified.
- All prior concurrent remote work is preserved through normal merges.
  No merge to main or acceptance claim is made by this delivery.

## Decisions and continuation dependencies

1. Replace the incomplete Rust phase label + arbitrary shell model with the
   preserved CargoRecipe/CheckCommand structured union. The alternate patch
   is in observations/checkpoint-20260920/typed-stages.patch.gz and the full
   evidence archive. Its SHA256 is
   ad24ec93a533658d895cec3f279e0cddd091b0ff15e32ba8c6e09d44bf959e25; base ec1bccf2.
   A materialized local comparison is ../velnor-stages-comparison. It passed
   all-targets cargo check; 1,835 tests passed and six failed: four release
   snapshots, generated-file drift, and a real-Cargo fixture failing to find
   cargo fmt. Do not copy it wholesale over newer bootstrap fixes.
2. Preserve formatting → Clippy → compile for selected prerequisites, and
   formatting → Clippy → tests → applicable doctests for full units. Add
   explicit feature/target/profile policy and bind runtime indexed execution
   to the exact admitted command identity. Remove obsolete duplicate models.
3. Independent closure review passed 32 tests but found Windows environment
   case folding could bypass reserved-name checks. The current partial fix
   needs case-variant regression proof, including task-reference env.
4. Concurrency fixes namespace PR/run numbers and hash exact workflow names
   to prevent GitHub case-insensitive collisions. Final isolated tests and
   independent re-review did not complete before the usage limit.
5. Hook review found installer rejection of existing hooks/hooksPath rather
   than preserving useful hooks. Canonical hook/CI phase correspondence,
   staged-byte isolation, platform and GUI proof remain incomplete.
6. Runtime publication must preserve the triggering source SHA in provenance.
   A workflow_run trigger can attest downstream default-branch SHA. Retain
   push identity, gate privileged attestation/publication on exact successful
   main CI, and keep native builds read-only. This gate is not implemented.
7. MBX metadata/guest/test writers collide on an immutable snapshot key.
   Separate typed workload writer identity from compatible CAS reuse. Cache
   helper implementation was not written before interruption.
8. Package preflight is an archived draft; required unprivileged validation
   graph and aggregate/native verification remain incomplete.
9. Diagnostics partial-success failure still lacks demonstrated root cause:
   server saw seven log RPCs, one exporter call and trace/metric count one.
   SDK retry timing cannot explain seven calls in 50ms. Instrument exact
   payloads/transport and test isolation; do not weaken the assertion.
10. Full retained inventory/disposition, strict merge-blocking proof, complete
    120-second pipeline scenarios, protected merges, runtime adoption and
    live main verification remain open. No six-nines claim is supported.

## Evidence recovery

Concatenate evidence.tar.gz.part-* in filename order, verify the concatenated
SHA256 against evidence-manifest.json, then extract the gzip tar archive.
The archive contains collected raw pages, logs, research, scripts and test
results under ci-evidence/. Downloaded executable runtime binaries and build
caches are excluded; their identities and attestation evidence are included.
The manifest records each included file hash. Collection coverage is partial;
use the archived collector checkpoint as the authoritative resume boundary.

All child agents terminated on the service usage limit. No live task process
was found during the final handoff check. Do not automatically restart work
in this session: the user explicitly requested delivery and stopping.
