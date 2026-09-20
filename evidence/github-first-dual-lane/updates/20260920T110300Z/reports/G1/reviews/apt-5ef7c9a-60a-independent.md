# Independent APT source review: 5ef7c9a / 60a7343

Date: 2026-09-20
Reviewer: Codex
Scope: read-only source review; no publication, install, dispatch, release, or source edits.

## Exact pins

- Source change: 5ef7c9a10cf91446c59181fd8b3e25cc6eacfb91
- Attribution successor reviewed: 60a7343c793626f6f3cad17283e0ff936fbc54df
- Source tree for both: 4b34b9e9381cd6323eb952d293bb02db8931b297
- 5ef parent: bc2b11731fcd625e5dfef398a45ed0761c93a2b2
- 60 parent: 5ef7c9a10cf91446c59181fd8b3e25cc6eacfb91
- Detached review tree: /private/tmp/velnor-apt-60a-review
- Worktree was clean. The source diff from bc2 is limited to:
  - crates/velnor-workflow/src/apt.rs
  - crates/velnor-workflow/src/s2/runtime.rs
  - crates/velnor-workflow/src/s2/primitives/release.rs
  - 80 insertions, 5 deletions.
- Remote branch resolved to 60a7343c793626f6f3cad17283e0ff936fbc54df at review time.

## Verdict

Bounded source fix: PASS.

Publication/install approval: NOT GRANTED.

The fix closes the reviewed schema-2 fetch admission seam. It does not establish a native producer-to-APT authoritative handoff, and it does not prove live publication or deployment safety.

## Source findings

1. Configured source admission is ordered before fetch API and destination directory work.

   In apt.rs:3537-3558, run_fetch_selection now takes expected_source_repository, validates it as an owner/name slug, and requires byte-exact equality with the parsed discovery selection before acquire_provider_release. open_or_create_directory is later at apt.rs:3560. This rejects a mismatched selection before the provider release/API acquisition and before incoming-directory creation.

2. The schema-2 CLI makes source repository mandatory.

   s2/runtime.rs:3861-3868 parses exactly selection, source-repo, and dir; required_option(source-repo) is passed to run_fetch_selection. parse_options rejects unknown options and duplicates at s2/runtime.rs:1332-1352. There is no optional-source or alternate fetch selector in the schema-2 runtime.

3. The schema-2 renderer carries the validated configured identity into fetch.

   s2/primitives/release.rs:4301-4305 emits:
   apt-fetch --selection selection.json --source-repo <configured source> --dir incoming.
   The renderer test at s2/primitives/release.rs:8131-8135 checks the exact configured slug, not merely the presence of a flag.

4. All fetch callsites were traced.

   The only production run_fetch_selection call is s2/runtime.rs:3863. The schema-2 renderer emits the required option. The separate schema-1 renderer still contains its historical apt-fetch text and also emits --source-repo, but schema-1 runtime release() intentionally rejects removed legacy APT commands (runtime.rs:2961-2981, 5929-5948). Dispatch routes schema-2 targets to s2 (s2/dispatch.rs:36-52, 84-99). No schema-2 alias bypass was found.

5. No-network/no-directory regression is real fixture behavior.

   apt.rs:9710-9735 calls run_fetch_selection with a mismatched configured source, a real gh stub, and a fresh destination. The test requires the configured-source error, asserts gh.log does not exist, and asserts the destination does not exist. The stub writes gh.log before handling any endpoint, so this detects an actual provider-command invocation rather than a string-only assertion.

6. Missing CLI source is covered.

   s2/runtime.rs:4795-4810 invokes apt_fetch without --source-repo and requires the --source-repo needs a value failure before selection/destination use.

## Verification

All commands ran in the detached exact tree unless noted.

- Focused mismatch/no-network test: 1 passed.
- Focused missing-source CLI test: 1 passed.
- Generated schema-2 APT renderer test: 2 passed.
- Full apt::tests module: 88 passed, 0 failed, 146.81s.
- cargo check --locked -p velnor-workflow: PASS; 49 crates compiled, 42.14s.
- cargo fmt --all -- --check: PASS.
- actionlint: PASS.
- git diff --check: PASS.
- Strict clippy comparison:
  - exact parent bc2: 13 errors, 1 warning.
  - exact tip 60a7343: 13 errors, 1 warning.
  - diagnostics are identical after the expected line shift; 12 are unrelated existing schema-2 baseline diagnostics.
  - the one APT-file diagnostic is the older 19062e6 signer-format line (parent line 4482, tip line 4486), not introduced by 5ef.
  - Therefore the 5ef/60 source-repo change adds no clippy diagnostic, but strict clippy remains a pre-existing baseline failure.

## Limits / remaining authority gap

The guard is correctly scoped to the fetch handoff: the preceding discovery script is itself allowed to query the provider to produce selection.json, and the generated shell removes workspace paths before discovery. The new check prevents the subsequent asset-acquisition API and incoming-directory open when the selection identity differs; it is not a claim that no network or shell cleanup occurs before discovery output exists.

The source repository passed by the generated workflow is configuration-derived. This review found no native producer attestation/handoff that independently authorizes that configuration value. A generated self-report or a source-repository CLI argument is not native producer authority. Native producer binding remains a publication blocker.

No source files were changed. No release/install/dispatch/live publication was run. Latest Mac/Xcode policy is outside this diff and no regression was observed in the scoped checks.

## Reviewed source hashes

- crates/velnor-workflow/src/apt.rs
  39fc2f7f6ba8925711cd17eefa9120b384ba23a54a22ef97f9161603c1d610d4
- crates/velnor-workflow/src/s2/runtime.rs
  cabade7556460fdba7c7b378f3d5c2557dd308314a2e48c46c24db1d8c58c9a8
- crates/velnor-workflow/src/s2/primitives/release.rs
  c7723eecccbb5c0c504f44dbe1a88d951bd36ec3a524e4d4d5da56ee7ec3f7f0

