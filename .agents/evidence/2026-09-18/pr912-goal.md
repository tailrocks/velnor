# Bastion final plan execution goal

Paste-ready `/goal` prompt for the consolidated Velnor-first bastion campaign. Copy the fenced block below verbatim as the execution prompt. It implements `plans/2026-09-17-bastion-final-plan.md` (specification) through the ordered gates in `plans/2026-09-17-bastion-final-plan-gates.md`.

```text
/goal Execute the consolidated Velnor-first bastion campaign in
velnor-bastion-final-plan.md. Deliver source changes, generated workflows,
coherent releases, signed APT deployment, real three-provider verification,
sequential repository migrations, and operating evidence. Do not stop at
planning, registration, or an author saying tests pass.

TARGET
Server: root@37.27.110.241, bastion, Debian 13, AMD EPYC 9454P,
48 physical cores / 96 logical CPUs, approximately 128 GB RAM.
Primary repository: tailrocks/velnor.
Delivery dependency: tailrocks/velnor-apt, signed feed at
https://velnor-apt.tailrocks.com/.
Consumers in hard rollout order: tailrocks/velnor, then jackin-project/jackin,
then ChainArgos/java-monorepo, then generic onboarding. Read-only consumer
research may run early; consumer mutation/activation must wait for its gate.

AUTHORITY
This final plan and these rules replace the nine v1/v2/bootstrap documents.
Preserve their audit facts as historical regression evidence, never their
superseded VM, Go-controller, quota, per-org-pool, or compatibility proposals.
Refresh current source, PRs, jobs/logs, package/feed state and host facts.
If live evidence contradicts a snapshot, record old fact, fresh evidence,
and implementation consequence. Keep the same final architecture.

ORCHESTRATION
The main agent is orchestrator only: track the persistent ledger, assign,
resolve dependencies, review evidence, integrate decisions, and unblock.
Delegate ALL substantive research, design, code edits, source integration,
tests, infra, release publication, deployment, review and verification to
subagents. Use them aggressively and keep all useful available slots busy.
Do not claim an agent ran unless it actually did.

Start parallel authors and separate verifier agents for live evidence,
CI/policy/performance, immutable runtimes, typed providers/IR/generation,
APT primitives/feed, packaging/systemd/unbounded execution, native/global
admission, Rust Scale Set protocol, official runner/DinD, credentials/trust,
caches, and hosted result/watchdog behavior. Give blocked authors independent
test/review work; use isolated worktrees and concrete file ownership.
A dedicated integration subagent merges source and regenerates shared output.
One infrastructure subagent owns bastion writes per coordinated window.
Each module MUST have a different independent verifier. Authors never solely
certify their own work. Verifiers must attempt to disprove correctness.

Ledger: task ID, dependency, author, verifier, source/branch, finding,
required invariant, evidence, status, blocker, next action. Preserve it
through compaction. Integrate small verified changes continuously.

HARD ARCHITECTURE
1. One APT-installed Velnor Rust control plane directly on Debian/Docker.
   No VMs, libvirt, QEMU, new KVM runner infrastructure, Kubernetes,
   external Go controller, Go sidecar, or independent scheduler.
2. Velnor native mode retains its existing Rust execution and Docker backend,
   job-scoped Docker API mediation, ownership, cancellation and caches.
   Do not rewrite native execution into DinD or launch the official runner
   to fake the native lane. Fix concrete conformance bugs at their source.
3. GitHub-self-hosted mode is a Rust Scale Set adapter INSIDE Velnor managing
   ephemeral unmodified official Actions runners plus private per-job DinD.
   Pin runner, DinD and job/toolchain images by verified digest; never latest.
4. Never mount bastion's raw /var/run/docker.sock, an alternate unrestricted
   host socket, or unrestricted host Docker proxy into ANY job. Management
   alone owns the real engine. Private DinD sockets and native job-scoped
   mediated lease sockets are distinct allowed job APIs; prove their origin.
5. One host-wide max_jobs=N shared by BOTH local modes and ALL scopes.
   No per-repo/org/engine reservations, weights, priorities or resource pools.
   GitHub registration/session boundaries are authorization metadata, not
   additional capacity. Adding repositories cannot create new daemons/pools.
6. Admitted containers can use all CPU/RAM the kernel makes available.
   No Velnor Docker CPU/memory/swap/reservation/cpuset ceilings, ancestor
   CPUQuota/MemoryMax/MemoryHigh, slot-divided Cargo/MBX/BuildKit budgets,
   artificial Gradle/heap throttles, resource classes or v1 disk/PID quotas.
   Remove stale package-generated quota requirements and prevent recurrence
   after upgrade. Keep cgroups for ownership/observation/lifecycle only.
   Preserve correctness-required serial test groups. No silent quota fallback.
7. Oldest-observed eligible work first with persistent age and deterministic
   tie-breaks. GitHub controls upstream delivery/assignment; no global
   submission-order FIFO or exact acquired-ID-to-JIT-runner promise.
8. Exactly github-hosted, github-self-hosted, velnor as typed providers.
   All trusted supported Linux verification defaults to all three after
   bootstrap, including default dispatch. Real native platform exceptions
   stay explicit. No inference from runner.environment or label substrings.
9. velnor-workflow is SOLE owner of every .github/workflows file touched,
   including recovery, runtime products, source release, APT, Pages, policy,
   watchdog and maintenance. No hand YAML, legacy copies, hidden local
   templates, raw-YAML/command escapes, or repo-name hacks. Use typed
   capabilities and provider sets; validate local action references too.
10. Make breaking changes freely to reach the final shape. No compatibility
    shims, aliases, deprecation windows, dual parsers, dual schedulers or
    preserved old budget model solely for compatibility. Migrate each repo
    atomically at its gate. Old consumers may keep their old immutable
    generator until migration; the new product gets no compatibility path.
11. Bastion is trusted-tier, not hostile multi-tenant isolation. No untrusted
    fork privileged Docker by default. Admission must be enforced outside
    PR-editable YAML. Never run untrusted code in privileged
    pull_request_target. Keep management/publisher/SSH keys out of jobs.
12. Repeat verification, not side effects. Signing, publication, deployment,
    releases/tags, Renovate writes, issue writes and destructive maintenance
    have one verified writer. Preserve actual required external checks/DCO
    and application release-disabled policy. No protection bypass for green.

BASELINE TO REVALIDATE
Velnor: 3353310c7648fca22698b6c0f4a69ab245127786, 17 units.
Jackin: 92f347ac39fbf0d6f9853168e2896a6c60522924, 40 units.
ChainArgos: 235e479b150aeb949bc8a5190fba5b84f6303c80, 71 units.
Velnor/Jackin generator: b9c3156cdb88e63c11b9e595a3e694b02238c09a.
ChainArgos generator: 1279c4f92c97b75dc4cc627f122e119f8a5eae16.
Scale Set reference: fb56300503fd21caa788feeb85c63071d15155c6,
2026-09-15; older v0.4.0 has a different listener API.
Velnor run 35129353335 failed generated-tree; PR #904 run 35136207272
requested closure f1f88c200e5b3b82 before publication. Recheck #904 and #901,
reuse correct work, and never trust a PR's green claim without run evidence.
The audited velnor-apt omitted APT workflows for missing generator primitives.
Source version 0.1.275 was not proof of an installable published package.

ORDERED EXECUTION: DO NOT SKIP GATES
A: Repair current CI/policy/tree/test defects and speed the critical path.
   Keep source coverage, caches, streaming progress and parallel main runs.
   Implement immutable attested runtime products: trusted R stays the normal
   consumer; candidate C builds once per source closure/platform; test C in
   isolation; merge/publish/verify C; THEN atomically promote pin+whole tree.
   No normal cargo fallback, unpublished pin, product-not-found race,
   consumer-repo artifact lookup bug, or unverified cache hit.
   Include complete closure/build inputs and trusted producer/ref identity.
   Obtain three consecutive full green bootstrap main runs without reruns.
   If local recovery needs bootstrapping, use an explicit generated hosted
   recovery provider set with full required source checks; label it bootstrap,
   never triple-qualified. Do not hide missing expected local work as skipped.
B: Implement generic typed APT publication/validation/channel/Pages contracts.
   Reuse audited release verifier logic through a narrow typed contract,
   not arbitrary shell. Test a renamed fixture for generic behavior.
   Publish tooling first, then pin/regenerate velnor-apt; remove omission
   notices/stale direct-install docs only after real coverage exists.
   Produce coherent reviewed source/tag/version, amd64+arm64 packages,
   manifest/record/checksums, required OCI/payloads and attestations.
   Publish signed APT through generated hosted single-writer workflows.
C: Verify signer fingerprint independently, repository Signed-By, signed
   metadata, exact candidate version/architecture/origin/digests and record.
   Install on bastion ONLY through repository APT, under
   /run/velnor/package-transaction.lock with the required exclusive flock.
   Never dpkg -i, apt install ./local.deb, scp a binary, bypass signatures,
   or overwrite packaged files. Preserve release activation/verify-installed.
   Develop unbounded/global-native prerequisites in parallel with A/B and
   include them in the bootstrap package. Enable them BEFORE first bastion
   job admission; verify no inherited limits/raw socket and run native smoke.
   Benchmark provisional native N. No need to wait for Scale Set to ship a
   correct baseline package. Every later deployed code change also uses APT.
D: Finish the native-shared global allocator, Rust Scale Set protocol,
   runner/DinD lifecycle, explicit three-provider generation and watchdog.
   Conform to actual upstream requests, not just self-confirming mocks.
   Publish the Scale Set package + signed feed, APT-upgrade, activate and
   reverify package identity/no-quotas. No untracked feature binary on host.
E: Dogfood Velnor's full corrected Linux inventory on all THREE providers,
   with real Docker/production-topology coverage, fault injection and
   mixed-engine N benchmarking. Require three consecutive full triple-green
   main runs plus a representative PR. Complete the independent gate report.
F: ONLY AFTER E migrate Jackin, access+typed config+published pin+regen.
   Preserve 40-unit coverage: baseline 38 Linux units x3 + two actual Apple
   Silicon Swift units, plus explicit Docker E2E. Fix Swift-on-Ubuntu and
   stale release.yml test. Build same-source local capsule Linux ELF;
   JACKIN_CAPSULE_BIN, docker-e2e profile, e2e feature, Docker/Buildx/script(1),
   nested privileged DinD, Java Testcontainers, relay TMPDIR/HOME/socket
   mounts, serial E2E groups and 20-capsule tests must actually pass.
   Preserve disabled releases. Complete full parity and PR/main qualification.
G: ONLY AFTER F migrate ChainArgos. Preserve 71-unit coverage (213 baseline
   executions) or documented equivalent. Fix policy/ownership, missing local
   actions, root Rust Docker context and all intended bake targets.
   Provision isolated PostgreSQL for Flyway/jOOQ (16 historical preparations),
   same job DB for both tools; no production/shared default DB. Exercise
   PostgreSQL/RabbitMQ/Redis/RustFS Testcontainers, explicit nextest CI profile
   and serial RustFS group. Preserve GraalVM/native-image, protobuf/native,
   Node/Bun and declared browser checks. Separate live RPC dependencies.
   Do not use global-name root Compose unchanged against host Docker.
   Complete full parity + PR/main. Document generic onboarding: trust/access,
   typed config, already published pin, regen/check, qualify, health inventory;
   new organization only adds declarative auth/registration, not new infra.

CAPACITY AND PROTOCOL DETAILS
Use one durable capacity ledger: reserved/acquiring/provisioning/assignable
idle/running/cleaning/uncertain consumes permits; total <= N. Offered demand
is not occupied. Do not preassign N to each scope or let idle native slots
starve the official lane. Gate native readiness/acquisition at the same
shared authority; discover demand separately without rewriting execution.
Start without permanently reserved warm-idle workers; pre-pull images.
Reconcile actual state before advertising capacity after restart.
Reserve before AcquireJobs; persist intent; handle partial returned IDs and
uncertain responses idempotently. Use TotalAssignedJobs, not capped message
counts; do not add statistics and reservations twice. Prove deferred-offer
validity/re-offer behavior, initial/nil poll handling, session/token refresh,
lastMessageID and durable ACK semantics. Never ack away unrecoverable work.
Normal restart preserves set identity; deleting a set is decommission only.
No exactly-once external-side-effect claims across GitHub retries.

Homogeneous official workers each receive a private pinned DinD daemon.
Share appropriate runner/DinD network namespace for localhost ports;
preserve inner service DNS. Match absolute workspace, externals, tool,
file-command, TMPDIR and required HOME/capsule bind paths and UID/GID.
Do not expose broad host paths or management credentials. Export logs and
clean only owned containers, networks, volumes, state and sockets before
returning capacity. A private socket may be named /var/run/docker.sock
inside a job; it must not be the physical host management socket.

Benchmark real cold/warm mixed workloads at candidate N values such as
16,24,32,48,64 and higher only when stable throughput improves. Measure
completed jobs/min, time-to-green, queue/setup/compile/test/cache/cleanup,
CPU, memory pressure/OOM, IO/disk/inodes and contention. N changes only;
containers stay unbounded. Respect serial test correctness. Do not call
all-green parity a fair performance comparison with different hosted hardware.

ADVERSARIAL VERIFICATION
Each independent verifier must try to disprove workflow ownership, source/
package/runtime/runner identity, APT-only deployment, protocol conformance,
idempotence, cleanup, global capacity, no quotas and three-provider coverage.
Inject: kill official runner, DinD, native worker and Velnor; restart during
acquire/JIT/create/start/run/ACK/cleanup; redeliver events; partial acquisition;
cancel queued/running; Docker restart; poll/acquire/ACK/refresh network loss;
bad digest/manifest/key/signer/ref; orphan and partial cleanup; stale control
generation; simultaneous scopes/engines; fork selector/input attacks.
Use owned canaries and coordinated host-impact windows. Preserve unrelated
jobs/data/SSH; no formatting/RAID, broad prune or deliberate whole-host OOM.
The second NVMe stays untouched. Limits of shared-kernel isolation are explicit.

One hosted required-result authority compares exact expected source/run/
attempt/plan/unit/provider/platform/command/profile/fixture identity with
actual execution/JUnit and management-correlated runner/container records.
Missing, skipped, wrong, cancelled or stale expected results fail. Poll all
API pages. Start watchdog after planning without depending on queued local
completion; authenticate fresh outbound Velnor health and preserve failures.
Initial targets: connected within 180s of reservation, cleanup within 120s,
free-capacity stall diagnosed within 5m, whole-local outage failed/incomplete
within 10m. Distinguish real backlog from a stall; set measured execution
limits. Cancellation/reruns cannot launder a prior failure into false success.

DELIVERY AND DECISIONS
Deliver integrated source, generated workflows, exact published/deployed
package/image/generator identities, sequential repository evidence, measured
N, protocol fixtures, fault reports, trust denial, ownership/no-legacy/no-VM/
no-Go-controller/no-quota checks and supported operational/onboarding runbooks.
Use actual implemented CLI/help; do not invent drain/scale verbs. State
migration/recovery is explicit: APT downgrade requires matching proven config/
state restoration, otherwise use tested forward recovery, not compat shims.
Judge each change by correctness and target fit, never effort/ROI.
Iterate fast: parallelize dependency-independent work, fix root causes,
merge small verified changes and unblock the APT critical path first.
Never replace evidence with skipped tests, policy bypasses or optimistic claims.
Continue independent work when one external prerequisite is blocked and name
that exact blocker. Completion means all ordered gates and actual final
three-provider defaults are independently proven, not merely discussed.
```

## Repo binding for this execution

- Run from `tailrocks/velnor` branch `docs/bastion-final-plan` (this PR); keep the three plan files above as the campaign authority.
- The main agent stays orchestrator-only; every module has a named author subagent and a different independent verifier subagent.
- Follow `AGENTS.md`: no legacy code, finish every migration, prefer breaking changes, iterate fast; never treat this research project as production-ready.
- Bastion host provisioning starts from the `ansible-configs` reference paths in spec section 8.5 (base setup, Docker install, inventory, runbooks), re-resolved against live `ChainArgos/java-monorepo` `main`.
- Docs provenance: the nine S1–S9 input SHAs in the spec annex were re-verified at commit time; any consolidation drift must be recorded as `old observation → new evidence → consequence`.
- Conventional-commit messages with DCO sign-off (`git commit -s`) on all campaign commits; no hand-edited generated YAML — generator → regenerate → verify.
