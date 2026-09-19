# /goal Execute the bastion three-provider CI campaign: deliver source changes, generated workflows, coherent releases, signed APT deployment, real three-provider verification, sequential repository migrations, and operating evidence through every ordered gate. Do not stop at planning, registration, or an author saying tests pass

## Starting state (authoritative, verify before acting)

- Repo: <https://github.com/tailrocks/velnor> (origin), branch `docs/bastion-final-plan`, PR #912.
- Campaign authority (this branch): `plans/bastion-three-provider-ci/spec.md` (target state, §1–§9),
  `plans/bastion-three-provider-ci/work-plan.md` (ordered steps A0–G2),
  `plans/bastion-three-provider-ci/checklist.md` (acceptance, 1:1 with step IDs),
  `plans/bastion-three-provider-ci/evidence.md` (audit facts as revalidation inputs only).
- Target: `root@37.27.110.241` (bastion), Debian 13, AMD EPYC 9454P, 48 physical / 96 logical CPUs,
  ~128 GB RAM. Delivery dependency: `tailrocks/velnor-apt`, signed feed at
  <https://velnor-apt.tailrocks.com/>. Consumers in HARD order: `tailrocks/velnor`, then
  `jackin-project/jackin`, then `ChainArgos/java-monorepo`, then generic onboarding.
  Read-only consumer research may run early; consumer mutation/activation waits for its gate.
