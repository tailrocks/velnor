# Velnor five-repository rollout ledger — 2026-09-22

Status: G0 evidence checkpoint refreshed 2026-09-22. G1-G8 are not met. This
ledger supersedes older bastion-first and dual-provider plans for this
campaign. It does not claim published operation, installed operation,
consumer qualification, or completion.

## Authority and required order

The controlling specification is the pasted campaign prompt supplied on
2026-09-22. Consumer activation order is fixed:

1. `donbeave/essential-mac`
2. `ChainArgos/jackin-agent-brown`
3. `ChainArgos/cloudflare-tofu`
4. `ChainArgos/github-terraform`
5. `ChainArgos/java-monorepo`

`essential-mac` must qualify on the actual local Mac in this order:

1. Velnor Scale Set mode with an unmodified official runner and private DinD.
2. Velnor native mode with its existing Docker backend.
3. Both modes together, plus GitHub-hosted verification, under one host-wide
   capacity authority.

The same consumer order repeats on bastion only after all five Mac gates pass.

### Repository evidence anchors

The following repository heads and prior-run observations came from the
read-only access inventory. They are historical anchors, not current
qualification evidence; each head and workflow must be re-read before its
gate.

| Order | Repository | Observed source head / PR head | Prior observed run and result |
| --- | --- | --- | --- |
| 1 | `donbeave/essential-mac` | main `4665534690659898f8cc00fe04188aea7dd19c30` | main run `35639449694`: failed `github-self-hosted` `cargo-nextest` with HTTP 403; two repository runners offline |
| 2 | `ChainArgos/jackin-agent-brown` | PR #241 `092fe1db...` | run `35587944563`: Velnor command not found and no Docker socket |
| 3 | `ChainArgos/cloudflare-tofu` | PR #5 `fd3ba03...` | run `35587981747`: hosted `cargo-fmt` missing; self-hosted `tofu` missing |
| 4 | `ChainArgos/github-terraform` | PR #13 `2c440218...` | run `35587968574`: plan exited 130; frozen identity absent |
| 5 | `ChainArgos/java-monorepo` | PR #2063 `fbc3a925...` | run `35587996113`: canceled after runner shutdown |

No later result is counted here. Repository 2 is not eligible until
repository 1 passes every Mac gate; repository 5 remains last.

## Source checkpoint

| Item | Current evidence | State |
| --- | --- | --- |
| Velnor checkout | `tailrocks/velnor`, branch `main` | dirty; three commits ahead of `origin/main` |
| Local `HEAD` | `c76d2b932dc1a4f12eee3650f690d50b13816d48` | current checkout |
| Remote `origin/main` | `1e094e97c3ba86ec649a5c88589eaceeec485543` | remote checkpoint |
| Local-only history | `c48f2518 test: exercise bootstrap transport shells` is visible in the ahead history | not pushed in this checkout |
| Worktree | dirty; Scale Set, runner, workflow, `velnorctl`, packaging, and ledger changes are uncommitted | integration state, not release state |
| Formatting check | `rtk git diff --check` | passed at checkpoint |
| Compile checks | `rtk cargo check -p velnor-runner`; `rtk cargo check -p velnor-workflow` | passed before subsequent agent edits; workflow emitted five dead-code warnings; rerun required |
| Test-build attempts | concurrent Cargo/Mr Boxington jobs held package/build locks; no-run sessions were interrupted | no test-suite pass claim |

Current dirty source groups observed:

- runner/CLI host modes and capacity wiring: `args.rs`, `runner.rs`,
  `service.rs`, `velnorctl/src/{commands,host,runtime}.rs`;
- Docker isolation and lease transport: `container.rs`, `docker_lease.rs`,
  `executor.rs`;
- Scale Set demand identity/admission, fencing, worker lifecycle, schema, and
  tests: `velnor-control/src/store/migrations.rs`,
  `velnor-runner/src/scaleset/{demand,lane,scale}.rs`,
  `scaleset/worker/{mod,runner}.rs`, and `tests/scaleset_worker.rs`;
- workflow provider-mode/config/rendering changes under
  `crates/velnor-workflow/src/s2/`, plus deletion of the obsolete tracked
  `tests/bootstrap_transport.rs` fixture and edits to
  `tests/{provider_pairing,selection_plan_handoff,verify_set_build_inputs}.rs`;
- untracked `Formula/`, `packaging/`, and this ledger.

Source-only controls now visible, not yet deployed or live-qualified:

- runner `HostMode` accepts `native-only`, `scale-set-only`, and `both`, with
  explicit config and positive `max_jobs` validation for Scale Set modes;
- workflow `ProviderMode` accepts the same three names and keeps the hosted
  provider in the generated universe/automatic recovery path;
- Scale Set startup rejects a configured ledger path that differs from the
  host-wide permit ledger;
- worker provisioning uses digest-shaped official-runner/DinD image types and
  content-proof hooks;
