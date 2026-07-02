# Plan 013: Make lock renewal, completion, and idle polling refresh expired tokens instead of failing

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. Touches the auth/recycle path —
> keep every existing test green.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/protocol.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: 012 (uses the typed status for auth classification)
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

A GitHub Actions OAuth token lives ~1 hour, but jobs can run longer, and idle
slots sit for hours. The **cancellation poller** was deliberately made
refresh-aware (it refreshes on 401/403 mid-job, per its own comment), but three
sibling paths were **not**, so a token expiring mid-job or mid-idle causes
avoidable failures:

1. **Lock renewal + completion never refresh.** The renewal loop captures one
   client forever and only logs on error; `is_retriable_completion_status`
   treats 401/403 as non-retriable, so `complete_job` bails immediately. A job
   longer than the token lifetime loses its ~30s lock (GitHub may reassign it)
   and then fails to complete — even though it ran and produced side effects.

2. **Lock-renewal cadence has no fast retry.** Renewal sleeps a fixed 25s; the
   lock is valid ~30s. A single transient renew failure at t=25s leaves the lock
   expired from ~t=30s until the next attempt at t=50s — a ~20s window with an
   expired lock.

3. **Idle broker-poll auth errors do a full re-registration** instead of an
   in-place refresh (heavier, adds PAT-API churn) — the cheaper fix is to
   refresh the token in place, matching the cancellation poller.

4. **OAuth `expires_in` is discarded**; the proactive idle refresh interval is a
   hardcoded 40 min assuming a ~1h token. A shorter-lived token would expire
   before the proactive refresh fires.

This plan brings the renewal/completion/idle paths up to the cancellation
poller's refresh discipline and derives refresh timing from the real token
lifetime.

## Current state

- Renewal loop, excerpt at `runner.rs:2568-2588`:
  ```rust
  Ok(tokio::spawn(async move {
      loop {
          tokio::time::sleep(Duration::from_secs(25)).await;      // fixed 25s, no fast retry
          match client.renew_job(&run_service_url, &plan_id, &job_id).await {
              Ok(response) => { println!("Renewed ..."); }
              Err(error) => { eprintln!("Run-service job lock renewal failed: {error:#}"); }  // only logs; no refresh
          }
      }
  }))
  ```
  `client` is captured once and never rebuilt with a fresh token.
- Completion retry classifier, excerpt at `protocol.rs:955-957`:
  ```rust
  pub fn is_retriable_completion_status(status: u16) -> bool {
      !(400..500).contains(&status) || status == 408 || status == 429
  }   // 401/403 => non-retriable => complete_job bails
  ```
