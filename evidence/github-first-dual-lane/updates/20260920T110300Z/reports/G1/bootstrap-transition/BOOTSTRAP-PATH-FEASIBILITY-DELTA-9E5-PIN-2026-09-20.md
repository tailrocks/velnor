# Bootstrap-path feasibility delta — main `9e5c0eb`, pin `4fa7a3a`

Status: **read-only live refresh; no source, GitHub, release, check, ruleset,
App, dispatch, merge, or publication mutation**.

Observed UTC: `2026-09-20T10:36:01Z`.

## Answer

The stale-`0dc` admission failure is historical. Current main now has a
published immutable generic runtime for pin `4fa7a3a…`, and its live Policy job
passes the old checker. This proves a narrow positive:

**The existing base checker can admit the promoted generated tree without an
App transition when the declared pin is the already-published generic runtime
product.**

It does **not** prove the requested validator transition. The current tree has
no typed Linux-X64 `velnor-workflow-policy-validator` product, namespace,
publisher, consumer, or old-checker-to-B handoff. Current runtime publication
still builds the forbidden three-platform product with `macos-26`, while the
accepted SPEC requires current exact hosted label `xcode-27`. A PR changing the
generator closure still falls to the same-repository candidate path; that path
remains excluded as semantic authority. Therefore:

* existing generic source/generated-tree admission: **measured positive**;
* new native/platform or typed-validator semantic admission: **negative**;
* ordinary PR bootstrap of B without an explicit protected transition: **not
  available**.

No actual pull-request run against base 9e was available in this refresh; the
positive live run is the push/main execution of the promoted tree. Do not
generalize it to an arbitrary PR that changes generator inputs.

## Exact current tree and contract

| Item | Observed value |
| --- | --- |
| `refs/heads/main` | `9e5c0eb215d4169578d6f064806e89fe4c793e85` |
| parent/tree | parent `4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d`; tree `195fe763a91dece44ae2de34e12a613ec9432af6` |
| config | `.github-gen/velnor-workflow.toml`, blob `29d41a410d06a37dcb257b51be223c18fb42e9ee`; `[generator] revision = "4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d"` |
| policy | `.github/workflows/ci-policy.yml`, blob `30331c11bb45d176cea686ba8819794212992a6d`; setup/policy pin `4fa7a3a…`; candidate fallback remains |
| setup action | `.github/actions/setup-velnor-workflow/action.yml`, blob `4e48bc2694af7b3d1a969cb108234a9a92b515d3`; generic `runtime-v1-<closure16>` resolver |
| runtime publisher | `.github/workflows/ci-runtime-products.yml`, blob `c35974071e31f1cbef495776fc5167ab8748908b`; generic three-platform publisher |
| generator source | `crates/velnor-workflow/src/s2/mod.rs`, blob `32238d24572da2a9dffe8f1bf6647d3758b64d05`; revision 54 |
| validator tree search | no current tree path matching `validator` or `policy-validator` |
| validator tag | `refs/tags/velnor-workflow-policy-validator` returned HTTP 404 |
| validator artifact search | no matching Actions artifact |

The config has no `[policy.validator]`. `setup-velnor-workflow` resolves only
the generic runtime release tag and asset names. It has no validator namespace
or typed-product branch.

## Runner acceptance

The accepted [SPEC](/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-records/docs/ci/github-first-dual-lane/SPEC.md)
requires newest actual hosted macOS arm64, currently exact label `xcode-27`,
and explicitly rejects `macos-26`, `macos-26-intel`, older labels, and a
lagging alias. Current main's workflow tree contains no `xcode-27` label. The
runtime publisher explicitly contains:

```text
runner: macos-26
```

Other current workflows use `ubuntu-24.04`, `ubuntu-24.04-arm`, or the
repository-scoped trusted Velnor labels. No current workflow provides the
required typed validator native-runner contract.

## Immutable generic runtime proof

The prior mainline publisher run `35503134829` succeeded from source revision
`4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d`:

| Field | Value |
| --- | --- |
| release/tag | `392384468` / `velnor-workflow-runtime-v1-af140ad4d8d84326` |
| tag target | commit `4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d` |
| closure | `af140ad4d8d84326d5676f626ce4261fd7914ac41d521d851484dcb8253270e7` |
| manifest SHA-256 | `1c3d1b0776d21fb6ba988064a18cbe2752639dcbde3ab58e12e615df72082f88` |
| Linux-X64 asset SHA-256 | `672de6b171eba0f9e8053e96974cf361a6788af22ca2d498eabba71b1d73c236` |
| Linux-ARM64 asset SHA-256 | `1df81f61fc3a654e30bd872077085892c57c74e4bfb156f8cf5dbabe8b25b97b` |
| macOS-ARM64 asset SHA-256 | `7bee5aabdea114e3c7a869101b7bd951619455605addfcb896e69e229e5af49c` |

The manifest declares `revision=4fa7a3a…`, `profile=release`, and only
`Linux-X64`, `Linux-ARM64`, and `macOS-ARM64` products. Downloaded bytes
matched the manifest. `gh attestation verify` succeeded for all four bytes
(manifest plus three binaries) with:

```text
signer: tailrocks/velnor/.github/workflows/ci-runtime-products.yml
source digest: 4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d
source ref: refs/heads/main
predicate: SLSA provenance v1
run: 35503134829
```

The attestation certificate SAN was the runtime publisher on `refs/heads/main`.
Publisher job labels were `ubuntu-24.04`, `ubuntu-24.04-arm`, and
`macos-26`; the latter is the SPEC-forbidden Mac execution. This is valid
generic-product provenance, not B authority.

## Current run status

### Runtime products

