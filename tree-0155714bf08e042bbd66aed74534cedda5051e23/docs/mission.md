# Velnor Mission

Every prompt in this folder serves this mission. Read it first; keep it in mind
for every decision and trade-off.

## What Velnor is

Velnor is a **GitHub Actions-compatible, self-hosted runner built specifically
for Rust projects** — not a generic CI runner. It appears to GitHub as a
self-hosted runner (V2 JIT / broker / run-service), runs assigned Linux jobs in
Docker, and executes the known action surface through **native Rust adapters**
instead of marketplace JavaScript.

Because it is self-hosted on a beefy machine and Velnor controls the execution
and the output, it can be **faster, cheaper, nicer, and more informative** than
GitHub-hosted runners — and it is tuned end-to-end for Rust.

## The four pillars

1. **Fastest possible.** Beat GitHub-hosted runners on wall-clock.
   - Exploit the self-hosted beefy host: run work in parallel wherever the job
     graph allows; keep daemon slots warm; minimize assignment→first-log latency.
   - Avoid marketplace JS/TS startup; native Rust adapters are cheaper to launch.
   - Aggressive, Rust-aware cache tactics (cargo registry/git, target dir,
     sccache, rust-cache) — more aggressive than GitHub's defaults because we own
     the host and the storage.

2. **Rust-first, not generic.** Everything is prioritized around Rust.
   - Provide the meaningful tools and solutions a Rust project actually needs.
   - **Pre-cached and pre-optimized for Rust**, especially the **latest and
     greatest** toolchain/tool versions — we mostly build on the latest with AI
     agents. This is a hard rule.
   - The job image, adapters, and caches assume modern Rust CI (fmt, clippy,
     check, test, nextest, MSRV, mise, mold, sccache, rust-cache, Docker/Buildx).

3. **Nicer, more informative output.** Velnor authors its own log stream, so the
   UI experience should be **equal-or-better** than GitHub — grouped, colored,
   padded, timestamped, easy to debug — never less informative. (User-command
   output passes through verbatim; Velnor frames it but does not rewrite it.)

4. **Cheaper.** Self-hosting on owned hardware avoids paying per-minute for the
   same (or better) capability. Faster + warm caches + parallelism compound the
   savings.

## Hard rules (inherited by every prompt)

- **Rust-first mission above is non-negotiable.** Optimize for the latest Rust.
- **Fixture is the contract** — fix Velnor, never weaken
  `tailrocks/velnor-actions-fixture`.
- **`actions/runner` is the protocol source of truth** —
  <https://github.com/actions/runner>. Don't guess.
- **Latest protocol path only** (Results Service over timeline, V2 over classic,
  JIT over registration tokens).
- **Rust-first automation** — prefer `velnor-tools` subcommands over new
  shell/Python.
- **Agents stop at the public fixture** — never run the real ChainArgos / Jackin
  repos or set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.

## How to use this in a prompt

When working any goal, continuously ask:

- Is this **faster** than the GitHub-hosted equivalent? If not, why, and can we
  close the gap with parallelism or warmer caches?
- Is this **Rust-optimal** — latest toolchain, best cache strategy, right tools?
- Is the **output** at least as informative and nicer than GitHub's?
- Does this keep Velnor **cheaper** to operate than paying for hosted minutes?

Capture timing and cache evidence so improvements are measurable.
