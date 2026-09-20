# Exact APT producer discovery review

## Decision

**Reject complete producer/G2 integration approval; retain a narrow source
pass for the discovery helper.** Exact commit
`3b669d871798cfb2bf9aeb12bca4197794b26118` correctly obtains the provider
repository identity from GitHub, requires a positive numeric provider ID and
exact `full_name`, uses paginated release candidates, validates immutable tag
resolution and preview ancestry, and fails closed on the covered hostile
fixtures. The helper is not wired into the producer's generated release
workflow, and it accepts non-canonical SemVer numeric components with leading
zeros. No merge, publication, install, or remote write was performed.

## Exact scope and provenance

- Repository: `tailrocks/velnor-apt`
- Branch: `codex/github-first-apt-discovery`
- Reviewed commit: `3b669d871798cfb2bf9aeb12bca4197794b26118`
- Parent/base product source: `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`
- Worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-apt`
- Worktree status: clean before and after review.
- Read-only remote check: `origin/codex/github-first-apt-discovery` equals the
  reviewed SHA; `origin/main` is `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`.
- Changed paths from base: `mise.toml`,
  `scripts/release-discovery.sh`, and `scripts/test-release-discovery.sh`.
  No workflow, publisher, secret, permission, or generated release file was
  changed by this branch.

Source file hashes at the reviewed commit:

```text
scripts/release-discovery.sh      08e7c2e29512fc0a719be34f1f7c3e1196e51d82c61ce082254ff9e09e705e20
scripts/test-release-discovery.sh 8f0cf077e810d7cc9f66cb6dede8f6b19b71f5f8dd86870a4e415dcdf97e455f
mise.toml                         6e20818094d8be2e4df36b7125eb33da421f54dfb977f83c0ade828b09b0c8dc
.github/ci/project.toml           5a60cafd4717953a60b567079fbdf3cf278290dbef222cbb133aae91de382056
.github-gen/velnor-workflow.toml  b13b4b61e1bf98854eaf1f09df0e982b21f0582633e2c4a245b3f38236793746
.github/workflows/release.yml    b81a568c9c7950e9bfebe3f53b4cef8b401d98c6591c3bb1403dab6da577a340
```

## Narrow helper results

Observed in `scripts/release-discovery.sh`:

- `fetch_repository` calls `repos/$SOURCE_REPOSITORY` before candidate
  inspection. `.id` must be a JSON number greater than zero and integral;
  `.full_name` must equal the selected `owner/repo` exactly. The serialized
  output carries `provider_repository_id` from this API response, not from the
  product manifest (`lines 155-162, 233-240, 685-706`).
- Release listing uses `gh api --paginate` (`lines 147-153`). The aggregate
  rejects any non-array page before selection (`lines 724-729`). The fixture
  has two API pages and selects a valid candidate on page two.
- Candidates are validated after listing, so runtime releases, malformed
  manifests, incomplete APT inventory, bad subordinate records, mismatched
  source/tag/ref data, and duplicate winning versions do not get selected.
- Stable source tags are resolved through GitHub's tag ref API and compared to
  the release commit. Preview source uses an immutable preview tag plus a
  GitHub compare response proving ancestry to `main` (`lines 175-230,
  635-645`). This is source/API evidence only; the helper does not run a
  cryptographic attestation verifier.
- Asset URLs are derived and checked against the exact GitHub release download
  URL. Manifest, release records, all manifest artifact bytes, sidecars, and
  APT `SHA256SUMS` are checked before output.

## Hostile and regression verification

Commands against the exact worktree:

```text
mise run check                         PASS
  actionlint                            PASS
  shellcheck                             PASS
  verify-release checks                 45 PASS
  release-discovery checks              PASS
