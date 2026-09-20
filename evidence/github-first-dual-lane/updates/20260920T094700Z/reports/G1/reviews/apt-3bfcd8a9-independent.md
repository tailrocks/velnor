# Independent APT provider-identity review

Date: 2026-09-20

## Scope and pins

Read-only review. No source edits, publication, installation, dispatch, release, or
runtime execution. The source review was performed in detached isolated trees:

| pin | parent | tree | isolated tree |
|---|---|---|---|
| `3bfcd8a9f2edf6f8dc71465c5a9350da4b7f6976` | `a34b4ef8cbc60ca0a8753cf8d0b724cfd4d99577` | `f1570a9ef3ac77a6158f82a1b4bad22a9889ed84` | `/private/tmp/velnor-apt-3bf-review` |
| `bc2b11731fcd625e5dfef398a45ed0761c93a2b2` | `3bfcd8a9f2edf6f8dc71465c5a9350da4b7f6976` | `c9e09d600d5e2f5041926c1bb70cf38a7eb581e9` | `/private/tmp/velnor-apt-bc2-review` |

At review time `origin/dual-lane-apt-schema2` resolved to
`bc2b11731fcd625e5dfef398a45ed0761c93a2b2`. The source-file hashes are:

```text
3bf: crates/velnor-workflow/src/apt.rs  0fb383bed74e830e9c68e360243fe7da234f0247745ca7493659a46dbf0ce52d
bc2: crates/velnor-workflow/src/apt.rs  7061f79d2408357f5f2cdc57536f3884978e657711afc186f63da2b92aba4d17
```

## Verdict

**Bounded provider-repository-ID fix: PASS. Full APT publication authority: NOT
APPROVED / BLOCKED.**

The production schema-2 fetch path now requires a positive
`provider_repository_id`, reads the fresh GitHub repository before any selected
asset download, and rejects an ID mismatch. Repository slug, canonical URL,
owner route/type/ID, release, ref, asset census/digests, payload bytes, and
provider attestations are reconciled before the incoming selection handoff.

This proves the relation “selection ID == fresh API ID”; it does not make the
selection producer authoritative. The current source has no native producer to
emit a trusted/signed discovery handoff, and the generated pre-fetch shell does
not compare `.source_repository` with the configured source. A selection naming a
different valid repository and its matching fresh numeric ID can therefore be
fetched before the later configured-source/attestation gates reject it. This is
not a demonstrated publish bypass, but it fails a strict source-authority-before-
acquire/download boundary.

## Identity binding evidence

- `crates/velnor-workflow/src/apt.rs:1792-1808` adds
  `DiscoverySelection.provider_repository_id`.
- `apt.rs:2273-2302,2391-2400,2532-2554` exact-parses the field as a positive
  integer, requires it in the exact top-level key set, and keeps it equal to the
  provider release ID's independently parsed release identity where applicable.
- `apt.rs:3141-3185` performs `repos/{source_repository}` first, compares its
  numeric `id` at `3147-3152`, then checks canonical `full_name`/`html_url` and
  owner routing. `apt.rs:3082-3134` binds the embedded owner to the typed
  `/users/` or `/orgs/` response, including ID, login, type, API URL, and HTML
  URL.
- `apt.rs:3187-3241` uses the selected repository and immutable provider release
  ID for release/ref/assets API reads and checks release identity, publication
  state, paginated asset census/digests, and source-ref commit before returning
  provider facts. `validate_provider_facts` also repeats the repository-ID
  equality at `2925-2964`.
- `apt.rs:3537-3547` calls this reconciliation before creating the incoming
  directory or downloading any selected asset. Asset requests at
  `3605-3620` use immutable provider asset IDs. Payload and attestation checks
  at `3648-3650` precede persistence of `incoming/discovery.json` at
  `3651-3678`.
- `s2/runtime.rs:3861-3873` has the only schema-2 production `apt-fetch` caller;
  it passes only selection and destination, not an independently trusted source
  or repository ID. `s2/runtime.rs:3876-3918` checks configured source only in
  the later `apt-verify` boundary. Publish re-verifies the incoming handoff at
  `3922-3995`.

## Authority and producer residuals

The numeric field still arrives in the external discovery JSON. Parsing a
positive number cannot create authority. In production, the only fresh authority
shown by this pin is the provider API read in `acquire_provider_release`; no
checked-in producer emits a signed/native discovery selection or binds the
selection to a trusted producer identity. The commit itself describes the native
release-attestation/live cryptographic canary as external.

