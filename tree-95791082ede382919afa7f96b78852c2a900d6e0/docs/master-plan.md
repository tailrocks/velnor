# Velnor Master Plan — Goal, Current State, and Execution Sequence

Status date: 2026-06-10. This document is the single top-level plan for making
Velnor the fastest, most stable GitHub-Actions-compatible runner for the two
production target repositories, and for making the whole CI/CD estate
(ChainArgos + Jackin) maximally fast on both runner lanes. It extends
[roadmap.md](roadmap.md) (runner-internal implementation detail) and is bound
by [mission.md](mission.md) and the hard rules in [`../AGENTS.md`](../AGENTS.md).

Decision rule for everything below: work is judged by **correctness against
the goal**, never by effort or cost. Velnor is allowed to be expensive and
resource-hungry; it is not allowed to be slow, lossy, or fragile. Every bug
gets a root-cause analysis (why did the architecture permit it?) and the fix
removes the enabling structure, not just the symptom.

---

## 1. What Velnor is (current implementation, verified 2026-06-10)

A Rust (tokio) daemon (`crates/velnor-runner`, ~33k LOC + `velnor-tools` ~4.7k)
that impersonates GitHub's official runner over the **V2 protocol only**:
JIT runner configuration → broker long-poll session → run-service
acquire/renew/complete → Results Service (Twirp + WebSocket live log feed).
Each assigned job runs in a fresh Docker container (`velnor/job-ubuntu:24.04`,
Rust 1.96 + mise + sccache + mold + buildx preinstalled) on a per-job Docker
network, with the host Docker socket mounted for Docker/Buildx workloads.
Known marketplace actions are executed by **19 native Rust adapters**
(checkout, cache, artifacts, rust-cache, sccache, mise, paths-filter, docker
login/buildx/bake/metadata/build-push, pages, runtime-export, …); unknown JS
actions run in a `node:20` sidecar container. One daemon supervises `--slots N`
independent JIT runner identities (currently fixed count, 10 on Sentry for
java-monorepo, 4 for blockchain-nodes, 2 for the fixture).

Deployment: Debian package built by `cargo deb` (`release-deb.yml` on tag),
published to the apt repo `velnor-apt.tailrocks.com` (reprepro + GitHub Pages),
installed on the Sentry host as three systemd units
(`velnor-daemon`, `velnor-daemon-blockchain-nodes`, `velnor-daemon-fixture`),
each reading `/etc/velnor*/velnor.env`.

