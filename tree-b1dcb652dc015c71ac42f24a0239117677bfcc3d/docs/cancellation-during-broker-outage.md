# Cancellation During Broker Outage

Plan 032 investigated whether Velnor can detect job cancellation while the broker
poller is unreachable by reusing an in-flight run-service call. The answer is no:
the current GitHub runner protocol does not expose a cancellation indicator on
run-service renewal, and Velnor should not infer cancellation from renewal errors.

## Source-of-truth check

The official `actions/runner` implementation was checked at the latest cloned
HEAD on 2026-07-05:

- `src/Sdk/RSWebApi/Contracts/RenewJobRequest.cs` sends only `planId` and
  `jobId`.
- `src/Sdk/RSWebApi/Contracts/RenewJobResponse.cs` defines only `LockedUntil`.
- `src/Sdk/RSWebApi/RunServiceHttpClient.cs` posts the renew payload to
  `renewjob`, returns the typed `RenewJobResponse` on success, and treats
  `404` as job-not-found. It does not parse a cancel state.
- `src/Runner.Listener/JobDispatcher.cs` uses `RenewJobAsync` only to keep the
  job lock alive and logs `renewResponse.LockedUntil`. Cancellation is driven by
  `JobCancelMessage`, which fires the worker cancellation token and sends a
  `CancelRequest` to the worker.

Velnor matches that shape today. `crates/velnor-runner/src/protocol.rs` defines
`RenewJobRequest` as `planId`/`jobId` and `RenewJobResponse` as `lockedUntil`.
`start_run_service_lock_renewal` in `crates/velnor-runner/src/runner.rs` renews
and logs the lock deadline; it has no protocol field that can safely set the
shared `canceled` flag.

## Decision

Do not add a cancellation fallback on run-service renewal. A renewal success only
means the job lock remains valid. A renewal failure can mean an auth problem,
transient network failure, service error, or the job no longer being valid; none
of those is a reliable user cancellation signal. Treating any of them as
`Canceled` would create false cancellations, which is worse than the known blind
spot.

The remaining limitation is explicit: if GitHub sends `JobCancellation` while a
slot's broker polling path is unreachable, Velnor may miss that broker message
and complete the container based on its eventual step result. Closing that gap
requires an accepted new cancellation source, such as a dedicated state poll or a
future protocol field from GitHub; it cannot be solved safely by reusing the
current renew response.
