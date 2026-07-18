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

## 2026-07-18 — v0.1.36 deployment and drain-backoff defect

- Source release run `29626083112` passed for amd64 and arm64. Signed apt
  publisher run `29626443164` passed; isolated `gpgv` verification reports a
  good `InRelease` signature from the Velnor APT key, and both architecture
  indexes advertise `0.1.36`.
- Sentry upgraded only after `apt-get update` exposed candidate `0.1.36`, then
  `apt-get install -y velnor-runner`; `dpkg-query` confirms `0.1.36`. No local
  package or direct binary deployment was used.
- The default 0.1.35 daemon did not finish SIGTERM drain: forensic logs prove
  slots 1 and 7 were idle but sleeping in capacity-backpressure retry delays
  capped at ten minutes. Eight other slots deregistered immediately, no Velnor
  job container remained, and systemd correctly kept the process in
  `deactivating`; it was not force-killed.
- Root cause: slot retry sleeps did not observe the global drain flag. The
  0.1.37 fix checks drain at most once per second across disk-pressure,
  local-failure, post-reconfigure, and JIT-reconfigure backoffs. Full runner
  gates passed (568 nextest tests, fmt, clippy, actionlint). Live drain proof
  will run after 0.1.37 arrives through the same signed apt chain.

## 2026-07-18 — Fixture 0.1.37 image-freshness failure

- Clean Velnor-only fixture run `29627143419` used runner 0.1.37 and failed
  the Kache job immediately because the host's pre-existing
  `velnor/job-ubuntu:26.04` image did not contain `kache`; remaining jobs were
  canceled. The fixture was unchanged.
- Root cause: the Debian package updated the runner binary and units but did
  not ship or refresh the job image even though native adapter versions are
  coupled to tools baked there. This allowed a new runner to advertise a
  capability its deployed image could not execute.
- Fix: the package now ships the canonical Dockerfile, labels the built image
  with the package version, and `postinst` builds a mismatched image before it
  restarts any daemon. A build failure fails the apt transaction instead of
  silently running a stale image. Full gates passed (570 nextest tests, fmt,
  clippy, actionlint, and postinst shell syntax).

## 2026-07-18 — Fixture 0.1.38 service/Kache contract failures

- Source release run `29627247170` and signed apt publisher run `29627608735`
  passed. Isolated `gpgv` verification reported a good signature, both public
  architecture indexes advertised `0.1.38`, and Sentry installed it only with
  `apt-get update && apt-get install -y velnor-runner`. Package, image label,
  Kache, and sccache verified as `0.1.38`, `0.1.38`, `0.10.0`, and `0.16.0`.
- Clean fixture run `29627820008` passed the former missing-Kache-image point,
  then exposed two runner defects: the Postgres service key was not resolvable
  through Docker DNS, and the native Kache action did not persist
  `RUSTC_WRAPPER=kache` for the following step. Remaining jobs were canceled;
  fixture semantics were unchanged.
- Root causes: service network/alias arguments preceded expanded service
  options instead of being final runner policy, and native shell adapters do
  not collect the `GITHUB_ENV` file they create. The fix makes the owned
  network/alias the final Docker options and returns Kache's three environment
  values directly as action state. Regression tests cover both enabling
  structures; the complete runner crate suite passed (539 tests).
- Host-window decision: capacity admission correctly kept fixture slot 2
  offline while the other fleets reserved more than 600 GiB. Registry checks
  proved the blockchain-nodes, agent-brown, and jackin fleets idle, so those
  three Velnor-owned daemons were temporarily drained for the fixture campaign.
  The default fleet was left running because one registration was busy.
- v0.1.39 source run `29628055096` and publisher run `29628454180` passed;
  public signature/index and the Sentry apt installation verified `0.1.39`.
  Fixture retry `29628618869` still could not resolve `postgres`, proving the
  first network fix was incomplete; it was canceled after evidence capture.
  The remaining enabling structure was identical on the job side: expanded
  job options followed its runner network argument. v0.1.40 makes the owned
  network final for both job and service containers and adds separate
  precedence regressions for both paths.
- v0.1.40 source run `29628766170`, Debian delivery run `29628766147`, and
  apt publisher run `29629164057` passed. The public InRelease signature
  (key `CD4693750A4BA4F12BC9ABFD857FCD279679A34B`) and both architecture
  indexes verified `0.1.40`; Sentry then installed `0.1.40` through apt and
  verified the package-built image and pinned tools. Fixture run
  `29629359514` still failed service DNS, falsifying create-option precedence
  as the complete cause. A direct Sentry Docker bridge/alias probe resolved
  `postgres`, isolating the defect to Velnor's implicit create-time topology.
  v0.1.41 explicitly reconciles the runner-owned job endpoint and service DNS
  aliases after container creation and before the first workflow step.
- v0.1.41 source run `29629595781`, Debian delivery `29629595771`, and apt
  publisher `29629951741` passed; public signature/index and Sentry package,
  image, pinned tools, and hardened-unit score verified. Fixture run
  `29630088620` proved both Kache and sccache jobs green but service DNS still
  failed, falsifying endpoint reconciliation as the complete cause. v0.1.42
  adds a pre-step DNS assertion that records sanitized network membership,
  aliases, and resolver state on the live job before cleanup; this replaces
  further speculative service fixes with direct evidence.

## 2026-07-18 — Fixture 0.1.42 runtime-credential argv exposure

- v0.1.42 source run `29630247538`, Debian delivery run `29630247507`, and
  apt publisher run `29630609981` passed. The public InRelease signature and
  both architecture indexes independently verified `0.1.42`; Sentry installed
  it only through `apt-get update && apt-get install -y velnor-runner`, then
  package, image label, and pinned tools were verified.