Proven results so far: java-monorepo runs Velnor-by-default with GitHub lane
on demand (PR #1343/#1366); Velnor executes Rust jobs **2.5–3× faster** than
`ubuntu-latest` (1.1–2.3 min vs 3.4–5.7 min per test job), equal Ansible,
faster Docker with warm buildkit cache.

## 2. The goal

1. **Both target repos use Velnor by default** for every PR and push to main —
   `ChainArgos/java-monorepo` (done) and `ChainArgos/blockchain-nodes` (default
   is set, but the Velnor lane must be proven green) — while the GitHub-hosted
   lane stays selectable forever (`lanes`/`lane` input: `velnor | github |
   both`) so any run can be executed on both for comparison.
2. **Full parity, then superiority, with GitHub-hosted runners** for exactly
   the feature surface those two repos use: every job, every step, every log
   line, live streaming, ANSI color, timestamps, conclusions, artifacts,
   caches. Velnor must never be less informative than GitHub. Same YAML —
   drop-in replacement, no workflow rewrites to accommodate Velnor.
3. **Significantly faster than GitHub-hosted** — both per-job execution
   (already 2.5–3×) and, critically, **queue-to-start latency**: jobs must be
   picked up immediately, parallel as wide as the host allows.
4. **Fully autonomous operation**: installed/upgraded only via
   `apt-get update && apt-get upgrade`, survives reboots and crashes, cleans up
   after itself, scales its own slot count to the host's available resources,
   and never requires manual runner-registration surgery. Eventually: multiple
   servers form a cluster by simply installing the package — GitHub's broker
   distributes jobs across all registered runners with the same labels.
5. **Rust + tokio only**, zero-copy and allocation-discipline in hot paths,
   maximum parallelism. All tool installation in CI flows through **mise** —
   no ad-hoc installers.
6. `actions/runner` (C#) remains the protocol source of truth; latest protocol
   paths only.

## 3. Incident review 2026-06-10 — and the bulletproofing it mandates

PR ChainArgos/java-monorepo#1380 sat queued for hours. Root causes found and
fixed today, each with the structural defect it exposed:

| # | Failure | Root cause | Structural defect → mandated fix |
|---|---------|-----------|----------------------------------|
| 1 | All 3 daemons 401 on JIT config since Jun 9 11:49; 0 runners registered; every Velnor job queued forever | All three `/etc/velnor*/velnor.env` were rewritten in the same second with a literal `GITHUB_TOKEN=${VELNOR_GITHUB_TOKEN}` placeholder (systemd `EnvironmentFile` does not expand variables) — the real PAT was destroyed; labels were rewritten too (bcn/fixture lost `velnor-target-mvp`) | Secrets live in a file that tooling can casually rewrite; daemon happily runs with a syntactically impossible token; nothing observes "0 runners registered" (P1.1–P1.4) |
| 2 | Daemons exit(1) when all slots fail to configure → systemd restart loop every ~3 min (counters 508 / 2292) | "0 usable slots" is treated as fatal | A runner daemon must never give up: keep retrying registration with backoff while alive (P1.2) |
| 3 | v0.1.1/v0.1.2 debs never built; apt repo stuck at 0.1.0-rc12; Sentry running 14-releases-old code | Flush-left heredoc body in `release-deb.yml` (commit 130a858) terminated the YAML block scalar → workflow file invalid since Jun 9 → every tag/branch push produced a phantom 0-job failed run | No workflow linting in velnor CI; no alert on release-pipeline failure (P1.5, P1.6) |
| 4 | Hours-long queues on Jun 8 (11–12.7 h) during fleet rebuild | Fixed slot fleet + registration churn from idle-timeout exits + stale "busy" runners | Slot management is static and fragile (P3.4 dynamic slots; P1.2 no idle-exit) |
| 5 | Workspace uncompilable since Jun 8; v0.1.2 `--locked` builds failed | A "remove license headers" commit blindly deleted the first 2 lines of 20 source files (all real code); Cargo.lock never regenerated after the rustls removal + version bump | The repo had **no compile CI at all** — added `ci.yml` (fmt, clippy -D warnings, tests, actionlint, jackin cache stack) (P1.5) |
| 6 | The published 0.1.2 deb was 3.7 KB — **no binary**; upgrading removed `/usr/bin/velnor-runner` and took the fleet down again (~13 min, rolled back from the local apt cache) | Current cargo-deb treats an explicit `assets` list as a full replacement of the implicit defaults, silently dropping the `[[bin]]` | Binary listed explicitly + release-deb now fails if the deb lacks `usr/bin/velnor-runner` or is < 1 MB; same heredoc YAML bug also fixed in velnor-apt `publish.yml`; apt signing key had rotated (keyring refreshed on Sentry) |
| 7 | Fixture compat Velnor lanes failed at planning: "action metadata not found in …/.github/actions/check-fixture-output" | GitHub sets `Condition: "success()"` on every step; `CheckoutPlan::requires_runtime_context` treated any condition as runtime → eager checkout never ran → local composite actions unresolvable (verified at the wire from a job dump) | Trivial default conditions (`success()`/`always()`/empty) now stay eager (0.1.4); longer-term: run checkout as a real ordered executor step (no eager/deferred split) |
| 8 | jsonwebtoken 10.4 bump would panic OAuth signing at runtime ("could not determine CryptoProvider") | Crate now requires exactly one crypto-provider feature; defaults gave none | Pinned `rust_crypto` + `use_pem`; caught only by the new clean-room test run — CI now gates this class |
| 9 | **Zombie fleet (2026-06-11)**: daemons alive (NRestarts=0), broker polls returning "no message" forever, while GitHub's runner registry showed the runners offline/missing → jobs queued indefinitely (jm 0/10, brown 0/2, fixture 0/2, bcn 1/4); doctor alerted but nothing healed | Split-brain between the two health signals: `get_runner_message` mapped **any** empty-body response (401 expired-token, 403, 404, even curl status 0) to "no message", so an idle slot's OAuth token expired (~1h) and the slot kept "successfully" polling a dead session; idle slots never refresh tokens (only job/control paths do) and nothing ever reconciled local state against GitHub's runner registry | P1.9: poll classification fixed (non-2xx/status-0 = error → supervised recycle), proactive idle token refresh (40 min), idle registry reconciler (3 min interval, 404 = recycle now, offline 2 strikes = recycle), bounded max idle slot age (4 h), forensic per-slot log files + tracing (full investigation: `docs/velnor-fleet-health-investigation-2026-06-11.md`) |

Fixes shipped today: token + labels restored and all three daemons
re-registered (10+4+2 runners); PR #1380 merged green; release pipeline fixed
end-to-end (YAML, lock, imports, vendored OpenSSL for zigbuild, deb binary +
size guard) — v0.1.3 released with holla-style tar.gz (mac arm64/x86_64 +
linux arm64/x86_64) + a real deb; v0.1.4 adds the eager-checkout fix; Sentry
runs rc25 (first live-streaming build) pending the 0.1.4 apt upgrade.

**"Never again" requirements (Phase 1) follow directly from this table.**

## 3a. Universal caching mandate (operator hard rule, 2026-06-11)

**Everything that can be cached must be cached — in every pipeline, in every
repository.** Watching dependencies compile in CI (`Compiling memchr …` ×100)
is a defect, not noise. Concretely, every GitHub Actions pipeline in the
estate must cache:

- **Rust compilation**: sccache enabled on every compiling job (GHA backend
  on hosted runners, host-local on Velnor), plus the cargo registry/git cache
  (registry cache/index/src + git db keyed on Cargo.lock) and a target-dir
  cache with branch-fallback restore-keys where profiles allow.
- **Docker layers**: every `docker build`/buildx invocation carries
  cache-from/cache-to (registry buildcache refs for published images, gha for
  PR-only builds) — never a cold layer rebuild of unchanged layers.
- **Inside Dockerfiles**: apt/apk installs behind `--mount=type=cache`
  (and per-tool layers so one bump doesn't invalidate everything).
- **Tool installs**: mise tools pinned + mise/tool caches persisted;
  `cargo:` backends never silently recompile (pin versions; cache the
  binaries).
- Anything downloaded repeatedly (LFS objects, SDKs, collections, pip) gets
  an actions/cache entry keyed on its lockfile/OID.

**Acceptance test — rerun idempotency:** re-running a pipeline that just
finished on the same commit must hit caches for effectively everything: no
crate downloads, no dependency compilation walls, no docker layer rebuilds,
no tool re-installs. Any re-download/recompile on an unchanged rerun is a
defect to root-cause (wrong key, evicted cache, missing restore-keys, cold
backend) and fix — iterate until the maximum the tooling allows.

**Performance-first rule:** CI/CD speed is a first-priority mandate, not a
nice-to-have. Anything parallelizable runs in parallel; anything sequential
gets challenged; every pipeline is aggressively tuned (and re-tuned) for the
fastest result the tools can deliver. Velnor itself is designed under this
rule — if maximum performance is not yet reached, keep iterating.

Scope (the ten-repo estate, audited and enforced):
| Repo | Status |
|---|---|
| jackin-project/jackin | **pipeline source of truth.** 4-layer stack + sccache; cache-quota eviction fix in PR #563 (bench merge + nextest fallback ladder) |
| jackin-project/jackin-the-architect | agent role; per-tool layers + cache mounts; PR/publish caches bridged (role-action#57 + PR #113) |
| jackin-project/jackin-agent-smith | agent role; caching mandate applied (PR #67: registry-warm CI, gitleaks + Renovate caches) |
| jackin-project/jackin-sentinel | agent role; caching mandate applied (PR #7, identical to smith) |
| ChainArgos/jackin-agent-brown | agent role; **dual-lane, Velnor default — FULLY PROVEN** (PR #89 + publish-on-Velnor green end-to-end on 0.1.14: digest builds amd64 + arm64-QEMU, artifact fan-in, metadata, imagetools manifest, cosign — 13–32 s/job with warm registry layer cache) |
| ChainArgos/java-monorepo | registry cache (PR #1382) + sccache + job-level gating (PR #1384) + kestra layer cache (PR #1386) |
| ChainArgos/blockchain-nodes | registry layer cache + exists-sweep (PR #603); cached-build proof pending next version bump |
| tailrocks/holla | NOT YET AUDITED |
| tailrocks/velnor | ci.yml jackin-style stack; release-deb has sccache+registry cache |
| tailrocks/velnor-actions-fixture | NOT YET AUDITED |

**Estate unification rule (operator mandate):** these repositories share very
similar dependencies and tooling, so they must share the same techniques,
configuration shape, and writing style. jackin is the base source of truth
for pipeline design (itself continuously perfected); every improvement made
anywhere is propagated to every applicable repo. The four agent-role repos
(jackin-the-architect, jackin-agent-smith, jackin-sentinel,
jackin-agent-brown) must use the shared `jackin-project/jackin-role-action`
(composite action + reusable publish workflow) with identical configuration —
no per-repo drift. **Lane policy (operator, 2026-06-11): exactly three repos are dual-lane —
ChainArgos/jackin-agent-brown, ChainArgos/java-monorepo,
ChainArgos/blockchain-nodes. All three default to Velnor, with `github` and
`both` selectable per run.** Every other estate repo runs GitHub-hosted only
(the fixture keeps its own both-lanes-by-default contract role) — but all
repos share the same approaches and code shape, and **Velnor must support
the full feature surface of every estate repo** so any of them can be
switched to Velnor at any moment without workflow rewrites. Velnor must
support the role-action pattern end-to-end for agent-brown (hadolint JS
action, jackin-role binary download, buildx build with gha/registry caches,
digest publish + cosign).

## 3b. Stability-first mandate (operator directive, 2026-06-11)

**Stability has NOT been achieved yet** — incident #9 (zombie fleet) proved
the daemons could look healthy locally while GitHub had no schedulable
runners. Until the P1.9 gates pass (live split-brain repro heals itself +
24 h zero-zombie soak), stability work outranks new performance features.
Standing rules that follow:

- **Two health signals, both trusted**: broker-poll success ("my session can
  ask for work") and GitHub runner-registry state ("the scheduler can assign
  to me") are independent; the daemon must continuously reconcile them and
  self-heal on divergence. Doctor alerts; the daemon heals.
- **Forensics from logs alone**: every failure mode of this class must be
  diagnosable purely from on-disk logs (`logs/{broker,registry,lifecycle,
  daemon}.log` + `trace.jsonl`). Any new daemon behavior ships with log lines
  detailed enough to reconstruct the incident pattern after the fact —
  identity-prefixed (slot/agent_id/session), statuses included, control
  messages enumerated. If an incident cannot be analyzed from the logs, the
  logging — not just the bug — gets fixed.
- **Tracing as the performance lens**: `tracing` spans (JSON file layer with
  busy/idle close timings; OTLP via the `otel` feature +
  `VELNOR_OTLP_ENDPOINT`) are the standard way to find slow blocks in the
  runner itself. New hot paths get spans; performance claims cite span data.
- Every benchmark/timing campaign first verifies fleet health (doctor green
  across all daemons) so queue time is never silently folded into lane
  timings.

## 4. Hard constraints (inherited, non-negotiable)

- Rust-first, tokio, latest stable toolchain; `actions/runner` as protocol
  truth; latest protocol path only; fixture
  (`tailrocks/velnor-actions-fixture`) is the contract — fix Velnor, never the
  fixture.
- Both lanes forever: never delete the GitHub-hosted path from any workflow.
- mise is the only tool installer in target workflows.
- Velnor ships as a `.deb` via `velnor-apt.tailrocks.com`; upgrades via plain
  `apt-get upgrade`.
- Jackin repos stay on GitHub-hosted runners (no Velnor there for now); they
  serve as the optimization reference and receive improvements too.

## 5. Execution plan

Phases are ordered; items inside a phase are parallelizable unless marked.
Every item names its verification gate. P0 is complete (today's incident);
P1 must land before anything else because fleet reliability gates all
downstream proof runs.

### Phase 0 — restore service (DONE 2026-06-10)

- [x] Diagnose + fix Sentry daemon 401s (token + labels), re-register 16
      runners across 3 daemons.
- [x] Fix `release-deb.yml` invalid YAML on main; dispatch v0.1.2 deb build.
- [x] PR java-monorepo#1380 unblocked → merge once all checks green.
- [x] Sentry upgraded via apt (0.1.4 → 0.1.5 with the P1 daemon); all three
      daemons healthy, fleet re-registered, fixture compat green on both
      lanes including compare-results, live per-line log streaming verified.

### Phase 1 — bulletproof operations ("this never happens again")

1. **[DONE 0.1.5] Fail-fast credential validation**: `diagnose_github_token`
   classifies empty / `${...}` placeholder / implausible shapes; reported on
   startup, in `systemctl status` (sd_notify STATUS), and on every retry with
   the file to fix. Kill-tested live on Sentry: placeholder token → unit
   stays active, NRestarts=0, status line shows the exact problem.
2. **[DONE 0.1.5] Never-exit resilience loop**: supervised daemon pass with
   capped exponential backoff + jitter retries forever; slot JIT
   reconfiguration retries forever instead of killing the slot; failed slot
   tasks are respawned (one broken slot cannot stop the rest); one-shot modes
   (`--once`, dry-run) keep fail-fast semantics for tests/tooling.
   Remaining sub-item: 24 h restart-counter-stays-0 observation.
3. **[DONE 0.1.5] Secrets that survive tooling**: token lives in
   `/etc/velnor/secrets.env` (0600, never shipped/touched by the package);
   `postinst` migrates idempotently (verified live during the 0.1.5 upgrade);
   units read `velnor.env` then `-secrets.env` (secrets win). Template
   instances `velnor-daemon@<name>` use `/etc/velnor/<name>.env` +
   `<name>.secrets.env` — Sentry's three hand-rolled units migrated.
4. **[DONE 0.1.6] Fleet watchdog**: `Type=notify` + `WatchdogSec=180` +
   sd_notify READY/STATUS/WATCHDOG (0.1.5), plus `velnor-runner doctor` — a
   packaged 10-minute timer (default + per-instance, auto-enabled by
   postinst) that lists this daemon's registered runners and fails the unit
   loudly when 0 are online (token diagnosis included). Verified live on
   Sentry. Release-pipeline failures already email the tag pusher via
   GitHub's native workflow-failure notifications.
5. **[DONE] Lint + compile gate**: `ci.yml` (fmt, clippy -D warnings, full
   test suite, actionlint+shellcheck, ci-required aggregate) with the jackin
   cache stack, on GitHub-hosted runners.
6. **[DONE] Release pipeline end-to-end automation**: proven on
   v0.1.4–v0.1.6 — tag → CI → deb (binary + size guard) → GitHub release
   (tar.gz ×4 targets) → velnor-apt upload → signed apt publish → Pages,
   zero manual steps, **amd64 + arm64** (v0.1.6: cross-safe packaging —
   link-time strip + explicit depends instead of $auto/dpkg-shlibdeps).
7. **Token strategy** (durable fix for incident #1): move from a personal
   classic PAT to a **GitHub App** installation (org-scoped, runner-admin
   permissions) minting installation tokens in-daemon, or at minimum a
   long-lived fine-grained PAT dedicated to Velnor with expiry monitoring in
   the doctor probe. Gate: registration works with the new credential; doctor
   warns ≥14 days before expiry.
8. **Release parity with holla** (operator mandate, model:
   tailrocks/holla v0.4.2): every `v*` tag publishes (a) `tar.gz` + sha256 for
   macOS arm64/x86_64 and Linux arm64/x86_64 (release.yml — already built this
   way), (b) the `.deb` for latest Debian via velnor-apt (release-deb.yml),
   and (c) a Homebrew formula in `tailrocks/homebrew-velnor` (job currently
   stubbed `if: false` — needs the tap repo + a push token, modeled on
   holla's release.yml/homebrew-holla). Velnor's own CI/release always runs on
   GitHub-hosted runners. Gate: `brew install tailrocks/velnor/velnor-runner`
   and `apt install velnor-runner` both install the tagged version with no
   manual steps.
9. **[CODE DONE 0.1.15 — live repro gate pending] Fleet health
   reconciliation + forensic observability** (incident #9; operator
   directive 2026-06-11: *stability was NOT achieved — it is the current
   focus*). A long-running daemon must survive GitHub-side runner
   disappearance; broker-poll success alone is not fleet health. Shipped in
   0.1.15:
   - **Poll truthfulness**: `classify_broker_poll` — only 204/2xx-empty is
     "no message"; 401/403/404/5xx/curl-status-0 are errors that feed the
     supervised error path and recycle the slot with a fresh JIT config.
   - **Proactive idle token refresh** every 40 min (inside the ~1 h OAuth
     lifetime), rebuilding broker + run-service clients in place.
   - **Registry reconciler**: every 3 min an idle slot looks up its own
     `agent_id` (curl transport): 404 → recycle immediately; not-online for
     2 consecutive checks → recycle; lookup errors never kill a healthy
     session. Recycle reasons land in sd_notify STATUS.
   - **Bounded idle age**: idle slots recycle every 4 h regardless
     (`--max-idle-slot-age-seconds`, 0 disables) so unknown decay modes
     cannot accumulate.
   - **Forensic logs** (per-slot folders, analyzable offline — operator
     mandate): `<slot-config>/logs/{broker,registry,lifecycle}.log` +
     `<config-base>/logs/daemon.log`, every line timestamped +
     identity-prefixed (runner/agent_id/session), 32 MB rotation; every
     broker poll status, control message type, refresh, reconcile verdict,
     and recycle reason is recorded.
   - **Tracing** (operator mandate): `tracing` + JSON file layer
     (`logs/trace.jsonl`, span-close busy/idle timings for performance
     analysis; `VELNOR_LOG` filter) + forensic-event bridge; `otel` cargo
     feature adds `tracing-opentelemetry` OTLP export gated on
     `VELNOR_OTLP_ENDPOINT`. Next: instrument executor step phases.
   Gate (the incident's repro, not yet run): delete an idle slot's runner
   registration via the GitHub API → daemon detects within ≤3 min and
   recycles without restart → doctor shows full fleet → a Velnor-lane
   fixture dispatch gets picked up. Then a 24 h zero-zombie soak during the
   benchmark campaign.

### Phase 2 — make the GitHub-hosted lane as fast as possible (reference parity baseline)

Why first: the GitHub lane is the benchmark Velnor must beat; it must be in
its best possible shape (jackin playbook applied) before timing comparisons
mean anything. Authorized: create + merge PRs autonomously in all repos;
never remove testing/release correctness while speeding things up.

**ChainArgos/blockchain-nodes** (weakest CI today — biggest wins):
1. **[DONE PR #603]** Buildx registry layer caching
   (`chainargos/<pkg>:buildcache-<arch>`, `mode=max`) opt-in in
   `docker-build.rs` (BUILDX_CACHE=registry, set workflow-wide); named
   builder (`bcn-ci`) reused. Cold real builds were geth 95 min / cardano 97
   / arbitrum 105 — cached-repeat proof lands with the next version bump.
2. **[DONE PR #603]** 36 copy-pasted jobs (2035 lines) → discovery job +
   base/build chain + one matrixed leaf job (~290 lines).
3. **[DONE PR #603]** One Docker-Hub exists-sweep feeds only missing
   packages into the matrix — validated live: no-op dispatch = ~1 min
   (22 s sweep, leaf matrix skipped) vs the former 13–17 min fleet sweep.
4. **[PARTIAL PR #603]** `timeout-minutes: 180` everywhere + workflow-level
   run-stacking concurrency shipped. Remaining: Renovate hourly churn
   restriction + GitHub-lane fallback so repo automation survives a fleet
   outage.
5. arm64 without QEMU: native arm64 builders (ubuntu-24.04-arm on the GitHub
   lane; Velnor arm64 host later) + per-arch jobs + manifest merge, replacing
   serial QEMU emulation (2–5× tax on compile-heavy images).
6. Minimal PR CI (actionlint + build dry-run on `.github/**` changes) and
   automerge restricted to digest/patch — today a bad bump lands on main
   unvalidated (it caused the early-June failures).
7. heimdall LFS object (235.7 MB) cached by OID instead of re-downloaded.
8. `GITHUB_TOKEN` via `--secret id=github_token` instead of `--build-arg`.
   Gate for all of the above: `lane=github` full sweep ≤ baseline wall time;
   a one-package version bump builds exactly that package with warm cache;
   `lane=both` dispatch compares clean.

**ChainArgos/java-monorepo** (already strong; close the gaps):
1. **[DONE PR #1384]** No-op test jobs eliminated: the 8 `test-*` jobs gate
   at job level on the decide expression (verbatim hoist); unchanged
   packages skip before scheduling. (Cargo registry cache for all compiling
   jobs landed earlier as PR #1382.)
2. Pin and binstall tools: `cargo:cargo-nextest = "latest"` compiled from
   source on cache miss → pin version, prefer prebuilt (`ubi:`/binstall
   backend in mise). Same for `cargo:rust-script` in kestra workflow.
3. Ansible job: install only python via `install_args` (today it installs
   GraalVM + Node + Rust for a syntax check); cache pip + galaxy collections.
4. Renovate: path-filter or schedule-only (today every main push triggers it
   with no filter, racing the daily run); per-ref concurrency.
5. Kestra docker jobs: buildx layer cache (none today), concurrency group,
   run the cheap existence check first in a plumbing job.
6. `rust-required` gate: stop matching display-name prefixes (brittle —
   already broke once); query by job IDs/needs results.
7. Buildx `type=gha` on the Velnor lane: swap to local/registry cache on
   self-hosted (paying GitHub cache API from Sentry is wasted latency).
   Gate: PR with one package changed runs exactly that scope; full-touch PR
   runs everything; `lanes=both` timing report shows GitHub lane at its best.

**Jackin repos** (reference stays GitHub-hosted; fix their own gaps):
1. **[PARTIAL — PR jackin#561 merged]** `test` un-serialized from
   `check-all-features` (up to ~2.5 min off the critical path) and `audit`
   gated on the rust filter. Remaining: spell-check gating, bench-matrix
   merge, cross-job target-cache restore-keys.
2. **[PARTIAL — PR architect#112 merged]** `create-pull-request` bumped to
   v8.1.1 (node24; forced migration 2026-06-16). The toolchain workflow's
   PR-creation 403 is the org-level "Allow GitHub Actions to create and
   approve pull requests" setting — **needs org admin** (repo-level PUT is
   overridden; my token lacks admin:org). Remaining: PR/publish buildx cache
   unification, paths-filter + concurrency.
3. jackin-role-action: add self-CI (actionlint + shellcheck + smoke-validate
   against the fixture role); deduplicate the inlined download logic.
   Gate: all three repos green; jackin PR wall time ≤ current 4.2–5.2 min
   with the test job no longer serialized.

### Phase 3 — Velnor core: fastest runner engineering

Ordered by measured impact on queue-to-first-log and job wall time; all are
"correct architecture" items, not micro-optimizations. Full refactoring is
explicitly in scope.

1. **Native HTTP client, kill the curl subprocesses.** Today every GitHub
   call (OAuth, broker poll, acquire/renew/complete, Twirp, artifact upload)
   spawns `curl` with temp-file config (TLS-fingerprint throttle workaround)
   — 6–15 process spawns + file round-trips per job, in `spawn_blocking`.
   Root-cause the fingerprint throttle properly: reproduce, then fix with a
   TLS stack whose fingerprint GitHub accepts (rustls with tuned ClientHello,
   curl-impersonate as a linked library, or hyper with the exact cipher/ALPN
   ordering curl presents), behind one persistent connection-pooled HTTP/2
   client. Keep a curl fallback flag until the native path is proven over
   ≥1 week of production load. Gate: zero curl spawns in the job lifecycle;
   no 403/throttle over a week; assignment→acquire latency reduced and
   measured.
2. **Zero-copy log pipeline.** Today: per-line `String` allocation
   (`executor.rs` ~367 clones; `stdout.lines().map(ToOwned::to_owned)`),
   Vec regrowth, whole-job buffers. Rebuild around `bytes::Bytes` slices with
   pre-sized buffers, masked in place, one allocation per chunk not per line;
   batch the WebSocket feed by 100 ms / N lines with backpressure (unbounded
   channels out). Gate: allocation profile (dhat/heaptrack) before/after on a
   10k-line job; live feed latency unchanged or better; UI output
   byte-identical (fixture compare).
3. **Streaming artifact/log upload.** In-memory zip of artifacts
   (`protocol.rs:3080`) OOMs on big artifacts; stream zip → signed-URL PUT
   with bounded memory. Gate: 5 GB artifact uploads with <200 MB RSS.
4. **Dynamic slot autoscaling (the operator-free fleet).** Replace fixed
   `--slots N` with a scheduler that sizes the active slot pool from host
   resources (CPU/mem/load + per-job container weight) and demand (queued
   long-poll responses), within `[min,max]` bounds: scale up instantly when
   jobs queue and capacity exists, retire idle slots gracefully. One daemon
   per host remains the unit; multi-repo support via **org-level JIT
   runners** (one fleet serves java-monorepo + blockchain-nodes + fixture
   instead of 3 static partitions of 10/4/2 — eliminates "right repo, wrong
   pool" starvation). Needs the GitHub App credential from P1.7 for org
   runner admin. Gate: burst of 20 queued jobs on idle Sentry saturates the
   host then drains; single-job day never holds >min slots; per-repo
   starvation impossible by construction.
5. **Assignment→first-log latency**: keep slots' broker sessions warm
   (already long-poll), pre-pull/pre-create what is legal ahead of
   assignment (job image refreshed on schedule, network pre-allocation pool,
   sidecar image warm), measure and publish p50/p95 from acquire to first
   step line. Gate: p95 < 2 s on warm host.
6. **Node-sidecar PATH parity** (the blockchain-nodes `cargo-install` class
   of bug): the JS-action sidecar must see the same effective PATH/env a
   GitHub runner process would (including `GITHUB_PATH` accumulation from
   prior steps and `/github/home/.cargo/bin`). Fix in
   `container.rs::node_action_shell_command` + tests; this is parity, even
   though both repos now avoid the pattern via mise. Gate: fixture lane that
   replays dtolnay/rust-toolchain → baptiste0928/cargo-install passes.
7. **Velnor-lane cache locality**: rust-cache/sccache already local; add
   buildx `type=local`/registry cache substitution when running on Velnor
   (workflows keep `type=gha`; the adapter rewrites the cache target, or the
   workflow's lane matrix sets it) so Docker builds never pay GitHub cache
   API latency from Sentry. Gate: repeat docker-bake on Velnor hits local
   cache; measured.
8. **Multi-server cluster**: document + support N hosts each running the deb
   with the same org labels; GitHub's broker spreads jobs across all
   registered runners — no Velnor-side coordination needed for correctness.
   Per-host caches stay local (sccache/registry warm per host). Optional
   later: shared sccache backend (S3/redis). Gate: second host added by
   `apt install velnor-runner` + one env file; jobs visibly distribute.
9. **Network resilience under fan-out** (observed 2026-06-10: with ~10
   concurrent jobs, two `cargo metadata` runs failed — `curl failed`
   downloading a sparse-index entry and a git-dependency fetch — flaking jobs
   that pass on retry). Velnor must make jobs *more* network-robust than
   GitHub-hosted, not less: ship a host-warm shared cargo registry + git-db
   cache mounted into every job container (so `cargo metadata` rarely touches
   the network at all), set `CARGO_NET_RETRY`/`CARGO_HTTP_TIMEOUT` defaults in
   the job environment, and consider a host-local crates/git read-through
   proxy. Related: the per-IP throttling that motivated the curl transport
   workaround (P3.1) is the same class — one host, many concurrent fetchers.
   Gate: 10-way parallel cold `cargo metadata` across slots, zero flakes,
   repeated 20×.

### Phase 3b — native-adapter completeness (operator mandate, 2026-06-11)

**Velnor never executes marketplace JavaScript or remote action bundles as
the product path** — every `uses:` across the estate is implemented as a
native Rust adapter, always matching the **latest** upstream version
(`actions/runner` + each action's own source are the references;
`docs/reference/target-action-registry.md` tracks the pins). The node:20
sidecar remains only as a diagnostic fallback, never the product path.

**Delivered (0.1.7–0.1.14, proven by the agent-brown dual-lane pipeline):**
native `hadolint/hadolint-action`, `docker/setup-qemu-action`,
`sigstore/cosign-installer` (binaries pinned in the job image,
Renovate-managed); `docker/build-push-action` outputs exporter +
digest/imageid/metadata step outputs; `docker/metadata-action`
DOCKER_METADATA_OUTPUT_* env export; docker-family adapters (login,
setup-buildx, build-push, bake) now execute inside the job container so
client state (builders, logins) is shared with run steps; broker
job-output template tokens parsed (cross-job `needs.*.outputs` on Velnor);
container `/tmp` mapped for native adapters. Remaining new-surface
adapters: `actions/attest-build-provenance`,
`peter-evans/create-pull-request`, `j178/prek-action`, full-native
`renovatebot/github-action`, gitleaks scan step.

### Phase 4 — UX parity and superiority (equal-or-better, verified)

1. Re-run the API-driven lane comparison (docs/comparison.md §method) on
   live runs for **every workflow** in both target repos: per-job, per-step —
   names, numbers, conclusions, timestamps, expandability, grouping, ANSI
   color, live-streaming behavior. Build the diff as a `velnor-tools`
   subcommand so it is repeatable and CI-runnable. Gate: zero rows where
   GitHub shows information Velnor lacks.
2. Live streaming verification: while a job runs, poll both lanes' UIs —
   lines must appear in real time (WebSocket feed already keepalive-pinged);
   verify under long silent compiles and high-volume bursts. Gate: recorded
   side-by-side capture, no gaps/stalls.
3. Remaining known gaps to close from the outstanding checklist: step
   display names for `run:` steps (broker sends `__run_N`; decide on the
   YAML-match recovery or GitHub-format fallback), downloadable log archive
   (v1 store absent from V2 messages — keep `job-log.txt` artifact
   workaround, document), step numbering parity (cosmetic), adapter output
   polish (group headers, key→value lines, cache hit/miss + bytes + ms,
   sccache stats — make Velnor's speed visible in the log).
4. Name the `job-log` artifact per job (`job-log-<job-name>`): a run with 9
   Velnor jobs currently uploads 9 artifacts all named `job-log`,
   indistinguishable in the UI (observed 2026-06-10).

### Phase 5 — default-on, continuously proven

1. blockchain-nodes: verify `lane=velnor` full sweep green (post P3.6 +
   fleet), then it stays Velnor-default (already configured); java-monorepo
   already default. GitHub lane remains one dispatch away forever.
2. Scheduled `lanes=both` canary (weekly per repo): runs the same jobs on
   both lanes and publishes a timing + parity report (velnor-tools); alert on
   regression (Velnor slower than GitHub on any job class, or parity diff).
3. Capacity policy: Velnor speed advantage must hold under fan-out — the
   canary includes a full-touch run (everything rebuilds) to prove the
   dynamic fleet absorbs it faster than GitHub's hosted pool.

## 6. Sequencing summary

```
P0 (done) ──► P1 bulletproofing (1–7 parallel) ──► P2 GitHub-lane optimization
                                                    (bcn ∥ jm ∥ jackin)
P3 runner core: 1,2,3 (independent) ∥ 6,7 ──► 4 (needs P1.7 App) ──► 5 ──► 8
P4 parity verification: after P2+P3 land per repo
P5 canary + default-on: last, continuous thereafter
```

Workstream parallelism: P2 (workflow PRs) and P3 (runner Rust) can proceed
concurrently; P4 gates on both; nothing in P2–P5 may start before P1.1–P1.4
are live on Sentry (fleet reliability gates every proof run).

## 7. Open items / decisions taken

- Personal classic PAT currently restored on Sentry as the interim credential
  (it is what ran the fleet before); P1.7 replaces it with a GitHub App /
  dedicated fine-grained PAT. Rotate the interim PAT at that point.
- Who/what rewrote the three env files on Jun 9 11:49:10 is unidentified
  (single event, placeholder style not from the deb template). P1.3 makes the
  class impossible regardless of the author.
- aarch64 deb leg re-enabled in P1.6 (was temp-disabled for sentry delivery).
- Org-level vs repo-level runners: target org-level JIT (one fleet) in P3.4;
  requires org-admin-capable credential (P1.7).
