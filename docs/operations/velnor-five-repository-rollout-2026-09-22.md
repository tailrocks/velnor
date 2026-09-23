# Velnor five-repository rollout ledger — 2026-09-22

Status: G0 read-only evidence refreshed 2026-09-23. G0 remains open. G1-G8
are not met. No live consumer qualification, publication, or completion claim
is made.

This ledger supersedes older bastion-first, dual-provider, and broader-fleet
plans. It records current evidence, not intent. Secrets, tokens, JIT payloads,
private keys, and credential values are excluded.

## Authority and required order

The controlling specification is the campaign prompt supplied on 2026-09-23.
Consumer activation order is fixed:

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
Repository 2 is not activated early. `java-monorepo` remains last on each
host.

## Evidence state vocabulary

`Implemented` means source or configuration exists in the current checkout.
`Tested` means a named check ran and passed. `Integrated` means reviewed,
committed, and pushed. `Published` means delivery artifacts and manifests are
available. `Installed` means the published product is installed at the target
version. `Qualified` means live, host-qualified consumer evidence exists.
These states never substitute for one another.

## Fresh G0 repository and access evidence — 2026-09-23

Authenticated GitHub account is `donbeave`. Observed account scopes are
`repo`, `workflow`, `read:org`, and `gist`. Admin access is present on all
eight audited repositories: five consumers plus `velnor`, `velnor-apt`, and
`homebrew-velnor`. Actions are enabled and allow all actions. GitHub SHA
pinning is not enforced. Repository protection and required-check visibility
is plan-limited for some private repositories; absence of visible checks is
not evidence that checks are unnecessary.

| Order | Repository | Current `main` head | Workflow/access evidence | Current disposition |
| --- | --- | --- | --- | --- |
| 1 | `donbeave/essential-mac` | `4665534690659898f8cc00fe04188aea7dd19c30` | 6 workflows; admin access; protection API returns GitHub Pro `403`; 3 registered runners offline | Not qualified. Rust unit reports 837 tests; generated provider placement is invalid. |
| 2 | `ChainArgos/jackin-agent-brown` | `3e128bd21ab9294841abb1c25f7ebe2d03a5b89f7` | 7 workflow records, 5 tracked files; `ci-required`; admin access; PR #241 head `092fe1d` | Not eligible before G2/G3. Pinned `ChainArgos/velnor-actions` reusable workflow returns 404. |
| 3 | `ChainArgos/cloudflare-tofu` | `5695374b3fc4ed1dcfb89ad50d8e589c52a4889a` | 3 workflows; active `protect-main`; admin access; PR #5 head `fd3ba03` | Not qualified. OpenTofu and Rust checks exist; provider placement and tool availability fail. |
| 4 | `ChainArgos/github-terraform` | `7576451adecfa6d86e0d6e4e4caff591877b3e16` | 2 workflows; active `protect-main` with no required checks; admin access; PR #13 head `2c44021` | Not qualified. Recent jobs did not start because of GitHub billing/spending-limit failure. |
| 5 | `ChainArgos/java-monorepo` | `05a7320bf9446e2c3ae826c469cb4adf961ef9ff` | 14 workflow records, 11 tracked files; 71-unit inventory; 0 repo runners; PR #2063 head `fbc3a92` | Not eligible before repositories 1-4. Current pipeline is effectively native Velnor-only. |

Product repository heads from the same read-only inventory:

| Repository | `main` head | Workflow records | Required-check/access note |
| --- | --- | ---: | --- |
| `tailrocks/velnor` | `0eeac75` | 17 | `DCO`, `Policy`, `ci-required`; 7 open PRs. |
| `tailrocks/velnor-apt` | `d7b6a0d` | 8 | `DCO`, `Policy`, `ci-required`; feed state is not current-source proof. |
| `tailrocks/homebrew-velnor` | `97c22e1` | 6 | `DCO`, `Policy`, `ci-required`; no verified G2 publication. |

### Consumer-specific workflow findings

- `essential-mac` declares `linux-x64`, but its generated
  `github-hosted` and `github-self-hosted` jobs both use
  `runs-on: velnor-scale-set`. Only `velnor` uses
  `[self-hosted, velnor-target-mvp]`. A prior logical hosted success ran on a
  Velnor Scale Set runner, so it is not GitHub-hosted evidence. The hosted
  condition also lacks the self-hosted fork exclusion.
- `essential-mac` run `35784170996` remains queued with only Policy and
  Control/Planning jobs visible; no runner is assigned. Registered Scale Set
  runners 417-419 are offline.
