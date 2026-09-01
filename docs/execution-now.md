# Execution now

Code-backed lifecycle for one acquired Actions job. Citations use `path:line` and only cover the requested source set. “Design-only” marks behavior not proven by this implementation.

## From broker message to admission

The V2 broker delivers a `RunnerJobRequest` reference. The message carries plan/job identity, timeline, variables and masks, resources, steps, environment, workspace, actions, dependencies, and billing owner. (`crates/velnor-runner/src/protocol.rs:3157-3234`, `crates/velnor-runner/src/job_message.rs:10-68`)

The runner:

1. Claims `(plan_id, job_id)` with an on-disk flock and writes an atomic in-flight marker. (`crates/velnor-runner/src/runner.rs:200-355`)
2. Parses and validates script/action steps, trust capabilities, and the execution backend before expensive work. Admission precedes run-service renewal, leases, checkout, downloads, containers, and credentials. (`crates/velnor-runner/src/runner.rs:5285-5609`, `crates/velnor-runner/src/runner.rs:6150-6167`)
3. Acquires active capacity while renewing the remote job lease. Cancellation, capacity timeout, or registration loss completes the job explicitly and clears local state. (`crates/velnor-runner/src/runner.rs:5611-5799`)
4. Starts timeline/job state, runs the backend, keeps renewal alive through completion, publishes completion, then clears the in-flight marker and tears down. (`crates/velnor-runner/src/runner.rs:5799-6059`)

The request plan is converted into `ValidatedPlan`: identity, ordered validated steps, services, image, environment, workspace, timeouts, cancellation, outputs, artifacts, annotations, summaries, cache, and optional buildx/testcontainer data. (`crates/velnor-runner/src/execution/backend.rs:30-79`, `crates/velnor-runner/src/execution/backend.rs:212-334`)

## Backend selection and common lifecycle

`execution.toml` must explicitly select `docker` or `microvm`. Resolution checks the configured path, then the config directory, then `/etc/velnor/execution.toml`; no file means failure. The selected backend is opened directly. (`crates/velnor-runner/src/execution/mod.rs:121-173`)

Every backend session uses a typed phase machine:

```text
New → Preflighted → Reserved → Prepared → Started → Executing
  → Stopped → Collecting → TornDown
```

Wrong-phase calls fail. `run_validated_job` reserves, prepares, starts, executes or cancels, collects, and always attempts teardown; the returned outcome records whether cleanup completed. (`crates/velnor-runner/src/execution/backend.rs:1-28`, `crates/velnor-runner/src/execution/backend.rs:775-1042`, `crates/velnor-runner/src/execution/mod.rs:175-204`)

`ExecutionOutcome` contains conclusion, exit code, executed actions, logs, command-file bytes, summaries, outputs, environment URL, annotations, cache/artifact state, buildx/testcontainer state, and `cleaned`. Events cover step boundaries, logs, command files, result export, backend calls, and job completion. (`crates/velnor-runner/src/execution/backend.rs:587-646`)

## Docker execution

Docker preflight requires the host Docker socket and verifies a systemd cgroup boundary. The Docker backend prepares image/environment/services and starts the job container. (`crates/velnor-runner/src/execution/docker.rs:20-81`)

In the production runner wiring, the Docker backend receives `RunnerDockerEngine`; the execution world carries `/var/run/docker.sock`, a Docker engine, and no vsock. There is no second executor path. (`crates/velnor-runner/src/runner.rs:8051-8141`, `crates/velnor-runner/src/executor.rs:1340-1372`)

The engine starts the job environment, attaches services, exposes service aliases/ports, prepares workflow environment and state, then executes ordered steps. Cleanup removes services before the job container, aborts Docker leases, reclaims owned Docker resources, and cleans BuildKit/network state. (`crates/velnor-runner/src/executor.rs:1515-1709`, `crates/velnor-runner/src/executor.rs:4045-4133`, `crates/velnor-runner/src/executor.rs:3827-3905`)

Step execution includes conditions, continue-on-error, timeouts, composite actions, JavaScript pre/post registration, checkout, script, Docker, and native step forms. Skipped steps are represented in execution state rather than silently omitted. (`crates/velnor-runner/src/executor.rs:918-1185`, `crates/velnor-runner/src/executor.rs:1769-1991`)

## MicroVM execution

