# G3 action scanner — exact detached review

## Disposition

**Rejected for G3 action-independent acceptance.** Commit `34121c09090b299a4beb531bf445c72c98f00c93` is clean and materially improves generic scanning, but it does not yet provide runner-compatible action coverage or a real consumer/failure-propagation proof. This is an exact-commit disposition, not a judgment of the earlier dirty snapshot.

Reviewed from detached checkout:

- Source: `/tmp/velnor-g3-action-review-exact`, `HEAD=34121c09090b299a4beb531bf445c72c98f00c93`, detached, clean.
- Parent/base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Protocol source: `/tmp/actions-runner-protocol-review`, `actions/runner` commit `80bb1fb827fa44d489263061e71ef4adba7ad8cd`.
- No source implementation, runtime, Docker, host, or moving author worktree was operated.

## Exact verification

Passed on the detached tree:

- `rtk cargo test -p velnor-workflow`: **1,867 passed** across 20 suites.
- `rtk cargo check --workspace`: **321 crates compiled; pass**.
- `rtk cargo fmt --all -- --check`: **pass**.
- `rtk cargo clippy --workspace --all-targets -- -D warnings`: **pass**.
- Focused action scanner: 6 passed.
- Focused file walker: 7 passed.
- Focused action-fixture primitive: 2 passed.

These are code/test health results, not proof that the action contract is complete.

## Capability matrix

| Capability | Exact evidence | Result |
|---|---|---|
| Typed `GithubAction` unit/pipeline/registry | `/tmp/velnor-g3-action-review-exact/crates/velnor-workflow/src/s2/scan/action.rs:75-117`; `.../primitives/mod.rs:580-627`; `.../primitives/pipeline.rs:216-220` | **Pass, structural.** Uses the shared shape/unit/primitive path. |
| Generated/embedded exclusion and action-local `dist` | `.../scan/file_walk.rs:122-145,149-281,544-586` | **Pass for tested boundary.** `.github-gen`/generic `dist` are excluded; owned action `dist` is retained. |
| `&&` / `;` shell token coverage | `.../scan/action.rs:377-492,535-549` | **Partial.** Tests cover the requested separators and quoting, but this remains heuristic token/file inference, not shell execution or complete path semantics. |
| Node `main`/`pre`/`post` local files | `.../scan/action.rs:230-242` | **Partial pass.** Required fields and local files are checked; expression-bearing paths are rejected rather than runner-resolved. |
| Composite `run`/`uses` shape | `.../scan/action.rs:188-228,264-292` | **Partial.** Basic shape and local nested metadata are checked; conditions, `with`, `env`, and `continue-on-error` are parsed/shape-checked but never executed. |
| Docker container entrypoint fields | `.../scan/action.rs:243-254`; runner `ContainerActionHandler.cs:54-123` | **Partial pass.** Main/pre/post container entrypoint strings are not incorrectly treated as host files; no exact pre/post fixture proves it. |
| `action.yml`/`action.yaml` precedence | `.../scan/action.rs:81-90`; runner `ActionManager.cs:725-739` | **Fail.** Velnor errors when both files exist; runner chooses `action.yml` when present and otherwise `action.yaml`. |
| Bare Dockerfile action fallback | `.../scan/action.rs:81-84`; runner `ActionManager.cs:1435-1544` | **Fail.** Scanner only discovers metadata files and cannot emit a unit for a repository action represented solely by `Dockerfile`/`dockerfile`. |
| Docker image-vs-Dockerfile classification | `.../scan/action.rs:243-254`; runner `DockerUtil.cs:69-77`, `ContainerActionHandler.cs:54-83` | **Fail.** Any `runs.image` without `://` and without `:` is treated as a local host file. Ordinary image names such as `ubuntu` are not Dockerfiles under runner semantics. |
| Canonical `${{ github.action_path }}` composite scripts | `.../scan/action.rs:294-328,377-423`; role action `/tmp/velnor-g3-action-roles.ueumsS/role-action/action.yml:31-40` | **Fail.** Exact `verify-action` run rejected `}}/scripts/download-jackin-role.sh` as a missing literal path. This is a standard action-local reference, not a role-specific build/publish feature. |
| Mutable external `uses` policy in action metadata | `.../scan/action.rs:216-227`; policy walker `.../s2/policy.rs:2556-2595` | **Fail/omitted.** Scanner accepts non-empty `actions/foo@main`; the existing action-pin policy is workflow-file traversal and is not applied to `action.yml` composite metadata. G0 requires mutable `@main` rejection. |
| Real consumer success/failure propagation | `.../primitives/github_action.rs:16-126,186-311` | **Fail as evidence.** Tests execute standalone `tests/success.sh`/`tests/failure.sh`; they do not invoke the scanned action, nested `uses`, downloader, validator, or downstream marker. The expected-failure test proves only the wrapper guard. |

