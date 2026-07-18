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