- `cloudflare-tofu` PR #5 repeats the hosted-to-Scale-Set misrouting. Its
  observed run `35587981747` lacked `tofu` on self-hosted and `cargo-fmt` on a
  logical hosted lane; its declared `linux-x64` target ran on ARM64 Velnor
  infrastructure.
- `github-terraform` PR #13 also maps logical hosted jobs to
  `velnor-scale-set`. Its generated lane installs OpenTofu 1.12.6 while the
  repository pins 1.12.5; self-hosted/native lanes do not install OpenTofu.
  Recent runs were blocked by GitHub billing/payment or spending-limit state.
- `jackin-agent-brown` is a shell/Docker agent image repository, not the
  missing Java/Rust workload. Its pinned reusable workflow
  `ChainArgos/velnor-actions/.github/workflows/ci-code.yml@77173e8e...` is
  inaccessible with current authorization and returned 404 through Git and
  the authenticated API. No workload substitution is allowed.
- `java-monorepo` has 71 declared units across Gradle, Rust, Docker, Bun,
  Node, and Docs, plus PostgreSQL/RabbitMQ/Redis/RustFS/Testcontainers and
  browser requirements. Current generated workflows dispatch only `velnor`;
  no three-provider qualification exists.

## Source checkpoint

| Item | Current evidence | State |
| --- | --- | --- |
| Velnor checkout | `tailrocks/velnor`, branch `codex/scale-set-control-plane` | current checkout |
| Local `HEAD` | `8ab3eac6ef3f136fbb7781c294982a8c2573b92e` | unchanged; equals remote feature branch |
| Worktree | Docker client, Scale Set lane/listener/ownership/worker lifecycle, daemon tests, worker tests, packaging test, and this ledger are dirty | integration state, not release state |
| Current formula source pin | `e5a0c249157a6fa79e82b502843fef945030d225` | does not include current dirty source |
| Current formula packaging pin | `cce2bc61d3d446797d857ca09202ade99c54615a` | static pin only; no current publication |
| Formatting/diff check | `rtk cargo fmt --all -- --check`; `rtk git diff --check` | passed for observed checkpoint |
| Broad Cargo validation | Concurrent builds were stopped; no current integrated suite pass | not claimed |

Current source changes are preserved and remain uncommitted. This ledger turn
does not edit source or generated consumer files. Source presence, focused
tests, and prior compile output do not establish integration, publication,
installation, or qualification.

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

## Fresh G0 host evidence — 2026-09-23

### Actual local Mac

Read-only inventory used the actual Mac. No Docker context change, deletion, or
workstation profile mutation occurred:

- macOS 27.0, Apple M5 Max, `arm64`, 18 CPUs, 128 GiB RAM.
- OrbStack 2.2.3 is running. Docker context is `orbstack`, using the OrbStack
  Unix socket. The `default` context points at absent `/var/run/docker.sock`
  and fails; service startup must select and attest `orbstack` explicitly.
- Docker server is Linux `arm64`, Engine 29.4.0, 18 CPUs, approximately
  121.7 GiB reported, cgroup v2, `cgroupfs`, and `overlayfs`. The engine
  reports `DOCKER_INSECURE_NO_IPTABLES_RAW`; privileged DinD is not qualified.
- Existing machine `capture-linux-validation` is Debian Bookworm `arm64` and
  is not assumed Velnor-owned. No machine was changed.
- Installed Homebrew formula is `donbeave/velnor-local/velnorctl`, version
  `0.1.277 development`. No Velnor process, Homebrew service, or launchd
  label is active. `velnorctl host status` reports zero packaged instances
  and zero active hosts.
- Installed launcher is
  `/opt/homebrew/opt/velnorctl/libexec/velnor-runner-launch`; its observed
  SHA-256 is `f025ac790fd3395e09c6fbb5c9e12e4db71eefc6d73c76371efa749414d65eaf`.
  This is installed development-era evidence, not current-source release
  evidence.
- Existing protected Velnor state/config/ledger paths remain mode-restricted.
  Existing Docker state includes 5 containers (1 running), 15 Velnor Scale
  Set networks, and 43 volumes. No broad cleanup occurred.
- A stale official runner container is stopped/exited with obsolete command
  `/bin/bash`; its paired DinD container is running. The pair has no host
  Docker socket and no published ports. The stale runner must be reconciled
  by ownership and immutable identity, not blanket deletion.
- Fresh Scale Set demand `35784170996` remains queued with no assigned runner.
  No live runner connection, artifact/log upload, cleanup, or recovery
  evidence exists.

