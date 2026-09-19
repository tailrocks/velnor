# PR957 exact source review — `25d87b200d09b9c8d04dc7a48b53d196d8beb7ff`

Review timestamp: 2026-09-20. Review tree: detached
`/private/tmp/pr957-review-25d`. Base: `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`.
No source files, owner worktrees, branches, remotes, or merges were changed.

## Disposition

**CHANGES REQUIRED; not ready for merge.** This is source review only. No
G0/G1/G2/G3/G4/G5/G6/G7 gate is approved.

## Findings

### 1. The new rule is immediately violated by the committed generated path

The PR changes only `AGENTS.md`, but the same commit still contains an actual
GitHub-hosted macOS job using the forbidden older major:

- `.github/workflows/ci-runtime-products.yml:104-107` emits
  `runner: macos-15` for the ARM64 runtime-product build.
- The generator source still emits it at
  `crates/velnor-workflow/src/primitives/runtime_products.rs:70-80`
  (`MACOS_ARM64_RUNNER = "macos-15"`), with associated defaults and tests
  retaining `macos-15`.

Regeneration therefore preserves the violation. The open review thread
`4054691696` identifies this same defect and is unresolved/non-outdated. The
smallest safe correction is to update the typed generator mapping, its
associated defaults/tests, and generated workflow together, then prove the
architecture-specific output. Do not land an instruction that the current
generated tree cannot satisfy.

The PR's required `Policy` check is currently failed (run
`35468737767`, job `105965710686`). Its direct log reports generated-tree
drift in four files and no same-repository candidate artifact. That failure is
not itself proof of this macOS finding, but it independently means the PR is
not green. `CI / PR` run `35468737911` passed its required aggregate while
most leaf jobs were skipped; `mergeStateStatus` is `BLOCKED`.

### 2. “Highest stable” narrows the requested latest-offered policy

The accepted policy context says to use the newest actual macOS major offered
for the required architecture (`macos-27` or `macos-26` when available), never
macOS 15 or an older fallback, with an explicit unsupported/blocker result when
the required-architecture label is absent. It does not authorize a global
stable-only filter.

The official current sources distinguish these labels:

- The [runner image matrix](https://github.com/actions/runner-images#available-images)
  currently lists stable `macos-26` (arm64) and `macos-26-intel` (Intel), and
  `xcode-27` as an arm64 public-preview label.
- The [official xcode-27 image README](https://github.com/actions/runner-images/blob/main/images/macos/xcode-27-arm64-Readme.md)
  identifies that preview image as macOS 27.0 with Xcode 27.
- [GitHub-hosted runner documentation](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
  lists `xcode-27` as public preview and does not list a separate `macos-27`
  label at this snapshot.

Therefore the added phrase “highest stable macOS major” would reject an
officially offered latest public-preview major instead of following the
requested latest-greatest preference. Replace it with precise semantics such
as “highest officially offered macOS major/label supported by the required
architecture,” explicitly defining whether the latest public-preview label is
allowed for that workload. If allowed, an arm64 latest lane may select
`xcode-27`; Intel has no corresponding macOS-27/xcode-27 label in this
snapshot and must fail as unsupported rather than substitute arm64 or fall
back to macOS 15. If a workload deliberately requires GA-only tooling, that
must be a typed workload constraint, not a repository-wide stable-only rule.

The added “latest stable versions” dependency sentence should likewise be
aligned with the requested latest-available preference or explicitly scoped to
GA-only dependencies. The immutable action/image/release-asset pin boundary
is correct and must remain: this PR changes no action/image/digest references.

## PR discussion and state audit

All available discussion surfaces were fetched fully:

- issue comments: 1 (Codex summary);
- reviews: 1 (`COMMENTED`, Codex, reviewed commit `25d87b2`);
- review comments: 1;
- GraphQL review threads: 1 thread, 1 comment, unresolved and not outdated,
  with no reply.

PR 957 is open, non-draft, one commit, and its head/base are exact:

```text
head 25d87b200d09b9c8d04dc7a48b53d196d8beb7ff
base 0dc79895ff1c5e88be7c3822c437e1c5b5282e12 (main)
diff  AGENTS.md only (2 insertions)
```

`git diff --check` passes. No source/action/image pin changed. The current
official label evidence was read directly from the linked GitHub-maintained
sources at review time; it is capability evidence, not a fleet or gate claim.

## Required next action

Do not merge PR957 as-is. First decide and record preview-label semantics,
remove the stable-only narrowing if it is not intended, and migrate the
runtime-product generator/defaults/tests/generated workflow off `macos-15`
with explicit architecture labels and no fallback/substitution. Re-run the
policy and complete PR checks from the resulting exact head.