- During clean fixture run `29630764634`, host diagnosis of an active setup
  step revealed that a GitHub Actions runtime credential was present in the
  `docker exec` process argument vector. The credential value is deliberately
  omitted. The run was canceled immediately, the fixture daemon stopped, and
  all fixture runner registrations deleted; no further job was dispatched on
  the affected version.
- Root cause: the secure env-file transport classified secrets only by values
  already present in the mask set. Protocol-provided runtime credentials were
  not guaranteed to be members of that set. JavaScript and Docker action
  sidecars also retained older inline environment construction. v0.1.43 makes
  credential names independently secret, applies env-file transport to every
  execution path, and rejects rather than exposes multiline secrets. Regression
  tests prove unmasked runtime credentials never occur in process arguments.

## 2026-07-18 — v0.1.43 apt handoff rate-limit failure

- Source release run `29631069587` passed. Debian run `29631069580` built
  both architectures and attached the packages to the source release, but its
  final `velnor-apt` handoff failed after the GitHub API core quota for the
  service identity reached 5,000/5,000 requests. The API reported reset at
  `2026-07-18T05:45:04Z`; no package was installed directly or outside apt.
- Decision: do not bypass the signed repository. The next package release
  includes the completed Pages protocol work already in flight; retry the
  normal `velnor-apt` handoff after reset and deploy that superseding version
  through `apt-get update && apt-get install`.

## 2026-07-18 — Plan 042 Pages protocol completion

- The planned-code drift check showed substantial expected program drift
  (`action.rs` 39 lines and `executor.rs` 678 lines since `48b04ad`). Symbols
  and scope boundaries remained identifiable, so the progress-STOP fallback
  was to re-locate by symbol and continue without reverting intervening work.
- `actions/deploy-pages@v5` now mirrors the current upstream flow: Results
  Service V2 artifact lookup by backend identity, OIDC token acquisition,
  Pages deployment creation, bounded status polling, cancellation on timeout
  or repeated errors, preview payload, and real outputs. Artifact upload now
  propagates the database ID returned by `FinalizeArtifact` instead of a local
  placeholder. A five-endpoint protocol test covers the successful loop.

## 2026-07-18 — Plan 043 drift resolution

- The lifecycle drift check against `48b04ad` reported 1,437 changed lines in
  `runner.rs` and `executor.rs`, including intervening P0 capacity, service,
  security, and Pages work. Per the progress-STOP rule, implementation is
  proceeding from re-located lifecycle symbols while preserving the current
  never-abandon, renewal, and capability-validation invariants.

## 2026-07-18 — Linux package and deployment documentation audit

- Verified `docs/debian-apt-repo.md` against Velnor's `release-deb.yml`, the
  live `tailrocks/velnor-apt` publisher, and its repository README. The source
  workflow builds and guards amd64+arm64 packages, hands them to a same-tag apt
  release, and dispatches the signed reprepro/Pages publisher. Hosts verify the
  indexed candidate and install or roll back only through APT.
- The apt repository README incorrectly pointed to the former personal GitHub
  namespace and implied that the token belonged in `velnor.env`. Commit
  `81255c1` on its single `velnor-estate-standard` branch corrects those facts,
  documents the full operator deployment sequence, and is pushed as
  `tailrocks/velnor-apt#9`.
- Removed a stale sentence that described v0.1.38 as the next candidate. The
  linked v0.1.37 runs remain useful historical evidence; candidate selection is
  now explicitly dynamic through `apt-cache policy`.

## 2026-07-18 — Plan 044 drift resolution

- The prescribed drift check against `48b04ad` reports 1,988 changed lines in
  `executor.rs`, `runner.rs`, and `storage.rs`, chiefly from the already landed
  capability, storage, service, adapter, security, and lifecycle plans. The
  mirror, checkout-copy, and storage-class symbols remain independently
  identifiable. Per the program's progress-STOP rule, plan 044 proceeds by
  symbol relocation without reverting the intervening contract work.

## 2026-07-18 — Plan 045 drift and queue-time fallback

- The timing-plan drift check reports 1,789 changed lines in `runner.rs` and
  `executor.rs`; `telemetry.rs` and `slot_log.rs` remain unchanged from the
  plan base. Lifecycle and adapter symbols are still identifiable, so work
  proceeds by symbol relocation under the progress-STOP fallback.
- The current V2 acquired-job reference and expanded job message carry lease
  expiry (`locked_until`) but no authoritative GitHub queued-at timestamp.
  Following plan 045's documented fallback, the timing record omits queue wait
  rather than deriving a false value; doctor will use the locally measured
  broker-message-to-acquire/pickup interval.

## 2026-07-18 — Release tooling compile defect

- v0.1.47 Debian run `29632781256` remained in `Install cargo-deb` on both
  architectures for more than two minutes. Investigation showed the
  `cargo:cargo-deb` mise backend had no external cargo-binstall available and
  therefore fell back to `cargo install`; the workflow comment claiming it
  used a prebuilt tool was false. The stale run was canceled.
- Current mise documentation confirms `cargo.binstall_only=true` forbids the
  source-build fallback and `cargo.binstall_quickinstall=true` permits the
  external prebuilt artifact host. A local forced dry run with pinned
  cargo-binstall 1.21.0 verified the signed cargo-deb 3.7.0 artifact for the
  host platform. The release workflow now installs cargo-binstall through
  mise first and fails closed if no prebuilt cargo-deb exists.

## 2026-07-18 — Apt deployment and hardened-unit live evidence

- Release-deb run `29633354819` built and delivered v0.1.49 for amd64 and
  arm64; apt publisher run `29633535644` regenerated and signed the repository.
  The public amd64 index advertised `0.1.49` with SHA-256
  `015b8b4c8a3e6cb62d676306e8109db599f4310b5cf5c9bab17509e7f918949b`.