Run `35504500569`, head 9e, completed `success` at `10:14:34Z`:

| Job | ID | Result | Meaning |
| --- | --- | --- | --- |
| Resolve runtime closure | `106061909689` | success | resolved existing closure/product |
| Build runtime | `106061932013` | skipped | release already existed |
| Publish runtime products | `106061932161` | skipped | release already existed |

This run did not execute Mac work; the immutable product it reused was
published by the prior run above, which did execute `macos-26`.

### CI / main

Run `35504500738`, head 9e, completed `success` at `10:29:36Z`.
It had 69 jobs: **21 success, 48 skipped, 0 failure**. The successful
required path included Policy `106061912271`, Control / Planning
`106061912424`, all selected GitHub-hosted workload jobs, `ci-required`
`106063742286`, and Control / Required `106063750441`. Velnor-lane jobs and
prepare jobs were skipped by the plan; skipped is not evidence that native
obligations were fulfilled.

The live Policy log proves the generic admission:

```text
pin 4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d shares the base closure and renders the tree; the Stage-0 validator renders
PASS pin-declared
PASS pin-reachable
PASS pin-monotonic
PASS entrypoint-pin
PASS generated-tree
PASS pull-request-target
PASS entrypoint-privileges
PASS trusted-runners
PASS action-pins
PASS workflow-structure
PASS required-checks
policy: 11 rules, 0 failed
```

The same log records `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` empty and
`HEAD_SHA=BASE_SHA=9e5c0eb…`; this is a main push validation, not candidate
semantic authority.

### Preview

Run `35504500719`, head 9e, completed `failure` at `10:32:14Z`:

| Job | ID | Result | Exact cause |
| --- | --- | --- | --- |
| Resolve preview identity | `106061909884` | success | — |
| Compile preview metadata once | `106061958516` | success | — |
| Guest payload aarch64 | `106061958543` | success | — |
| Guest payload x86_64 | `106061958585` | success | — |
| Build arm64 preview deb | `106063528246` | failure | missing `aarch64-linux-gnu-gcc`; `aws-lc-sys`/`openssl-sys` failed |
| Build amd64 preview deb | `106063528303` | failure | `preview-build: refusing to embed identity from a dirty tree (1 changed path(s))` |
| Sign preview deb | `106064093594` | skipped | upstream build failures |
| Replace rolling preview release | `106064093748` | skipped | upstream build failures |

These failures are separate platform/source obligations; they must not be
narrowed away to call the generic Policy pass a complete transition.

## Historical failed log and transition boundary

The immediately prior pre-promotion run `35503134923` on commit 4fa failed
because its checked-in config still declared the historical `0dc` pin:

```text
error: generated files differ: ...
pin 0dc79895ff1c5e88be7c3822c437e1c5b5282e12 shares the base closure but the tree differs from its render; falling through to the candidate path
::error::no candidate product velnor-workflow-candidate-ce870d7c6fbfb4a4-Linux-X64 was published within 15 minutes
required CI prerequisite policy did not pass: failure
```

Main 9e repaired that stale generated-tree admission by promoting the pin and
generated outputs after the 4fa generic product was already published. This
fixes source admission for the existing generic contract only.

For a future PR that changes the generator closure, current `ci-policy.yml`
still computes a candidate name, polls the same-repository `ci-pr.yml` run for
that artifact, verifies the candidate manifest/digest/closure, and executes
the candidate. That remains the excluded candidate-semantic path. There is no
protected pre-main B publisher or old-checker-to-B handoff in the current tree.

## Commands and mutation boundary

Read-only commands used:

```text
rtk gh api repos/tailrocks/velnor/git/ref/heads/main
rtk gh api 'repos/tailrocks/velnor/commits/9e5c0eb215d4169578d6f064806e89fe4c793e85'
rtk gh api 'repos/tailrocks/velnor/contents/.github-gen/velnor-workflow.toml?ref=9e5c0eb215d4169578d6f064806e89fe4c793e85'
rtk gh api 'repos/tailrocks/velnor/contents/.github/workflows/ci-policy.yml?ref=9e5c0eb215d4169578d6f064806e89fe4c793e85'
rtk gh api 'repos/tailrocks/velnor/contents/.github/actions/setup-velnor-workflow/action.yml?ref=9e5c0eb215d4169578d6f064806e89fe4c793e85'
rtk gh api 'repos/tailrocks/velnor/contents/.github/workflows/ci-runtime-products.yml?ref=9e5c0eb215d4169578d6f064806e89fe4c793e85'
rtk gh api 'repos/tailrocks/velnor/git/trees/195fe763a91dece44ae2de34e12a613ec9432af6?recursive=1'
rtk gh api 'repos/tailrocks/velnor/releases?per_page=100' --paginate
rtk gh api 'repos/tailrocks/velnor/git/ref/tags/velnor-workflow-runtime-v1-af140ad4d8d84326'
rtk gh run view 35504500569 --json databaseId,status,conclusion,updatedAt,jobs
rtk gh run view 35504500738 --json databaseId,status,conclusion,updatedAt,jobs
rtk gh run view 35504500719 --json databaseId,status,conclusion,updatedAt,jobs
gh run view 35504500738 --job 106061912271 --log
gh run view 35503134923 --log-failed
gh run view 35504500719 --log-failed
gh attestation verify <each downloaded 4fa release asset> --repo tailrocks/velnor --signer-workflow tailrocks/velnor/.github/workflows/ci-runtime-products.yml --source-digest 4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d --source-ref refs/heads/main --format json
```

No isolated fixture was executed in this live refresh; `fixture_hash` is
`null`. Historical 0dc/e717 evidence remains separate.