The microVM path is jailed Firecracker with Docker local to the guest. It rejects use of the host Docker socket. Preflight requires KVM, a verified packaged artifact set, and a working jailer. (`crates/velnor-runner/src/execution/firecracker.rs:1-20`, `crates/velnor-runner/src/execution/firecracker.rs:172-236`)

Preparation binds the plan identity to the isolation identity, validates the guest plan, loads artifacts, configures boot/network state, and cold-boots or restores only a matching golden snapshot. A restored snapshot with the wrong identity/version is not reused. (`crates/velnor-runner/src/execution/firecracker.rs:239-280`, `crates/velnor-runner/src/execution/firecracker.rs:724-744`, `crates/velnor-runner/src/execution/firecracker.rs:820-840`, `crates/velnor-runner/src/execution/firecracker.rs:1081-1098`)

Start establishes the guest through Firecracker’s API and vsock handshake. Execution sends a guest plan containing job/isolation/generation identity, plan hash, and an execution nonce. The driver verifies identities, hashes, ordering, result-export digests, stdio, and teardown acknowledgement; replayed or mismatched frames fail. (`crates/velnor-runner/src/execution/firecracker.rs:282-394`, `crates/velnor-runner/src/execution/firecracker.rs:842-1079`)

The production runner supplies no inline guest-plan escape hatch. Inline execution is allowed only when the world explicitly enables it, which is a test/probe facility. (`crates/velnor-runner/src/execution/mod.rs:279-309`, `crates/velnor-runner/src/execution/firecracker.rs:322-394`, `crates/velnor-runner/src/runner.rs:7297-7378`)

## Reporting, cancellation, cleanup

Cancellation is legal in prepared, started, or executing phases and transitions the session to stopped before collection. Docker cancellation force-removes the job container; microVM cancellation stops the jailer. Teardown removes the exact resource paths and reports uncertain isolation teardown as an error. (`crates/velnor-runner/src/execution/backend.rs:882-1042`, `crates/velnor-runner/src/execution/docker.rs:101-119`, `crates/velnor-runner/src/execution/firecracker.rs:396-456`)

The run-service completion payload carries plan/job identity, conclusion, outputs, step results, annotations, telemetry, environment URL, billing owner, and infrastructure-failure category. Completion retries are bounded to six attempts and preserve exact serialized bytes; only classified transient failures retry. (`crates/velnor-runner/src/protocol.rs:3209-3274`, `crates/velnor-runner/src/protocol.rs:2113-2207`)

Acquire and completion are not infinite: acquire uses up to five attempts with bounded jitter; completion uses up to six attempts. A typed actions-run-service 404 can be treated as a remote terminal acknowledgement, while unrelated 404s are not silently accepted. (`crates/velnor-runner/src/protocol.rs:1986-2094`, `crates/velnor-runner/src/protocol.rs:1769-1820`, `crates/velnor-runner/src/protocol.rs:2113-2207`)

## Explicit limits and design-only notes

- Docker requires host Docker plus the tested systemd cgroup boundary. MicroVM requires KVM, packaged artifacts, jailer, and guest transport. Backend selection never falls back across isolation modes. (`crates/velnor-runner/src/execution/docker.rs:20-38`, `crates/velnor-runner/src/execution/firecracker.rs:172-236`, `crates/velnor-runner/src/execution/mod.rs:1-5`)
- MicroVM support is intentionally narrower than “all Actions”: the executable-step planner permits supported script/checkout/native repository adapters and rejects unsupported action forms, cache actions, empty context, and host-socket mounts. (`crates/velnor-runner/src/runner.rs:7381-7462`, `crates/velnor-runner/src/runner.rs:7580-7623`)
- Whole-plan timeout defaults to 360 minutes; the normalized plan enforces a minimum total timeout of 60 seconds and a minimum step timeout of one minute. (`crates/velnor-runner/src/execution/backend.rs:353-369`)
- Trust policy controls host-Docker capability. Non-trusted execution disables shared Docker socket access and rejects user secrets. (`crates/velnor-runner/src/service.rs:193-195`, `crates/velnor-runner/src/runner.rs:6150-6167`)
- Design-only / not established here: complete GitHub Actions parity, arbitrary guest feature parity, crash-resume of an executing job, and a cleanup guarantee after an uncertain teardown. The implementation exposes explicit validation and failure states instead.