- Sentry accepted non-interactive key-only SSH as root. The host was drained,
  upgraded from 0.1.42 to 0.1.49 exclusively with `apt-get`, rebuilt the
  canonical job image from the package, and restarted successfully. No
  1Password agent or approval was used.
- `systemd-analyze security velnor-daemon.service` reports 7.9 EXPOSED after
  the conservative plan-014 sandbox. The required directives are live:
  `NoNewPrivileges=yes`, `PrivateTmp=yes`, `ProtectSystem=full`,
  `ProtectHome=read-only`, kernel/control-group protections, and
  `RestrictSUIDSGID=yes`. Root, host networking, devices, and Docker-compatible
  namespace/syscall access remain deliberately open per plan scope.
- Hardened-unit fixture run `29633739080` proved Docker startup, bind mounts,
  cache adapters, live/result logs, completion-before-teardown, and JIT overlap;
  the cache jobs plus both Rust matrix jobs completed green. Its PostgreSQL job
  exposed a runner protocol defect: current V2 service scalar tokens use `lit`,
  while Velnor decoded only legacy `value`. Commit `1586297` fixes the decoder;
  the workflow was not weakened. The same run exposed missing storage identity
  because current V2 carries `github.repository` in ContextData rather than
  Variables; commit `64366d5` normalizes that current representation for the
  existing storage identity boundary.
- Live lifecycle evidence from that run shows completion posted before detached
  teardown, new JIT configuration ready while teardown was still active, and
  subsequent jobs acquired cleanly on both slots. This closes plan 043's live
  acceptance in addition to its full 588-test gate.
- The canonical mirror is live at
  `/var/cache/velnor/v1/trusted/git-mirrors/tailrocks__velnor-actions-fixture.git`
  (172 KiB during the fixture campaign). It has no persisted `remote.origin.url`
  and therefore no credential-bearing remote. Trace close events measured warm
  checkout at 407–454 ms, under plan 044's one-second acceptance threshold.

## 2026-07-18 — Fixture Kache cross-lane fallback

- GitHub-lane run `29634562086` proved the pinned upstream Kache action
  deliberately skips `RUSTC_WRAPPER` when both GitHub cache and S3 are
  disabled. Enabling either backend would expand the approved local-only
  capability surface, so no remote backend was enabled.
- Plan 041 explicitly accepts three-lane proof with `cache-kache` disabled.
  Its literal `if: false` form is rejected by actionlint's `if-cond` rule, so
  the closest working expression is the false repository-identity guard
  `github.repository == 'tailrocks/velnor-kache-canary-disabled'`. The job and
  its assertions remain visible and unchanged for a future separately approved
  backend decision.
- Clean GitHub-only fixture run `29634753256` passed with the Kache canary
  skipped. No fixture workload or successful assertion was weakened.
- Clean Velnor-only run `29634781889` and combined parity run `29634836133`
  also passed. The combined run completed `compare-results` successfully,
  proving the GitHub and Velnor job/step result sets remain equivalent.

## 2026-07-18 — Plan 047 target drift resolution

- The java-monorepo plan was written against `7b3dfdb3`; current `origin/main`
  is `18e534e2`. The scoped drift is 85 insertions and 36 deletions across the
  five existing workflow files plus `.github/AGENTS.md`, primarily refreshed
  pins and lane prose. All plan-named defects and symbols remain present.
- Under the program's progress-STOP override, work proceeds from current main
  by symbol relocation. The user's existing
  `feature/lightdash-migration-finish` checkout remains untouched; the single
  program branch lives in a separate worktree at
  `/Users/donbeave/Projects/ChainArgos/java-monorepo-velnor-estate`.

## 2026-07-18 — Plan 046 audit and compare evidence

- The new audit intentionally reports the fixture's remaining floating action
  refs and non-uniform workflow names instead of treating plan 041's lane proof
  as a static-contract waiver. Plan 058 owns that reconciliation; weakening the
  audit would contradict the latest/freshness and uniform-shape laws.
- `audit-ci` found the expected pre-migration violations in holla and the
  expected remaining warnings/errors in java-monorepo; after plan 047's static
  edits, java-monorepo has no ERROR except concerns deferred by the plan's
  explicit workflow-name scope.
- Fixture both-lane run `29636145660` passed. `velnor-tools compare --class d`
  reported every Velnor job inside 60 seconds and no information-loss rows,
  including exact `Initialize containers` and `Stop containers` service steps.
  Sanitized logs, job JSON, and the report are committed under
  `.velnor-compare/lane-compare-run-29636145660/`; no rendered HTML is present.

## 2026-07-18 — java-monorepo paths-filter runner defect

- Program-branch push runs `29636133868` and `29636133883` failed because the
  native paths-filter adapter diffed base `18e534e2` against head `dbbab7f8`
  after shallow checkout had fetched only the head. Sentry journal evidence was
  `fatal: Invalid revision range ...`; the workflow was not weakened.
- Upstream dorny/paths-filter v4 fetches both refs at depth 10 and uses the
  merge-base (`base...head`) comparison. Velnor now matches that behavior in
  `97e84e4`, with a shallow-fetch regression test in `5092237`. v0.1.54 is
  being delivered through the signed apt chain before the controlled rerun.

## 2026-07-18 — Plan 048 target drift resolution

- blockchain-nodes advanced from planned `d156fe4` to `47d0a65`. The scoped
  diff is four insertions and four deletions in `build-publish.yml` (dependency
  pin refresh); all plan-named structures remain present. Work proceeds from
  current main on the sole `velnor-estate-standard` branch in the separate
  `/Users/donbeave/Projects/ChainArgos/blockchain-nodes-velnor-estate`
  worktree, leaving the user's main checkout untouched.

## 2026-07-18 — Plan 048 single-writer fallback

