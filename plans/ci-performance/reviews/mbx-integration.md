# MBX directory transport review

Scope: Velnor commit `f8ac97b1` in `/tmp/velnor-ci-integration`, the staged
Mr. Boxington 1.12.0 integration for PR [#967](https://github.com/tailrocks/velnor/pull/967).
Read-only review. No source or generated workflow files were changed here.

## Verdict

**Transport: conditional PASS. Bootstrap/policy: HOLD. Performance: no claim.**

The source change consistently upgrades the hosted Mr. Boxington path to the
directory bundle format, pins the action and release assets, rotates snapshot
compatibility keys, and raises the runner manifest version. The upstream action
implementation and the 1.12.0 release support the claimed restore/import/export
behavior. This is a functional transport fix hypothesis, not a measured speedup.

The staged commit has `GENERATOR_REVISION = "54"` but no staged `.github` output.
Generated consumers must be regenerated and committed with the final generator
revision and runtime product pin. The current working tree was regenerated later
while this review was made; that output is not evidence until it is reviewed as
one coherent generated change. Do not accept a mixed revision-54/revision-55
tree or stale generator-state file.

## What the staged change proves

- `MR_BOXINGTON_VERSION` changes from `1.11.1` to `1.12.0` in both generator
  implementations. Hosted Rust jobs still render `backend: github`,
  `github-cache-mode: objects`, `MBX_GC_MAX_SIZE=12GiB`, and
  `save-on-workflow-dispatch: true`.
- `ActionPin::MrBoxington` changes to
  `jdx/mr-boxington-action@867fc530102eec5b756075d70d850dc8330d2272`
  (v1.4.0). `crates/velnor-runner/src/manifest.rs` admits the same full SHA,
  declares Node 24 and `dist/index.js`, and raises `MANIFEST_VERSION` 14 to 15.
  The admission and manifest tests use the same identity.
- Both Docker mise lockfiles point to the v1.12.0 arm64 and x64 release assets
  with fixed checksums. The Dockerfiles verify `mbx --version` 1.12.0. The
  Docker seed path remains a tar bundle; 1.12.0 keeps tar as a standalone
  format, so this change must not be described as converting Docker seed
  transport to a directory.
- All generated snapshot compatibility prefixes in
  `pre_parameterization_cache_keys.rs` rotate. The new test proves that an MBX
  version change changes the compatibility digest. This creates an intentional
  cold-cache boundary; it prevents an old tar bundle from being restored into a
  directory-form job.

The pinned action source confirms the transport boundary:

- [v1.4.0 action metadata](https://raw.githubusercontent.com/jdx/mr-boxington-action/867fc530102eec5b756075d70d850dc8330d2272/action.yml)
  is a Node 24 action and exposes `github-cache-mode: objects`.
- [Pinned action implementation](https://raw.githubusercontent.com/jdx/mr-boxington-action/867fc530102eec5b756075d70d850dc8330d2272/src/index.ts)
  selects directory export for MBX >= 1.12.0, imports a restored directory in
  place, and includes the bundle form in cache identity. Its post step exports
  the directory form before saving the GitHub cache.
- [MBX v1.12.0 release](https://github.com/jdx/mr-boxington/releases/tag/v1.12.0)
  documents in-place directory import, atomic export, parallel hashing, and
  staging inside the MBX store instead of `$TMPDIR`. It retains tar export for
  portable bundles.
- [Pinned action README](https://raw.githubusercontent.com/jdx/mr-boxington-action/867fc530102eec5b756075d70d850dc8330d2272/README.md)
  states that PR and fork workflows restore only; default-branch and opted-in
  trusted dispatches may save. Hosted object mode defaults `MBX_GC_AUTO=0`, so
  the explicit 12 GiB bound remains the ceiling rather than a guarantee of
  available disk.

These upstream facts support the directory transport mechanism. They do not
prove that every Velnor job fits under 12 GiB or that compiler work is reused.

## Blocking policy/bootstrap defect

Run [35483490582](https://github.com/tailrocks/velnor/actions/runs/35483490582)
completed the all-units CI path. Its separate policy run
[35483489289](https://github.com/tailrocks/velnor/actions/runs/35483489289),
job [106005422506](https://github.com/tailrocks/velnor/actions/runs/35483489289/job/106005422506),
downloaded the candidate artifact and then rejected the generated tree. The
candidate reported closure
`af47d1e21c7775e9598e32cc8d855dd3d8747142b42647782b0f306bd732a3e8`; the policy
runtime was pinned to `325719f1e05d3d46322c9fd3eeb9ad545e175638` and expected its
declared closure. This is a real fail-closed result, not a transport failure.

The enabling architecture is visible in the generated policy and policy
runtime:

1. `ci-policy.yml` exports the downloaded candidate as
   `VELNOR_WORKFLOW_PINNED_BINARY` and exports its manifest separately.
2. `PinnedBinaryLookup::from_env` puts that variable in `pinned_binary`.
3. `regenerate_and_compare` first calls `resolve_pinned_binary` for the
   declared pin. An explicit binary is passed to `prove_candidate`, which
   requires one of the declared pin's closures.
4. Only after that succeeds can `render_with_candidate` use the candidate
   manifest and candidate closure.

Thus a candidate whose closure differs from the declared pin is rejected at the
pin renderer slot. The candidate exception cannot be reached through the same
environment variable. Changing the comparison to accept the candidate as the
declared pin would weaken the policy and would be wrong.

The structural fix is two independent identities: retain a verified base/pin
binary for the declared-pin render, and pass a separately named verified
candidate binary to the candidate-render path. The candidate manifest must bind
the candidate to the audited tree before execution. Add tests that prove:

- a matching pin binary is used for the first render;
- a candidate with a different closure is accepted only by the candidate path
  after digest and manifest checks;
- a candidate with a wrong closure, digest, repository, producer run/attempt,
  profile, or platform is rejected before execution; and
- a mainline tree matching only a candidate still fails until the pin advances.

The current shell manifest checks repository, run, platform, profile, closure,
and binary digest. The Rust candidate parser currently retains only revision,
closure, and binary digest. A pre-plan bootstrap handoff should carry and verify
the complete producer identity (repository, workflow, run, attempt, source
SHA, closure, project/schema digest, platform, profile/features, artifact
identity, and binary digest) rather than relying on an artifact name or latest
run selection.

## Bootstrap feasibility

The existing candidate producer cannot bootstrap a generator/schema change. In
the generated PR workflow, `plan` runs first with the pinned runtime. The
candidate producer is later in the reusable Rust unit workflow, after checkout
and unit execution. A new configuration schema or generator behavior therefore
has to be parsed by the old runtime before the candidate exists. The existing
candidate artifact is useful for post-plan policy comparison once the tree is
already parseable; it is not a pre-plan product.

Two feasible routes preserve the trust boundary:

1. Add a pre-plan bootstrap job for same-repository trusted candidates. It
   checks out the exact merge/head source, builds one debug candidate, emits an
   immutable identity manifest, and makes plan, policy, and consumers depend on
   the verified artifact. The old candidate producer can remain for cases where
   the pinned runtime can already parse the tree.
2. Publish an attested immutable runtime product for the exact final generator
   source closure, then regenerate and pin consumers in a follow-up. This is
   viable after merge. It cannot validate a schema-changing generated tree in
   the same PR without the pre-plan route.

The existing published runtime at `325719f1` proves the old product only. The
candidate's changed Cargo closure is why it cannot be silently substituted. A
policy pass requires the runtime binary, declared source pin, closure, generated
bytes, and artifact manifest to identify the same product. Do not waive closure
checks, compile an unpinned source fallback in CI, or use a `--closure` report
from unverified bytes as identity proof.

## Required validation before acceptance

1. Regenerate every generator-owned output from the final generator revision;
   update `.github/ci/.github-actions-generator-state` and all coupled runtime
   pins atomically. Verify no old MBX action SHA, old MBX version, or old
   compatibility prefix remains in generated hosted jobs. Verify the runner
   manifest version and exact action SHA agree with generated YAML.
2. Run focused generator, manifest/admission, and cache-key tests. Add the
   separate pin/candidate policy fixtures above. Run policy once with a
   matching declared-pin binary and once with a genuinely different candidate
   closure; record both outcomes.
3. Run a cold hosted Rust job after the key rotation, then a warm repeat. Check
   raw logs for directory restore/import/export, no nested tar unpack, no
   `$TMPDIR` exhaustion, no `EDQUOT`, no missing MBX results, and store size
   against the 12 GiB bound. Exercise the largest Rust unit; one small green
   job is insufficient.
4. Exercise trust cases: fork PR restore-only, same-repository PR restore-only,
   trusted default-branch push save, and opted-in trusted dispatch save. Verify
   no policy candidate step receives secrets or a writable token.
5. Compare plan job keys, dependencies, required gates, provider/lane
   expressions, and fork/trust conditions before and after regeneration. Run
   actionlint with the pinned version. A successful all-units run alone does
   not prove policy or generated-tree correctness.
6. For performance, collect separate cold and warm cohorts with raw job/step
   timestamps. Report MBX transfer/import time, compiler reuse, and wall time
   separately. Existing [V-MBX-001](../experiments/V-MBX-001.md) evidence is
   functional transport evidence only; it is not a 10x or any speedup result.

## Historical context

The earlier successful run
[35481387575](https://github.com/tailrocks/velnor/actions/runs/35481387575)
actually exercised MBX 1.12.0 directory import/export, but used the old
published Velnor runtime and did not hand a candidate artifact to privileged
policy. The failed runner observation in V-MBX-001 was a test failure with no
`EDQUOT`, so it is not a quota success cohort. The current f8 policy rejection
now proves the candidate handoff is reaching acquisition but failing at the
pin/candidate identity boundary.

## Parent integration validation

The parent raised both emitter revisions to 55 because main and PR #967 had
independently used 54 for different generated behavior. Regeneration changes
only MBX versions/action identities, compatibility keys and ownership metadata;
job IDs, needs and command selection remain unchanged. Generator library tests:
1774 passed. Repository contract tests: 6 passed. Generator all-target Clippy,
workspace fmt and actionlint 1.7.12 (shellcheck disabled) passed. Runner manifest
tests passed 53/53. Commit f0fb1c012adc2b7e604eaab332785eb5bf780caa is pushed;
a clean detached rebuild reports that exact revision and candidate closure
b510e2700de756686a995968ad999cbfd1b14d170d21e1a18b3e76ed366875e5.
Its generated check passes through the candidate exception. CI run 35484350008
is pending completion; documentation lint found an unescaped table pipe in an
earlier evidence record, repaired separately.

These are local checks of modified source, not proof of an immutable binary:
current build.rs stamps the HEAD closure even when closure source is dirty.
That distinct identity defect is assigned for reproduction and structural repair.
No local dirty binary will be published as an immutable runtime. Final clean
committed source and real CI must validate the integrated product.
