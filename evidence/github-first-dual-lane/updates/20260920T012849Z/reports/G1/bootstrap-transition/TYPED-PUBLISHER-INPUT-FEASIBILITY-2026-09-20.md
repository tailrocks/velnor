# Typed publisher input feasibility probe

Status: **measured compatibility and semantic rejection evidence; no approval**.

Observed 2026-09-20. No production checkout, ruleset, ref, release, artifact,
dispatch, merge, Mac runner, Mac Docker, candidate binary, candidate manifest,
or `--pin-build` was used. All writes below were isolated `/private/tmp`
fixtures or this external evidence record.

## Result

One old-parser-safe typed input exists: the existing `[[declare]]` primitive
`package-release`. It renders a source-bound package publisher and old 0dc
accepts it. It is **not** the required validator B contract. It always
includes a consumer updater, mutable rolling-release refresh, and the
renderer/policy revision; it has no typed product ID, purpose, platform, B
closure tag, protected artifact/job binding, or old-policy-to-B check handoff.

The existing `[[static_files]]` field is the only old-schema path that can
transport a source-owned B workflow body without adding `[policy.validator]` or
teaching generic S2 repository-specific metadata. The old generator copies the
bytes and can pass its fixed-point check. It performs no B semantic validation.
No actual B workflow/action/report source exists on current `origin/main`, so
there is no honest old-0dc semantic admission of B under the stated
constraints. The exact next input needed from the owner is the reviewed
source-owned B publisher body plus its protected authority/provenance contract;
this probe does not invent or approve one.

## Revisions and binaries

| Item | Exact value |
| --- | --- |
| Refreshed `origin/main` | `d20d4d1d17590cca85b501d982cbaad70d42c641` |
| Main parent used for policy arguments | `1048337062ea625fada1b4f7c07f2feed75f60c7` |
| Old validator source | `0dc79895ff1c5e88be7c3822c437e1c5b5282e12` |
| Old validator closure | `8b96d5108550dfa61a57ff65c6c357b4119493c660417bf3beb4af2b03742269` |
| Old validator binary | `/private/tmp/velnor-old-target/release/velnor-workflow` |
| Old binary SHA-256 | `ade826fd3a27de43cfce55acfda870493b4921770199fe0afaa653b9a96f1750` |
| New renderer source | `d20d4d1d17590cca85b501d982cbaad70d42c641` |
| New renderer closure | `9179c5421fa9bc4ef1ca424696df211cdbf1ee6fdaea3dcdfbccde943011e0830` |
| New renderer binary | `/private/tmp/velnor-current-generator-target/release/velnor-workflow` |
| New binary SHA-256 | `07e184bb6e6388023a06716147e177a7d212fe00db2c78c97b68a09fa6f888c7` |
| Old `package_release.rs` blob SHA-256 | `9941dc21202ac8b0107eaf4ed92defa600931bba8d02c65d81253325f4a86f0b` |

The new renderer was built outside the repository target tree:

```sh
CARGO_TARGET_DIR=/private/tmp/velnor-current-generator-target \
  cargo build --release --no-default-features --package velnor-workflow
```

## Exact isolated commands

For each fixture, `ROOT` was a detached worktree at `origin/main`; config/source
changes were made only in that fixture. The package fixture omitted
`[generator] revision` to test an unpinned declaration. Old rendering seeded
the fixture's generated `.github` tree; that is why its raw fixed-point result
is meaningful and not a claim about the unmodified current-main tree.

```sh
OLD=/private/tmp/velnor-old-target/release/velnor-workflow
NEW=/private/tmp/velnor-current-generator-target/release/velnor-workflow

"$OLD" --plain --output "$OLD_OUT" "$ROOT"
"$OLD" --plain --check "$ROOT"
"$OLD" policy --workflow-root "$ROOT" \
  --head-sha d20d4d1d17590cca85b501d982cbaad70d42c641 \
  --base-sha 1048337062ea625fada1b4f7c07f2feed75f60c7 \
  --base-revision 0dc79895ff1c5e88be7c3822c437e1c5b5282e12 \
  --ruleset-contexts DCO,Policy,ci-required \
  --candidate-manifest ""

"$NEW" --plain --output "$NEW_OUT_1" "$ROOT"
"$NEW" --plain --output "$NEW_OUT_2" "$ROOT"
```