- blockchain-nodes' existing Rust helper inseparably performs Buildx
  `--push` plus manifest publication, while plan 048 keeps that helper out of
  scope. The closest contract-conformant `both` implementation uses the
  helper's existing `build-dry` task on the non-writer lane and the real
  `build` task only on the writer lane. Both lanes retain the same workflow
  step set; the secondary validates the exact generated commands without
  Docker Hub mutation. PR #651 records this prominently.

## 2026-07-18 — Acquired-job rejection recovery

- java-monorepo run `29636133868` attempt 2 exposed unsupported
  `docker/setup-buildx-action` input `cleanup: false` (manifest v2 accepts
  `name`, `driver`, and `install`). The unnecessary override was removed in
  `dd508dbf`; no capability was silently expanded.
- The strict pre-execution rejection escaped after GitHub marked the JIT
  runner busy, so the run stayed in progress and the registration could not
  be deleted (HTTP 422). Normal cancel plus the force-cancel endpoint were
  attempted; after GitHub released the registration the stale runner was
  removed and the run concluded cancelled. Commit `3d0077d` now posts an
  explicit failed completion for step-mapping, trust-policy, and capability
  validation failures before returning the runner error. v0.1.55 carries the
  fix through apt.

## 2026-07-18 — Plan 059 completion

- The committed host denominator is
  `docs/host-baseline-2026-07-18.md`: root use 36%, zero persistent
  `unknown-repository` identities, no stale owned containers/networks, and no
  deletion outside exact Velnor-owned paths. Fixture both-lane run
  `29636145660` is the final post-cleanup smoke and parity proof.

## 2026-07-18 — Plan 045 live timing evidence

- Apt-installed v0.1.55 doctor read five fixture records from the named daemon
  logs and printed p50/p95 values. Pickup (917 ms p95) and teardown (1,715 ms)
  pass; pickup-to-first-step (15,138 ms, driven by the PostgreSQL service cold
  start) and finalize (2,775 ms) warn against their 5,000/2,000 ms defaults.
  Plan 045's observability acceptance requires truthful warnings, not masking
  or retuning measured breaches; the estate performance campaign retains them.
- The same host trace contains all six stable spans: `job-pickup`,
  `job-checkout`, `job-container-boot`, `job-steps`, `job-finalize`, and
  `job-teardown`. Fixture run `29636145660` carries adapter post steps and the
  versioned lifecycle `job-timing` records used by compare and doctor.
### 2026-07-18 — Documentation lookup fallback and estate runner defects

- Context7 MCP was not exposed in this execution environment while validating
  current mise installation behavior. I used mise's official documentation and
  proved the Aqua-backed `cargo-nextest` install locally; this avoids the cargo
  backend's source-build fallback in CI.
- Java run <https://github.com/ChainArgos/java-monorepo/actions/runs/29636991525>
  was canceled after the GitHub lane spent more than five minutes compiling
  `cargo-nextest`; the program branch now installs its pinned prebuilt Aqua
  artifact instead.
- Blockchain run <https://github.com/ChainArgos/blockchain-nodes/actions/runs/29637290031>
  proved Velnor validated the raw `${{ matrix.package == 'heimdall' }}` checkout
  `lfs` input. Current `actions/runner` evaluates step inputs against expression
  context before action handling (`Runner.Worker/ActionRunner.cs`); Velnor now
  follows that order and tests the exact estate expression.
- Local GPG tag signing for `v0.1.57` could not use the configured curses
  pinentry in this non-interactive session (`Inappropriate ioctl for device`).
  The release uses an annotated tag; every source commit retains DCO signoff,
  and server package authenticity is verified by the repository-scoped signed
  apt `InRelease` chain. No 1Password approval or secret export was requested.

### 2026-07-18 — Plan 048 blocked by lane-independent package sources

- Velnor run <https://github.com/ChainArgos/blockchain-nodes/actions/runs/29637999142>
  and GitHub run <https://github.com/ChainArgos/blockchain-nodes/actions/runs/29638091607>
  pass sweep/base/build and fail the same three leaf packages. This proves
  runner parity and exhausts the plan's GitHub-lane fallback.
- `arbitrum`: upstream tag `offchainlabs/nitro-node:v3.11.2-db30ef0` does not
  exist. `fraxtal-op-node`: its upstream image has no `/bin/sh`, but the
  repository Dockerfile executes `RUN ls`. `op-node`: the selected source is
  missing generated `op-core/superchain/superchain-configs.zip`.
- Plan 048 explicitly excludes Dockerfiles and package source/configuration.
  Changing workflow semantics would mask the repository defects, so the static
  estate branch/PR remains pushed but unmerged. Resolve those three package
  sources in `ChainArgos/blockchain-nodes`, then rerun the documented three
  lanes; no Velnor capability expansion is needed.

### 2026-07-18 — Apt-only v0.1.58 deployment and campaign capacity

- Java verification exposed a legacy Cargo checkout whose matching bare
  repository had already been reclaimed. Commit `cfdc931` repairs that
  runner-owned orphan under an atomic lock before Cargo executes. All 613
  workspace tests and static gates passed; source release run
  <https://github.com/tailrocks/velnor/actions/runs/29638641012> and signed apt
  publication run
  <https://github.com/tailrocks/velnor-apt/actions/runs/29638791029> are green.
- Sentry upgraded from 0.1.57 to 0.1.58 exclusively with `apt-get update` and
  `apt-get install velnor-runner`. The signed repository advertised candidate
  0.1.58, `dpkg-query` reports 0.1.58, the rebuilt job image OCI version is
  0.1.58, and the Java and dogfood systemd instances restarted active.
- The dedicated dogfood daemon uses five slots. The Java campaign daemon was
  reduced from ten to four slots so 30-GiB per-slot reservations fit the
  recorded host capacity; blockchain and fixture instances remain stopped
  until their own controlled windows. This is fleet scheduling, not workflow
  weakening.
