# CI/CD performance goal

Act as CI/CD performance lead for tailrocks/velnor. Execute fixes, not only recommendations.

Goal: make CI/CD as fast as technically possible while preserving correctness, coverage, required checks, and compatibility.

Scope:

1. Analyze PR #557:
   [PR #557](https://github.com/tailrocks/velnor/pull/557)

2. Inventory every CI/CD workflow on `main`.

3. Trace workflow history from the migration to the current well-generated workflows. Identify the migration commit, design changes, regressions, and accumulated bottlenecks.

4. Inspect the workload/workflow generator, generated artifacts, GitHub-hosted runners, and Velnor runners.

5. Inspect every newly created or updated PR from parallel agents. Reanalyze each one for CI/CD regressions and optimization opportunities.

6. Verify whether this tool is used:
   [Mr. Boxington](https://mr-boxington.jdx.dev/)

   The generator must enable this tool by default for both GitHub-hosted and Velnor nodes. Verify actual generated output, integration code, defaults, opt-outs, and tests. Do not assume usage from documentation alone.

Always make sure Velnor is using the latest [Mr. Boxington](https://github.com/jdx/mr-boxington) version even for Github Actions.

Subagents:

Use multiple independent subagents for every material decision. Use `gpt-5.6-luna max` for all subagents.

At minimum, use these roles:

- CI/CD historian: workflow and migration history.
- PR analyzer: PR #557 and all later PR changes.
- GitHub Actions specialist: triggers, matrices, concurrency, reusable workflows, job duplication, runner behavior.
- Rust/Cargo specialist: workspace graph, changed-crate detection, transitive dependencies, lockfiles, target directories, incremental compilation.
- Cache specialist: dependency, registry, git, build, compiler, artifact, and runner cache correctness.
- Generator specialist: templates, defaults, generated output, Mr. Boxington integration, GitHub/Velnor compatibility.
- Performance analyst: run timing, logs, cache hit/miss behavior, cold versus warm runs.
- Adversarial reviewer: correctness, missing dependency closure, false cache hits, security, and coverage regressions.

Do not trust one subagent. Reconcile conflicting conclusions with repository evidence, workflow logs, tests, and measurements.

Investigation requirements:

- Establish measured baselines for PR, push, main, matrix, cold-cache, and warm-cache runs.
- Break duration into queue, checkout, setup, cache restore, dependency download, compilation, tests, artifact upload, and cleanup.
- Identify every avoidable dependency download, cache miss, recompilation, duplicate job, redundant checkout, and full-workspace build.
- Verify cache paths, keys, restore keys, invalidation inputs, branch/PR scope, OS, architecture, toolchain, target triple, features, lockfiles, and concurrent cache saves.
- Verify GitHub-hosted and Velnor runner behavior separately. Account for ephemeral versus persistent storage.
- Verify whether generated workflows use Mr. Boxington in practice.
- Locate the actual generator. Fix generator behavior first when generator owns the problem. Regenerate checked-in workflows instead of making untracked manual edits.
- Add regression tests, fixtures, or snapshots for every important generator and caching fix.
- Preserve required validation. Do not disable tests or checks merely to reduce duration.
- For pull requests, build and test only affected crates plus the correct transitive dependency/dependent closure. Do not rebuild unrelated crates.
- Treat changes to workspace manifests, lockfiles, toolchains, shared libraries, build scripts, code generation, CI files, generator code, and global configuration as broad-impact changes.
- Avoid duplicate full-workspace execution across jobs.
- Use concurrency cancellation for obsolete PR runs where safe.
- Preserve correct status checks and branch protection behavior.

Iteration loop:

1. Spawn research subagents.
2. Rank bottlenecks by measured impact and confidence.
3. Spawn design and implementation subagents for the highest-impact fixes.
4. Apply the smallest correct change.
5. Regenerate artifacts.
6. Run focused tests, generator tests, workflow validation, and relevant CI simulations.
7. Spawn independent review and re-verification subagents.
8. Measure before versus after.
9. Inspect all resulting PRs and workflow changes.
10. Repeat until no confirmed avoidable bottleneck remains.

Never stop only because tests pass, one run is fast, or a cache happened to hit. Continue until remaining costs are proven intrinsic, externally imposed, or unsafe to remove. Do not claim “instant” CI without measurements. Distinguish cold-run costs, cache-eviction costs, queue time, provider limits, and avoidable project overhead.

Safety:

- Obey repository `AGENTS.md` instructions.
- Inspect untrusted workflow changes before executing them.
- Do not expose secrets.
- Do not merge, approve, close, delete, force-push, or create external PRs unless explicitly authorized.
- Avoid destructive commands.
- If access to GitHub history, logs, runners, or Mr. Boxington is unavailable, report the exact limitation and continue with local evidence.

Final report must include:

- Workflow and runner inventory.
- Migration-history findings.
- PR #557 findings.
- Measured baseline and final timings.
- Root cause for every major slowdown.
- Files and generator changes made.
- Cache and affected-crate strategy.
- GitHub-hosted/Velnor compatibility results.
- Tests and validation performed.
- Remaining unavoidable costs and why they cannot be removed.
- New PRs reviewed.
- Clear conclusion: whether any further evidence-based optimization remains.