- Retained baselines to re-resolve (never pins): Velnor `3353310c…` (17 units), Jackin `92f347ac…`
  (40 units), ChainArgos `235e479b…` (71 units); generator pins `b9c3156c…` (Velnor/Jackin),
  `1279c4f9…` (ChainArgos); Scale Set ref `fb563005…` (2026-09-15; `v0.4.0` differs);
  failure runs `35129353335`, `35136207272` (PR #904), `35114867283`, `35094895601`.
- Follow `AGENTS.md`: no legacy code, finish every migration, prefer breaking changes, iterate fast;
  never treat this research project as production-ready. Conventional commits with DCO sign-off
  (`git commit -s`) on all campaign commits; no hand-edited generated YAML ever.

## What remains

1. **Orchestrate, do not implement directly.** The main agent is orchestrator only: keep the
   persistent ledger (task ID, dependency, author, verifier, source/branch, finding, target
   invariant, evidence location, status, blocker, next action), assign work, resolve
   dependencies/decisions, review evidence, unblock. Delegate ALL substantive research, design,
   code edits, integration, tests, infra, publication, deployment, and verification to
   subagents — aggressively, keeping every useful slot busy. A designated integration subagent
   merges source and regenerates shared output; one infrastructure subagent owns bastion writes
   per window. Every module gets a named author AND a different independent verifier who tries
   to disprove the invariants and reruns critical checks. Never claim an agent ran unless it did.
2. **A0: refresh the live ledger.** Read `AGENTS.md`; `git fetch origin`;
   `gh api repos/tailrocks/velnor/git/ref/heads/main --jq .object.sha` (same for the other two
   repos); `gh pr view 904 --repo tailrocks/velnor --json state,headRefOid` (+ #901);
   `gh run view <35129353335|35136207272|35114867283|35094895601> --log-failed` with attempts;
   `gh release list --repo tailrocks/velnor --limit 20`; Scale Set main ref; APT omission blob;
   permissions without values; read-only bastion inventory
   (`uname -a; lscpu; free -m; lsblk; df -hT; docker info; systemctl list-units --type=service`).
   Record every drift as `old observation → new evidence → consequence`; architecture unchanged.
3. **A1–A3: repair bootstrap CI, qualify runtime products, prove the streak.** Reproduce → fix
   at the source (remove the enabling condition; name any deferred root cause) → regression-test
   each failure. `./target/debug/velnor-workflow --plain --force`, then `--plain --dry-run` = 0
   files; ownership inventory, structured policy, `actionlint` clean. `cargo test -p
   velnor-workflow`, contract tests, `cargo clippy --all-targets -p velnor-workflow -- -D
   warnings`, `cargo fmt --check` green. Implement publish-before-pin (trusted `R` stays bound;
   candidate `C` builds once per closure/platform; full closure; hosted producer + attestations;
   atomic pin+tree promotion; missing product = precise defect; zero cargo fallback; cold
   consumer verified; no `releases/latest` for the daemon). Then three consecutive FULL green
   `main` runs, zero reruns, bootstrap scope explicitly declared (never triple-qualified).
   `gh run list --branch main` + `gh run view` per run; record URLs, SHAs, plan digests,
   expected sets, timings. No protection bypass, DCO enforced.
4. **B1–B4: APT primitives, velnor-apt, bootstrap release, signed feed.** Implement ALL typed APT
   capabilities (source+identity, package, archs, suites, signer+secrets, coherence, assembly,
   records, retention, Pages, channel tasks); reuse `scripts/verify-release.sh` via a narrow
   typed contract; no shell/YAML execution from config; renamed-fixture genericity; negatives
   (bad source/digest/key/arch/incoherent) rejected pre-mutation. Publish + attest the generator
   product, then land ONE atomic velnor-apt commit (pin + full tree); remove the omission notice
   and stale direct-install docs only on real coverage. Cut the coherent bootstrap release
   (exact tag/commit/version, amd64+arm64, manifests/record, OCI/payloads incl. native arm64,
   attestations; unbounded/global-native prerequisites inside; coherence negatives) and publish
   the signed feed (verify-before-mutate, staged index, previous version retained, signed
   InRelease/Release + record, single-writer Pages, no rollback), then independently verify the
   LIVE candidate (`apt-get update`, `apt-cache policy velnor-runner`, hash/record comparison).
5. **C1–C2: locked bastion install, quota-free native, provisional N.** Authenticate the signer
   fingerprint against an independent reference, configure repository `Signed-By`, verify
   metadata/version/arch/hash/source/origin (attestation as a distinct check). Finish idempotent
   host setup from the spec §6.1 `ansible-configs` paths re-resolved against live
   `ChainArgos/java-monorepo` `main` (SSH preserved, second NVMe untouched, no libvirt). Drain,
   preserve config/secrets, then with VERSION set to the B4-verified candidate run the
   hold-aware exact-version transaction from spec §7, which unholds and re-holds under one
   exclusive package lock. First run `install -d -m 0750 /run/velnor`, `apt-get update`,
   and `apt-cache policy velnor-runner`; finish with `dpkg-query -W velnor-runner`. Then binary/record/
   manifest identity, `release verify-installed` BEFORE start, package-derived activation/drain/
   health (never invent verbs). Enable unbounded + native-backed global N BEFORE the first job;
   prove quota-free (`docker inspect` HostConfig, cgroup `cpu.max`/`memory.max`/`memory.high`/
   swap/cpuset ancestry incl. inherited limits, `systemctl cat` units/drop-ins, build env) and
   no raw socket mount; authenticated health flowing; one real native smoke job (URL + engine +
   occupancy + cleanup). Benchmark provisional native N at 16/24/32/48/64+ (comparable
   conditions, full metric set, unrelated jobs unharmed). Never `dpkg -i`, local `.deb`,
   copied binaries, or signature bypass.
6. **D1–D3: Rust Scale Set adapter, three-provider model, upgraded package.** Pin the audited
   upstream rev; implement the full Rust protocol in the spec §5.1 9-step order
   (idempotent observations → queue+trust → reserve+intent → returned IDs → JIT intent →
   durable ACK → `TotalAssignedJobs` → shared-grant advertisement → idle-poll reconcile);
   recorded fixtures AND live-canary conformance (mocks alone rejected); App credentials with
   management keys out of jobs + in-process refresh; homogeneous digest-pinned runner + private
   DinD (never `latest`); private-Docker semantics on real jobs (private socket, shared netns,
   identical binds + ownership, log export, collision-free); shared-allocator races prove
   ≤N/no-per-scope-N/no-starvation/oldest-observed; every lifecycle boundary reconciled; set
   identity survives restart. Then the one provider-set schema everywhere (mechanical no-legacy
   proof), typed platform/trust/capabilities with explicit-failure negatives, planner→3-provider
   fanout with provider display names + disjoint selectors, strict expected-result set with
   per-class negatives, capability tests, hosted watchdog per spec §8 proven against
   180s/120s/5m/10m, trust negatives denying bastion. Ship it all as the second signed release +
   feed and upgrade bastion on the identical locked path; repeat the FULL quota-free inspection
   after upgrade. No Go controller, no native rewrite, no sideloaded binaries.
7. **E1–E2: Velnor full qualification + adversarial proof.** Confirm the 17-unit inventory (or
   reviewed correction); fan every eligible Linux unit to all three providers with identical
   inputs incl. production-topology + Docker conformance on both local engines; record the full
   unit×provider matrix (engines, digests, tests/JUnit, cache/timing, cleanup); planner-declared
   exceptions only; fail-fast disabled; sole-ownership proof. Execute EVERY spec §8 fault-matrix
   row on canaries (kill runner/DinD/worker/Velnor at each point; redelivery + partial acquire;
   cancel incl. reassignment; permit races; Docker restart + network loss; bad
   digest/manifest/signer/ref/key/package/arch/record; orphans + stale generations; fork
   spoofing + input substitution; bad/missing reports; hidden limits) in a coordinated window
   preserving unrelated work/SSH (no destructive ops, broad prune, or deliberate host OOM);
   prove each row's invariant. Benchmark mixed-engine N cold+warm. Obtain three consecutive full
   triple-green mains + one representative PR; complete the independent gate report.
8. **F1–F2: ONLY AFTER E — migrate + qualify Jackin.** Authorization + scope + typed config only;
   already-published pin; regenerate + own + policy/lint clean. 40-unit ledger (38 Linux×3 + 2
   real Apple Silicon Swift + E2E/controls; never 120 Linux). Fix native Apple routing (same-job
   XCFramework→Swift); re-resolve tools (Rust 1.97.1, Bun 1.3.14, Node 24.18.0, Ubuntu 26.04,
   macOS 26) with no silent substitution; reconcile `release.yml` without legacy restore or
   release-enabling (releases stay disabled). Explicit `docker-e2e` with `e2e`: same-source
   capsule ELF + `JACKIN_CAPSULE_BIN`, Docker/Buildx/Compose/`script(1)`/nested-DinD/
   Testcontainers/TLS-relay proven, serial groups kept, 20-capsule fanout exercised (one job),
   zero fixture weakening. Then full parity (counts/engines/digests/caches/no-quotas/cleanup) +
   PR + main; remeasure N iff the mix changed; prove Velnor non-regression for any generic fix.
9. **G1–G2: ONLY AFTER F — migrate + qualify ChainArgos, then close.** Same drill: auth + scope
   - typed config, published pin, same controller + global N, regenerate; ownership/trust +
   missing local actions FIRST; 71-unit/213-execution ledger; job-local PostgreSQL with
   graph-justified dedup of the 16 `flywayMigrate jooqCodegen` preparations, same job DB via
   build-supported vars (`POSTGRESQL_DB_HOST/PORT`), never prod/shared; Testcontainers
   PG/RabbitMQ/Redis/RustFS with explicit nextest profile + serial RustFS group + verified
   effective config; digests locked (`rustfs/rustfs:1.0.0-beta.8`, `postgres:18-alpine`, …);
   root-context 12-target bake; no unchanged global-name root Compose on shared host;
   GraalVM-25/wrapper-9.5.1/Rust-1.98.1/protobuf/Node-24.20.0/Bun-1.3.14/browsers kept or
   tested-updated; Playwright audited; Micronaut `1.44` checked; single-writer +
   release-disabled kept; external/RPC failures labeled; PR + main. Then close: mechanical
   no-regression proofs (no VM/Go/quota/reservations/legacy/hand-YAML) on all three trees;
   fresh renamed-fixture onboarding end to end with no new infra (new org = auth metadata
   only); pins + deployed identities recorded; idempotent setup + reinstall + recovery verified
   (APT-downgrade-only-with-proven-snapshot else forward recovery, no shims); runbooks match
   implemented help; final N + pool/provenance/fault bundle; full acceptance report signed by
   the final independent verifier.

## Rules

- Gate discipline: implement in parallel, but NEVER cross a deployment/consumer gate before its
  prerequisites (F only after E, G only after F). Gate regression per work-plan §0.8: publish
  tooling before repin, package before upgrade, requalify affected migrated repos before
  advancing. Bootstrap work is labeled bootstrap, never triple-qualified.
- Hard architecture (spec): one APT-installed Velnor Rust control plane on Debian/Docker — no
  VMs/libvirt/QEMU/KVM runner infra, no Kubernetes, no Go controller/sidecar/scheduler; native
  keeps its execution + mediated lease API (never DinD-rewritten, never raw socket, never faked
  with the official runner); official lane = unmodified runner + private per-job DinD, all pins
  by digest, never `latest`; raw `/var/run/docker.sock` (or alternate/proxy) never in any job;
  one host-wide `max_jobs=N` for both local modes and all scopes (no reservations/weights/
  pools; registration = authorization, not capacity); admitted containers unbounded (no Docker/
  systemd/slot/heap/disk/PID ceilings; drop-ins removed, never recreated; cgroups for
  ownership/observation only; serial test groups kept); oldest-observed-first admission (no
  global-FIFO or JIT-runner promises); exactly `github-hosted`, `github-self-hosted`, `velnor`
  (no aliases/shims/dual parsers/no inference); `velnor-workflow` sole owner of every touched
  workflow/action/manifest (no hand YAML, copies, hidden templates, raw escapes, repo-name
  hacks); breaking changes freely, no compat shims/aliases/deprecation/dual models; bastion is
  trusted-tier, not hostile multi-tenant (forks default hosted, trust outside PR YAML, never
  privileged-untrusted `pull_request_target`, management keys out of jobs); one writer for
  signing/publication/releases/deployments/Renovate/issues/destructive maintenance; DCO and
  real required checks kept; release-disabled stays disabled.
- No hand-edited generated YAML, ever; generator → regenerate → verify, always. No weakening
  of policy, rulesets, or trust to go faster. No mutable `latest`, no unverified artifacts, no
  cache-dependent correctness, no merge-SHA product identity. No invented CLI verbs — derive
  activation/drain/health from implemented package/help. Judge correctness and target fit, never
  effort/ROI. Name exact external blockers and keep independent work moving.

## Deliverable

Report: ledger with authors/verifiers per module; per-gate evidence (run URLs, SHAs, plan
digests, unit×provider matrices, test counts, digests, benchmark tables, fault records,
trust denials, no-legacy/no-VM/no-Go/no-quota proofs); exact published/deployed identities
(pins, packages, images, signer, APT records); sequential repository evidence in hard order;
runbooks. Completion = every item in `plans/bastion-three-provider-ci/checklist.md` checked
with cited evidence AND signed off by its independent verifier — actual final three-provider
defaults independently proven, not merely discussed.