- Three Testcontainers PostgreSQL containers (`eager_johnson`,
  `zen_chandrasekhar`, and `affectionate_noyce`) have no Velnor ownership
  labels. Plan 059's owned-resource-only safety rule forbids guessing, so they
  were not deleted.
- Plan 050 removes the old macOS release artifacts because the binding estate
  standard is Ubuntu-only. Linux amd64/arm64 artifacts retain identical
  dual-lane jobs and single-writer publication; this is a documented scope
  substitution, not a runner workaround.

### 2026-07-18 — Java parity and cross-slot artifact root causes

- Java Velnor run
  <https://github.com/ChainArgos/java-monorepo/actions/runs/29638934505>
  failed Clippy under the image-baked Rust 1.97.0 while GitHub run
  <https://github.com/ChainArgos/java-monorepo/actions/runs/29639140341>
  passed with the repository-pinned 1.97.1. The native mise adapter had placed
  `/root/.cargo/bin` before mise's selected toolchain after setup. Commit
  `b28cf0f` reverses that precedence after installation while retaining the
  baked toolchain only as a fallback; the workflow remains unchanged.
- v0.1.59 release run
  <https://github.com/tailrocks/velnor/actions/runs/29639441884> proved a second
  runner defect: upload-artifact finalized both packages in Results Service,
  but download-artifact searched only the consuming slot's local filesystem.
  The earlier one-slot unit fixture masked this class. Native downloads now
  follow the official current artifact toolkit path: ListArtifacts,
  GetSignedArtifactURL, credential-safe zip download, and path-safe extraction.
- Plan 051 stated that Holla's release artifact set should stay unchanged,
  while higher-priority `VELNOR_PROJECTS_SETUP.md` §2.0/§2.2 forbids macOS
  execution and requires Ubuntu cross-build or a proven blocker. Apple targets
  require the Apple SDK and cannot be produced by Ubuntu zigbuild, so PR #36
  removes macOS artifacts/formula blocks and preserves Linux amd64/arm64
  tarball, Homebrew-on-Linux, and Debian delivery. The deviation is also
  prominent in the PR description.
- The configured GPG tag key requires an interactive pinentry. Because this
  campaign forbids operator approval, v0.1.60 uses an annotated tag; every
  program commit still carries the required DCO signoff. No credential or
  signing prompt was bypassed.
- Bootstrap recovery used the workflow's GitHub lane because the installed
  v0.1.58 runner could not consume its own cross-job release artifacts. Debian
  release run <https://github.com/tailrocks/velnor/actions/runs/29639975111>
  and signed apt publication run
  <https://github.com/tailrocks/velnor-apt/actions/runs/29640107569> passed;
  Sentry then upgraded exclusively through apt to package and image 0.1.60.
- Java run <https://github.com/ChainArgos/java-monorepo/actions/runs/29640241554>
  exposed the remaining mise root cause. The adapter injected `CARGO_HOME` and
  `RUSTUP_HOME` only while mise installed tools, causing mise's Rust backend to
  publish the image-baked rustup proxy path and select 1.97.0. A controlled
  v0.1.60 image reproduction selected 1.97.1 when those non-upstream overrides
  were absent. The fix matches current `jdx/mise-action`: retain truthful job
  step storage variables, but do not override mise's own Rust resolution.
- Java v0.1.61 run
  <https://github.com/ChainArgos/java-monorepo/actions/runs/29640840497>
  proved the earlier override had also persisted a poisoned, Velnor-owned
  install link: `_velnor_mise/.../rust/1.97.1 -> /root/.cargo/bin`, with aliases
  retaining it. The adapter now removes only Rust/tool links targeting the two
  known container-local Cargo proxy paths and their dangling aliases before
  reinstalling. Valid installs and non-Velnor paths are untouched.
- The v0.1.62 live fallback removed only the validated Java Rust links after
  GitHub run `29641214190` was cancelled and `velnor-daemon.service` stopped.
  The graceful stop remained in `deactivating` despite the run being fully
  cancelled; with no active Java run, only that Velnor daemon was SIGKILLed
  (dogfood remained active). This is additional drain-bug evidence.
- The deeper toolchain root cause is storage topology: mise persists its Rust
  selection under `/opt/mise`, while rustup stores the compiler under
  `/root/.rustup`. Only the first path was durable, so every new job container
  reverted to image Rust 1.97.0. The runner now seeds and mounts rustup in the
  same trust/repository-scoped mise class; no workflow semantics changed.
- Java Velnor run
  <https://github.com/ChainArgos/java-monorepo/actions/runs/29642010121>
  proved the persisted 1.97.1 rustup store was healthy but later steps still
  selected 1.97.0. Inspection of the current official `jdx/mise-action`
  identified the missing protocol behavior: after installation it exports
  every non-`PATH` string from `mise env --json` and masks values reported by
  `mise env --redacted --json`. The native adapter now performs the same
  operation through mode-0600 files on the mounted job temp directory, then
  deletes them; environment documents never enter the live or uploaded log.
- v0.1.64 source release runs
  <https://github.com/tailrocks/velnor/actions/runs/29642286481> and
  <https://github.com/tailrocks/velnor/actions/runs/29642286484> built both
  architectures successfully, then failed delivery because the baked `gh`
  shim had no executable in the repository-scoped install mount. The image
  seed marker incorrectly lived in the daemon-shared mise cache, so the first
  repository marked every other repository as seeded. The marker now lives in
  the same trust/repository-scoped executable store it governs; workflow
  commands and release semantics remain unchanged.
