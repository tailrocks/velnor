# Branch-merge sweep + Sentry deployment verification (2026-08-24 ~19:00Z)

## Directive

Merge every branch to main resolving conflicts per modern Velnor direction;
verify every delivered branch landed via PR; delete merged branches locally
and remotely; after merges verify Sentry deployment and fleet CI health.

## Branch disposition (all delivered via PRs)

| Branch | PR | Outcome |
|---|---|---|
| velnor-estate-standard | #286, #294, #297 | MERGED; branch deleted by operator after final sync |
| velnorctl-clap-migration | #291 (+ #293 closed superseded by operator) | MERGED; deleted |
| chore/prepare-0.1.179/.180 | #288, #289 | MERGED |
| fix/admit-paths-filter-v4.0.3 | #292 | MERGED; deleted |
| fix/jackin-fleet-stale-runner-queue | #290 | MERGED |
| docs/consolidate-main-flow | #296 | MERGED; deleted |
| fix/velnorctl-typed-globals | #298 closed, re-opened by operator as #299, closed; content landed via estate in #297 | content on main |

Conflict resolutions during sweep: plans/TASKS.md progress counter (kept
newer 4/94), COORDINATION.md clap claim rows (kept YIELDED superset),
crates/velnorctl lib/tests (kept typed-fix side over pre-#291 main).

## Final topology

Remote heads: `main` only. Open PRs: 0. main @ feeaebe includes the native
clap CLI (#291) AND typed globals (`--since` -> `velnor_model::Since`,
`--timeout` -> `std::time::Duration`, saturating `Verbosity`; verified via
`git grep 'Option<Since>' origin/main`) plus campaign plan state.

## Sentry deployment verification

- Installed `velnor-runner 0.1.181` == apt candidate, published through
  signed `https://velnor-apt.tailrocks.com stable/main` (no dpkg -i paths).
- 9/9 `velnor-daemon@*` units active running; zero daemon failures.
- Doctor probes: transient FLEET-DOWN alarms on five idle pools
  (dogfood/fixture/blockchain-nodes/jackin-agent-brown/termrock) during the
  post-burst ephemeral-runner recycle window; next 10-minute cycle
  self-healed — slots re-registered (`velnor-dogfood-slot-{3,4,5} [online]`)
  and all doctors pass. Timing SLO WARNs recorded (pickup-to-first-step,
  finalize, teardown p95) — performance observations, not outages.
- Recent tailrocks CI runs on main: success.

## Gates run during sweep

- `rtk mise run check` exit 0 at: merge commit 8563d0d (estate), e5e3b24 +
  1fb72ab (clap), aed964e (estate typed-globals; 1036/1036 nextest),
  docs-consolidation tip.
- Fleet-side: velnor lane build/test gates success for #293-run
  32756241688, #298-run 32758459691, #297-run 32761899297.
