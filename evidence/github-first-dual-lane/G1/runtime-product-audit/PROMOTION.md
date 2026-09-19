# G1 published-runtime audit and staged promotion

Observed `2026-09-19T16:53:31Z` against `tailrocks/velnor` main
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`. No Velnor host was started. No
dispatch, release, cancellation, merge, comment, or other remote write was
performed. No runtime binary was downloaded or executed.

## Current immutable product

The checked-in consumer pin is
`fdeed261bd2247a38db6922a7726cd45d3d6f31e`. Its canonical release closure is
`81ba31f87a699c4e24e68baa1cf7cd0b5d765e5970934ec72c38b83772b274dc` (187 tree
entries under `crates/velnor-workflow`, `Cargo.toml`, `Cargo.lock`, both
toolchain names, and `.cargo`, followed by
`closure-version:1`, empty features, and `profile:release`). The immutable
release is:

- release ID `391347842`, tag
  `velnor-workflow-runtime-v1-81ba31f87a699c4e`; tag ref and target commit are
  exactly `fdeed261bd2247a38db6922a7726cd45d3d6f31e`;
- manifest asset ID `572270515`, 671 bytes, release digest and independently
  retrieved SHA-256 `f62da45ca4559249a222214c8ff2bf2d7f009b367f21ab78b47984d1ee3ee205`;
- `Linux-X64`: asset ID `572270518`, 10,309,808 bytes,
  `7e51258a4c670df88d4fd2984a8cc175206ada24a3c27d75aeba8ecab853b05c`;
- `Linux-ARM64`: asset ID `572270517`, 8,393,640 bytes,
  `46c28001bf4f3ee50bdfc498c0e5aa7b375c3f4dd6ca702f675d959c409d14e4`;
- `macOS-ARM64`: asset ID `572270516`, 8,377,392 bytes,
  `626cd30e77f2161db2e2771bdefe25f61eed4ae8b92d2b5213a44530daa4431a`.

Every manifest product digest equals the corresponding GitHub release asset
digest. The setup action can therefore acquire exactly these three platform
paths. `macOS-X64` and Windows paths have no producer matrix entry or release
asset.

The latest main product is a different object: closure
`63cea86d9b7bf2d5d63b10747f9bd8a1e0c7b378f9d60e0b103374ce630ab590`, release
ID `391412744`, tag `velnor-workflow-runtime-v1-63cea86d9b7bf2d5`, built from
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` by producer run `35338351715`
(run 67, push, success). It cannot satisfy the `fdeed…` consumer: the setup
action compares the full closure before accepting a release.

## Producer contract and observed run

`.github/workflows/ci-runtime-products.yml` is generated from
`crates/velnor-workflow/src/s2/primitives/runtime_products.rs`; the generated
action and its source are byte-identical (SHA-256
`a9d836b3a3aaac30b42ac13caa250efe660062c5f9d9aff1d3cbeb9ee84a77e5`). The
producer is owner-only infrastructure and admits:

1. `push` to `refs/heads/main`;
2. `workflow_dispatch`, immediately rejected unless `github.ref` is exactly
   `refs/heads/main`.

Default permissions are `contents: read`. Build jobs add
`id-token: write` and `attestations: write`, while the publish job adds
`contents: write` plus those two provenance permissions. The build matrix is
fixed in generator source (`ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-15`), not
selected by consumer lane labels.

Admission and exposure gates are ordered as follows:

1. checkout with credentials persisted false;
2. resolve the source closure from `HEAD` and skip when its immutable tag
   already exists;
3. build locked, release, no-default-feature binaries in an isolated Cargo
   home;
4. prove binary `--closure == CLOSURE` and `--revision == HEAD`, attest each
   asset, and upload it;
5. verify transport digests, construct and validate `manifest.json`, attest
   the manifest, smoke-test the exact consumer install/self-report path, then
   recheck the tag and run `gh release create` once.

The observed pin producer run was `35328690937` (run 62, push at
`fdeed261…`, success). Closure, all three native builds, and publish completed
successfully. The latest main producer run 67 also completed all five jobs
successfully. No current producer run is pending; no dispatch is required for
the recovery sequence.

## Candidate `12cc` implications