- Recovery v0.1.65 Debian run
  <https://github.com/tailrocks/velnor/actions/runs/29642522113> and signed apt
  publication run
  <https://github.com/tailrocks/velnor-apt/actions/runs/29642646740> passed on
  the documented GitHub bootstrap lane. Sentry advertised candidate 0.1.65
  and was upgraded from 0.1.63 exclusively with `apt-get update` and
  `apt-get install velnor-runner`; package, rebuilt job image, and active Java
  and dogfood services all report 0.1.65.
- The parallel general release run
  <https://github.com/tailrocks/velnor/actions/runs/29642523160> built both
  tarballs, then raced the Debian delivery job that had already created the
  same GitHub Release. The general release writer is now idempotent: it creates
  the release only when absent and uploads its own assets with `--clobber`, so
  both release workflows can safely converge on one tag.
- Java Velnor run
  <https://github.com/ChainArgos/java-monorepo/actions/runs/29642797679>
  confirmed mise exported `RUSTUP_TOOLCHAIN=1.97.1`, but Clippy linted the
  entire workspace while GitHub run
  <https://github.com/ChainArgos/java-monorepo/actions/runs/29642876348>
  selected the nine path-filtered packages and passed. A recoverable rename of
  only the inactive Velnor-owned format/lint target bucket followed by run
  <https://github.com/ChainArgos/java-monorepo/actions/runs/29643164464>
  disproved target-cache contamination. The native paths-filter adapter was
  incorrectly diffing `HEAD` against itself on `workflow_dispatch`; current
  upstream defaults to the repository default branch and its merge base. The
  adapter now fetches that remote-tracking ref and uses the upstream diff
  boundary. The quarantined bucket remains at
  `/var/lib/velnor/work/_velnor_targets/trusted/ChainArgos_java-monorepo/.github_workflows_rust.yml/Format_and_lint__Velnor_.quarantine-pre-1.97.1`
  until the succeeding proof permits safe removal.
- v0.1.66 Velnor Debian run
  <https://github.com/tailrocks/velnor/actions/runs/29643421015> and signed apt
  publication run
  <https://github.com/tailrocks/velnor-apt/actions/runs/29643516294> passed.
  Sentry was upgraded exclusively through the signed apt source; package and
  job image report 0.1.66 and the default/Java and dogfood services are active.
- Deleting two dogfood registrations that GitHub reported offline while their
  daemon was active exposed a self-heal defect: the idle slots retained their
  now-invalid registration IDs and retried `invalid_client` instead of
  re-registering. The three active release builds were not interrupted; the
  parallel tarball run `29643421010` was cancelled, the Debian delivery was
  allowed to finish, and the dogfood instance was then restarted to recreate
  all five slots. Future stale cleanup distinguishes registrations belonging
  to a currently active daemon from abandoned registrations. Durable automatic
  recovery of a registration deleted externally remains runner follow-up.
- Corrected Java Velnor run
  <https://github.com/ChainArgos/java-monorepo/actions/runs/29643642046>
  passed every job. Its default-branch diff selected the same nine package
  gates as GitHub and format/lint passed. The recoverable target-bucket rename
  had disproved cache contamination; after this green proof, the explicit
  Velnor-owned quarantine path recorded above was removed.
- Holla PR <https://github.com/tailrocks/holla/pull/36> merged after Velnor
  run `29644699959`, GitHub run `29644855730`, combined parity run
  `29645094502`, and the PR required-check rerun `29639626469` all passed.
  The initial combined run revealed that the new per-repository Holla daemon
  lacked `VELNOR_CARGO_TARGET_PERSIST=1`; its otherwise green Velnor lane
  rebuilt dependencies. The trusted Holla instance was corrected with an
  explicit `VELNOR_TRUST_SCOPE=trusted` and persistent targets, without a
  workflow semantic change. Warm population run `29645253465` passed and
  unchanged acceptance run `29645393337` completed in 57 seconds with zero
  dependency downloads/compiles and zero tool installs (only the local Holla
  crate rebuilt). Attempt `29645326167` hit one non-repeating Holla
  filesystem-mtime test failure after three green lane proofs; the unchanged
  retry passed, establishing a repository test flake rather than a runner
  divergence. The Holla daemon then drained and deregistered every slot; PR
  and program branches were deleted and the repository now ends on `main`.
- Ruxel required-check run `29644979281` exposed a base-image parity defect:
  its existing static-musl assertion invokes the standard Ubuntu `file`
  utility, but the Velnor Ubuntu 26.04 image omitted that package. The runner
  image now installs `file`, and daemon image preflight asserts it alongside
  sh, bash, and git so this class fails before accepting work. All runner
  gates passed (618 tests). Release v0.1.67 is the apt delivery carrying the
  corrected image; Ruxel verification resumes only after that package is
  installed through the signed repository.
- The corrected image revealed a deeper portability defect in Ruxel runs
  `29646127544` and `29646441642`: Cargo wrote the valid static artifact to
  Velnor's synthetic `/__cargo_target`, while the unchanged workflow checked
  GitHub's ordinary `target/...` path. GitHub run `29646219377` passed the
  same job. Root cause was runner-visible optimization state: persistent
  targets changed `CARGO_TARGET_DIR` and therefore observable filesystem
  semantics. Velnor now mounts the scoped durable bucket directly at
  `/__w/target`, exports no synthetic target variable, and tracks persistence
  only in internal execution state. The `/__cargo_target` runtime path is
  retired. A `workspace-v2` target generation prevents binaries compiled with
  the retired absolute path from entering the new mount; old generations are
  inactive Velnor-owned cache data and remain eligible for guarded GC.
- The local macOS `cargo nextest` rerun for the target-generation-only change
  reached the linker and was refused because this workstation has not accepted
  the installed Xcode license (`xcrun --sdk macosx --show-sdk-path`, exit 69).
  Accepting a vendor license is not inferred operator authority. Formatting
  and clippy passed locally; the mandatory full 618-test gate is instead run
  unchanged by the Linux Velnor dogfood workflow and its run URL is recorded
  with the release evidence.
