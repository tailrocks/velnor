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

Fixes shipped today: token + labels restored and all three daemons
re-registered (10+4+2 runners); PR #1380 merged green; release pipeline fixed
end-to-end (YAML, lock, imports, vendored OpenSSL for zigbuild, deb binary +
size guard) — v0.1.3 released with holla-style tar.gz (mac arm64/x86_64 +
linux arm64/x86_64) + a real deb; v0.1.4 adds the eager-checkout fix; Sentry
runs rc25 (first live-streaming build) pending the 0.1.4 apt upgrade.

**"Never again" requirements (Phase 1) follow directly from this table.**

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
4. **[PARTIAL 0.1.5] Fleet watchdog**: `Type=notify` + `WatchdogSec=180` +
   sd_notify READY/STATUS/WATCHDOG live on Sentry. Remaining: a
   `velnor-runner doctor` probe + timer comparing registered-runner count to
   expected slots, and/or a GitHub-side scheduled canary that fails loudly
   when the fleet is gone.
5. **[DONE] Lint + compile gate**: `ci.yml` (fmt, clippy -D warnings, full
   test suite, actionlint+shellcheck, ci-required aggregate) with the jackin
   cache stack, on GitHub-hosted runners.
6. **[DONE for amd64; arm64 re-enabled pending next tag] Release pipeline
   end-to-end automation**: proven on v0.1.4/v0.1.5 — tag → CI → deb (binary
   + size guard) → GitHub release (tar.gz ×4 targets) → velnor-apt upload →
   signed apt publish → Pages, zero manual steps. aarch64 deb leg re-enabled
   (vendored OpenSSL removed the cross-apt dependency); proof on the next
   tag. Remaining: alerting on release-pipeline failure.
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
1. jackin: un-serialize `test` from `check-all-features` (pure critical-path
   latency, up to ~2.5 min); gate `audit`/spell-checks on relevant paths;
   merge the bench matrix; share clippy/check target caches via restore-keys.
2. jackin-the-architect: unify PR (gha) and publish (registry) buildx caches
   — post-merge currently rebuilds ~15 min of identical layers; add
   paths-filter + concurrency; bump `peter-evans/create-pull-request` off
   node20 (forced node24 on 2026-06-16 — six days); fix the red
   `jackin-toolchain.yml` (PR-creation permission).
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
