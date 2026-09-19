# Hosted producer-admission follow-up fixture plan

Prepared against the owner worktree on 2026-09-20. No new hosted commit is
present: the reviewable branch still points at
`3aecc6ed0bf83b1f6dd1d99a1a8a29884f3d710a`; `release.rs` has an uncommitted
working-tree diff. This file is a fixture plan only, not approval evidence.

## Rendered graph fixture

Use the existing typed `bound_spec()` fixture, with producer identity fields
set to `CI`, workflow ID `42`, path `.github/workflows/ci.yml`, success, and
the exact `main`/`push` contract. Render both:

1. native preview with identity metadata, guest payload, Debian matrix, and
   signer enabled;
2. tarball preview with guest payload, archive manifest/checksum, and rolling
   publish enabled.

Parse the rendered YAML (and run `actionlint`) before string assertions. For
every job that checks out, compiles, packages, signs, or publishes the
producer-controlled source, assert all of:

- direct `needs` contains `publish-gate` (not only a transitive identity,
  metadata, or guest dependency);
- job `if` requires `needs.publish-gate.outputs.admitted == 'true'` and the
  applicable publish-mode condition;
- checkout ref is the resolved immutable source output (`needs.source.outputs.sha`
  or the identity artifact whose source was itself gate-bound), never a moving
  branch tip or unbound `github.sha`;
- producer verifier/build/publish jobs use direct source/gate output edges;
- no source-consuming job can run when the gate is skipped/rejected.

Required native jobs: `source`, `publish-gate`, identity, native guest,
metadata, Debian, signer, native unit/build consumers, and publisher.
Required tarball jobs: `source`, `publish-gate`, tarball build, tarball guest,
archive/package, and publisher. Include the full `needs` graph in diagnostics
so a skipped transitive dependency cannot masquerade as a direct admission
edge.

## Provenance negatives

For each native and tarball fixture, mutate one producer field at a time:
repository/full name, repository object ID, head repository/ID, workflow ID,
workflow path, event, branch, ref, run ID, head SHA, run SHA, and source SHA.
The gate must reject each mutation, and no downstream source consumer may be
eligible. Also exercise fork and manual-dispatch events; dispatch is
diagnostic only and must not publish. Confirm source checkout remains pinned to
the admitted producer SHA on the positive workflow-run path.

## Current uncommitted gap to recheck at next exact commit

The owner diff currently adds direct gating only in native guest/metadata
rendering. Identity injection, tarball guest injection, Debian/signing direct
edges, and the full rendered native/tarball graph still require exact-commit
review. The existing 3ae release assertion is stale and must be updated; no
generated outputs are part of this fixture review.