- Ruxel unchanged runs `29647708018` and `29647811689` remained green but
  emitted 307 dependency `Compiling` markers. Host evidence showed that apt
  had installed v0.1.69 while the Ruxel daemon still held the pre-upgrade
  executable started at 14:11:01 UTC; an idle-window systemd restart at
  14:22:35 activated the apt-installed binary. The next cold-generation run
  `29647936109` then proved a second root cause: native checkout's faithful
  `git clean -ffdx` emptied the bind-mounted `/__w/target` before every job.
  The runner now excludes only its manifest-approved persistent `target/`
  mount from checkout cleaning and retains normal clean semantics everywhere
  else. No workflow was weakened.
- Ruxel plan 052 completed in PR
  <https://github.com/tailrocks/ruxel/pull/2>. Final branch runs were Velnor
  `29648690451` (52 seconds; zero dependency downloads/compiles and zero tool
  installs), GitHub `29648738056`, and combined `29648808556`; the combined
  run had the same five job ids on both lanes. Merged-main run `29648916790`
  passed before the owned Ruxel daemon was disabled and its four runner
  registrations deleted. Release v0.1.70 was built by runs `29648071433` and
  `29648071447`, published by `velnor-apt` run `29648199356`, and installed on
  Sentry exclusively with `apt-get install --only-upgrade velnor-runner`.
- Ruxel's cache evidence exposed an existing runner expression deviation:
  cache step inputs render `hashFiles(...)` as an empty suffix even though the
  files exist after checkout. The workflow keeps the canonical expression and
  uses an explicit `oracle-v2` generation; cache restore correctness and the
  no-change acceptance proof are green. The runner-side step-time evaluation
  defect remains in scope for the Velnor commit series and must not be worked
  around by removing dependency-based invalidation estate-wide.
- Plan 054 started from Termrock main `dd8bed1`, two source-only commits after
  the planned `b7f34da`; scoped workflow excerpts were unchanged. Static work
  is signed and pushed as `b4d6d185` in PR
  <https://github.com/tailrocks/termrock/pull/4>. All workflow gates and 332
  nextest tests passed. Existing source-only rustfmt drift and five strict
  clippy warnings in `completion_menu.rs` predate the program branch and were
  not masked with workflow changes; they remain repository-source evidence
  to address before the PR can merge.
- Java-monorepo plan 047 has exhausted its executable gates. Final Velnor,
  GitHub, and combined runs are `29643642046`, `29643795326`, and
  `29644162613`; required PR run `29637250998` attempt 2 was rerun after the
  v0.1.71 apt deployment and passed every job, including the formerly failing
  coingecko job. `gh pr merge 1753 --repo ChainArgos/java-monorepo --merge
  --delete-branch` is refused with “base branch policy prohibits the merge”
  despite no non-success checks. Human-only action: approve PR
  <https://github.com/ChainArgos/java-monorepo/pull/1753>, then click **Merge
  pull request**, or run the same `gh pr merge` command with an authorized
  reviewer session. The owned Java daemon and all registrations were removed.
- Parallax plan 053 static delivery is pushed as signed commits `92e4a68c`
  and `7e3412b9`. Native Darwin preview/release builders remain intentionally:
  their aarch64/x86_64 Apple artifacts require native `dsymutil`, codesign,
  and Apple ld header padding to preserve signed DWARF-bearing package
  semantics. Cargo-zigbuild is not equivalent. Every Linux-capable job uses
  the canonical lanes; static workflow and repository policy fixtures pass.
  The broader local test glob additionally requires an uninstalled `vite`
  binary; this is recorded as a local prerequisite, not masked in CI.
- Plan 057 cannot be applied safely. Tablerock moved from planned `03c6dd9`
  to `0bb0119` and added `checks.yml` with macOS, Ubuntu, real-server, UniFFI,
  import/export, performance, and ENOSPC coverage absent from the plan. Its
  repository `AGENTS.md` explicitly mandates trunk-only work and prohibits
  creating/publishing branches or PRs, conflicting with the binding estate
  delivery branch. The worktree also contains unrelated user edits in
  `TableRockApp.swift` and `PageV1.swift`. The documented plan fallback cannot
  preserve both instruction sets, and applying its stale template would
  weaken current coverage. No branch or file was changed. Human-only decision:
  reconcile Tablerock's trunk-only `AGENTS.md` with the estate branch model and
  re-plan from `0bb0119`; do not delete the new coverage.
- Plan 055 static delivery is signed and pushed: Schemalane `b02994b` in
  <https://github.com/tailrocks/schemalane/pull/2>, pg-bigdecimal `42b6e05`
  in <https://github.com/tailrocks/pg-bigdecimal/pull/1>, and
  tracing-request-level `3670236` in
  <https://github.com/tailrocks/tracing-request-level/pull/1>. Local format,
  clippy, nextest, doc, actionlint, policy, DCO, and pin gates pass. Schemalane
  exercises its database through testcontainers, so adding a redundant YAML
  service would change semantics. The other two intentionally do not commit
  `Cargo.lock`; clean-checkout `--locked` failed with exit 101, so their
  canonical gates omit that inapplicable flag. PR-created runs `29649999281`,
  `29650000793`, and `29650002002` were cancelled before controlled dispatch.
- Plan 056 static delivery is signed and pushed as `7049da8` in
  <https://github.com/tailrocks/parallax-telemetry-playground/pull/7>.
  Actionlint, Rust gates, the 76-scenario suite, workflow policy, DCO, and
  pin checks pass. PR-created run `29649859177` was cancelled before the
  controlled verification campaign.
