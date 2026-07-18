# Estate program operator report

Append-only record of autonomous decisions, deviations, safety skips, and
human-only actions for plans 014–015 and 033–059.

## 2026-07-18 — Context7 unavailable

- Decision: the repository requires Context7 for current third-party docs, but
  no Context7 MCP tools are exposed in this execution environment. Continue
  using primary upstream sources only, as the closest safe fallback, and cite
  the source in implementation/PR evidence.

## 2026-07-18 — Plan 015 history purge deferred

- Current HEAD already contains the safe half: no tracked
  `.velnor-compare/*.html` file remains, `.gitignore` rejects such files, and
  `AGENTS.md` contains the never-commit policy.
- Evidence: `git log --all --diff-filter=A --name-only --
  '.velnor-compare/*.html'` identifies addition commit `55ed22fe3162228876c1c5829d3c4d966b28e0d2`.
- Safety decision: do not rewrite or force-push shared history without the
  explicitly required coordinated operator window.
- Human action during an approved window:
  `git filter-repo --path .velnor-compare/velnor-job.html --invert-paths`,
  force-push every rewritten ref, notify collaborators, and require fresh
  clones. Verify with `git log --all --oneline -- '.velnor-compare/*.html'`.

## 2026-07-18 — Plan 039 organization permissions unavailable

- Evidence: both the local authenticated `gh` session and each existing Sentry
  daemon credential returned HTTP 403 for
  `GET /orgs/{ChainArgos,jackin-project,tailrocks}/actions/runner-groups`.
  Repository-scoped fleet inspection still succeeds; all 22 current slots were
  online and idle at the inventory point.
- Code and the migration runbook can land, but live organization registration
  cannot pass until a credential has organization `Self-hosted runners: read
  and write` (fine-grained/GitHub App) or classic `admin:org`, and the runner
  groups plus repository access lists exist.
- Human action: grant the daemon credential that permission in each
  organization, create the restricted trusted/fork runner groups described in
  `docs/org-fleet-migration.md`, then run the documented tailrocks-first smoke.

## 2026-07-18 — Debian deployment process (operator decision)

All live Sentry Velnor deployments must use the Debian standard process: the
change is committed with DCO signoff and pushed, a `.deb` is built from that
exact commit, and the artifact is installed through `apt`. Direct `dpkg -i`
deployment is prohibited. Same-version host validation uses
`apt-get install --reinstall -y ./velnor-runner_<version>_amd64.deb`.

## 2026-07-18 — Deployment-process correction after repository audit

Inspection of `tailrocks/velnor-apt` established that “apt deployment” means
the complete signed-repository path, not a local `.deb` passed to apt. The
authoritative sequence is commit + push → version tag → Velnor
`release-deb.yml` → cross-upload to the matching `velnor-apt` Release →
`publish.yml` signed reprepro/Pages deployment → repository version check →
`apt-get update && apt-get install velnor-runner` on Sentry. The preceding
same-version local-artifact sentence is superseded and must not be used.

## 2026-07-18 — v0.1.35 signed apt deployment evidence

- Source release run `29623974593` passed for amd64 and arm64, attached both
  packages, cross-uploaded them to `tailrocks/velnor-apt`, and dispatched the
  publisher. Signed reprepro/Pages run `29624452820` passed.
- Public evidence: `dists/stable/InRelease` is PGP signed, dated
  `2026-07-18 01:04:15 UTC`, and its amd64 `Packages` entry advertises
  `velnor-runner 0.1.35` with SHA-256
  `16cbbd312a41ecec5e45b538844ed5184f333727ac539fa472a6d6a1e07e42fc`.
- Sentry evidence after `apt-get update`: candidate `0.1.35`; upgraded only by
  `apt-get install -y velnor-runner`; `dpkg-query` reports `0.1.35`. Fixture
  was started first and both broker sessions became ready, then all four other
  daemon units were started. All five units are active; the default daemon
  reports ten supervised slots and capacity-limited slots expose explicit
  backpressure.
- Shutdown deviation/root-cause evidence: four units drained immediately, but
  the default ten-slot daemon removed every JIT registration and local slot
  config, then remained in `deactivating` with no `velnor-job-*` container.
  The installed unit exposed `TimeoutStopUSec=3h`, so after the documented
  two-minute stuck-state investigation the registration-free process was
  killed before apt proceeded. This is a runner drain-completion defect, not
  permission to shorten or bypass a drain while any job/resource remains.
- Hardened-unit score is unchanged after repository deployment:
  `systemd-analyze security` reports `7.9 EXPOSED` for both the default and
  fixture units (pre-hardening baseline was `9.4 UNSAFE`). Final plan 014/059
  scoring and live smoke remain gated on fixture plan 041.

## 2026-07-18 — Plan 033 manifest rollout boundary

- Decision: keep only the fixture's currently observed floating refs as
  explicit, annotated transition entries through plan 041; all other action
  entries use immutable commits resolved from the current estate sweep and
  upstream repositories. No wildcard ref is accepted.
- Estate/local-composite sweep also found action families that intentionally
  remain rejected by manifest version 1, including
  `actions/attest-build-provenance`, plus legacy setup actions slated for
  workflow replacement. They were not silently admitted: plan 042 owns the
  already-approved native attest/composite completion, while estate plans
  remove superseded setup actions. Dispatch remains gated by fixture plan 041.
- Seam deviation: validation reads repository/ref/input values directly from
  the expanded broker job rather than rebuilding repository plans before the
  worker closure. This is the smaller pure-data boundary and runs immediately
  after trust validation, before `execute_script_job` can create `_work`,
  download an action, mutate a cache, or create a container.

## 2026-07-18 — Plan 034 Kache topology decision

- Primary-source inspection of Kache v0.10.0 confirmed that its SQLite index,
  content-addressed blobs, daemon socket, event log, and transfers live below
  one `KACHE_CACHE_DIR`. Velnor therefore binds that whole root as the selected
  20 GiB local store; it never mounts Kache and sccache together.
- The arm64 Ubuntu job-image build completed locally and verified the pinned
  release checksums plus `sccache 0.16.0` and `kache 0.10.0`. Kache remains a
  non-default canary until plan 041 and the representative concurrency/crash
  soak prove the documented shared SQLite/socket lifecycle safe.
- Context7 was required by repository policy but its MCP tools were unavailable
  in this execution environment. The documented fallback used exact upstream
  tags/commits and release assets from the primary repositories instead.

## 2026-07-18 — Plan 040 upstream service-container inventory

- Current `actions/runner` `JobExtension.cs` evaluates the modern
  `AgentJobRequestMessage.JobServiceContainers` mapping and assigns each key as
  the service network alias. `AgentJobRequestMessage.OnDeserialized` confirms
  `Resources.Containers`/sidecars are only the legacy feature-flag fallback.
- `DockerCommandManager.DockerCreate` puts the job and services on the same
  per-job network, adds `--network-alias <service-id>`, and publishes each
  declared port. Container jobs therefore reach `<service-id>:<container-port>`;
  host jobs use the published host port.
- `ContainerOperationProvider.StartContainer` waits for health, then populates
  `job.services.<id>.id`, `.network`, and `.ports[container-port]` from Docker's
  actual mapping. It does not synthesize service host/port environment
  variables. Velnor now matches that surface and preserves legacy broker input
  only as a fallback. Fixture Postgres acceptance remains plan 041's V-A gate.