- macOS native jobs avoid direct host Unix-socket mounts and select the scoped
  TCP lease path; private DinD remains a separate path.

These controls have no published artifact, installed identity, generated
consumer run, or live recovery proof at this checkpoint.

The current source already contains native Docker execution, a host-wide
permit ledger, Scale Set protocol modules, worker/DinD code, and daemon wiring.
Presence of those modules is not deployed or live qualification evidence.

## G0 fresh host evidence

### Local macOS

Observed on the actual host, without changing Docker context or deleting data:

- Darwin 27.0, `arm64`; host name `Alexeys-MacBook-Pro.local`.
- Docker context is `orbstack`; endpoint is the user OrbStack Unix socket.
- OrbStack was initially stopped and became running during observation. Current
  provider status is `Running`.
- Existing OrbStack machine `capture-linux-validation` is running Debian
  bookworm `arm64`. It is not assumed to be Velnor-owned and was not changed.
- Docker Engine is Linux `arm64`, version 29.4.0, 18 CPUs, 121.7 GiB, cgroup
  v2, `cgroupfs`, `overlayfs`. The provider reports
  `DOCKER_INSECURE_NO_IPTABLES_RAW`; this must be disclosed and tested before
  privileged DinD qualification.
- No `velnorctl` or `velnor-runner` is installed on `PATH`. No Velnor launchd
  agent or daemon is present.
- Existing Velnor Application Support state is stale forensic material: its
  health document says `state=degraded`, `actual_ready_slots=0`, and its log
  records prior canonical-storage and invalid-capacity-journal failures. It is
  not used as current qualification evidence and must not be deleted blindly.

Consequence: the Mac product is not installed or running. OrbStack is usable
now, but the first pilot still needs a published, verified Homebrew install,
explicit mode configuration, isolated state, pinned Linux images, and live
Scale Set evidence. The untracked Homebrew/launchd files are source artifacts,
not installation evidence.

### Bastion

Read-only SSH inventory refreshed against `root@37.27.110.241`:

- Host is `bastion`, Debian 13, x86_64, kernel `6.12.94+deb13-amd64`.
- Docker is installed and active/enabled, despite the older supplied shell
  report. Docker Engine is 29.8.1, 96 CPUs, 125.5 GiB, cgroup v2 with the
  `systemd` driver, `overlayfs`, Compose 5.5.1, and Buildx 0.37.1.
- Installed Docker packages are `docker-ce`, `docker-ce-cli`, `containerd.io`,
  `docker-buildx-plugin`, and `docker-compose-plugin`, all versioned
  `29.8.1`/`2.3.5`/`0.37.1`/`5.5.1` as applicable. APT candidate/origin points
  to `https://download.docker.com/linux/debian`.
- Root is XFS on `/dev/nvme0n1p4`, approximately 3.5 TB with approximately
  3.5 TB free. `nvme1n1` is a separate 3.5 TB disk with no mounted
  filesystem; it remains untouched.
- `/usr/bin/velnor-runner` is `0.1.274`; `/usr/bin/velnorctl` is `0.1.0`.
  `velnor-controller@default.service` and `velnor-guardian.service` are
  currently active but disabled. The controller repeatedly reports that
  registration reconciliation is skipped because GitHub URL/PAT is
  unavailable. A separate `velnor-runner.service` is inactive/not installed,
  while long-lived `slot` processes remain under the installed state root.

Consequence: Docker installation is not currently a blocker. APT key,
`Signed-By`, repository metadata, package provenance, current-source package
identity, clean stale-state reconciliation, and a configured GitHub
registration are unverified. The active disabled services and old slot
processes are inventory evidence only, not this campaign's activation. No
bastion activation is authorized before the Mac five-repository gate.

## Gate ledger

| Gate | Author | Independent verifier | Evidence required | State |
| --- | --- | --- | --- | --- |
| G0 truth/access | campaign integrator | evidence/access reviewer | exact repo, workflow, host, provider, access, and stale-observation reconciliation | in progress: source/host inventory recorded; current consumer-head and workflow revalidation remain open |
| G1 Mac product prerequisites | product/runtime owners | macOS, generator, protocol, supply-chain reviewers | published installed Mac product, typed modes, pinned images, generated workflow and recovery checks | blocked: no Mac product installed; source controls are uncommitted and packaging is unpublished |
| G2 essential-mac Mac Scale Set first | Scale Set owner | official-engine/Mac reviewer | real portable tests, private DinD, placement, logs, artifacts, cleanup, restart/recovery | not started; no live Scale Set registration or job |
| G3 essential-mac native then combined | native owner | native/capacity/result reviewers | native proof after G2, both modes, all three providers, three green main runs | not started; no native/combined consumer run |
| G4 three next Mac consumers | per-repository owners | stack and shared-runtime reviewers | exact sequential qualification for repositories 2–4 | not started; blocked by G2/G3 and order rule |
| G5 java-monorepo Mac last | Java/Rust/Docker/frontend owners | coverage/parity reviewers | complete 71-unit-equivalent coverage and three-provider evidence | not started; blocked by preceding gates |
| G6 bastion Docker/APT product | infrastructure writer | Debian/Docker and package reviewers | signed APT-only Velnor install, shared N, no raw socket/quotas | blocked: existing package/services are not current-source or campaign activation evidence; URL/PAT unavailable |
| G7 bastion replay | host/repository owners | placement/parity reviewers | exact five-repository order, java last, native then Scale Set | not started; prohibited before Mac gates |
| G8 final acceptance | campaign integrator | independent final verifier | installed identities, recovery, performance, watchdog, onboarding, no open correctness defect | not started |

