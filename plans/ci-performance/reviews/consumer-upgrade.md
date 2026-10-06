# Consumer runtime upgrade review

Reviewed Parallax commit `0da45dafabc7a46cf1a5c1ff461e2d193d115cb1` against
parent `6a12bf47a816b63e848b563aaa45ef9694159c79`, read-only. The target
product is the attested Velnor runtime revision
`e94b48406c4ed206fce2bbf39b788264e72cf39c`; its closure is
`8f88f2905f853bd9c86b84c1ca308758010270fe5c01a77b2a4eb729824e4999` and its
observed binary SHA-256 is
`c11066244c0e9442fc51ac73f7bb580d74b338eef1059e84e20781e0c545b1e8`.

## Verdict

**HOLD for merge; conditional structural PASS.** The commit preserves the
declared source contract: `.github/ci/project.toml` is byte-identical to its
parent (`0fa68f880b4d206a938ba7ed04c987638835430d9de4aa6a77cb38e96f1385b1`),
and `.github-gen/velnor-workflow.toml` remains schema 1 while the generated
runtime project remains schema 2. The workflow job-key sets are unchanged:
`ci-main` 52, `ci-pr` 51, `ci-policy` 1, each unit template 2, and maintenance
2. The generated policy still declares DCO and `ci-required`.

The commit is not a pin-only change. It contains 1,381 additions and 633
deletions across nine generated files. Forty-seven common job contracts change
in both `ci-main` and `ci-pr`; unit `if`/`with` admission, dependency and
provider expressions change throughout. It also changes the policy bootstrap
from source compilation to the published runtime, adds closure/manifest
checks, adds candidate/dependency/admission inputs and summary output, bumps
Mr. Boxington, and changes the Rust download retry command. These are upstream
accumulated behavior changes that require real CI evidence before acceptance.

`actionlint` is unavailable in the review environment. The parent reports the
e94 exact check passed with `VELNOR_WORKFLOW_PINNED_BINARY` set to the same
attested product. That proves parsing/check behavior for the candidate tree;
it does not prove the new policy path or provider graph on Actions. The
candidate PR run `35481807065` and base-owned policy run `35481806980` do not
prove the new policy together; the attempted workflow dispatch failed with
GitHub CLI HTTP 401 (`Requires authentication`). No speedup claim is made.

## Required real-CI gates

- Run the e94 candidate policy and an e94 consumer run. Verify closure,
  manifest, binary digest, revision, and no source-build/manual-copy fallback.
- Compare parent and candidate plan outputs: every unit, dependency edge,
  provider/lane admission, matrix entry, required gate, trust/fork condition,
  and failure propagation must be accounted for. Equal job-key counts alone
  do not prove equivalent `if`/`with` expressions.
- Exercise GitHub and Velnor lanes, fork pull requests, push/schedule, and
  dispatch paths. Confirm `ci-required`, DCO, policy, and control jobs remain
  required and fail closed.
- Exercise the Rust, Bun, Docker, and maintenance templates, including the
  changed dependency/admission inputs and Rust download retry path. Confirm
  release workflows and declared package/signing obligations remain unchanged
  because the source config is unchanged.
- Re-run generated ownership/check validation after any integration with
  overlapping #109/#110 work. Preserve those changes; do not overwrite them
  with a stale regeneration.

The immutable published-product path is the correct integration direction.
Consumer source installation, copied binaries, and source-build fallbacks are
not acceptable substitutes for the attested runtime product.

## Jackin follow-up

Reviewed `95b437e735aafea5fe9b2e638c122345c5d141c3` against
`41796158b1e45535ae4e74d5ff048cb5bb4e0488`. The source project config is
byte-identical (`.github/ci/project.toml` SHA-256
`05b13db49101b212c5b06bbb2de57937db29eb48e6d187da84cd03e5cbcda12f`). The
change is 11 generated paths and exactly 54 additions/54 deletions. It updates
the generator pin and all coupled runtime/action references to e94; the only
non-pin content change is removal of trailing whitespace in `renovate.yml`.
Job-key counts are unchanged: main 46, PR 45, policy 3, Bun 2, Docker 2,
Rust 2, Swift 2, maintenance 5, and Renovate 3. `git diff --check` is clean.
Pinned `actionlint@1.7.12` passed with `MISE_NO_CONFIG=1`.

This is a **structural PASS, real-CI pending**. The unchanged source config and
pin-only generated diff preserve the declared obligations, providers, gates,
matrices, release declarations, and trust inputs. CI must still exercise the
published artifact on GitHub, including policy, Swift/macOS, fork/dispatch
conditions, and release/maintenance paths.

## Velnor baseline convergence follow-up

Reviewed `/tmp/velnor-ci-integration` commit
`d63cc3cfe9f9c0ef43e625ec2464740984c34504` against parent
`3844f53c0c2c53fe3ce6a388a6163b0ad7b76356`. The change is 13 generated paths
and exactly 95 additions/95 deletions. `.github/ci/project.toml` is unchanged
(SHA-256 `64023c903452dffb00503c5a3859a9da9226c2755584cbe1218157e8c5e0d170`),
`.github-gen/velnor-workflow.toml` remains schema 2 with the e94 revision, and
the runtime project remains schema 3. Every workflow job-key set is unchanged:
ci-main 39, ci-policy 1, ci-pr 38, package signer 1, runtime products 3, Bun
2, Docker 2, docs 2, OpenTofu 2, Rust 3, maintenance 2, nightly 3, preview 6,
and release 45. The workflow diff is limited to the e94 revision, BASE/PINNED
revision values, runtime artifact names, and matching policy environment
values. No `pr_stages`/`full_stages` or partial stage implementation is
present. `git diff --check` and pinned `actionlint@1.7.12` pass; the exact
published-binary `--check` with `VELNOR_WORKFLOW_PINNED_BINARY` also passed.

This is a **structural PASS, real-CI pending**. It proves convergence is a
consumer pin migration with preserved task graph, not acceptance of a new
stage schema or a performance result. A real GitHub run must still prove
artifact acquisition/attestation, policy and required gates, provider/trust
conditions, release paths, and failure behavior.
