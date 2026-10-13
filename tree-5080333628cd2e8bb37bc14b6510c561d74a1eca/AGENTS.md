<!-- context7 -->
Use Context7 MCP to fetch current documentation whenever the user asks about a library, framework, SDK, API, CLI tool, or cloud service -- even well-known ones like React, Next.js, Prisma, Express, Tailwind, Django, or Spring Boot. This includes API syntax, configuration, version migration, library-specific debugging, setup instructions, and CLI tool usage. Use even when you think you know the answer -- your training data may not reflect recent changes. Prefer this over web search for library docs.

Do not use for: refactoring, writing scripts from scratch, debugging business logic, code review, or general programming concepts.

## Steps

1. Always start with `resolve-library-id` using the library name and the user's question, unless the user provides an exact library ID in `/org/project` format
2. Pick the best match (ID format: `/org/project`) by: exact name match, description relevance, code snippet count, source reputation (High/Medium preferred), and benchmark score (higher is better). If results don't look right, try alternate names or queries (e.g., "next.js" not "nextjs", or rephrase the question). Use version-specific IDs when the user mentions a version
3. `query-docs` with the selected library ID and the user's full question (not single words)
4. Answer using the fetched docs
<!-- context7 -->

## HARD RULE: Max 2-minute wait cycles — verify progress, don't wait blindly

Never wait more than 2 minutes without checking state. The pattern:
1. Wait ≤ 60 seconds
2. Check state (is progress visible? did the step start? any error?)
3. If stuck: diagnose — don't keep waiting
4. Repeat

Signs of a stuck state to investigate immediately:
- Job pending for >2 minutes (runner not picked up)
- No renewal firing after >25s during an active step
- Step in the same state for >2 minutes

Common causes: wrong runner label, stale runner registration, network error, configuration issue.

## HARD RULE: Workflow verification sequence — always cancel old before monitoring new

When running fixture smoke tests or any GitHub Actions workflow verification:

1. **Cancel all pending/in-progress runs** from previous attempts before dispatching a new one:
   ```
   gh run list --repo ... --json databaseId,status --jq '.[] | select(.status != "completed") | .databaseId' | while read id; do
     gh run cancel "$id" --repo ...
   done
   ```
2. **Delete stale runner registrations**: runners from aborted runs stay "online" and block future runs.
3. **Dispatch the new run** only after step 1 and 2 are confirmed clean.
4. **Monitor ONLY the new run ID** — never wait on old runs blindly.

Violation: starting a new smoke test while previous Velnor lanes are still queued leads to the daemon picking up stale jobs and infinite waits.

## HARD RULE: Never modify velnor-actions-fixture to work around Velnor gaps

The `tailrocks/velnor-actions-fixture` repository exists to verify that Velnor can execute the exact GitHub Actions patterns used by ChainArgos and Jackin. It is the ground truth.

**Never remove, simplify, or work around fixture content because Velnor does not support it.**

If Velnor fails on a fixture step or pattern:
1. Identify the missing feature in Velnor.
2. Implement support for that feature in Velnor.
3. Verify the fixture passes with the Velnor fix.

The fixture defines the contract. Velnor must meet it — not the other way around.

Also applies to: `fixture_required_snippets` audits, `check-fixture-lanes` checks, and all fixture-related tooling. Strengthen coverage; never weaken it.

## HARD RULE: Never commit rendered GitHub HTML captures

Lane-comparison evidence under `.velnor-compare/` may include sanitized `.log`,
`.json`, `.md`, and archive artifacts. Never commit saved/rendered GitHub HTML
pages there: those pages embed live channel/visitor tokens in attributes.

## HARD RULE: Always use the latest protocol version and approach

When comparing to `actions/runner` or any GitHub Actions protocol: **always target the latest implementation, never legacy approaches.**

- If GitHub has V1 and V2 of a protocol, always implement V2.
- If there's a legacy API and a new Results Service path, always implement the Results Service path.
- If a crate has a new major version, always use the latest major version.
- Never copy deprecated code paths from the runner just because they existed before.

