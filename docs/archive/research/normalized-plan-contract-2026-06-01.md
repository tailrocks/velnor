# Normalized Plan Contract

Date: 2026-06-01

This document defines the boundary between workflow inputs and Velnor execution.
The initial Rust surface lives in `crates/velnor-runner/src/plan.rs` and wraps
the current `ExecutableStep` enum used by the Docker executor.

The contract matters for two reasons:

- Phase 0 GitHub compatibility should keep executing GitHub-expanded
  `AgentJobRequestMessage` payloads.
- The executor boundary should stay source-neutral, but non-GitHub workflow
  sources are not current work.

```text
GitHub YAML -> GitHub scheduler -> AgentJobRequestMessage -> GitHub adapter

GitHub adapter -> NormalizedJobPlan -> Docker executor -> reporter
```

## Contract Rule

The Docker executor and reporter must not depend on raw GitHub wire payloads.
The current source is GitHub-expanded job messages only.

All source-specific behavior belongs before `NormalizedJobPlan`:

- GitHub wire decoding
- V2 typed expression-value map normalization
- GitHub Actions YAML compatibility shims

Everything after `NormalizedJobPlan` is runtime behavior:

- workspace/temp/home/actions/tools directory layout
- Docker job container and service containers
- checkout, script, JavaScript action, Docker action, and composite execution
- command files and workflow commands
- cache/artifact/runtime environment variables
- masking, logs, annotations, step results, job outputs, and completion

## NormalizedJobPlan Shape

The initial Rust shape should stay conservative and map closely to current code.

```rust
pub struct NormalizedJobPlan {
    pub identity: JobIdentity,
    pub github_report: Option<GitHubReportTarget>,
    pub execution: JobExecutionPlan,
    pub steps: Vec<ExecutableStep>,
    pub outputs: BTreeMap<String, OutputExpression>,
}
```

`github_report` is optional so the same plan can later run outside GitHub. In
Phase 0 it is always present because GitHub owns scheduling and UI reporting.

### JobIdentity

```rust
pub struct JobIdentity {
    pub plan_id: String,
    pub job_id: String,
    pub request_id: Option<String>,
    pub name: String,
    pub display_name: String,
    pub workflow_name: Option<String>,
    pub repository: Option<String>,
    pub run_id: Option<String>,
    pub run_attempt: Option<String>,
}
```

GitHub adapter source:

- `AgentJobRequestMessage.Plan`
- `JobId`, `JobName`, `JobDisplayName`, `RequestId`
- `Variables` and `ContextData.github`

Future typed adapter source:

- workflow name
- job id/key
- optional run metadata from a Velnor scheduler

### GitHubReportTarget

```rust
pub struct GitHubReportTarget {
    pub run_service_url: String,
    pub billing_owner_id: Option<String>,
    pub system_connection_token: Option<String>,
    pub timeline_id: Option<String>,
    pub mask_values: Vec<String>,
}
```

This keeps reporting data out of step execution. The executor returns a summary;
the reporter turns that summary into GitHub run-service `completejob` payloads.

Phase 0 reporter fields:

- run-service URL from `RunnerJobRequestRef`
- job-scoped `SystemVssConnection` token from acquired job resources
- billing owner id from broker/run-service reference
- mask hints and secret variables from the job message

Future non-GitHub runners can use a different report target without changing
the executor.

### JobExecutionPlan

```rust
pub struct JobExecutionPlan {
    pub runner_labels: Vec<String>,
    pub workspace_container: String,
    pub workspace_host: PathBuf,
    pub temp_host: PathBuf,
    pub home_host: PathBuf,
    pub actions_host: PathBuf,
    pub tools_host: PathBuf,
    pub job_container: JobContainerSpec,
    pub services: Vec<ServiceContainerSpec>,
    pub env: Vec<(String, String)>,
    pub context_data: Vec<(String, serde_json::Value)>,
    pub defaults: RunDefaults,
}
```

Current implementation has these pieces split across `github_adapter.rs`,
`runner.rs`, `script_step.rs`, `container.rs`, and `executor.rs`. GitHub-owned
job container and service-container planning now lives in `github_adapter.rs`.
The first `plan.rs` surface makes the target shape explicit for the GitHub job
message adapter and keeps the Docker executor from depending directly on GitHub
wire payloads.

GitHub adapter source:

- job variables, typed env maps, defaults, container resources, service
  resources, repository resources, and `ContextData`
- V2 typed expression maps normalized into plain key/value maps before planning
- supported marketplace action families become native invocations without
  requiring downloaded marketplace metadata

### ExecutableStep

The current implementation already has the core enum:

```rust
pub enum ExecutableStep {
    CompositeStart { step_id: String },
    CompositeEnd { step_id: String },
    Checkout(CheckoutPlan),
    Script(ScriptStep),
    JavaScript {
        step_id: String,
        invocation: JavaScriptActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
    },
    Docker {
        step_id: String,
        invocation: DockerActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
    },
    Native {
        step_id: String,
        invocation: NativeActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
    },
    CompositeOutputs {
        step_id: String,
        outputs: BTreeMap<String, String>,
        condition: Option<String>,
    },
}
```

This is the right Phase 0 surface. It preserves GitHub-compatible execution
order, supports post hooks, and is concrete enough for Docker execution.
`Native` is the preferred Phase 0 representation for supported marketplace
action families. It carries resolved `with:` inputs and step `env`, but not the
downloaded marketplace implementation. The GitHub `@ref`/SHA is compatibility
syntax only for these families; Velnor selects the Rust adapter by action family
such as `actions/cache`, `jdx/mise-action`, or `docker/bake-action`.

## GitHub Adapter Responsibilities

The GitHub adapter converts one acquired `AgentJobRequestMessage` into
`NormalizedJobPlan`.

Required responsibilities:

- normalize V2 typed expression maps into plain maps for inputs, env, defaults,
  and outputs
- derive self repository data from `ContextData.github`/`Variables` if run
  service omits repository resources
- resolve `actions/checkout` into a native `CheckoutPlan`
- route supported repository actions to `NativeActionInvocation` by action
  family, ignoring the pinned `@ref` for implementation selection
- download only non-native repository actions into
  `_actions/<owner>/<repo>/<ref>/<path>`
- parse non-native action metadata into JavaScript, Docker, or composite
  invocations
- expand local and repository composite actions into ordered executable steps
- preserve original step order and post-hook reverse order
- evaluate only the target expression subset needed at runner time
- attach GitHub report target metadata for lock renew and completion

Non-responsibilities:

- parse workflow YAML
- expand matrices
- schedule `needs`
- evaluate workflow/job-level conditions
- expand reusable workflows

GitHub already performs those before assigning a job to the runner.

## Future Adapter Responsibilities

Any future adapter would need to convert evaluated source definitions into the
same `NormalizedJobPlan`. This is not current Phase 0 work and is not a current
requirement.

Required responsibilities:

- enforce strict validation before execution
- reject unknown workflow/job/step fields
- validate job ids, `needs`, output references, runner labels, permissions, and
  typed helper inputs
- lower source-level helper primitives to normal steps
- produce either GitHub-compatible YAML or a Velnor-native plan

Hypothetical lowering result:

- `Checkout` -> `ExecutableStep::Checkout`
- `Use` with JavaScript metadata -> `ExecutableStep::JavaScript`
- `Use` with Docker metadata -> `ExecutableStep::Docker`
- `Use` with composite metadata -> composite start, nested steps, composite
  outputs, composite end
- `Use` with a supported native action family -> `ExecutableStep::Native`
- helpers such as `DockerBake`, `Cache`, `UploadArtifact`, and
  `DownloadArtifact` -> `ExecutableStep::Native`

## Reporter Contract

The executor should return a source-neutral summary:

```rust
pub struct JobExecutionSummary {
    pub step_results: Vec<StepExecutionResult>,
    pub job_outputs: BTreeMap<String, String>,
    pub step_logs: Vec<StepLog>,
}
```

The reporter then performs source-specific completion:

- GitHub V2 reporter: `renewjob`, log/timeline upload, `completejob`
- future Velnor reporter: local/server logs, artifacts, status API

The current runner already sends V2 `completejob` with conclusion, outputs,
step results, workflow-command annotations, evaluated environment URL, masked
job telemetry for implemented workflow-command cases, billing owner id, and
known Docker environment/bootstrap `infrastructureFailureCategory`.
Best-effort in-progress job and task timeline records are sent as execution
starts; completed timeline task records are uploaded as executable steps exit,
with masked feed lines when output exists. Silent and skipped executable steps
still produce completion records and run-service step results. Still-open
GitHub parity work is true line-by-line streaming during long-running steps and
live validation.

## Migration Strategy

1. Keep Phase 0 on GitHub YAML and self-hosted runner compatibility.
2. Continue moving GitHub job-message planning from `runner.rs` into
   `github_adapter.rs`; the adapter already creates `NormalizedJobPlan` and
   owns Docker container planning.
3. Make the Docker executor consume `NormalizedJobPlan` instead of separately
   passed container, environment, context, and step slices.
4. Split GitHub run-service reporting into a reporter that consumes
   `NormalizedJobSummary`.

## Current Gap List

- `NormalizedJobPlan` exists as a Rust type, and `github_adapter.rs` builds it
  after `runner.rs` resolves checkouts/actions.
- some GitHub planning is still inside `runner.rs`, especially checkout/action
  resolution.
- reporting is coupled to `runner.rs` instead of a report-target interface.
- configuration-language work is intentionally not active; no package skeleton should exist
  under the repository now.
- live GitHub UI proof for target workflows is still missing.