The reviewed local candidate is
`12cc87b629802c294da9840325cb21087c020df6`, parent current main
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`, on local branch
`codex/github-first-generator`. GitHub currently returns `422 No commit found`
for that SHA; the runtime tag
`velnor-workflow-runtime-v1-1cbf31ad515ae513` is also absent. Its three changed
files are generator source files under `crates/velnor-workflow/src/s2/`, so the
release closure recomputes to
`1cbf31ad515ae51365b58394120da0568e313879015f4deb53ed5cdb83f050c0`, not
`81ba…` or `63cea…`. Its candidate debug (`tui`, `debug`) closure is
`b622c5e7e1087d337ccf5ce5a824855f342548014a7f4afe09ea89d5a90682c4`.

The local source-only candidate has a known generated-output gap: its review
reports exactly `.github/workflows/release.yml` and
`.github/ci/.github-actions-generator-state` as stale until candidate-source
regeneration. Current policy does not accept arbitrary stale output: on a PR,
the candidate exception passes only when the tree is byte-identical to the
candidate renderer; on mainline, a candidate match explicitly fails and says
to bump the pin and regenerate. Thus the first source-admission PR must either
carry the candidate-rendered outputs/state or use the separately reviewed
bootstrap producer that supplies the candidate without a consumer→producer
wait. It must not merely change the pin.

## Acyclic sequence: source admission → runtime publication → pin adoption

Use a source revision variable `S` for the exact source commit admitted to
main. If the merge creates a merge commit, use that resulting main `HEAD` for
the publication check and later pin; do not assume it remains `12cc…`.

### 1. Admit the source candidate while retaining the old pin

The same-repository PR must run the normal events:

- `pull_request` → `ci-pr.yml`; its hosted `rust-velnor-workflow` leg has
  `candidate_publish: true` and publishes a debug candidate artifact named
  `velnor-workflow-candidate-<candidate-closure-prefix>-Linux-X64`;
- `pull_request_target` → `ci-policy.yml`; it obtains the old immutable
  `fdeed…` product, finds the sibling same-repository PR run by `head_sha`,
  downloads the exact candidate artifact, checks profile/platform/repository/
  run ID, recomputes the binary digest before execution, compares the manifest
  closure to `closure --rev=$HEAD_SHA --candidate`, and only then executes the
  candidate tokenless for render comparison;
- DCO and the other required status producers must report the existing
  required contexts. At the observed ruleset these are `DCO`, `ci-required`,
  and `Policy`.

The old runtime is available during this phase because `fdeed…` resolves to
the published `81ba…` release. The source PR must remain same-repository for
the candidate path (`HEAD_REPOSITORY == GITHUB_REPOSITORY`). A fork candidate
is rejected by policy and cannot provide the trusted artifact.

Useful read-only checks for the exact candidate are:

```sh
gh api repos/tailrocks/velnor/commits/$S
gh api repos/tailrocks/velnor/actions/workflows/ci-pr.yml/runs?head_sha=$S\&event=pull_request\&per_page=5
gh api repos/tailrocks/velnor/actions/workflows/ci-policy.yml/runs?head_sha=$S\&event=pull_request_target\&per_page=5
gh api repos/tailrocks/velnor/rulesets/19573071
```

Do not adopt `[generator].revision = S` in this phase. The source PR is
allowed to prove the candidate render while the validator remains on the old
published pin.

### 2. Merge `S`, then let the push producer publish

After the PR's real required checks pass, merge the source candidate to
`main`. The required event is a normal `push` to `refs/heads/main`; it starts
both `ci-main.yml` and `ci-runtime-products.yml`. Do not dispatch the producer
from a feature ref, and do not make it wait for a consumer that already needs
the unpublished product.

The producer computes the closure from the resulting main tree. If no other
closure input changed, the expected tag is
`velnor-workflow-runtime-v1-1cbf31ad515ae513`; otherwise derive a new tag from
the full recomputed closure. The required publication proof is:

```sh
gh run list --repo tailrocks/velnor --workflow ci-runtime-products.yml \
  --branch main --event push --limit 10
gh api repos/tailrocks/velnor/releases/tags/$TAG
gh api repos/tailrocks/velnor/releases/tags/$TAG \
  --jq '.assets[] | {id,name,size,digest,state}'
```

Accept only a completed-success producer run whose `head_sha` is the merged
main SHA, whose release target is that SHA, whose manifest revision is that
SHA, whose full closure matches the computed closure, and whose three asset
digests match the manifest. Attestations must be from
`tailrocks/velnor/.github/workflows/ci-runtime-products.yml` on
`refs/heads/main`; the producer's own smoke test is the pre-release proof.
The first release is therefore available before any consumer pin can request
it. A later push that changes only the pin/config sees the existing closure
tag, skips build/publish, and never overwrites it.

### 3. Adopt the published source pin and regenerate consumers

Only after the release metadata and digest proof pass, create the next
consumer pin commit. Set `[generator].revision` to the exact merged main SHA
`S` (or merge result `M`), then regenerate through the actual generator from an
isolated checkout:

```sh
cd crates/velnor-workflow
mbx run --locked --manifest-path Cargo.toml -- --plain --force ../..
mbx run --locked --manifest-path Cargo.toml -- --plain --check ../..
cd ../..
git diff --check
git status --short
```

The regeneration must update all owned generated workflows and
`.github/ci/.github-actions-generator-state` together; do not hand-edit
`.github`. Validate from a clean checkout at the pin, including shallow
history behavior:

```sh
git clone --no-tags --depth=1 https://github.com/tailrocks/velnor.git /tmp/velnor-pin-check
git -C /tmp/velnor-pin-check fetch --no-tags --depth=1 origin $S
git -C /tmp/velnor-pin-check checkout --detach $S
cd /tmp/velnor-pin-check/crates/velnor-workflow
mbx run --locked --manifest-path Cargo.toml -- --plain --check ../..
```

Open the pin-adoption PR. Its `pull_request` and `pull_request_target` runs
now resolve the newly published immutable tag, and the same three required
contexts must pass. After that PR merges, the next `ci-main` plan installs the
new product through the setup action and publishes its per-run workflow
artifact; no workflow producer waits on that consumer.

## State and blockers

- The old pin is source-reachable in a fresh depth-1 checkout and recomputes
  exactly to `81ba…`; this is a positive bootstrap fact.
- `12cc…` is local-only in the observed remote state, and its immutable
  runtime tag does not exist. It is not safe to pin consumers to it yet.
- The current main product `63cea…` is not a substitute for the old pin.
- The source-only `12cc` tree has the two known generated-output/state gaps;
  source admission must resolve them through candidate regeneration or the
  authorized bootstrap path before merge.
- No code, generated workflow, remote, release, host, or binary state was
  changed by this audit.

See `runtime-product-audit.json` for the machine-readable release, asset,
closure, producer-run, candidate, and shallow-clone records.
