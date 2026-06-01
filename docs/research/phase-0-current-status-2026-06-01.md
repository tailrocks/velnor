# Phase 0 Current Status

Date: 2026-06-01

This is the working status for the real Phase 0 goal: run the current
`.github` trees from `jackin-project/jackin` and `ChainArgos/java-monorepo` on
Velnor as a GitHub-scheduled self-hosted runner.

The authoritative green local gate is:

```sh
scripts/target_verify.sh
cargo test -q
```

This is not yet completion proof. Completion still requires live GitHub Actions
UI runs on the two target repositories.

## Upstream Runner Facts To Preserve

Reference upstream:

- `actions/runner` latest release: `v2.334.0`
- Velnor drift check: `scripts/check_runner_reference.py`
- source audit: `docs/research/actions-runner-source-audit-2026-06-01.md`
- latest V2 refresh: `docs/research/latest-runner-v2-refresh-2026-06-01.md`

Implementation facts:

- Hosted GitHub target path is V2 broker/run-service, not classic polling.
- GitHub owns YAML parsing, trigger matching, reusable workflow expansion,
  matrix expansion, job graph scheduling, secrets, permissions, and UI.
- Velnor receives `AgentJobRequestMessage` payloads after GitHub expansion.
- Velnor should execute one active job per registered runner process.
- Each target Linux job runs in a fresh Docker job container.
- Supported marketplace action families are selected by normalized action name;
  YAML refs, tags, and SHAs are ignored for Velnor's Rust-native implementation.

## Implemented And Locally Guarded

| Area | Current evidence |
| --- | --- |
| Latest runner pin | `RUNNER_VERSION = 2.334.0`; `scripts/check_runner_reference.py` checks GitHub latest release and user-agent drift. |
| V2-only hosted path | `velnor-runner run` requires `UseV2Flow` and `ServerUrlV2`; normal path uses broker session/message plus run-service acquire/renew/complete, with bounded broker session startup retry and best-effort session deletion on runner-loop errors. |
| Broker controls | `BrokerMigration`, `ForceTokenRefresh`, runner update/refresh, hosted shutdown, busy-job cancellation, transient broker poll retry, and empty-message backoff are recognized. |
| V2 job tokens | Run-service renew/complete use the job-scoped `SystemVssConnection` token when available. |
| Job acquisition | Run-service acquire handles non-retriable `404`, `409`, and `422` as stale/unusable messages. |
| Docker job isolation | Fresh workspace/temp/home/actions/tools layout, Docker job container, network, host Docker socket/CLI/Buildx mounting, and bind-mount preflight exist. |
| Script steps | Bash/sh steps, defaults, working directory, env/context rendering, command files, outputs, summaries, and protected env handling are covered. |
| Expression subset | Target-shaped `github`, `runner`, `env`, `steps`, `needs`, `matrix`, `inputs`, `vars`, `secrets`, `contains`, `toJSON`, `hashFiles`, status functions, and boolean/value `&&`/`||` are guarded. |
| Checkout | Native `actions/checkout` covers self/external repository, path, ref, token, fetch-depth, fetch-tags, persist-credentials, clean, safe-directory, cleanup, step-output refs, and needs/github refs. |
| Native action routing | Target action inventory maps to Rust-native adapters by normalized family name, independent of pinned refs/SHAs. |
| Setup/tool adapters | `jdx/mise-action`, `actions/setup-python`, `dtolnay/rust-toolchain`, `extractions/setup-just`, `baptiste0928/cargo-install`, `mozilla-actions/sccache-action`, `Swatinem/rust-cache`, and `rui314/setup-mold` have target-shaped coverage. |
| Cache/artifacts | Native cache plus upload/download artifact adapters work through Velnor shared workdir storage for same-host same-run target handoff. |
| Docker adapters | Native Docker login, Buildx setup, metadata, build-push, and bake adapters invoke Docker CLI without Node sidecars for target shapes. |
| Pages/renovate/runtime | Target-shaped Pages, Renovate, and GitHub runtime-export behavior has native/adapter coverage. |
| Composite actions | Local and repository composite expansion covers target local actions and target nested repository action usage. |
| Timeline/results | Velnor sends best-effort in-progress job/step records, step feed lines, annotations, telemetry, step results, job outputs, environment URL, and final run-service completion. |
| Target drift audit | `scripts/target_audit.py --check-target-mvp` gates current workflow files, action inventory, triggers, matrices, env, outputs, permissions, needs, conditions, defaults, labels, and unsupported feature drift. |

## Still Not Proven

| Gap | Why it matters | Proof needed |
| --- | --- | --- |
| Live GitHub UI run on `java-monorepo` | Local unit tests cannot prove GitHub accepts the full live protocol and renders logs/status correctly. | Run `ansible.yml`, then `rust.yml`, then Docker-heavy workflows through a registered Velnor runner. |
| Live GitHub UI run on `jackin` Linux paths | `jackin` has Linux hosted labels plus artifact/pages/homebrew flows that must work without YAML changes for selected Linux jobs. | Register with target x64 labels and run `ci.yml`, `construct.yml`, and `docs.yml` Linux paths. |
| GitHub artifact/cache service transport | Current artifact/cache handoff is shared-workdir local storage, good for one-host proof but not multi-runner parity. | Either prove target live jobs run on one host with shared workdir or implement service-backed transport before multi-host validation. |
| MacOS matrix legs | Docker Linux runner cannot truthfully replace `macos-latest`. | Explicitly defer or run those legs on real macOS Velnor workers later. |
| Hosted image parity | Velnor uses a Docker image with common tools; it is not a byte-for-byte GitHub-hosted runner image. | Target workflow live runs prove enough parity for current scripts. |
| Broad GitHub Actions compatibility | Not a Phase 0 requirement. | Add only when new target workflows need more features. |

## Next Best Proof Path

1. Run `scripts/target_verify.sh` and `cargo test -q`.
2. Run `velnor-runner preflight` on the live Linux host and intended workdir.
3. Register `ChainArgos/java-monorepo` with `--target-mvp-labels`.
4. Run `ansible.yml` with `--once` and capture sanitized job dump on failure.
5. Fix only failures backed by live payloads or target workflow evidence.
6. Move to `rust.yml`, then Docker-heavy Java workflows.
7. Register `jackin-project/jackin` with x64 target labels and prove Linux
   `ci.yml`, `construct.yml`, and `docs.yml` paths.

## Completion Rule

Do not call Phase 0 done because the local verifier passes. Phase 0 is done only
when current target workflows assigned to Velnor complete successfully in GitHub
Actions UI with readable logs, correct step/job conclusions, required job
outputs, cache/artifact behavior needed by the target jobs, and no unsupported
feature drift in the target audit.