Consequence: Mac product is installed but not campaign-installed. Current
formula pins source commit `e5a0c249...` and packaging commit `cce2bc61...`,
while local checkout is `8ab3eac6ef3f136fbb7781c294982a8c2573b92e` with dirty
source changes. The installed product does not contain the current dirty
source. Homebrew publication and launchd service qualification remain open.

### Bastion

Read-only SSH inventory against `root@37.27.110.241` reports:

- Host `bastion`, Debian 13, `x86_64`, 96 logical CPUs, approximately
  134.7 GB RAM, cgroup v2. Docker Engine 29.8.1, containerd 2.3.5, Buildx
  0.37.1, and Compose 5.5.1 are installed. Docker/containerd are active and
  enabled. Docker root is `/var/lib/docker`; socket is
  `/run/docker.sock`, `root:docker`, mode `0660`. No containers are running;
  public listener inventory shows SSH only.
- Docker packages originate from Docker's signed Debian repository.
- Installed Velnor package is `velnor-runner 0.1.274`; APT candidate matches.
  Origin is `https://velnor-apt.tailrocks.com`, architecture `amd64`, with
  `Signed-By: /etc/apt/keyrings/velnor.gpg`. Observed package SHA-256 is
  `3a35d3aba1b93f0031960827fb55a7ab0488d72e032ba9bd8e2c1cc24569a84a`.
- `velnor-controller@default.service` and `velnor-guardian.service` are
  active. Registration reconciliation reports unavailable GitHub URL or
  registration credential. Existing slot/process state is not campaign
  activation evidence.
- Root is XFS on `/dev/nvme0n1p4`, approximately 3.5 TB. Second NVMe
  `/dev/nvme1n1` is unmounted and untouched; it is not declared empty.
- Existing quota fragment
  `/etc/systemd/system/velnor-jobs.slice.d/10-host-cpu.conf` sets
  `CPUQuota=9120%`, `MemoryHigh=121263063040`,
  `MemoryMax=127999901696`, `MemorySwapMax=0`, and `TasksMax=4096`. The
  slice is currently inactive, but this violates the no-artificial-quota
  contract and blocks bastion qualification until structurally removed.

Consequence: Docker installation is not the current bastion blocker. Current
APT package provenance is old-product evidence, not current campaign delivery.
No bastion activation is authorized before the five Mac gates pass.

## Product and qualification state

| Capability or artifact | Implemented | Tested | Integrated | Published | Installed | Qualified |
| --- | --- | --- | --- | --- | --- | --- |
| Typed native/Scale Set/both modes and shared ledger | Yes, dirty checkout | Focused checks only | No | No | No current-source install | No |
| Strict Docker inspect, lifecycle, and restart attestation | Yes, dirty checkout | Partial focused evidence below | No | No | No | No |
| Fail-closed cleanup with immutable container/network IDs | Yes, dirty checkout | Cleanup-focused 21/21 reported | No | No | No | No |
| Anonymous volume-holder replacement for named volumes | No; design recorded below | No | No | No | No | No |
| Mac Homebrew product | Formula and launcher exist | Static/package gates incomplete | No current release | Existing 0.1.277 development | No |
| Bastion APT product | APT feed exists | Existing install only | No current release | Existing 0.1.274 | No |
| Consumer generated workflows | Existing outputs, provider mapping invalid | No current qualification | No | Existing repo outputs | Existing repos only | No |
| Five-repository Mac rollout | No | No | No | No | No | No |

Focused evidence reported during this refresh: DinD tests 17 passed;
supervision tests 21/21 passed; one worker transport-regression test passed;
runner tests previously reached 39 passed and 1 ignored before the final
label-attestation addition. Formatting and `git diff --check` passed. Shared
Cargo contention prevented a current integrated test-suite pass; daemon and
full worker green status is not claimed.

## Gate ledger

