# Plan 024: Clean up Docker resources when the second job-environment start attempt fails

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

`start_job_environment` retries once on failure: first attempt fails →
`cleanup_stale` → retry. But if the **second** attempt also fails, its
partially-created resources (network/services/container) are **not** cleaned —
the caller returns via `?` before its own cleanup runs. On a persistent start
failure (bad image, daemon hiccup) this leaks a network/container per attempt. It
is self-healing on the next job only because names are deterministic per slot
(the next job's pre-start `cleanup_stale` removes them), but a failing slot can
accumulate dangling resources until then. This plan cleans up on the second
failure too.

## Current state

- `crates/velnor-runner/src/executor.rs` — `start_job_environment`, excerpt at
  `executor.rs:2339-2347`:
  ```rust
  fn start_job_environment(&mut self, container: &JobContainerSpec) -> Result<()> {
      if let Err(error) = self.start_job_environment_once(container) {
          eprintln!("Docker job environment start failed, removing stale resources: {error:#}");
          self.cleanup_stale(container);
          self.start_job_environment_once(container)?;   // <-- second failure leaves resources uncleaned
      }
      Ok(())
  }
  ```
- `cleanup_stale(&container)` removes the container/network/services for this
  slot (it is already called before the retry). The fix is to also call it if
  the retry fails, before returning `Err`.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner start_job --locked`         | pass            |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — `start_job_environment` retry cleanup;
  a unit test if the runner is mockable here.

**Out of scope**:
- The broader per-cycle docker prune (already exists elsewhere) — not touched.
- `cleanup_stale` itself — reused as-is.

## Git workflow

- Branch: `advisor/024-docker-cleanup-on-double-start-failure`
- Commit: `fix(executor): clean up docker resources when the retried job-env start also fails`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Clean up on the second failure

Wrap the retry so a failed second attempt runs `cleanup_stale(container)` before
returning `Err`:

```rust
fn start_job_environment(&mut self, container: &JobContainerSpec) -> Result<()> {
    if let Err(error) = self.start_job_environment_once(container) {
        eprintln!("Docker job environment start failed, removing stale resources: {error:#}");
        self.cleanup_stale(container);
        if let Err(retry_error) = self.start_job_environment_once(container) {
            self.cleanup_stale(container);   // don't leak resources from the failed retry
            return Err(retry_error);
        }
    }
    Ok(())
}
```

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy --workspace
--all-targets --locked -- -D warnings` → exit 0.

### Step 2: Test (if mockable)

If the executor's `CommandRunner` can be scripted to fail
`start_job_environment_once` twice (grep the test module for how docker calls are
faked — `RecordingRunner`/scripted codes), add
`double_start_failure_cleans_up`: assert `cleanup_stale`'s docker-remove commands
are issued after the second failure. If the runner cannot be made to fail start
twice without heavy setup, add a note that this is covered by the code shape
(the `cleanup_stale` call is unconditional on the retry-failure path) and skip
the unit test — do not build elaborate scaffolding for a two-line fix.

**Verify**: `cargo nextest run -p velnor-runner start_job --locked` → pass (or
the note is recorded); `cargo nextest run --workspace --locked` → all pass.

## Test plan

- If feasible: a scripted-runner test that both start attempts fail and cleanup
  commands are issued after the second.
- Otherwise: rely on the code shape + existing start-environment tests.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] A failed retry calls `cleanup_stale` before returning `Err`
- [ ] `cargo nextest run --workspace --locked` exits 0
- [ ] Only `executor.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- `start_job_environment` doesn't match the excerpt (drift).
- `cleanup_stale` has side effects that make calling it twice unsafe (read it
  first) — report.

## Maintenance notes

- This bounds resource leakage to zero-per-failed-start; the deterministic-name
  pre-start cleanup remains the backstop for anything missed.
- Reviewer: confirm `cleanup_stale` is idempotent (safe to call on the
  retry-failure path even if the first cleanup already ran).