The raw current-main check was separately run without normalization:

```sh
"$OLD" --plain --check /private/tmp/velnor-current-d20-admission-fixture
"$OLD" policy --workflow-root /private/tmp/velnor-current-d20-admission-fixture \
  --head-sha d20d4d1d17590cca85b501d982cbaad70d42c641 \
  --base-sha 1048337062ea625fada1b4f7c07f2feed75f60c7 \
  --base-revision 0dc79895ff1c5e88be7c3822c437e1c5b5282e12 \
  --ruleset-contexts DCO,Policy,ci-required \
  --candidate-manifest ""
```

No `--force` was used for any result in this record. The raw d20 check exited
1 with `generated files differ: .github/actionlint.yaml,
.github/workflows/ci-runtime-products.yml, .github/ci/.github-actions-generator-state`;
raw policy exited 1 with only `generated-tree` failed (`10/11` other rules
passed). This is kept distinct from the old-rendered fixture fixedpoints.

## Typed `package-release` probe

The isolated config row was:

```toml
[[declare]]
primitive = "package-release"
file = "policy-validator-release.yml"

[declare.args]
build_tasks = ["fmt"]
package_dir = ".velnor-validator-package"
manifest_schema = "velnor.workflow-policy-validator.v1"
source_repository = "tailrocks/velnor"
source_ref = "refs/heads/main"
payloads = ["velnor-workflow-policy-validator-Linux-X64"]
supporting_assets = ["SHA256SUMS", "validator-attestation.json"]
channel = "stable"
release_tag = "velnor-workflow-policy-validator-v1"
github_release_type = "release"
publish_environment = "github-release"
release_title_prefix = "Velnor policy validator"
consumer_repository = "tailrocks/velnor"
consumer_branch = "main"
updater = "scripts/update-validator-pin"
updater_token_secret = "VELNOR_VALIDATOR_UPDATER_TOKEN"
update_commit_message = "chore: adopt policy validator"
concurrency_group = "velnor-policy-validator-publisher"
```

This uses only the old primitive's accepted arguments. The old parser/render
accepted it (`old render exit=0`), generated
`.github/workflows/policy-validator-release.yml`, and the old-rendered
fixedpoint fixture passed:

```text
fixture: /private/tmp/velnor-package-unpinned-fixedpoint-final
fixture digest: 6520a6f3d3b97080ef25c2a6cc24379e31c36f28e9a774eb5654912b036db95a
config SHA-256: a998b9aa2dc85912981ec2a2bd01b6cd2a26cbe4ac4a6492e9d77dea5cc80d21
old --plain --check: 0
old policy, candidate empty: 0
policy result: 11 rules, 0 failed
old workflow SHA-256: 697b5232b455c911f5055147004cd0a75c750b4b7f778d06aebe77b59d3edb58
new render #1: 0
new render #2: 0
new render hash comparison: byte-identical (no diff)
new workflow SHA-256: 6bac4af85f02a9e19e291b018be8436186050ac5706a4aa36177644e4437f7e3
```

The old output's no-revision fallback proves only that the **renderer** uses
its own source revision when `[generator] revision` is absent. Lines 49-52 of
the output contain `rev: 0dc79895...`; the same config rendered by the d20
binary contains `rev: d20d4d1d...`. Thus “unpinned config” is not an
unpinned runtime/product authority.

Adding the required product identity as a free-form row argument is rejected
before render:

```text
error: `[[declare]]` primitive `package-release` does not accept the argument `product_id`; accepted arguments: build_tasks, channel, concurrency_group, consumer_branch, consumer_repository, github_release_type, manifest_schema, package_dir, payloads, publish_environment, release_tag, release_title_prefix, source_ref, source_repository, supporting_assets, update_commit_message, updater, updater_token_secret
```

The generated workflow is not B-semantic:

| Required B fact | Measured package-release behavior | Result |
| --- | --- | --- |
| Product ID/purpose/platform fields | Only `manifest_schema` and free-form asset names; `product_id`, `purpose`, `platform` are not schema keys | reject |
| B tag `...-<closure16>`; immutable/no overwrite | Immutable staging tag is `RELEASE_TAG-$EXPECTED_SOURCE_COMMIT`; it also refreshes mutable `$RELEASE_TAG` and force-moves that tag | reject |
| Separate Linux-X64 publisher | Payload name can contain `Linux-X64`; no typed platform or separate product namespace | insufficient |
| Protected B job graph/binding | Jobs are only `build` and `publish`; no fixed B job key, numeric artifact/job/check binding, custom binding predicate, or `verify-B` | reject |
| Publisher permissions | `publish` has `contents: write`, `attestations: read`, and `pull-requests: write`; it mounts an updater secret | reject for isolated B |
| No consumer mutation | It checks out a consumer, pushes an automation branch, and creates a consumer PR | reject |
| B-backed policy | Build runs `velnor-workflow policy` after setup with renderer/policy revision; it is not the new B verifier | reject |
| Main source | Build checks out `github.sha` and verifies source commit/repository | partial positive only |
| Draft-first release | Draft is created and later published, but the same generic lane also performs rolling mutable publication and consumer handoff | reject |

The current generic runtime publisher is not an alternative. Its source fixes
the product to `velnor-workflow`, `ci-runtime-products.yml`, and exactly three
platforms (`Linux-X64`, `Linux-ARM64`, `macOS-ARM64`) with fixed runners. Reusing
it violates the separate Linux-X64 product/namespace rule and narrows the
application obligation.

## Old-supported source-owned workflow transport

`[[static_files]]` has the exact old schema:

```toml
[[static_files]]
file = ".github/workflows/ci-policy-validator-products.yml"
source = ".github-gen/sources/workflows/ci-policy-validator-products.yml"
```

Its source is repository-relative; its destination must be under `.github/`.
The generator reads UTF-8 source bytes and inserts them verbatim into the
generated output. It does not parse or semantically authorize the workflow.

To test only this transport, the isolated fixture copied the old
`package-release` output bytes into the source path above. This is a benign
fixture input, **not** a proposed B implementation. Measured results:

```text
fixture: /private/tmp/velnor-static-publisher-fixture
fixture digest: e430c39a264d96f47a39d6dd31a3e7f3c6a2c48803c49d0cfbf1ced1a0d441c8
source SHA-256: 697b5232b455c911f5055147004cd0a75c750b4b7f778d06aebe77b59d3edb58
old destination SHA-256: 697b5232b455c911f5055147004cd0a75c750b4b7f778d06aebe77b59d3edb58
old --plain --check: 0
old policy, candidate empty: 0 (11 rules, 0 failed)
new render: 0
new render repeat: 0
new repeated hashes: byte-identical
```

This proves source admission/byte transport only. The transported body still
has `name: Package release`, `build`/`publish` only, the old `0dc` runtime
revision, consumer PR mutation, and mutable rolling release behavior. It is
not a B publisher and must not be relabeled as one.

Current main already has only these static source mappings:

| Target | Source |
| --- | --- |
| `.github/actions/setup-velnor-workflow/action.yml` | `.github-gen/sources/actions/setup-velnor-workflow/action.yml` |
| `.github/actions/report-velnor-ci-outcomes/action.yml` | `.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml` |
| `.github/workflows/AGENTS.md` | `.github-gen/sources/workflows-AGENTS.md` |

There is no current source or mapping for the B publisher, a validator-only
setup action, or a B-specific report action. The exact source-owned targets a
future reviewed B body would need are:

```text
.github/workflows/ci-policy-validator-products.yml
  <- .github-gen/sources/workflows/ci-policy-validator-products.yml
.github/actions/setup-velnor-policy-validator/action.yml
  <- .github-gen/sources/actions/setup-velnor-policy-validator/action.yml
.github/actions/report-velnor-ci-outcomes/action.yml
  <- .github-gen/sources/actions/report-velnor-ci-outcomes/action.yml
```

