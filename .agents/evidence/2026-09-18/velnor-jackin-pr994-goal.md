Implement the generic CI/CD capabilities needed to replace Jackin PR #994 with Velnor-generated workflows, migrate Jackin to those capabilities, and retire the obsolete override implementation once its replacement is working.

Primary implementation repository:
https://github.com/tailrocks/velnor

Consumer and extraction source:
https://github.com/jackin-project/jackin/pull/994

Regression context:
https://github.com/jackin-project/jackin/pull/992

This is an implementation and migration task. Continue through research, design, implementation, independent verification, Jackin adoption, and cleanup. Do not stop after producing a plan or a list of recommendations.

The goal is to recover every valid, reusable CI/CD behavior represented by this PR through good generic Velnor abstractions. The PR is evidence of requirements, not an approved implementation. Do not copy its workflows, composite actions, shell scripts, JavaScript runtime, or repository-specific assumptions into Velnor.

1. Establish the current evidence and complete scope.

Read the applicable AGENTS.md instructions in both repositories. Resolve the latest Velnor main, Jackin main, and PR #994 head/base to immutable SHAs. Inspect current PR state, all changed files, discussions, check results and relevant failing logs. Refresh this evidence when integrating; other agents may be advancing either repository.

Research starting points, to revalidate:
- PR #994 was open at head ab5b0c4e7517d5b7b96d26d4e96fe6ff623c54bc, base 92f347ac39fbf0d6f9853168e2896a6c60522924.
- It restored 27 static-file mappings across 63 changed files, adding 13,408 lines, and pinned Velnor to 541d8926737213542156ee32e420010817f8a323.
- PR #992 removed the custom workflows and sources. Its regeneration checks did not prove preservation of the removed behavior. Inspect the preceding working configuration as well as the current baseline.
- The PR's CI run succeeded, but policy run https://github.com/jackin-project/jackin/actions/runs/35114917830 failed generated-tree verification. Logs identified drift in generator ownership state, ci-main.yml, ci-policy.yml and ci-unit-rust.yml.
- The copied Swift workflow used macos-26 while retaining runtime/policy references to e05aee6de1d1614d752b4d1b1d26a49ac2c5ef91, inconsistent with the main generator pin.
- Velnor main was inspected at 33688938297eee3933997dbbf368ee20fb1779d4. This is research context; start implementation from the current main and pin the resulting verified revision for consumers.

Read every unique source workflow/action and follow its actual dependencies: invoked scripts, mise tasks, Rust xtask functions, manifests, lockfiles, Dockerfiles, build files, verification contracts, downstream artifact consumers and generated runtime configuration. Do not infer behavior solely from filenames, comments, generated headers or PR descriptions.

