# Sentry recovery — 2026-08-26

## Objective

Restore Velnor's Sentry fleet to healthy GitHub admission and prove one real
Docker-backed job creates a live `velnor-job-*` container before teardown.

## Safety boundary

- Read-only diagnosis first.
- Preserve `velnor-actions-fixture`; never weaken its workflow.
- No broad Docker prune, host cleanup, or deletion of healthy registrations.
- GitHub policy mutation only through reviewed `velnor-tools fleet-policy apply`
  plan digest, one organization at a time.
- Do not claim success from systemd `active` alone; require runner health,
  assignment, job result, and live container evidence.

## Iteration checklist

- [ ] Capture Sentry baseline: units, process tree, backend, Docker, health.
- [ ] Audit exact GitHub runner-group/routing drift; save sanitized evidence.
- [ ] Reconcile policy with reviewed digest, starting with Tailrocks fixture.
- [ ] Reconcile stale validation registrations only where ownership is proven.
- [ ] Verify `velnorctl doctor` healthy for the pilot fleet.
- [ ] Cancel old verification runs before dispatching a new smoke run.
- [ ] Dispatch and monitor one sufficiently long fixture job; observe Docker.
- [ ] Verify teardown removes only owned job resources.
- [ ] Repeat admission/health proof for ChainArgos and jackin-project if needed.
- [ ] Record root cause, evidence, commands, and final state here.

## Baseline captured

At 2026-08-26 15:36–15:39 UTC:

- Docker service active; `docker ps -a` empty at idle.
- `/etc/velnor/execution.toml` selects `backend = "docker"`.
- Main daemon restarted at 15:37:26–15:37:46 UTC and reached active supervision.
- Main daemon created and completed a `ci-required` job; teardown finished at
  15:38:30 UTC, explaining the empty Docker listing afterward.
- Health vector reported degraded: GitHub unreachable, routing invalid, runner
  group invalid, zero registered/executor-ready slots.
- Doctor reported `0/N expected runner(s) healthy`; many stale registrations no
  longer existed at GitHub.
- Live policy audit reported workflow-restriction and membership drift in all
  three organizations.

## Iteration 1 result

At 2026-08-26 15:40–15:50 UTC, per-daemon health split the problem:

- `velnor-sentry` (ChainArgos/java-monorepo) is `ready`: 4 desired, 5
  registered, 5 executor-ready.
- `velnor-dogfood` and several repository-scoped pools are also `ready`.
- `velnor-fixture`, `velnor-tailrocks`, `velnor-chainargos`, and
  `velnor-jackin-project` remain degraded primarily on `routing_valid=false`;
  some have zero actual ready slots and queued jobs.
- Sentry has no installed `velnor-tools`, no fleet-policy audit environment,
  and only legacy per-daemon `routing-policy.json` state. Safe Plan 039 policy
  reconciliation cannot run on the host yet.

## Iteration 2 result

Read-only GitHub API inspection using the existing daemon credential confirmed:

- `tailrocks/velnor-trusted` id 3 is selected/public-enabled but
  `restricted_to_workflows=false`, with 21 repositories including stale
  `cloudflare-tofu` and `github-terraform`.
- `ChainArgos/velnor-trusted` id 4 has workflow restriction disabled.
- `jackin-project/velnor-trusted` id 3 has workflow restriction disabled.
- Desired policy digests are unchanged and locally reproducible:
  `tailrocks` `sha256:b9f497117c5a4d6bc13b48ac5dbc857de92f9465df06631fcd3d8cb516e8cd57`,
  `ChainArgos` `sha256:db3edaa1e0f2e058708fb3310bfc5ca9eca8cbe1c71cdeb76e33fe7ab47f68c0`,
  `jackin-project` `sha256:97b13ff43e2132fc92fb34cbea4e34bca9c1754457b2899ece08a858ed39571f`.
- `cargo nextest run -p velnor-tools fleet_policy`: 57 passed, 92 skipped.

Next action requires explicit approval for the exact reviewed GitHub policy
mutation, starting with Tailrocks, because it changes public-code admission.

This disproves Docker as the root cause. The Docker executor path is proven;
the remaining fleet outage is organization routing/admission drift plus stale
JIT registrations.

## Current hypothesis

The empty Docker listing is normal after Velnor's `--rm` job teardown. The
actual availability defect is GitHub control-plane admission/registration
drift. The next iteration must prove whether policy reconciliation restores
healthy slots; Docker preflight already passes.

## Stop conditions

Stop and report if the exact generated policy closure, release-ref ledger, or
operator-reviewed digest is missing; if a live API readback disagrees; or if
the only proposed workaround weakens fixture coverage or silently falls back
between execution backends.

## Iteration 3 — package-path unblock (2026-08-26)

At 2026-08-26 15:49 UTC, sanitized read-only Sentry inspection showed:

- `docker.service` active; Docker server `29.7.2`.
- Installed signed-APT package `velnor-runner 0.1.215`; apt candidate also
  `0.1.215` from `https://velnor-apt.tailrocks.com`.
- `/usr/bin/velnorctl` present; `/usr/bin/velnor-tools` absent.
- No `velnor-tools` package exists in `apt-cache search velnor`.
- The live Tailrocks group readback was `id=3`, `name=velnor-trusted`,
  `visibility=selected`, `allows_public_repositories=true`,
  `restricted_to_workflows=false`; no mutation was performed.

The release path was incomplete: `.github/workflows/release.yml` built only
`velnor-runner` and `velnorctl`, and the Debian asset/guard surface omitted
`velnor-tools`. The narrow fix now builds `velnor-tools`, packages it as
`/usr/bin/velnor-tools`, and guards its presence; the runner-usage note now
matches the package. No GitHub, Sentry, fixture, or policy mutation occurred.

Verification:

- `cargo nextest run -p velnor-tools fleet_policy`: 57 passed, 92 skipped.
- `mise run check`: green; workspace nextest 1372/1372 passed, fmt,
  clippy, actionlint, deny, and fleet generation passed.
- `cargo build -p velnor-tools --release --locked`: green; local release
  binary exists at `target/release/velnor-tools`.
- Reproduced reviewed Tailrocks digest with the tool:
  `sha256:b9f497117c5a4d6bc13b48ac5dbc857de92f9465df06631fcd3d8cb516e8cd57`.

Remaining gate: commit/push this package-path fix through the signed release
workflow, verify signed APT publication and exact candidate on Sentry, then
install via the documented locked apt transaction. Only after that may the
Tailrocks policy plan/audit/apply and fixture verification proceed.