/bin/bash -n scripts/release-discovery.sh scripts/test-release-discovery.sh PASS
shellcheck scripts/release-discovery.sh scripts/test-release-discovery.sh    PASS
git diff --check base..3b669d8                                               PASS
```

The committed discovery fixture covers pass/fail behavior for stable and
preview selection, page-two pagination, no eligible candidate, old/runtime-only
candidate, preview non-ancestry, API failure, repository API failure, exact
repository mismatch, asset API failure, rolling preview rejection, binary-name
drift, SHA256SUMS tamper, release-manifest census tamper, release-record census
tamper, non-APT artifact digest tamper, missing URL, source-ref mismatch,
ambiguous version, release-ID grammar, and package identity/version fields.

Additional independent runs:

1. Provider numeric type. Running the committed fixture with
   `FAKE_REPOSITORY_ID='"1255367013"'` made the first helper invocation fail
   with `GitHub API repository identity has no positive numeric ID` (exit 1).
   No string-to-number coercion occurred. Captured stderr SHA-256:
   `99067db0833eb68b874fd2bf437927167466ffa47e2c56cde4b844b860dc3224`.

2. Partial paginated API. An external copy of the committed fixture (SHA-256
   `d94f21a73e7d14c4d10c791613be9ce2f9df727fa923710964c68ef48b7cefc8`) emitted
   a complete page then a non-zero API status. The helper failed closed with
   `GitHub API failed while listing releases`; the full fixture exited 0 after
   asserting that failure.

3. Leading-zero SemVer. An external copy of the fixture changed the valid
   candidate to tag/version `v01.2.3`/`01.2.3` and regenerated every dependent
   fixture field (copy SHA-256
   `d3c9c7e2ce06e5d6224212dd6fcfa7b9e8b14592148ad259344aa084f452a50a`). The
   helper accepted it; the full fixture exited 0. This is a real contract
   defect, not a test harness failure.

## Blocking findings

### 1. Discovery is not integrated into the producer release path

The helper is only added to the local `mise` test aggregate. The generated
project config remains `[release]` schema 2 with package/consumer/source fields
but no discovery script, canonical manifest asset, or discovery-output binding
(`.github/ci/project.toml:1-29`). The generator input remains schema 1 and also
has no discovery declaration (`.github-gen/velnor-workflow.toml:1-47`).

The checked-in generated release workflow still performs independent product
selection:

- stable: `gh release list ... --limit 1` and
  `velnor-workflow release apt-resolve-commit` (`.github/workflows/release.yml:83-102`);
- preview: `gh release download preview`, reads `release-manifest.json`, and
  uses `gh release view preview --json targetCommitish` (`lines 103-118`).

It never invokes `scripts/release-discovery.sh`, never persists its selected
`provider_repository_id`/manifest digest snapshot, and never passes that output
to `apt-fetch`, `apt-verify`, or `apt-publish`. Therefore the product-release
selection helper and the actual APT feed publisher are separate contracts.

### 2. SemVer grammar accepts leading-zero identity aliases

Stable and preview manifest/version checks use `[0-9]+`
(`release-discovery.sh:136-141,524-525`), and stable/preview selection converts components with
`tonumber` (`745-763`). `v01.2.3`/`01.2.3` passed the independent hostile
fixture above. Canonical producer/consumer grammar must reject leading-zero
major/minor/patch and preview sequence numbers before selection; otherwise
semantically duplicate versions can receive distinct release identities.

### 3. Provider/API evidence is not an attestation or publication proof

The helper binds release metadata, ref resolution, ancestry, bytes, and
sidecars to GitHub API responses. It does not invoke `gh attestation verify`
or produce a cryptographic source/build attestation. The existing workflow's
attestation step is separate (`release.yml:124-129`) and is not bound to this
helper because the workflow does not call it. No install, feed, package, or
publisher execution was observed here.

## Permissions and publication boundary

No hidden publisher or trust permission was introduced in the reviewed diff.
Existing generated workflow permissions remain separate: verify has
`contents: read`; publish has `contents: write` under its environment
(`release.yml:42-44,148-160`). This review does not approve those existing
publication paths, because they do not consume the helper's immutable
selection output.

## Gate disposition

- Provider repository ID/full-name admission: **source fixture pass**.
- Paginated candidate aggregation and no-selection failure: **source fixture
  pass**, including independent partial-API failure.
- Numeric provider identity: **pass; strings rejected**.
- Leading-zero SemVer identity: **blocker; accepted**.
- Product-release helper source quality: **narrow conditional pass**.
- Generator/config/workflow integration: **missing; blocker**.
- Provider attestation and actual release publication: **unproven/not run**.
- G2 APT delivery, install, upgrade, channel switch, rollback: **none**.

No source or generated workflow files were edited. This report is the only
review artifact created by this bounded audit.
