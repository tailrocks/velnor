# Lane A report — provider parsers: Claude / Codex / Amp

Branch: `feat/multi-account-support`, shared workspace. No commit (per brief).
Files owned and changed (nothing else touched):
- `crates/jackin-usage/src/usage/claude.rs` (+42/-~6)
- `crates/jackin-usage/src/usage/codex.rs` (+65/-~15)
- `crates/jackin-usage/src/usage/amp.rs` (+390/-~30)
- `crates/jackin-usage/src/usage/tests.rs` (+322/-7: 5 Amp `buckets(now)`
  call-site updates, 2 `codex::` qualification removals, 10 new tests)
- `crates/jackin-usage/src/usage.rs` (coordinator re-exports for the new
  lane-A items only — 3 hunks, no other lines)

Read first, as briefed: `/tmp/ref-contracts-A.md`, `/tmp/audit-t01-providers.md`,
`jackin-provider-research.md` §6–8, `plans/multi-account/001-t02-domain-contracts.md`.
Additionally pulled evidentiary line shapes from CodexBar `AmpUsageParser.swift`
at the pinned SHA (`b6e65a83…`) and `amp usage --help` locally (no authenticated
reads executed). No new fixture files: the suite's existing inline-const style
covers the new cases.

All mapping stays inside the existing `QuotaBucketView` / `FocusedUsageView`
types. No new canonical types, no Agent-enum or cross-file edits.

## What was already contract-complete (verified, untouched)

- Claude: named windows (`five_hour`/`seven_day*`) + `limits[]` array with
  `kind`/`percent`/`severity`/`scope.model.display_name`, legacy backfill,
  `extra_usage` + `spend{}` money bucket, rotating-codename dollar windows.
- Codex: app-server `rateLimitsByLimitId` (Spark etc.), variable
  `windowDurationMins`, credits, reset-credit inventory with expiries,
  individual/spend-control limits.
- Amp: `Amp Free` daily %, individual credits, per-workspace balances.

## Claude changes (`usage/claude.rs`)

1. **Inference-only-token state.** New pure `claude_error_is_scope_restriction`
   (403/forbidden/scope/permission, never 401/transport/decode) and
   `claude_provider_error_label` (OAuth-first, verbatim passthrough otherwise).
   `claude_resolved_view` now surfaces
   `Claude token lacks usage scope (inference-only); quota unavailable`
   instead of a bare HTTP 403. Status stays `Stale` (quota unknown; last-good
   cache still applies) — `NeedsLogin` would be a lie, the login works.
2. **Scope-restriction finding: `is_active` is NOT a render gate.** I first
   implemented skip-when-`is_active:false`, but the repo's own fixtures
   (`tests.rs:237,1038,1041`) send `false` on headline limits that must
   render, and an existing test failed. Reverted; `as_quota` documents that
   live responses send `false` on quota-carrying limits, and a new test locks
   the true contract. Per-model scoping (`weekly_scoped` + display name)
   remains the operative scope restriction and was already complete.

## Codex changes (`usage/codex.rs`)

1. **Wham relative resets.** `CodexWindowSnapshot` gains
   `reset_after_seconds`; new `resets_at(now)` resolves absolute-first,
   relative-offset second (negatives ignored, saturating). `push_codex_window`
   pace + bucket use the effective reset.
2. **Tolerant `used_percent`.** Now `Option<Value>` decoded via `json_number`
   (int/float/numeric-string, round + clamp 0–100); garbage → used-less
   window instead of a failed whole-response decode. RPC `from_rpc` maps
   through the same type.
3. **RPC missing/extra-field tolerance.** `rateLimits` object defaulted
   (absent → empty snapshot, still `Ok`); `usedPercent` optional (window
   keeps reset/duration, bucket carries no used/remaining); `availableCount`
   defaulted to 0 (no bucket); credit flags defaulted to false (no bucket).
   Unknown fields were and are ignored by serde; account-tag drift already
   degraded to no-label. No `#[deny_unknown]` anywhere on these shapes.

## Amp changes (`usage/amp.rs`)

Parser extended to the full CodexBar-evidenced `displayText` contract:

