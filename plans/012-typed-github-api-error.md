# Plan 012: Introduce a typed GitHub API error; stop classifying HTTP failures by string matching

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. This changes control-flow on the
> auth/recycle path — proceed carefully and keep every existing test green.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/protocol.rs crates/velnor-runner/src/runner.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: none (unblocks cleaner 009/013)
- **Category**: tech-debt
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The HTTP status of a GitHub API call is available at the protocol boundary but
is stringified into an `anyhow` message and then **re-parsed downstream by
substring**. The credential-refresh decision matches `text.contains("status=401")
|| text.contains("status=403")`, and a JIT-conflict retry matches
`e.to_string().contains("409")` (which matches `"409"` *anywhere* in the
message). Any wording drift in an error message silently breaks these decisions —
and misclassified HTTP failures are exactly the mechanism behind the documented
"zombie fleet" incident (a slot that looks healthy while GitHub has no
schedulable runner). This plan introduces one typed error carrying the numeric
status, and switches the fragile substring classifiers to match on the status —
a structural fix that removes the enabling condition for that whole bug class.

## Current state

- Stringly classifiers (the fragile control flow):
  - `runner.rs:285`: `Err(e) if e.to_string().contains("409") => { ... }`
  - `runner.rs:2599-2602`:
    ```rust
    fn is_credential_poll_error(error: &anyhow::Error) -> bool {
        let text = format!("{error:#}");
        text.contains("status=401") || text.contains("status=403")
    }
    ```
- The status **is** available where errors are created — `protocol.rs` has ~18
  `bail!("... failed: status={status}, body=...")` sites (e.g.
  `protocol.rs:424,601,642,724,1113,1132,1167`) that already hold a `u16`
  `status`.
- The **good** pattern already in the file: `is_retriable_completion_status(status:
  u16)` (`protocol.rs:955`) and `parse_runner_lookup(status: u16, body: &str)`
  (`protocol.rs:962`) classify on the typed status — mirror this.
- `thiserror` is already a workspace dependency, currently used once
  (`runner.rs:470`, a `#[derive(thiserror::Error)]`). Reuse it.
- Existing tests assert on error substrings (e.g. `runner.rs` tests around 5501,
  `protocol.rs:3405`) — these must be updated in lockstep when the error shape
  changes.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| Tests     | `cargo nextest run -p velnor-runner --locked`                   | all pass        |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/protocol.rs` — define the typed error; return it (or
  attach it via `anyhow` context that is downcastable) from the `curl_json_request`
  callers / bail sites.
- `crates/velnor-runner/src/runner.rs` — switch `is_credential_poll_error` and
  the `409` match to inspect the typed status via `downcast_ref`.
- Tests that assert on the old error strings — update to assert on the status.

**Out of scope**:
- The `GitHubClient` transport trait (that is a separate, larger refactor —
  plan it later; this plan only introduces the error type).
- Swapping curl for a native HTTP client — unrelated.

## Git workflow

- Branch: `advisor/012-typed-github-api-error`
- Commit: `refactor(protocol): typed GitHubApiError; classify HTTP failures by status not string`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Define the typed error

In `protocol.rs`, add:

```rust
#[derive(Debug, thiserror::Error)]
#[error("{action} failed: status={status}, body={body}")]
pub struct GitHubApiError {
    pub status: u16,
    pub action: String,
    pub body: String,
}
```

Keep the `Display` format close to the current `bail!` strings so any log output
reads the same.

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Return the typed error from the API bail sites

At the `bail!("... failed: status={status}, body=...")` sites in `protocol.rs`,
return `GitHubApiError { status, action, body }` instead — via
`anyhow::Error::from(GitHubApiError { ... })` (so callers using `anyhow::Result`
keep compiling, and downstream can `downcast_ref::<GitHubApiError>()`). Do this
for the sites on the auth/recycle path first (401/403/404/409/5xx producers); a
full sweep of all 18 is ideal but if some are non-classification error strings,
converting them is optional — prioritize the ones whose status is later
inspected.

**Verify**: `cargo clippy ...` → exit 0; `cargo nextest run --workspace --locked`
→ all pass (fix any test that asserted the old string; assert on `.status` via
downcast instead).

### Step 3: Switch the classifiers to status

Rewrite `is_credential_poll_error` (`runner.rs:2599`) to downcast and check the
status:

```rust
fn is_credential_poll_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<crate::protocol::GitHubApiError>()
        .is_some_and(|e| e.status == 401 || e.status == 403)
}
```

Rewrite the `409` match at `runner.rs:285` to downcast and compare
`e.status == 409` rather than substring. Keep a conservative fallback only if a
non-typed error can still legitimately reach these points (document it); the goal
is that classification no longer depends on message wording.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 4: Tests

- Add `github_api_error_classifies_by_status` in `runner.rs` `#[cfg(test)]`:
  construct `anyhow::Error::from(GitHubApiError { status: 401, ... })` and assert
  `is_credential_poll_error` is true; status 500 → false; a plain
  `anyhow::anyhow!("status=401 somewhere")` string → **false** (proves it no
  longer matches by substring).
- Update any existing test that asserted the old error string to assert the
  typed status.

**Verify**: `cargo nextest run --workspace --locked` → all pass.

## Test plan

- Classifier-by-status test including the "string that merely contains 401 is not
  matched" case (the regression this prevents).
- Update in-lockstep the tests that asserted error substrings.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] `GitHubApiError { status, action, body }` exists and is returned from the auth/recycle-path API failures
- [ ] `is_credential_poll_error` and the `409` retry classify on `.status` via downcast, not `contains(...)`
- [ ] `grep -n 'contains("status=401"\|contains("409"' crates/velnor-runner/src/runner.rs` returns nothing on the classification paths
- [ ] `cargo nextest run --workspace --locked` exits 0; new + updated tests pass
- [ ] Only `protocol.rs` + `runner.rs` (+ their tests) modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The classifier or bail-site excerpts don't match (drift).
- Converting a bail site to the typed error breaks a caller that pattern-matches
  the error in a way not covered here — report the caller.
- A classification point can be reached by an error that is legitimately not a
  `GitHubApiError` and has no status — report; decide the fallback explicitly.

## Maintenance notes

- This unblocks plan 009 (distinguish step-logic from infra failures) and plan
  013 (refresh on `.status == 401/403`) — both become clean status checks.
- Follow-up (not this plan): a `GitHubTransport` trait returning
  `Result<HttpResponse, GitHubApiError>` would let the planned native-HTTP client
  swap in behind one seam. Record as deferred.
- Reviewer: verify no remaining control-flow decision keys off an error message
  substring (grep `to_string().contains` / `format!("{error` across the crate).
