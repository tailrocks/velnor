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