Audit these surfaces explicitly:
- .github-gen/velnor-workflow.toml and .github-gen/sources/**
- .github/workflows/**, .github/actions/** and .github/ci/**
- Swift CI; construct; desktop-cadence; docs; hygiene; jackin-dev; preview; release; Renovate and its validation; REUSE; cache-cleanup.
- aggregate-needs; Cargo registry preparation; download-ci-xtask and its runtime; download-codebook; release archives; signing, SBOMs, attestations and capsule manifests.
- Related scripts/ci/**, mise tasks and jackin-xtask functionality, including relevant behavior not newly changed in #994.

Count source copies and generated copies once as behavior, while tracking both for removal. Compare base/head/history before attributing a flaw to this PR. A workflow's absence from current main is not proof that its functionality is obsolete.

2. Use parallel agents for analysis, implementation and independent verification.

Use all useful available parallelism. Start independent workstreams for:
- Velnor architecture, existing capabilities and schema.
- Jackin behavior inventory and dependency tracing.
- Platform routing, Swift/FFI prerequisites and provider selection.
- Caching, prepared tools, artifacts and successful-result reuse.
- Docker, release/preview, signing and publication.
- Documentation, scheduled checks, maintenance and policy.
- Independent design criticism, regression verification and migration review.

Combine workstreams when capacity is limited. Give each a bounded deliverable, explicit file ownership and acceptance evidence. Use isolated worktrees where appropriate. Avoid simultaneous edits to shared schema/IR files without coordination. Keep one integration owner.

Have an independent agent challenge each proposed abstraction before its implementation: is it genuinely generic, is it already supported, does it preserve necessary behavior, and does it remove the underlying bug class? Once a slice is resolved, implement it while other audits continue. The implementing agent must not be the sole verifier.

3. Build a behavior-to-capability ledger and reuse Velnor's existing architecture.

Create one version-controlled inventory with:
source location and observed behavior; intended requirement; callers/inputs/outputs; event/platform/trust constraints; current Velnor support with code evidence; proposed change; repository-owned remainder; verification; replacement/deletion status.

Classify each behavior as already supported, an existing capability needing correction/extension, a genuinely new reusable capability, product-specific behavior retained in Jackin, or obsolete/incorrect behavior removed with evidence. Every discovered behavior needs a disposition. Do not quietly omit difficult areas.

Velnor already has scanning, affected/dependency selection, aggregation, lane handling, typed release/preview/maintenance/Renovate primitives, cache contracts and runtime-product distribution. Audit their actual semantics before creating alternatives.

Start with crates/velnor-workflow:
- AGENTS.md; src/config/mod.rs; src/scan/**
- src/primitives/{mod,ir,pipeline,plan,aggregate,lanes,watch,cache,snapshot}.rs
- src/primitives/{release,renovate,runtime_products}.rs
- src/{closure,runtime,policy,runners,lib}.rs
- Existing synthetic-surface, genericity, lane-pairing, selection-handoff, policy and workflow-contract tests.

Current architecture rejects raw unit command arrays and static-workflow bodies. static_files copies repository-owned bytes; using it to carry these workflows would reproduce the problem.

Follow scan -> repository configuration -> validated graph/IR -> generated outputs -> runtime execution. Prefer extensions to that chain over parallel engines. Put reusable orchestration/runtime logic in Rust and preserve existing tool and action integration where appropriate.

No Jackin package names, estate catalogs, paths, domains, runner labels, secret names or special-case branches in generic implementation. Configuration selects products and policies. Preserve the existing prohibition on consumer names in generic fixtures; use neutral examples.

Named repository tasks are appropriate for product build/test logic. Moving generic orchestration into a large mise task or xtask, raw YAML, shell blobs, universal pre/post steps or an opaque configuration field does not satisfy extraction. Do not create a second workflow programming language.

4. Implement all proven reusable requirements using the smallest coherent set of capabilities.

Treat the following as mandatory audit coverage, not a demand for one new subsystem per bullet. Reuse existing support whenever it is correct.

A. Platform requirements, prerequisites and environment.
Model execution OS, architecture, SDK/toolchain and other required capabilities independently of provider names. Apple/Xcode/XCFramework work must reach an eligible macOS executor; ordinary Swift support must not imply macOS universally. Reject unsupported placement clearly.

Express prerequisite producers, named product tasks, environment inputs and outputs. Ensure Rust FFI changes and transitive inputs select the Swift consumer and prepare the needed XCFramework. Model whether outputs remain in one job or require verified artifact transfer. Preserve necessary build flags and Mr. Boxington integration through appropriate generic configuration.

B. Prepared tools, caches and artifact handoff.
Extend existing cache/input-closure machinery for reusable built tools and prerequisite outputs. Discover or declare workspace, standalone and nested lockfiles without hardcoded crates/fuzz/lints paths. Respect effective Cargo directories and distinguish dependency-source caches, compiler state, installed tools and final artifacts.

Bind reusable binaries to their compilation inputs, recipe/version, toolchain, relevant platform/ABI and trust boundary. Prefer exact current-run producer outputs. Historical reuse must validate authorized producer identity, source/input equivalence, outcome, manifest and byte integrity. A successful producing job can feed its current workflow; do not wait for that consuming workflow to finish.

Use clean extraction, file validation and atomic installation. Prevent stale files, traversal, invalid types or partial bundles from satisfying completeness. Keep requested and resolved fallback identities distinct; never save incompatible fallback bytes under the requested exact key.

Distinguish ordinary misses, incompatible/corrupt contents, denied access and transient API failures. Bound retries, pagination and waits. Use verified prebuilt tools on the warm path; avoid repeated source compilation or metadata work.

C. Affected selection, successful-result reuse and required checks.
Use one auditable input/dependency model for selection, artifact identity and result validity. Include relevant source, configuration, generator/check implementation, tool/action pins and transitive inputs. Account for renamed/deleted files and cross-language dependencies.

Successful-result reuse is stronger than finding a cached artifact. Validate the producing evidence, complete expected check set, effective recipe, compatible inputs and trust. Never make required checks green from artifact name/existence alone. External live-state checks need explicit freshness semantics.

Aggregate against the planner's expected work. Accept explicit planned no-work outcomes; reject missing results, unexpected skips, failed prerequisites, cancelled required work and incomplete matrices. Preserve stable required check names and useful explanations of executed, reused and skipped work.

D. Docker and image publication.
Extend existing Docker support for required multi-architecture builds, BuildKit caches, digest transport, verification, manifests, SBOM/provenance and resumable publication.

Derive provider selection and artifact references from one graph. Require exactly one authorized publisher for each required platform/artifact, plus a complete verified set before publishing its combined manifest. Comparison jobs must not duplicate publication or omit architectures. Dockerfiles and image contents remain consumer-owned.

E. Releases, previews and native products.
Extend existing release/preview/signing primitives for the required package, target and multi-output sets. Separate source selection, validate/build/rehearse/publish modes and permissions. A validation mode must run its declared build/check work.

Handle PR, push, tag, dispatch, schedule and upstream-workflow completion deliberately. Bind workflow_run operations to the intended source revision and trusted successful producer. Feature-branch rehearsals should finish their declared work without waiting for an unrelated main-branch run.

Support declared archive contents, checksums, manifests, retention, signing/SBOM/attestation stages and verification. Keep signing identity and required permissions explicit. Deterministic archive metadata alone does not establish reproducible binary builds.

Reconcile every immutable asset: absent -> upload; identical -> verified no-op; different -> fail with an explicit conflict. Verify final remote completeness; propagate upload failures. Rolling previews need separate explicit mutability and concurrency rules.

Keep Jackin's capsule schema, application metadata, entitlements, bundle contents and product-specific verifiers in Jackin. Velnor may orchestrate their production, verification and signing. Credential setup must clean up and restore affected host state after success, failure, cancellation and timeout.

F. Documentation pipelines.
Compose build, source-link checks, built-site checks, spelling, artifact reuse, Pages deployment and post-deployment verification. Support separate local/internal checks and scheduled external live-link checks.

Replace repeated setup and hand-built result lookup with shared capabilities. Retry transient deployment handoffs within explicit limits; retain final failure reporting. Site addresses, source mappings, content rules and product documentation generators remain repository configuration/tasks.

G. Scheduled checks, cadences and reports.
Cover the existing fuzz, benchmark, performance/baseline, allocation, coverage, Miri, mutation, beta-toolchain, rust-analyzer, build-time, dylint, DinD E2E and health-trend needs.

Use composable check profiles with explicit cadence, platform/tools, task references, dependencies, timeouts, artifacts, thresholds and required/advisory status. Add tool adapters only when supported by evidence; do not invent a bespoke subsystem for every Jackin job. Product assertions and chosen thresholds remain consumer-owned. Advisory results must stay observable.

H. Maintenance, Renovate and policy.
Reuse/extend current Renovate run/validation, cache maintenance and compliance support. Keep schedules, credential references, repository targets, author/DCO settings and necessary execution allowances declarative.

Preserve trusted policy evaluation and pin consistency across generator, runtime, policy and helpers. Remove obsolete policy exclusions when replacement workflows conform; never achieve a pass by disabling rules or deleting required coverage.

Generate actual dependency/admission information for nested actions and runtime dependencies. Replace fake never-triggered steps used solely to make dependencies visible.

Keep cleanup bounded and scoped to owned resources. Avoid retry loops that cannot make progress after a deletion failure.

5. Reproduce or disprove the concrete failure cases before declaring success.

Static inspection found these high-priority candidates; revalidate against current code and add meaningful regression coverage:
- Copied Swift runtime/policy pins differ from the main generator pin.
- FFI changes may not select the Swift consumer.
- Prepared xtask/Codebook and Docs/Construct reuse pick artifacts by name and expiry without sufficient producer or integrity verification.
- Fallback xtask bytes can be cached under a different requested contract.
- Some build/result digests omit their own decision logic or disagree with path-selection inputs.
- Automatic preview workflow_run builds choose one provider while assembly expects another provider's artifact names.
- Default release validation can skip all substantive builds.
- Construct comparison mode lacks a publication producer for a requested architecture.
- Preview signing requests capabilities absent from its job permissions.
- Release repair swallows upload failures and incompletely checks conflicting assets.
- Generic aggregation accepts unexpected skipped work.
- URL inputs are interpolated into shell despite inadequate validation; pass inputs as data.
- Copied workflows add duplicated bootstrap, serial execution and long polling without proving those dependencies are necessary.

Fix the enabling architecture where feasible. Do not preserve these behaviors as compatibility requirements.

6. Prove genericity, behavior and performance independently.

Use the repository's required checks plus focused schema, graph, runtime and integration tests. Snapshots prove rendering stability; they do not prove execution.

Require:
- At least two neutral synthetic consumers with materially different layouts and capability combinations, plus the real Jackin consumer. Include nested manifests, an absent optional feature and changed names/paths. No consumer-name branching.
- Clean generation without the old copied workflows/actions, repeat generation with identical bytes, accurate ownership state and passing pinned policy checks.
- An event x mode x supported-provider/platform test matrix, including default dispatch, upstream completion, feature rehearsal, schedule, tag and untrusted PR.
- Cross-language selection, renamed/deleted inputs, tool/recipe changes, planned no-work and unexpected skips.
- Cold/warm/partial caches; missing, expired, forged, wrong-producer, wrong-ABI, corrupt and incomplete artifacts; producer interruption; API denial/rate limits; compatibility fallbacks.
- Adversarial and failure tests for shell inputs, extraction, trust boundaries, permissions and credential cleanup.
- Meaningful release rehearsals, complete architecture/artifact sets, duplicate-run publication, partial failure/retry and immutable conflicts across all asset types.
- Logs and reports that expose real commands, progress, reuse decisions, failure causes and final outcomes.

Measure comparable cold and warm runs and the critical path on equivalent inputs/runners. Preserve necessary diagnostic artifacts and cache effectiveness while eliminating duplicate builds, setup, unnecessary global serialization and avoidable polling. Parallel main CI runs must remain possible; serialize only operations sharing a resource that actually requires it.

Use isolated fixtures/rehearsals for publication tests. Do not release real consumer products merely to test these abstractions. If live runners or credentials are unavailable, complete independent work and report the exact unverified acceptance items; do not substitute mock success or close the migration as complete.

7. Roll out Velnor first, migrate Jackin, and retire #994 only with proof.

Implement and integrate coherent Velnor slices with tests, schema documentation and neutral examples. Follow normal repository merge policy. Do not downgrade the generator or bypass failing checks to accommodate old overrides.

Make the verified generator/runtime revision available through the current supported distribution path. Then update Jackin's repository-owned declarations/tasks, pin the intended immutable Velnor revision, scan the consumer and regenerate all affected outputs.

Remove each superseded static mapping, source workflow, composite/runtime copy, generic script/xtask implementation, stale dependency, obsolete exclusion and unused pin once its replacement is verified. Retain necessary product behavior and remove old generic paths completely. Do not hand-edit generated YAML or hide the copies elsewhere.

Coordinate the Velnor and Jackin changes so the consumer never references an unavailable implementation. Preserve unrelated work. Merge updates from main rather than rebasing shared work unless repository instructions require otherwise.

Update #994 or create a linked replacement according to its current state. If it has already merged or closed, migrate the remaining current code instead of recreating historical state.

Close #994 as superseded only after replacement changes have landed, Jackin consumes the verified revision, required CI/policy checks pass, and every valid behavior in the ledger is accounted for. A passing regeneration check, a smaller diff, or removing the old files is insufficient. Preserve any independent product fix in the minimal appropriate change.

Continue until all proven generic gaps are implemented and adopted. Do not defer a known-required capability because it is inconvenient, expensive or difficult. If a genuine external blocker prevents completion, record exact evidence and the remaining dependency while completing everything else possible.

Deliver the final capability ledger, concise architecture rationale, Velnor and Jackin change links/SHAs, configuration examples, removed-versus-retained inventory, independent verification and run evidence, measured performance, and the precise final status of PR #994.

Use subagents aggressively. Use subagents for everything. Spawn as many subagents as you can. Use subagenets to parallelize the work.