The last target is the existing generic report path and may only be replaced
by an actually reviewed report source; an alias or compatibility copy is not
an admission. `.github/ci/validator-pin-adoption.json` is not an old typed
semantic input and is explicitly excluded from this source-admission phase by
the current no-compatibility-sidecar constraint. Adoption belongs only after a
trusted B exists and the base policy handoff is live.

## First-main scheduling cycle

Current d20 live evidence records one same-push ordering:

* runtime-products run `35475920678`, CI/main run `35475920826`, and Preview
  run `35475920808` were all created at `2026-09-19T23:21:55Z`;
* runtime jobs began before old Policy: closure at `23:22:03Z`, CI plan at
  `23:22:12Z`, Linux jobs at `23:22:16Z`/`23:22:17Z`, Mac job at `23:22:25Z`;
* the old Policy job `105985041123` started at `23:22:47Z` and entered old
  candidate acquisition at `23:22:56Z`.

The current d20 label is `macos-26` (SPEC requires exact `xcode-27`); the
historical pre-d20 application path used `macos-15`. Either way, the runtime
publisher is not downstream of B. A publisher started by the same main push
cannot prevent the already-scheduled native Mac cell, and a post-push B release
cannot retroactively transfer the old required `Policy` check. Static source
admission fixes bytes only; it does not fix the main Mac ordering or old-policy
handoff.

## Authority-plan preflight assertions

These are executable-plan gates, not claims that mutation occurred:

| ID | Required assertion | Negative case |
| --- | --- | --- |
| `BASE_HEAD_CAS_RACE` | Capture merge-API PR `head.sha`, protected base ref SHA, lease transaction, and expected parent. Immediately before merge require live base ref SHA equals captured base CAS; after merge require returned SHA parent order `(BASE_SHA, PR_HEAD_SHA)`, exact tree, and lease state. | Any base SHA drift, changed PR head/tree, second writer, or merge API response mismatch aborts; no ETag assumption. |
| `APP_INTEGRATION_BOUND` | Require exact App ID, installation, Checks integration ID, repository ID, check-run provider, workflow path/SHA, event, head/base/tree, run ID/attempt/job/check IDs. | Context-name-only `Policy-bootstrap-A/B`, wrong integration, duplicate provider, stale attempt, or unbound status is rejected. |
| `PREMERGE_CHILD_CENSUS` | Before merge require DCO and `ci-required` terminal success plus every exact child run/check attempt in the dependency graph terminal success, non-neutral, non-skipped, and exact PR head/tree. | Aggregate green with skipped/absent child, wrong attempt, queued/cancelled child, or foreign SHA fails. |
| `OWNER_LEASE_FREEZE` | Signed lease names one operator, base SHA, PR head/tree, ruleset hashes, expiry, allowed endpoints, watchdog acknowledgement, and independent recovery actor; freeze bypass actor/writers. | Missing lease/expiry, same actor as builder and recovery, stale lease, or unverified ruleset full-object PUT blocks. |
| `MAIN_NATIVE_CENSUS` | First resulting-main run must show exact application jobs `Linux-X64`, `Linux-ARM64`, `xcode-27` plus publish, each terminal success and exact main SHA/attempt; no old Mac label or skipped cell. | `macos-15`, `macos-26`, missing/empty Mac job, fallback, skipped required job, or app publisher replaced by B fails. |
| `B_RELEASE_CENSUS` | Verify B draft release ID before manifest; exact tag closure16, target main SHA, one Linux-X64 asset, raw/inner/binary digests, run/attempt/job/check/artifact IDs, signer predicate, then `draft=false` and live API equality. | Zero/unknown release ID, mutable latest, overwrite, name/run-only binding, candidate/fork source, app namespace, or missing signed binding fails. |
| `BASE_POLICY_HANDOFF` | Base `ci-policy.yml` must have a direct dependency on live verified B for the exact run before emitting required `Policy`; retain DCO and all CI children. | B merely published after old Policy, wrapper/context shortcut, candidate result, or old checker still authoritative fails. |

No authority actor, freeze/lease, integration ID, B source body, or concrete
protected handoff was supplied to this probe. Therefore the result is **no
approval / source-admission evidence only**.