This applies to: Twirp vs REST, WebSocket vs polling, gRPC vs HTTP, results service vs distributed task timeline, V2 broker vs classic broker.

## HARD RULE: Use actions/runner as the source of truth for protocol behavior

When implementing or debugging any GitHub Actions runner protocol feature — job message parsing, broker messages, expression evaluation, credential handling, run-service, timeline, etc. — **always consult the official runner source first**:

**https://github.com/actions/runner**

Do not guess or implement blindly. Before writing new protocol code:
1. Find the equivalent logic in `actions/runner` (src/Runner.Worker, src/Runner.Sdk, etc.)
2. Understand how GitHub actually implements it.
3. Implement Velnor to match the observed behavior.

This is mandatory for: expression types (lit/expr/format), input structures, context data, credential schemes, JIT config fields, and V2 broker protocol messages.

## Rust-first scripting

Always use Rust for scripting as much as possible. Prefer Rust for verification helpers, repository automation, data parsing, audits, and repeatable maintenance tasks because Rust gives more predictable behavior and this repository already has Rust tooling pre-set up.

Rarely use Python or shell. Use them only when the task cannot reasonably be done properly in Rust, or when the existing interface is necessarily a shell entrypoint that delegates substantive work to Rust.

## HARD RULE: Keep direction docs and prompts consistent

`docs/` is the single source of truth for direction: [docs/mission.md](docs/mission.md), [docs/vision.md](docs/vision.md), [docs/roadmap.md](docs/roadmap.md) (the plan), and [docs/comparison.md](docs/comparison.md). The goal prompts in `prompts/` and the rest of the repository defer to it.

Whenever a discussion or change affects the **vision, plan, or roadmap**:

1. Update the relevant file in `docs/` first.
2. Record the direction change here in `AGENTS.md` (so the decision is captured where every agent reads it).
3. Reconcile any affected `prompts/*.md` / `prompts/*.checklist.md` and the `prompts/README.md` run sequence so they do not go stale.

Never let a prompt, README, or doc describe a direction that the current vision/plan/roadmap no longer holds. A new prompt must start from up-to-date `docs/`, not from outdated assumptions. If a prompt and `docs/` disagree, `docs/` wins — fix the prompt.

### Direction change log