## Immediate dependency graph

1. Finish G0 repository/workflow/access inventory and reconcile historical
   anchors against current SHAs.
2. Complete review and focused tests for the typed daemon mode set. Keep one
   host-wide ledger and prove Scale-Set-only without fake native slots.
3. Close macOS daemon/provider boundaries in a published product: explicit Docker endpoint identity,
  Darwin paths, launchd service, restart/drain, and isolated state.
4. Verify official `actions/runner` and `actions/scaleset` revisions before
   accepting protocol changes; then independently qualify worker/DinD lifecycle.
5. Repair, publish, and install Homebrew delivery on this Mac before G2.
6. Generate and qualify `essential-mac` only after G1; do not activate later
   repositories early.

## Evidence rules

- Implemented, tested, integrated, published, installed, and qualified remain
  separate states.
- A source test, runner registration, label, dispatcher success, or smoke test
  never substitutes for deployed CI evidence.
- No credential, token, private key, secret path, or raw recovery payload goes
  in this ledger.
- Every bug fix records violated invariant, reproduction, enabling structure,
  structural fix, and an independent counterexample.
- Every claimed live result records source SHA, run/attempt, plan digest, unit,
  provider, required host, target architecture, runtime/image/package
  identities, test counts, logs, and cleanup evidence.

## Defect records

### D-BOOTSTRAP-001 — obsolete bootstrap transport fixture drift

- Violated invariant: every generated fixture must parse against the current
  typed configuration schema.
- Reproduction: `cargo test -p velnor-workflow --test bootstrap_transport`
  failed before generation because schema-2 rejected
  `default_dispatch_providers`.
- Enabling structure: the fixture copied an older schema-2 field instead of
  using the current parser contract; generator changes therefore broke the
  fixture before transport behavior ran.
- Structural correction: do not revive the fixture with stale step names. The
  active renderer's candidate flow is now static contract coverage; the old
  executable acquisition/execute shell is no longer generated. The obsolete
  tracked test was removed after its current output was inspected.
- Counterexample: rerun the active workflow contract/provider test targets;
  the deleted test is not a qualification surface.

### D-BOOTSTRAP-002 — obsolete fixture also omitted visibility evidence

- Violated invariant: rendering must consume explicit repository-visibility
  evidence and never guess visibility from a network call or fixture name.
- Reproduction: after D-BOOTSTRAP-001, the same focused test failed before
  generation because `.github-gen/visibility.toml` was absent.
- Enabling structure: fixture setup created the workflow config but not its
  required evidence sidecar.
- Structural correction: no new fixture was created. Current generator tests
  must create visibility evidence through their shared current fixture helper;
  this obsolete test does not define a second transport path.
- Counterexample: active workflow tests remain the required rerun target.

## Current blockers and residual risks

- No Velnor product is installed on the Mac. The required first live gate
  cannot start until a published, verified Homebrew artifact and launchd
  service are installed from a clean state.
- Current source is uncommitted and the worktree is changing across runner,
  Scale Set, workflow, CLI, and packaging surfaces. Successful `cargo check`
  output is not an integrated or published build; the interrupted concurrent
  test-builds are not test evidence.
- `velnorctl/src/host.rs` and `velnor-runner/src/runner.rs` contain literal
  `+` characters in three newly added mode-validation error strings. This is
  a source correctness/diagnostic defect visible at the checkpoint and must
  be fixed before G1; this ledger-only task does not alter it.
- Mac Docker isolation now fails closed against direct host Unix-socket
  mounts and selects a scoped TCP lease for native jobs, but no OrbStack
  container-to-lease end-to-end proof exists. OrbStack also reports
  `DOCKER_INSECURE_NO_IPTABLES_RAW`; private DinD behavior is therefore not
  qualified.
- Scale Set worker code has source-level digest/provenance/signature hooks and
  durable identity/admission changes, but no official `actions/runner` wire
  trace, signed image attestation, live `JobAvailable`/ACK/cursor trace, or
  cleanup/restart proof is recorded.
- The bastion already has active disabled controller/guardian services,
  missing GitHub URL/PAT, and residual slot processes. Treating that state as
  a fresh deployment would invalidate G0 and G6; reconcile it only under an
  explicitly authorized activation after the Mac gates.