| Gate | Author | Independent verifier | Evidence required | State |
| --- | --- | --- | --- | --- |
| G0 truth/access | Campaign integrator | Evidence/access reviewer | Current repo heads, workflows, access, hosts, provider identity, and stale-observation reconciliation | In progress: current read-only inventory recorded; external access and product blockers remain. |
| G1 Mac product prerequisites | Product/runtime owners | macOS, generator, protocol, supply-chain reviewers | Published installed Mac product, typed modes, pinned images, generated workflow, recovery checks | Blocked: installed build is development-era; current source is dirty; no launchd service or published current release. |
| G2 essential-mac Mac Scale Set first | Scale Set owner | Official-engine/Mac reviewer | Real portable tests, official runner, private DinD, placement, logs, artifacts, cleanup, recovery | Not started: queued demand has no assigned runner; provider mapping is invalid. |
| G3 essential-mac Mac native then combined | Native owner | Native/capacity/result reviewers | Native proof after G2, both modes, all three providers, three green main runs | Not started; prohibited before G2. |
| G4 jackin-agent-brown, then cloudflare-tofu, then github-terraform | Per-repository owners | Stack and shared-runtime reviewers | Exact sequential qualification on Mac | Not started; order gate blocks activation and jackin workflow access is missing. |
| G5 java-monorepo Mac last | Java/Rust/Docker/frontend owners | Coverage/parity reviewers | Complete 71-unit-equivalent three-provider evidence | Not started; prohibited before G4. |
| G6 Docker + APT-only Velnor on bastion | Infrastructure writer | Debian/Docker and package reviewers | Current signed APT release, shared N, no quotas/raw socket | Blocked by quota fragment, old package, and unavailable registration configuration. |
| G7 bastion replay | Host/repository owners | Placement/parity reviewers | Exact five-repository order, java last, native then Scale Set | Not started; prohibited before G5. |
| G8 final acceptance | Campaign integrator | Independent final verifier | Installed identities, recovery, performance, watchdog, onboarding, no open correctness defect | Not started. |

## Immediate dependency graph

1. Finish source integration and independent tests for strict lifecycle,
   restart attestation, fail-closed cleanup, and the volume-holder design.
2. Publish immutable Homebrew artifacts and install the current verified Mac
   product. Configure explicit OrbStack endpoint, mode, shared ledger, and
   pinned image identities; install launchd service.
3. Correct generated provider placement and trust rules before local consumer
   activation. GitHub-hosted must use GitHub-hosted labels.
4. Run G2 on `essential-mac`, then G3 native and combined. Record run/attempt,
   source/plan, provider, host, architecture, image/package identities, tests,
   cleanup, and recovery evidence.
5. Activate repositories 2, 3, and 4 in order; activate `java-monorepo` only
   after them.
6. Remove bastion quotas, publish current signed APT delivery, install only
   through APT, then replay the same five-repository order on bastion.

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

Every record names invariant, reproduction, enabling condition, structural
correction, and independent counterexample. Source fixes below remain
uncommitted unless separately stated.

### D-DOCKER-001 — strict object-missing and status inspection

- Violated invariant: only an explicit object-specific Docker not-found result
  is absence. Empty success, malformed output, transport failure, unknown
  state, and transitional state fail closed.
- Reproduction: worker fixtures returned exit code 0 with empty inspect output;
  runner/DinD status fixtures returned Boolean `true`; broad `no such`
  matching could classify transport `no such file or directory` as object
  absence.
- Enabling condition: lifecycle code treated human-readable or empty inspect
  output as a permissive state and drove mutation from names/status booleans.
- Structural correction: typed `ContainerState`, exact status projections,
  object-specific missing classification, narrow inspect projections, and
  fail-closed handling for malformed/transport/transitional states.
- Independent counterexample: focused worker transport test preserves the
  transport error and performs no mutation; DinD focused tests report 17
  passed. Full integrated suite remains unverified.

### D-DOCKER-002 — restart adoption requires immutable pair attestation

- Violated invariant: restart adoption must not trust deterministic names or
  status alone. Network, DinD, and runner must match ownership labels, role,
  pinned image, network topology, command, and allowed lifecycle state.
- Reproduction: the live stale runner used `/bin/bash` instead of the required
  `/home/runner/run.sh`; same-name network replacement can leave residual
  containers apparently healthy.
- Enabling condition: adoption previously inserted workers by name and let
  supervision infer health without a complete read-only identity proof.
- Structural correction: inspect-only attestation for network, DinD, and
  runner; verify the Scale Set label, role labels, exact image and attachment,
  empty entrypoint, exact runner command, and lifecycle status. Return typed
  `Complete` or `Absent`; route absence to terminal/uncertain recovery rather
  than healthy adoption. Mismatch and transport errors abort adoption without
  cleanup or JIT recreation.
- Independent counterexample: FakeDocker models per-network identity and a
  same-name replacement test that rejects attachment mismatch without
  recreation. Current broad test pass is not claimed.

### D-DOCKER-003 — cleanup preflight and immutable mutation targets

- Violated invariant: cleanup must attest every target before any mutation;
  foreign, malformed, or transport-uncertain targets must receive zero
  stop/remove operations. Diagnostics must use attested immutable IDs.
- Reproduction: name-based cleanup could inspect one object and mutate a
  same-name replacement, or partially remove a pair after a later target
  failed attestation.
