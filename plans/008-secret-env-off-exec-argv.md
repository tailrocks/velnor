# Plan 008: Deliver secret env to `docker exec` via a 0600 `--env-file`, not argv

> **Executor instructions**: Follow step by step; run each verification. STOP
> ⇒ report. Update `plans/README.md` when done. **Security** plan — no real
> secret values in tests.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/container.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Every step's environment — including `GITHUB_TOKEN`, `ACTIONS_RUNTIME_TOKEN`,
and workflow `env:` secrets — is rendered as `-e NAME=VALUE` on the `docker
exec` **argv**. Argv is world-readable in the host process table
(`/proc/<pid>/cmdline`, `ps aux`) for the lifetime of each exec. Because the job
container mounts the host Docker socket (a documented tradeoff), a job can launch
a `--pid=host` container and read the cmdline of concurrent jobs' execs,
scraping their secrets. On a warm multi-job host this is a cross-job credential
path. The codebase already avoids exactly this for the curl transport (tokens go
in a mode-0600 temp config file, kept off argv). This plan applies the same
hygiene to `docker exec`: secret env values go into a per-exec 0600 `--env-file`,
unlinked after the exec; non-secret env stays inline.

## Current state

- `crates/velnor-runner/src/container.rs` — both exec builders put every env
  pair on argv. Excerpts:
  - `container.rs:200-207` (`exec_process_args`):
    ```rust
    let mut args = vec!["exec".into(), "--workdir".into(), working_directory.into()];
    self.append_base_exec_env(&mut args);
    for (name, value) in env {
        args.extend(["-e".into(), format!("{name}={value}")]);   // <-- secret values on argv
    }
    args.push(self.name.clone());
    args.extend(command.iter().cloned());
    ```
  - `container.rs:212-230` (`exec_process_stdin_args`) — same pattern with
    `-i`.
  - `append_base_exec_env` (`container.rs:243-251`) puts only non-secret base
    env (`HOME`, `RUSTUP_HOME`, `CARGO_HOME`) on argv — leave those inline.
- Docker semantics to preserve: **last `-e` wins**. `--env-file` entries are
  applied, then any later `-e` overrides — so the ordering must keep step
  overrides working. The current callers rely on later env overriding earlier
  base env.
- The existing 0600 temp-file pattern to mirror is in `protocol.rs` (grep
  `0o600` / `OpenOptions` in `protocol.rs` — the curl `--config` file is written
  mode-0600 and unlinked after use).
- To know which env pairs are secret, you need the mask/secret set. Determine
  what the exec builders have access to: they currently receive `env: &[(String,
  String)]` with no secret flag. You will need the caller to pass which names are
  secret (or the set of secret values). Trace the callers of `exec_process_args`
  (grep `exec_process_args` / `exec_process_stdin_args`) to see what secret
  information is available at the call site (the job's `is_secret` variables /
  mask set).

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| Tests     | `cargo nextest run -p velnor-runner exec --locked`              | new tests pass  |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/container.rs` — the two exec builders and a new
  env-file writer helper; tests.
- The immediate callers of the exec builders — **only** to thread through which
  env names are secret. If threading requires touching more than the direct
  callers, STOP and report.

**Out of scope**:
- The curl transport (already 0600) — do not change.
- Masking of log output — plan 005.

## Git workflow

- Branch: `advisor/008-secret-env-off-exec-argv`
- Commit: `fix(security): pass secret step env via 0600 --env-file, not docker exec argv`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Investigate the secret signal available at exec time

Trace the callers of `exec_process_args` / `exec_process_stdin_args`. Determine
whether the caller knows which env names/values are secret (the job's
`is_secret` variables + `::add-mask::` set). Write the finding in the PR
description. If **no** secret signal is reachable without a broad refactor, STOP
and report — a partial fix that guesses which env is secret is worse than none.

**Verify**: no code change; finding documented.

### Step 2: Add a 0600 env-file writer

Add a helper in `container.rs` that writes a set of `NAME=VALUE` lines to a
temp file created with mode 0600 (Unix), mirroring the `protocol.rs` curl-config
pattern (use `std::os::unix::fs::OpenOptionsExt::mode(0o600)`). Return the path.
The file must be created readable only by the owner. Handle values containing
newlines per Docker's `--env-file` format (Docker `--env-file` does **not**
support multi-line values; if a secret value contains a newline, keep it on
argv as a fallback and note this limitation, OR document that multi-line secrets
are passed another way — decide in Step 1's investigation and record it).

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 3: Split env into secret (file) and non-secret (argv)

In both exec builders, partition `env` into secret and non-secret using the
signal from Step 1. Write the secret pairs to a 0600 env file, add
`--env-file <path>` to the args **before** any overriding `-e`, keep non-secret
pairs inline as `-e`, and preserve the last-writer-wins ordering. Ensure the
env file is unlinked after the exec completes — this likely means the exec
builder returns the temp-file handle/path to the caller, or the caller creates
the file and passes the path in. Choose the approach that keeps unlink reliable
even on exec failure (RAII/`Drop` on a temp-file guard is ideal).

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 4: Tests

Add tests in `container.rs` `#[cfg(test)]`:
- `secret_env_is_not_on_exec_argv`: given an env with a name marked secret and a
  placeholder value `"PLACEHOLDER_SECRET"`, the produced argv contains
  `--env-file` and does **not** contain `PLACEHOLDER_SECRET` anywhere.
- `nonsecret_env_stays_inline`: a non-secret pair still appears as `-e NAME=VALUE`
  on argv.
- `override_ordering_preserved`: a step override of a base env var still wins
  (last `-e` after `--env-file`).
- If you can test file mode on Unix, assert the env file is `0o600`.

**Verify**: `cargo nextest run -p velnor-runner exec --locked` → new tests pass;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- The four tests above.
- Model after existing `container.rs` exec-args tests (grep `exec_process_args`
  in the test module).
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] PR description states which secret signal is available at exec time
- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] Secret env values do not appear on the `docker exec` argv (test proves it); they go through a mode-0600 `--env-file`
- [ ] The env file is unlinked after the exec, including on exec failure
- [ ] Step override ordering (last-writer-wins) preserved (test proves it)
- [ ] `cargo nextest run --workspace --locked` exits 0
- [ ] Only `container.rs` + direct callers modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The exec builders don't match the excerpts (drift).
- No secret signal is reachable at the call site without a broad refactor.
- Reliable unlink-on-failure would require an ownership change spanning more than
  the exec builders and their direct callers — report the call graph.

## Maintenance notes

- Docker `--env-file` cannot carry multi-line values; document how multi-line
  secrets are handled (fallback to argv is a **regression** for those — prefer
  passing multi-line secrets via stdin or a mounted 0600 file if the
  investigation shows they occur).
- Reviewer: confirm the temp env file is 0600 and always unlinked; confirm no
  secret value survives on argv for any code path (both builders).
- Related hardening: plan 026 (container `options:` denylist) and plan 014
  (systemd `PrivateTmp`) reduce the blast radius of the `/proc` read this
  defends against.
