# Plan 2026-09-13: typed-error pilot at the Docker boundary (GOAL 31/58)

Base: origin/main tip b91f837d. Branch: r0-err-taxonomy. Worktree: /tmp/velnor-errt.

## Problem

Retry decisions at the Docker boundary derive from re-parsing `anyhow` text
instead of from error category + idempotence (GOAL 31 violation). Three sites:

1. `executor.rs::docker_start_error_is_transient` — formats the whole error
   chain (`{error:#}`) and substring-matches 7 daemon/transport needles.
   Consumed by the `start_job_environment` retry loop.
2. `protocol.rs::runner_delete_is_busy_conflict` — lowercases the raw GitHub
   response body and substring-matches the busy vocabulary. Consumed by
   `classify_runner_delete` (already typed outcome) and the supervisor
   quarantine path (already typed downcast match).
3. `cache.rs::reclaim_work_root_with_layout` — `error.to_string()
   .contains("another gc holds the lock")` to detect GC leader contention.

## Fix (this branch, strict scope)

`docker/client.rs` (new vocabulary owner, following the existing
`NotFound` + `is_not_found` precedent):

- `DockerErrorCategory::{Transient, Conflict, Terminal}`.
- `classify_docker_stderr`: the 7 transient needles move here verbatim
  (transient checked first, so transient classification is unchanged) plus
  conflict needles (`already exists`, `already in use`,
  `already in progress`); everything else is terminal (fail closed,
  including exit-124 class-deadline timeouts per `deadline.rs` rationale).
- `DockerCommandError`: typed carrier whose `Display` reproduces the exact
  historical boundary message, so logs and downstream text are unchanged;
  only the category is new.
- `docker_error_category(&anyhow::Error)`: chain traversal returning the
  typed category, or `Terminal` when the boundary never classified the
  error (filesystem/config/unknown must not retry as daemon hiccups).

Attach the typed error, message-identically, at every docker-failure
production point reachable from `start_job_environment_once`:

- `executor.rs::run_docker`, `run_docker_with_env`;
- `Docker::call` job transport (service readiness polls);
- `verify_service_dns`, `verify_bind_mounts` composite bails
  (classified on the failing command's stderr).

`start_job_environment` becomes category-driven; `cleanup_stale` stays
unconditional (every failure still tidies partial state, as today) and only
the retry decision changes:

- Transient: backoff retry, <=5 attempts, 60 s overall deadline backstop
  (per-attempt class deadlines from `docker/deadline.rs` still bound each
  attempt). Idempotent: the previous network guard is retired before
  recreate and stale cleanup removes partial containers/network, so
  re-running start re-creates the same names.
- Conflict: stale cleanup plus a single immediate retry; a second conflict
  proves the conflict is not stale leftovers. Idempotent: cleanup removes
  the conflicting object before the one retry.
- Terminal: fail fast, no retry — same inputs fail identically.

`protocol.rs`: `runner_delete_is_busy_conflict` keeps its signature and the
4-word vocabulary but matches against the parsed JSON envelope message
fields (`message` + `errors[].message`) instead of the raw body, so a URL
or unrelated field cannot fake a conflict. Unparseable 422 fails closed to
the generic `GitHubApiError` path. Supervisor already matches typed
`RunnerBusyConflict` with a single bounded retry; add only the GOAL-31
policy comment (reason/bound/deadline/idempotence).

`cache.rs`: `GcLeaderLock::acquire` maps flock `WOULDBLOCK` to typed
`GcLeaderLockHeld` (errno, not text); the reclaim caller matches via
downcast. Other flock errnos keep context and abort (fail closed).

## Tests

- `docker/client.rs`: classifier unit tests (transient needles moved from
  the executor test, conflict needles, terminal default), message-shape
  preservation, `docker_error_category` chain/default behavior.
- `executor.rs`: new stderr-scripting test runner (wraps `RecordingRunner`
  so existing literals are untouched); new `transient retried then
  succeeds` and `terminal fails fast without retry` tests. The two existing
  retry tests keep names, intent, and call-sequence assertions, with
  failures re-shaped to the conflict category they always described
  (stale leftovers); `start_job_double_failure` stays a double failure
  (conflict, conflict) and still asserts both cleanup passes.
- `protocol.rs`: busy message nested in `errors[]` is a conflict; busy
  words outside message fields are not (pins the false-positive fix).
- `cache.rs`: second `acquire` downcasts to `GcLeaderLockHeld`.

## Follow-ups (untouched string-matched sites, found during this work)

1. `executor.rs::run_docker_remove_container` (~5968): stderr
   `removal of container ... is already in progress` tolerance — attach
   `DockerCommandError` and check `Conflict` instead.
2. `executor.rs::cleanup_stale` (~5891): `to_string().contains("not found")`
   on network removal — convert to a typed chain check.
3. `docker/client.rs::host_call` (~1044): `stderr.contains("already in
   progress")` — `Conflict` category; also attach `DockerCommandError` at
   the host-transport bail (~1047) for uniform categories on maintenance
   paths.
4. `buildkit.rs` (~1045): `detail.contains("no builder")` — needs a
   structured/typed builder-missing signal.
5. `buildkit.rs` / `execution/cancel.rs`: `daemon_reports_missing(&detail)`
   on re-serialized strings — thread typed `NotFound`/`DockerCommandError`
   through instead of re-matching text.
6. `docker/client.rs::daemon_reports_missing` vocabulary itself —
   boundary-local and single-sourced (acceptable); the future Engine-API
   migration should map API error codes instead of stderr text.

Checked and already type-driven, no action: `acquire_failure_is_transient`
(reqwest predicates, `io::ErrorKind`, status codes),
`renew_failure_is_job_gone` (typed `run_service_error_code == 404`),
`node/cleanup.rs::is_not_found` (`io::ErrorKind::NotFound`),
`registration_was_deleted` (chain `.is::<OAuthRegistrationNotFound>()`).

## Verification

`cargo fmt`, `cargo clippy -D warnings`, `velnor-runner` package tests.

## Pre-existing flake (not caused by, not fixed by this branch)

`cache::tests::gc_leader_lock_excludes_second_reaper` fails when run in
parallel with process-spawning tests (its third `acquire`, after
`drop(first)`, still sees the lock held — consistent with fd inheritance
by a `fork`/`spawn` from a parallel test, e.g. the real `docker`
invocations executor drop-guards shell out to). Reproduced on the
unmodified base (`git stash` of `cache.rs`, same filter set, same
failure); passes alone and serialized. Out of scope for this pilot;
needs its own work package (likely: stop shelling real docker from
test-path drops, or make the lock test hermetic).
