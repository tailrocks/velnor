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

The `donbeave/velnor-actions-fixture` repository exists to verify that Velnor can execute the exact GitHub Actions patterns used by ChainArgos and Jackin. It is the ground truth.

**Never remove, simplify, or work around fixture content because Velnor does not support it.**

If Velnor fails on a fixture step or pattern:
1. Identify the missing feature in Velnor.
2. Implement support for that feature in Velnor.
3. Verify the fixture passes with the Velnor fix.

The fixture defines the contract. Velnor must meet it — not the other way around.

Also applies to: `fixture_required_snippets` audits, `check-fixture-lanes` checks, and all fixture-related tooling. Strengthen coverage; never weaken it.

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