- 2026-06-11: **Operating principles are law** (operator doctrine, full text
  in [docs/mission.md](docs/mission.md) §Operating principles): (1) judge
  work by correctness against the goal, never by ROI/cost/effort — "low
  value", "edge case", "not worth it" are forbidden justifications; only a
  *proven* impossibility stops the right fix, and unproven limits must be
  tested before being declared. (2) Every bug gets a root-cause analysis
  first — why did the architecture permit it, what class does it belong to —
  and the fix removes the enabling structure, not the symptom; symptom
  patches require naming the deferred root cause. Every slot death / lost
  job / fleet degradation is a bug in this sense. Applied same day:
  upgrade-restart job kills → graceful SIGTERM drain + TimeoutStopSec
  (master-plan incident #10); empty-shared-store dangling shims → image
  seeding (incident #11).

- 2026-06-11: **Truthful step env + host-persistent stores**
  ([docs/perf-instant-cache-plan-2026-06-11.md](docs/perf-instant-cache-plan-2026-06-11.md)):
  run steps and adapters see HOME=/github/home (the bind-mounted job home) —
  never a fake HOME=/root — so `~` caches, docker login state and GITHUB_ENV
  overrides behave exactly like GitHub-hosted. The old prelude redirected
  cargo downloads into the unmounted container /root, making the
  cargo-registry cache unsaveable forever (root cause of "still downloading
  and compiling on a warm fleet"). Velnor-lane caching is mount-based, not
  tarball-based: host-persistent daemon-shared stores for the cargo
  registry/git (`_velnor_cargo`), mise tool installs (`_velnor_mise`),
  sccache (existing), and opt-in per-job-class CARGO_TARGET_DIR buckets
  (`_velnor_targets`, VELNOR_CARGO_TARGET_PERSIST). The actions/cache
  adapter treats those paths as always-warm no-ops, docker bake/build-push
  adapters drop type=gha cache options (persistent local builder covers
  them), and absolute container paths with no host mapping resolve to None
  instead of leaking the daemon host's filesystem.

- 2026-06-22: **Persistent target buckets stay in their trust lane**
  ([docs/runner-usage.md](docs/runner-usage.md)): opt-in
  `VELNOR_CARGO_TARGET_PERSIST` now stores Rust targets under
  `_velnor_targets/<trust-scope>/<repo>/<workflow>/<job-bucket>`.
  `VELNOR_TRUST_SCOPE` defaults to `trusted`; set it explicitly per daemon/pool
  (for example `public-forks`) before enabling persistent targets outside a
  trusted lane. Keep one daemon per trusted target scope; trust scope,
  repository, workflow, and job class are all part of the warm cache boundary.

- 2026-06-22: **Warm-runner jobs get daemon resource caps**
  ([docs/runner-usage.md](docs/runner-usage.md)): package daemons default job
  containers to `VELNOR_JOB_CPUS=4` and `VELNOR_JOB_MEMORY=12g`. The runner
  appends those Docker limits after workflow `container.options`, so operator
  policy wins for shared warm hosts. Tune or blank either env var per trusted
  daemon scope.

- 2026-06-23: **Trust scopes are runtime-enforced**
  ([docs/runner-usage.md](docs/runner-usage.md)): `trusted` daemons keep full
  capability, including the shared host Docker socket. Any other
  `VELNOR_TRUST_SCOPE` rejects jobs when GitHub sends user/repository secrets
  (`secrets.*`) and omits the host Docker socket from job/action containers.
  Operators still run separate labels/groups per trust lane; the runner now
  enforces the boundary so a public-fork pool cannot accidentally inherit
  trusted warm-runner capabilities.

- 2026-06-11: **Log format contract is law**
  ([docs/log-format-contract.md](docs/log-format-contract.md)): live
  WebSocket feed lines are RAW (the UI adds its own timestamp column);
  uploaded log-blob lines carry the .NET 7-digit timestamp prefix (the UI
  strips it into the toggle). This has been broken repeatedly during
  refactors (doubled timestamps live, or timestamps leaking into content).
  Any change near log emission must keep the guard tests green, update the
  contract doc in the same commit, and be verified visually against a
  GitHub-hosted job both live and completed.

- 2026-06-11: **Stability-first directive** (operator, master-plan §3b +
  P1.9): incident #9 ("zombie fleet" — broker polls kept returning empty
  success while GitHub's runner registry had the runners offline/missing;
  jobs queued forever; doctor alerted but nothing healed) proved stability
  was NOT achieved; stability work outranks new performance features until
  the P1.9 gates pass. Root causes fixed in 0.1.15: empty-body non-2xx
  broker responses (expired token 401, curl status 0) were classified as
  "no message"; idle slots never refreshed OAuth tokens; nothing reconciled
  local slot state against GitHub's registry. Standing rules: the daemon
  trusts BOTH health signals (broker poll + runner registry) and self-heals
  on divergence; every incident class must be diagnosable from on-disk
  forensic logs alone (per-slot `logs/{broker,registry,lifecycle}.log` +
  `daemon.log`, identity-prefixed, statuses + control messages recorded) —
  if logs can't explain an incident, the logging gets fixed too; `tracing`
  is the standard performance lens (JSON span timings in `logs/trace.jsonl`,
  `tracing-opentelemetry` OTLP export behind the `otel` feature +
  `VELNOR_OTLP_ENDPOINT`) — new hot paths get spans, performance claims cite
  span data; benchmark campaigns verify fleet health (doctor green) before
  timing anything.

- 2026-06-11: Docs refreshed to current truth: mission/vision rewritten
  (drop-in goal achieved, mandates referenced as law), docs/README index
  updated, runner-usage gains the apt/systemd production section,
  debian-apt-repo marked implemented, comparison status updated (live
  streaming verified, per-step numbering fixed), roadmap reframed as the
  runner-internal implementation reference (master-plan is the plan),
  prompts/README sequence marked complete.

- 2026-06-11: **Native-adapter completeness mandate** (operator): Velnor
  must not execute JavaScript/remote action bundles as the product path —
  every `uses:` in the estate gets a native Rust adapter pinned to the
  latest upstream behavior (master-plan P3b); node sidecar = diagnostic
  fallback only.
- 2026-06-11: **Universal caching + performance mandate** (operator hard
  rule, master-plan §3a): everything cacheable must be cached in every
  pipeline of the nine-repo estate; rerun-idempotency is the acceptance test
  (rerunning a just-finished pipeline must re-download/recompile nothing);
  anything parallelizable runs in parallel; pipelines are iteratively tuned
  until the maximum the tooling allows. Estate: (jackin, jackin-the-architect, jackin-agent-smith,
  jackin-sentinel, java-monorepo, blockchain-nodes, holla, velnor,
  velnor-actions-fixture) — sccache on every compiling job, cargo
  registry/target caches, docker layer cache-from/to on every build, apt
  cache mounts inside Dockerfiles, pinned+cached tool installs. Visible
  dependency compilation in CI is a defect. Estate addition + unification:
  ChainArgos/jackin-agent-brown joins (dual-lane: GitHub + Velnor); all four
  agent-role repos (the-architect, agent-smith, sentinel, agent-brown) use
  the shared jackin-role-action identically; jackin is the pipeline source
  of truth and improvements propagate to every applicable repo.
- 2026-06-10: Added `docs/master-plan.md` as the top-level goal + execution
  sequence (operator mandate): Velnor default on both ChainArgos repos with the
  GitHub lane kept forever; full parity then superiority vs GitHub-hosted;
  Phase 1 operational bulletproofing (fail-fast credential validation,
  never-exit resilience, secrets that survive tooling, watchdog/alerting,
  actionlint, automated tag→deb→apt delivery, GitHub App credential) comes
  before all other work — driven by the 2026-06-10 fleet outage (env-file
  token/label wipe → JIT 401s → 0 runners; invalid `release-deb.yml` YAML →
  apt repo stuck at rc12). Work is judged by correctness against the goal,
  never by effort/ROI; root-cause fixes over symptom patches; full refactors
  in scope. Velnor may be expensive; it may not be slow, lossy, or fragile.
  Dynamic slot autoscaling + org-level JIT (one fleet, multi-repo) and
  multi-host scale-out are committed direction (master-plan P3).

- 2026-06-03: Established `docs/` as the source of truth; added `docs/mission.md` (fastest, Rust-first, nicer output, cheaper) and `docs/comparison.md` (GitHub-vs-Velnor UI output). Added the `prompts/` goal-prompt system with a fixed run sequence (1: target workflow coverage, 2: GitHub UI parity, 3: fixture proof). Simplified root `README.md`; moved operator commands to `docs/runner-usage.md`.
- 2026-06-04: Operator authorized direct work on the real target repo `ChainArgos/java-monorepo`. The "Agents stop at the public fixture — never run the real ChainArgos / Jackin repos" rule is now scoped to **unattended Velnor execution / target validation** (still: do not set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true` or run the Velnor lane on a real repo without operator-provided host access). It does **not** forbid GitHub-hosted CI hygiene on ChainArgos. New prompt `prompts/chainargos-runner-parity.md` (+ checklist) added: **Phase A** repoints ChainArgos workflows from self-hosted `hetzner-sentry-ci` to GitHub-hosted runners and makes every pipeline green (fixing the project itself where self-hosted infra masked real breakage), via autonomous PR + merge; **Phase B** runs the *same* jobs on GitHub-hosted and Velnor lanes in parallel (fixture `compat.yml` pattern — parameterize only `runs-on`), where any Velnor-lane divergence is fixed in Velnor, never in the job (fixture-is-contract applied to a real repo).