The schema-2 renderer passes the configured source to the external discovery
script at `crates/velnor-workflow/src/s2/primitives/release.rs:4289-4297`, but its
pre-fetch `jq` gate at `4298-4301` checks only channel, manifest schema, and
non-empty assets. It does not check `.source_repository == configured source` or
that the producer-supplied repository ID came from an independently authenticated
provider response. `apt-fetch` then runs at `4301-4302`.

The current checked-in configs declare `release.kind = "native"`; neither
`.github-gen/velnor-workflow.toml` nor `.github/ci/project.toml` contains an APT
discovery producer, `provider_repository_id`, or release-attestation producer.
The native-assembly APT test (`apt.rs:9414-9586`) is a synthetic fixture parser
compatibility test; it is not a live native producer handoff or publication proof.

## Caller/migration audit

Schema-2 runtime callers are selection-bound: `apt-fetch`, `apt-verify`, and
`apt-publish` all consume `DiscoverySelection`, and no schema-2 tag/pointer
selector alias reaches `run_fetch_selection`. The test-only direct callers use
the same function.

Global migration is not complete if the repository's “no legacy code” rule is
applied to the whole crate. `crates/velnor-workflow/src/lib.rs:5691-5699` and
`s2/dispatch.rs:1-11,36-58` explicitly preserve a schema-1 fallback; the old
`AptContract::resolve` remains at `apt.rs:611-692`, alongside the schema-2
adapter at `700-730`, and the old APT renderer remains in
`crates/velnor-workflow/src/primitives/release.rs`. This is not a schema-2
provider-ID bypass, but it is a migration residual and prevents a claim that all
APT callers/aliases have been removed.

## Tests and checks

Executed in `/private/tmp/velnor-apt-bc2-review`:

```text
rtk cargo test --locked -p velnor-workflow --lib apt::tests -- --test-threads=1
  cargo test: 87 passed, 1672 filtered out (1 suite, 88.08s)

rtk cargo test --locked -p velnor-workflow --lib \
  's2::primitives::release::tests::a_declared_apt_' -- --test-threads=1
  cargo test: 2 passed, 1757 filtered out (1 suite, 0.22s)

rtk cargo check --locked -p velnor-workflow
  Finished dev profile; 109 crates compiled; 20.04s

rtk cargo fmt --all -- --check
  pass

actionlint -config-file .github/actionlint.yaml .github/workflows/*.yml
  pass (configured checked-in workflows; current checked-in release is native)
```

The 87 APT tests include the real captured `tailrocks/velnor` organization
routes, real captured `octocat/Hello-World` user route, wrong repository ID,
wrong source, owner ID/URL/type, release/tag/ref, asset URL/API URL/digest,
pagination/duplicates, status/timestamp, attestation identity, payload digest,
invalid JSON, and hostile incoming/archive cases. The exact test is
`provider_snapshot_rejects_adversarial_release_facts` (`apt.rs:9131-9353`), with
the owner fixtures at `9355-9410` and the immutable asset-ID fetch/verification
test at `9601-9635`.

## Successor `bc2` delta

`bc2b11731fcd625e5dfef398a45ed0761c93a2b2` changes tests only (parent is exact
`3bfcd8a9...`). It adds `captured_provider_repository_id()` at
`apt.rs:8187-8211`, deriving the fixture selection ID from captured repository
bytes after asserting `full_name == tailrocks/velnor`, and replaces three
hard-coded fixture IDs at `8678`, `8948`, and `9540`. It does not alter
production authority or fetch behavior. The full 87-test suite and focused
renderer/check/fmt/actionlint runs above pass on this successor.

## Required follow-up before publication approval

1. Add a trusted producer/native handoff that derives and authenticates
   `provider_repository_id` (and source/release/asset authority), then wire it to
   the generated APT consumer. Do not treat the JSON numeric field or synthetic
   native-assembly fixture as authority.
2. Bind configured source to the selection before `apt-fetch`/provider acquire
   (renderer gate and/or an explicit expected-source argument), so a valid
   alternate repository cannot be fetched before rejection.
3. Remove or complete the schema-1 APT renderer/runtime migration if “no legacy
   code / no aliases” is a gate criterion.

This review is source-only and is not approval to publish, install, dispatch, or
merge.