1. **Tier line** — `Amp <plan> Tier: agent usage $<rem> of $<limit>
   remaining [, orb usage <R>h of <L>h a1.small orb hours remaining]
   [, period YYYY-MM-DD to YYYY-MM-DD] [, resets upon renewal in N days|months]`.
   New `AmpSubscription`/`AmpSubscriptionKind::Tier`/`AmpRenewal` types (all
   lane-local, mapped into buckets, not canonical). Buckets: `Agent usage`
   (structured `Money`, `Spend` slot, full-precision remaining %,
   renewal-anchored `resets_at`) and `Orb usage` (whole hours floored,
   `< 1h` for positive sub-hour, full-precision %).
2. **Legacy Subscription lines** — both `Subscription <plan>:` and
   `Amp <plan> Subscription:` with `<N>% other usage and <M>% orb usage`
   → percent `Agent usage`/`Orb usage` buckets with renewal resets.
3. **Renewal + period.** `AmpRenewal` (days exact, months ≈ 30d, saturating;
   pace label always shows the raw countdown so the approximation is
   visible); strict `period` date validation (shape + end > start).
4. **Robustness.** `**` Markdown-bold stripped (API `displayText` may carry
   it; no-op for CLI); Orb segment optional and independently validated —
   unrecognized Orb data never hides the Agent pool; Tier wins over legacy;
   renewal/period optional (dollars kept without reset rather than dropped).
5. **Funding route / linked subscriptions.** The plan name is surfaced as
   `plan_label` (`Amp <plan>`, wins over `Amp Free`) so the paying
   subscription is recorded on the view. Note: the evidenced `displayText`
   contract (CodexBar parser at pinned SHA + local `amp usage --help`) has
   NO separate linked-subscription/billed-via line, so no such line parser
   was invented; if a linked route appears in live output it arrives as part
   of the plan/pool text and is preserved verbatim in labels. `amp usage
   --details/--start/--end` (T01-verified flags) not wired — the broker owns
   fetch-shape decisions, this lane owns the parser.
6. `buckets()` now takes `now: i64` for renewal anchoring; Daily headline
   untouched (subscription pools never leak into the Amp status bar).

## Tests

10 new tests in `usage/tests.rs` (all sanitized, `user@example.com` only):
`claude_limits_inactive_flag_does_not_gate_rendering`,
`claude_scope_restriction_error_is_explicit`,
`codex_wham_relative_reset_resolves_against_now`,
`codex_used_percent_tolerates_float_string_and_missing`,
`codex_rpc_tolerates_missing_windows_credits_and_counts`,
`amp_tier_line_maps_agent_dollars_orb_hours_and_renewal`,
`amp_tier_without_orb_keeps_agent_and_skips_orb`,
`amp_unrecognized_orb_data_does_not_hide_agent`,
`amp_legacy_subscription_line_maps_percent_pools`,
`amp_tier_wins_over_legacy_and_bold_markers_strip`.

## Verification (read carefully — shared-workspace caveat)

The shared working tree currently does NOT compile: other lanes' in-progress
edits break the build (`jackin-config` stores: 69 `unreachable_pub` denies;
`jackin-usage` lib: `kimi.rs:262` `Copy`-on-`String` enum,
`minimax.rs:559` missing `Clone`, `zai.rs:302` `f64::from(i64)`). Those files
are outside this lane; not touched, not fixed.

Verification therefore ran in a disposable worktree at `HEAD` (`abc2ca20`)
with ONLY the 5 lane-A files overlaid (then removed; `git worktree list` is
clean of it). Results there, full workspace lints, no caps:

- `cargo test -p jackin-usage --lib`: **314 passed, 0 failed**
- filters: `claude` 36/36, `codex` 21/21, `amp` 25/25
- `cargo clippy -p jackin-usage --all-targets`: **0 errors, 0 warnings**
- `cargo fmt -p jackin-usage -- --check`: **clean**

Reproduce: `git worktree add /tmp/x HEAD`, copy the 5 files, run the above.

## Risk / follow-ups for the orchestrator

- The `is_active:false`-renders finding contradicts the naive reading of the
  brief's "scope restrictions" clause; if a T10 typed-group design wants to
  gate on `is_active`, it needs live-response evidence first.
- Amp month-renewal ≈ 30 days is an approximation (CodexBar uses calendar
  months); exactness needs the T10 broker with calendar math.
- `Agent usage` Tier bucket takes the `Spend` slot: correct per its docs
  (Claude/Codex money precedent) and headline-safe for the Amp surface, but
  T10 should confirm slot semantics when typed groups land.