- The cancellation poller **is** refresh-aware — it takes `stored` and refreshes
  the broker token on auth error mid-job (grep `start_broker_cancellation_poll`
  in `runner.rs` around 2604; comment at `runner.rs:103` explains "a job can
  outlive the ~1h token"). Mirror its refresh mechanism.
- Idle broker poll, excerpt behavior: `poll_broker_message`
  (`runner.rs:~1947-1992`) only counts consecutive errors and sleeps; after
  `BROKER_POLL_MAX_CONSECUTIVE_ERRORS` (10) it bails, triggering
  `cleanup_failed_daemon_slot` + re-JIT (`runner.rs:932`) — full re-registration.
- OAuth exchange discards the lifetime: `exchange_client_credentials`
  (`protocol.rs:427`) returns only `access_token` though `OAuthTokenResponse`
  carries `expires_in` (`protocol.rs:337`). The proactive idle refresh constant
  is `IDLE_TOKEN_REFRESH_SECONDS = 40*60` (`runner.rs:77`).
- Plan 012 provides `GitHubApiError { status }`; use `.status == 401 || == 403`
  for auth classification instead of substrings.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| Tests     | `cargo nextest run -p velnor-runner --locked`                   | all pass        |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/runner.rs` — renewal loop (refresh + fast retry),
  idle broker-poll refresh-on-auth-error, expires_in-driven refresh timing.
- `crates/velnor-runner/src/protocol.rs` — plumb `expires_in` through the OAuth
  exchange; make completion treat post-refresh 401 as retriable (see Step 2).

**Out of scope**:
- The transient-5xx-escalates-to-re-registration concern (broker degradation) —
  note it in maintenance, but the safe minimum here is the auth-refresh cases.
- The cancellation poller — already correct; do not change it, just mirror it.

## Git workflow

- Branch: `advisor/013-mid-job-token-refresh-and-renewal`
- Commit(s): `fix(runner): refresh token on lock renewal/completion/idle auth errors`,
  `fix(runner): derive token refresh interval from OAuth expires_in`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Make lock renewal refresh-aware with fast retry

Thread the credential source (`stored`, as the cancellation poller receives it)
into the renewal task. On a renew error classified as auth
(`GitHubApiError.status == 401/403`), refresh via the same OAuth exchange the
cancellation poller uses, rebuild the `RunServiceClient`, and retry. On any
renew failure, retry after a **short** delay (e.g. 5s, capped, with a failure
streak) rather than waiting the full 25s — so a transient failure doesn't leave
the lock expired for ~20s. Keep the 25s steady cadence on success.

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Make completion refresh once on auth failure

On the completion path, if `complete_job` fails with `status == 401/403`,
refresh the token, rebuild the client, and retry the completion once before
giving up. Implement this at the caller of `complete_run_service_job` /
`complete_job` (do not weaken `is_retriable_completion_status` globally — a 401
is only retriable *after a refresh*, not by blind retry). Add a bounded
"refresh-and-retry-once" wrapper.

**Verify**: `cargo clippy ...` → exit 0.

### Step 3: Idle broker-poll refreshes in place before escalating

In `poll_broker_message`'s error handling, if the error is auth
(`status == 401/403`), refresh the token and rebuild the broker client **in
place** (mirroring the cancellation poller) before counting it toward the
consecutive-error bail. Only if refresh itself keeps failing should the slot
escalate to the existing re-registration path. This narrows full re-registration
to genuine session-gone cases.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 4: Derive refresh timing from `expires_in`

Plumb `expires_in` out of `exchange_client_credentials` (return it alongside the
token; update `oauth_access_token`'s return type/callers). Store the token
acquisition time + lifetime, and compute the proactive idle refresh deadline as
a fraction of the real lifetime (e.g. 60–75%), falling back to the current
`IDLE_TOKEN_REFRESH_SECONDS` constant only when `expires_in` is absent.

**Verify**: `cargo clippy ...` → exit 0.

### Step 5: Tests

Add tests in `runner.rs`/`protocol.rs` `#[cfg(test)]`:
- `renew_fast_retry_delay_is_short`: the renewal failure-retry delay is
  materially shorter than 25s (extract the delay as a pure function to test).
- `completion_retries_after_refresh_on_401`: the refresh-and-retry-once wrapper,
  given a 401 then success, completes; given persistent 401, gives up after one
  refresh (test the wrapper's decision logic as a pure function or with a fake
  client).
- `refresh_deadline_scales_with_expires_in`: given `expires_in = 1800`, the
  computed refresh deadline is ~0.6–0.75 × 1800; absent `expires_in`, it equals
  the fallback constant.
- Model after the existing backoff/classification pure-function tests
  (`supervised_retry_delay_backs_off_and_caps`, `cancellation_poll_backoff_grows_and_caps`).

**Verify**: `cargo nextest run --workspace --locked` → all pass.

## Test plan

- Pure-function tests for: fast-retry delay, refresh-and-retry-once decision,
  expires_in-scaled refresh deadline.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] Renewal refreshes the token on 401/403 and retries quickly (not a fixed 25s) on failure
- [ ] Completion refreshes once and retries on a 401/403
- [ ] Idle broker-poll refreshes in place on auth error before escalating to re-registration
- [ ] Refresh timing derives from `expires_in` with a fallback to the current constant
- [ ] `cargo nextest run --workspace --locked` exits 0; new tests pass
- [ ] Only `runner.rs` + `protocol.rs` (+ tests) modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- Plan 012's `GitHubApiError` is not present (land 012 first) — auth
  classification must key on status, not strings.
- Any excerpt doesn't match (drift).
- Threading `stored`/credentials into the renewal task requires ownership
  changes beyond the renewal spawn and its immediate caller — report the surface.
- Refreshing a run-service token requires a different exchange than the broker
  token and it's unclear which applies — report; the cancellation poller shows
  the working pattern for the broker, confirm run-service uses the same.

## Maintenance notes

- **Do not** reintroduce the zombie-slot behavior: refresh must be bounded — if
  refresh keeps failing, the slot must still eventually escalate/recycle, never
  loop forever "successfully."
- Deferred (note in PR): transient broker **5xx** still escalates to full
  re-registration (`classify_broker_poll` maps all non-2xx to `Error`); a
  cheaper session-recreate-first recovery is a follow-up, out of scope here.
- Reviewer: verify each refresh path has a ceiling and cannot spin; verify the
  run-service vs broker token distinction is correct.