- Enabling condition: cleanup interleaved inspection and mutation and used
  names for containers, networks, diagnostics, and volumes.
- Structural correction: pair-level read-only preflight; typed missing versus
  present results; immutable container/network IDs for stop, logs, and remove;
  zero mutation on foreign/transport failure; permit remains uncertain on
  cleanup failure. Raw `docker volume rm` was removed from current cleanup.
- Independent counterexample: supervision-focused tests report 21/21 passed,
  including foreign/transport zero-mutation cases. Workspace and DinD volume
  cleanup remains deliberately incomplete until the holder fix below.

### D-DOCKER-004 — named-volume TOCTOU requires volume-holder architecture

- Violated invariant: Velnor must never delete a same-name foreign volume.
  Labels prove provenance only; they do not make name-based Docker deletion
  race-safe.
- Reproduction: runner and DinD still use deterministic named volumes, while
  Docker volume deletion is name-based and current cleanup had
  `docker volume rm <name>` semantics. A same-name replacement can be removed
  after inspect.
- Enabling condition: worker ownership was represented by reusable volume
  names without a persisted immutable deletion handle.
- Structural correction: create one stopped, labeled holder container per
  worker with anonymous volumes; persist and attest its immutable container ID,
  mounts, role, and labels; attach runner and DinD through
  `--volumes-from <holder-id>`; remove holder with
  `docker rm --volumes -- <attested-holder-id>`; never call `docker volume rm`.
- Independent counterexample: Docker Engine API exposes volume deletion by
  name with no compare-and-delete precondition. Independent review confirmed
  labels alone cannot close this TOCTOU race.
- State: open. Holder implementation, holder attestation, and corresponding
  FakeDocker/live tests are not yet implemented, tested, published, installed,
  or qualified. Current cleanup retains permit uncertainty when volume proof
  is incomplete.

### D-DOCKER-005 — transitional lifecycle states must not consume restart budget

- Violated invariant: `paused`, `restarting`, `removing`, and unknown Docker
  states are not equivalent to a stopped worker and must not trigger destructive
  restart logic.
- Reproduction: status-only supervision mapped transitional states to down,
  then could spend restart budget and call `docker start`.
- Enabling condition: lifecycle parser used a Boolean running/not-running
  model instead of Docker's exact state vocabulary.
- Structural correction: exact state parsing; only `running` is healthy and
  only explicit `exited`/`dead` are restart candidates. Transitional and
  unknown states fail closed and retain uncertainty.
- Independent counterexample: supervision tests cover paused, restarting, and
  unknown status fixtures; focused tests passed, but no installed-service
  recovery proof exists.

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

## Exact external blockers and residual risks

1. GitHub's private-repository plan returns `403` for `essential-mac`
   protection/required-check inspection. Required-check state cannot be
   asserted from current access.
2. Scale Set App/JIT registration capability is not verified. The Mac's
   registered Scale Set runners are offline, and queued run `35784170996` has
   no assigned runner.
3. `ChainArgos/velnor-actions` returns 404 for the pinned reusable workflow
   required by `jackin-agent-brown`. No access grant or restored revision is
   available, so its workload cannot be qualified or substituted.
4. `github-terraform` recent jobs did not start because GitHub reported a
   billing/payment or spending-limit failure. This is external to Velnor and
   blocks live evidence.
5. Mac Homebrew delivery has no verified current publication or launchd
   service. Installed `0.1.277 development` does not contain current dirty
   source.
6. Bastion registration configuration lacks usable GitHub URL/credential
   evidence, and the existing quota fragment violates the no-quota contract.
7. Named-volume holder architecture is not implemented. Cleanup cannot be
   called complete until holder identity and `--volumes-from` deletion are
   proven.

Internal risks remain separate from external blockers:

- Current source and packaging changes are uncommitted. No integrated Cargo
  suite pass is claimed after the final lifecycle/attestation edits.
- Mac provider mapping is invalid: logical `github-hosted` executes on the
  Velnor Scale Set label. This blocks G2 until generated output is corrected.
- OrbStack reports `DOCKER_INSECURE_NO_IPTABLES_RAW`; private DinD and native
  scoped lease behavior have no installed end-to-end proof.
- Existing Mac runner/DinD/network/volume state needs ownership-scoped
  reconciliation. No blanket deletion or broad Docker prune is authorized.
- Bastion package `0.1.274` is old-product evidence. It is not current-source
  APT delivery or campaign activation evidence.

This ledger edit changed only this ledger. Existing source and packaging edits
were preserved; no generated consumer file, workflow, host, package, or GitHub
repository was mutated by this edit.