## Direct proof of the consumer gap

The exact binary was run in a temporary tracked fixture containing the pinned current role action metadata and its downloader:

```text
cargo run --manifest-path /tmp/velnor-g3-action-review-exact/Cargo.toml \
  -p velnor-workflow -- verify-action --path action.yml
```

Observed result:

```text
error: GitHub Action entrypoint `}}/scripts/download-jackin-role.sh`
resolves to missing file `}}/scripts/download-jackin-role.sh`
```

The source action uses `${{ github.action_path }}/scripts/download-jackin-role.sh` at `/tmp/velnor-g3-action-roles.ueumsS/role-action/action.yml:39`. The scanner’s static parser separates the expression and path, then rejects the resulting literal. This is independent of the role’s download/build/skip-build/Buildx obligations.

## Runner pins used for rejection

- Manifest precedence: `/tmp/actions-runner-protocol-review/src/Runner.Worker/ActionManager.cs:725-739`.
- Bare Dockerfile fallback: `/tmp/actions-runner-protocol-review/src/Runner.Worker/ActionManager.cs:1435-1544`.
- Docker metadata and container-only `entrypoint`, `pre-entrypoint`, `post-entrypoint`: `/tmp/actions-runner-protocol-review/src/Runner.Worker/ActionManifestManager.cs:363-461`; `/tmp/actions-runner-protocol-review/src/Runner.Worker/Handlers/ContainerActionHandler.cs:54-123`.
- Dockerfile detection: `/tmp/actions-runner-protocol-review/src/Runner.Worker/Container/DockerUtil.cs:69-77`.
- Composite condition and failed/cancelled result propagation: `/tmp/actions-runner-protocol-review/src/Runner.Worker/Handlers/CompositeActionHandler.cs:382-451,469-474`.
- Unix shell default: `/tmp/actions-runner-protocol-review/src/Runner.Worker/Handlers/ScriptHandler.cs:65-100`.

The exact candidate may intentionally impose stricter product policy, such as requiring an explicit composite `shell` (`.../scan/action.rs:206-210`), but that policy must be declared and tested separately from a claim of runner equivalence. G0’s fixture matrix currently asks for missing-shell rejection; retain that as an explicit Velnor policy if intended.

## Smallest bounded follow-up

1. Resolve canonical action-local expressions to the owning action root for static checks, while rejecting genuinely unbounded dynamic paths. Add a real pinned action consumer fixture using `github.action_path`.
2. Match runner manifest selection: choose `action.yml` over `action.yaml`; add bare `Dockerfile`/`dockerfile` fallback; classify Dockerfile names with runner-equivalent `IsDockerfile` behavior; keep container entrypoints container-only.
3. Apply the existing full-SHA action-pin policy to `uses` inside action metadata, or record a deliberate local-action exception. Add `@main` failure and immutable-SHA success fixtures.
4. Replace standalone shell-only fixture proof with an explicit, network-free consumer harness. Execute the action’s actual composite path using stubs for downloader, validator, hadolint, Buildx, and downstream publish. Assert success, each non-zero failure, conditional skip/no-build, and downstream-marker absence.
5. Add matrix tests for duplicate manifests, ordinary Docker image names, `docker://`, local Dockerfile, bare Dockerfile, pre/post container entrypoints, Node pre/post, composite conditions, expression paths, and shell `&&`/`;` cases.

Acceptance requires a new detached commit, clean-tree evidence, exact runner commit, full tests, and the consumer/failure matrix above. Do not claim G3 action/role full pass until the separate role-specific download/version, validator, skip-build, Buildx, role-image, runtime-smoke, publisher, multi-arch, and signing obligations in `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/action-roles/findings.md:111-151` are closed.