- Parallax run `29649939459` proves an unapproved capability surface rather
  than a workflow defect. Three native `actions/upload-artifact@v7` failure
  handlers use `retention-days: 14`; Velnor manifest v2 accepts only `name`,
  `path`, `if-no-files-found`, `include-hidden-files`, and `overwrite`, and
  rejected the complete expanded jobs before side effects. The strict
  capability contract requires explicit operator approval before adding an
  input with storage-lifetime implications, while this campaign forbids
  asking or waiting. The canonical workflow is retained unchanged and the
  item is recorded for a future explicit capability decision.
- Cancellation handling diverged from GitHub runner behavior during Parallax
  run `29649548414`: after GitHub cancellation, Velnor continued an active
  mise build and systemd drain did not complete. Because the run was already
  explicitly stale/cancelled, its owned cgroup was killed and GitHub's
  force-cancel endpoint completed the run. This is runner backlog evidence:
  cancellation must promptly terminate the active step and release the job;
  workflow semantics were not changed to hide it.
- Parallax repository follow-up `c54c39eb` restores the checksum-verifying
  `setup-macos-sdk` composite action still included by Rust contract tests and
  required by native Darwin releases, adds the exact 1,579-byte workflow-agent
  ratchet, and reconciles the stale Oxfmt fixture with the live 493-file,
  100-column contract. Local policy, Oxfmt, and strict clippy gates pass. Clean
  follow-up run `29650466272` again rejected a browser job before execution on
  the same `retention-days: 14` surface, proving the remaining blocker is the
  unapproved native action capability. Plan 053 is BLOCKED pending an explicit
  operator decision to add `actions/upload-artifact@v7` `retention-days` with
  its GitHub-equivalent storage lifetime semantics; after approval, rerun the
  three-lane and timing campaign from PR
  <https://github.com/tailrocks/parallax/pull/21>. Cancellation again required
  force-cancel plus termination of the already-cancelled owned systemd cgroup;
  the Parallax unit is disabled and its runner registrations are deleted.
- Termrock verification exposed two runner defects without weakening its
  workflow. Run `29650609270` showed that a failed native mise install was
  followed by an attempt to read the nonexistent `_velnor/mise-env.json`;
  signed Velnor commit `f458497` now returns the failed step result directly.
  Velnor v0.1.72 was built by runs `29650748123` and `29650748129`, published
  by `velnor-apt` run `29650875407`, and installed on Sentry exclusively by
  apt. Run `29651045178` then showed four jobs concurrently mutating one
  repository rustup tree, producing partial component state. Signed commit
  `3594332` serializes shared mise installation with an advisory `flock`, adds
  the pipx and util-linux runtime prerequisites, and preserves concurrency
  after installation. Its formatting, strict clippy, 619-test nextest, and
  actionlint gates passed. Velnor v0.1.73 was built by runs `29651170586` and
  `29651170620`, published by `velnor-apt` run `29651299231`, and installed
  on Sentry exclusively by apt; dpkg and the rebuilt job-image label both
  reported 0.1.73.
- Before the v0.1.73 retry, the inactive poisoned Velnor-owned cache path
  `/var/cache/velnor/v1/trusted/mise/rustup/tailrocks_termrock` was resolved
  after the Termrock unit stopped and removed under the owned-resource guard.
  During the preceding apt image rebuild, exact stale container
  `5351794961ee` from an already-cancelled Parallax job held only
  `/var/lib/velnor-parallax/...` mounts and was removed; no broad Docker prune
  or non-Velnor deletion was performed.
- Termrock commits `f276f86` and `ba45fe7` declare current `uv` as the mise
  pipx backend and commit its generated lock entry. This follows mise's
  current pipx backend dependency model and fixes the initial GitHub error
  that `pipx:reuse` had no backend, followed by the locked-install error that
  `uv@0.11.29` was absent from `mise.lock`. Clean GitHub run `29651936604`
  passed mise installation, gitleaks, actionlint, and docs-quality before
  failing `cargo fmt --all -- --check`. Clean Velnor run `29651695696` failed
  the identical formatting gate on pre-existing source drift in the
  completion-menu sources; the other three Rust jobs passed on each lane.
  Plan 054 explicitly excludes Rust source edits, so every in-scope fallback
  is exhausted and the plan is BLOCKED rather than masking the defect in YAML.
  The GitHub run's semver job remained in its cold full-tool installation for
  more than two minutes and was cancelled under the monitoring rule after the
  parity blocker was established. PR: <https://github.com/tailrocks/termrock/pull/4>.
- Schemalane's first controlled Velnor run `29652124778` exposed concurrent
  extraction into the daemon-shared Cargo registry `src` tree: two independent
  job containers raced while unpacking `astral-tokio-tar` and one received
  `EEXIST` for `.cargo-ok`. The shared store now contains only immutable Cargo
  archives/indexes and bare Git databases; extracted registry sources and Git
  checkouts remain job-local. Signed runner commit `73cf5ac` passed formatting,
  strict clippy, all 619 nextest tests, and actionlint. Velnor v0.1.74 was built
  by release runs `29652393700` and `29652393714`, published by signed apt run
  `29652519168`, and installed on Sentry exclusively through the configured apt
  repository. `dpkg-query` and the package-built job-image label both report
  0.1.74. Clean Schemalane Velnor run `29652678710` then passed all three jobs,
  proving the extraction race was removed without changing fixture or workflow
  semantics.
- Schemalane's clean GitHub lane exposed two host-image assumptions in its
  pre-existing audit job after the estate-wide workflow environment became
  explicit: it inherited `RUSTC_WRAPPER=sccache` and mold linker flags but did
  not provision either tool. Signed commits `195600f` and `b0e3090` give the
  audit job the same native setup steps as the other Rust jobs. Signed commit
  `2e629c9` replaces mise's Cargo-source installations of cargo-audit and
  cargo-nextest with their current release-binary GitHub backends, eliminating
  CI tool compilation while retaining mise as the sole tool authority.
