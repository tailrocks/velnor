# Public Fixture Repository Plan

Status: Phase 0 proof target

Velnor's real Phase 0 target remains `jackin-project/jackin` and
`ChainArgos/java-monorepo`, with their existing GitHub Actions YAML unchanged.
Before using those larger repositories as the live proof surface, create a
small public repository that exercises the same feature classes in controlled,
fast workflows.

Suggested repository name:

```text
velnor-actions-fixture
```

## Purpose

The fixture repository is not a product demo and not a replacement target. It
is a compatibility lab.

It should prove:

- GitHub-hosted runners execute the fixture workflows with normal GitHub
  Actions behavior.
- Velnor self-hosted runners receive equivalent GitHub-scheduled jobs and
  produce equivalent observable results.
- Every feature class used by the target repositories has at least one small,
  focused fixture case.
- Failures are easy to inspect through artifacts, job outputs, logs, summaries,
  and sanitized Velnor job-message dumps.

## Cost Assumption

GitHub's docs currently state that standard GitHub-hosted runners are free for
public repositories, and self-hosted runners are free to use with GitHub Actions
while the machine cost remains the user's responsibility.

This does not mean the fixture should be wasteful. Keep jobs small and avoid
large artifacts or caches. Larger GitHub-hosted runners are charged even for
public repositories, so the fixture should use standard `ubuntu-latest` unless a
specific target feature requires otherwise.

## Runner Split

Use the same repository with two runner lanes:

- GitHub lane: `runs-on: ubuntu-latest`
- Velnor lane: `runs-on: [self-hosted, velnor-target-mvp]`

Do not register Velnor with labels that can accidentally take
GitHub-hosted-runner jobs. Velnor jobs must require `self-hosted` and an
explicit Velnor label.

## Repository Shape

Use a small Rust workspace to mimic monorepo mechanics without carrying target
repository complexity:

```text
Cargo.toml
crates/
  app-a/
  app-b/
  shared/
.github/
  workflows/
    compat.yml
    compat-body.yml
    docker.yml
  actions/
    aggregate-needs/
      action.yml
    check-fixture-output/
      action.yml
```

The code can be intentionally tiny. The workflows are the important artifact.

## Feature Matrix

The first fixture version should cover these Phase 0 feature classes:

| Feature class | Fixture proof |
| --- | --- |
| Checkout | `actions/checkout` with default inputs and explicit `path` |
| Shell execution | bash scripts, working directory, non-zero failure path |
| Command files | `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH`, `GITHUB_STEP_SUMMARY` |
| Step outputs | later steps consume `steps.<id>.outputs.*` |
| Job outputs | downstream jobs consume `needs.<job>.outputs.*` |
| Conditions | `if`, `always()`, `success()`, output comparisons, `contains()` |
| Matrix | small include matrix over two Rust packages |
| Needs graph | required aggregate job verifies upstream results |
| Reusable workflow | caller workflow invokes `compat-body.yml` |
| Local composite actions | aggregate/check actions under `.github/actions` |
| Cache | `actions/cache` with exact hit, prefix restore, miss behavior |
| Artifacts | upload, download, pattern, merge/non-merge where needed |
| Path filter | simple source paths mapping to package outputs |
| Docker setup | `docker/setup-buildx-action`, Buildx command, Docker socket proof |
| Docker metadata/build | small local Dockerfile build with push disabled by default |
| Tool setup | minimal `jdx/mise-action`, `setup-just`, Rust toolchain style cases |

Defer real Docker registry login/push until the local non-push Buildx path is
stable. If registry behavior is needed, use repository secrets and make the job
manual-only.

## Workflow Pattern

Prefer paired jobs with clear names:

```text
compat-github
compat-velnor
compare-results
```

`compat-github` runs on GitHub-hosted Linux. `compat-velnor` runs on Velnor.
Both write a deterministic result artifact, for example:

```json
{
  "lane": "github-or-velnor",
  "package": "app-a",
  "step_output": "expected",
  "job_output": "expected",
  "cache_case": "miss-or-hit",
  "artifact_case": "ok"
}
```

`compare-results` should run on `ubuntu-latest`, download both result artifacts,
normalize runner-specific fields, and fail on behavioral differences.

## Verification Gates

The fixture is useful only if every feature has an explicit observable gate.

Required proof per fixture run:

- both lanes finish with the expected conclusion
- comparison job succeeds
- step summaries are readable
- outputs match expected values
- artifacts contain expected files and content
- cache behavior is deterministic or explicitly categorized as first-run miss
  versus later-run hit
- Velnor can dump sanitized job messages with contextual filenames

## Migration Path To Target Repositories

Use this sequence:

1. Create public fixture repository.
2. Run all fixture workflows on GitHub-hosted runners only.
3. Register Velnor as a repository self-hosted runner with
   `velnor-target-mvp`.
4. Run Velnor fixture lane with `--once`, `--idle-timeout-seconds`, and
   `--dump-job-message`.
5. Fix Velnor behavior until fixture comparison passes.
6. Move to `ChainArgos/java-monorepo` smoke workflows.
7. Move to `jackin-project/jackin` Linux paths.

## Out Of Scope

- No Pkl/PQL implementation.
- No broad marketplace action compatibility.
- No macOS replacement from Linux Docker.
- No hardcoding target workflow/job/step ids in Velnor.
- No large target monorepo clone in the fixture.
