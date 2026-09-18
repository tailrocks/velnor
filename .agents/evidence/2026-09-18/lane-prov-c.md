# Lane C report — Antigravity / Gemini / Cursor usage collectors

Date (UTC): 2026-09-17. Branch: `feat/multi-account-support`. No commit (per instructions).

## Files

NEW (only these + `usage.rs` hunks below):
- `crates/jackin-usage/src/usage/antigravity.rs` (895 lines, 9 tests)
- `crates/jackin-usage/src/usage/gemini.rs` (436 lines, 6 tests)
- `crates/jackin-usage/src/usage/cursor.rs` (1433 lines, 11 tests)
- `crates/jackin-usage/src/usage.rs`: 3 `mod` lines + 3 re-export blocks only (follows the
  existing `claude.rs` re-export pattern with the documented `expect(unused_imports)`).

All three map into the EXISTING `QuotaBucketView` / `FocusedUsageView` / `Money` /
`StatusSlot` types only. No parallel canonical types. Fixtures are sanitized (example.com
identities, synthetic JWT, no secrets).

## Antigravity (`antigravity.rs`)

- Official `agy -p /usage|/credits --output-format json`, version-gated `>= 1.1.11`
  (`ANTIGRAVITY_MIN_JSON_VERSION`, `parse_agy_version`, `antigravity_cli_version`); too-old
  binaries yield `Unsupported` (never run as prompt). Official commands only (no loopback LS
  API — the `>= 1.2.2` tokenless break makes it the fallback, documented in module docs).
- `groups[].buckets[]` (bare or `{response:{groups}}`-wrapped; `pools[]`/`buckets[]` aliases);
  exact `bucketId` match `gemini-5h|gemini-weekly|3p-5h|3p-weekly`. Parsed summary (even
  empty) wins over legacy. Summary entries with absent `remainingFraction` = depleted (0).
- Legacy `models{}` map: drops `isInternal`/empty-label, collapses worst fraction per
  family, 5h-only + weekly "No data" buckets. Availability-only rows are dropped — an
  all-available response yields zero pools (tested: no quota invention).
- Identity-may-omit: `account_label` empty + origin `CLI · agy (identity unverified)` so the
  broker binds to runtime. Tier prefers `userTier/currentTier/paidTier.name` over
  `planInfo.planName`. `/credits` attaches `Money` only with explicit minor+exponent.

## Gemini (`gemini.rs`)

- `GEMINI_CONSUMER_OAUTH_END = 1781740800` (2026-06-18T00:00Z, cross-checked against
  `parse_iso_epoch` in tests). `gemini_migration_action` gives old consumer logins a concrete
  Standard/Enterprise reconnect action; `gemini_error_needs_migration` is true ONLY for a
  403 on a consumer route past retirement — never blind-mapped.
- Entitlement parse prefers tier object over plan name; explicit unsupported/deprecated/
  retired flags (or `individual`-family tier id) set `consumer_unsupported`. Missing tier =
  unknown, never consumer.
- Project quotas (`quotas[]`/`limits[]`) → count buckets with `remaining` only when a
  denominator exists; no slots (rate limits, not allowance). `GEMINI_CLI_HOME` = parent +
  `.gemini/oauth_creds.json`; API-key/Vertex route reports a typed reporting-scope gap.

## Cursor (`cursor.rs`)

- Personal: `DashboardService` Connect POST (`GetCurrentPeriodUsage|GetPlanInfo|
  `GetCreditGrantsBalance|GetSandUsageStatus`) + `cursor.com` session REST (`/api/usage?user=`,
  `/api/usage-summary`, `/api/auth/stripe`). Billing-cycle meter on the Weekly slot (Grok
  precedent); limit-less `planUsage` → `cursor_needs_request_fallback` → REST request counts.
  Team inferred from `limitType=team || pooledLimit>0`. User id from JWT `sub` after `|`;
  REST enrichment only for OAuth-file auth + default base. Grok Bot: pooled/zero-allowance →
  no meter. Credits = grant cents + Stripe cents, one row, structured `Money` (explicit
  minor units).
- Enterprise: separate `CursorEnterpriseScope` (explicit admin token); `cursor_snapshot`
  never touches `api.cursor.com` (scope-URL test). Team spend (actual, Spend slot, labels)
  vs estimated model cost (detail-only, "estimate · not billed") vs per-member rows.
- Usage events: parser + buckets exist for drill-down, but snapshots never poll the hourly
  aggregated endpoint. Unverified-scale dollars stay labels, never `Money`.

## Verification

- `cargo test -p jackin-usage --lib -- usage::antigravity usage::gemini usage::cursor`:
  **26 passed, 0 failed** (final run; also green pre/post `rustfmt`).
- `rustfmt --check` clean on all three new files. Clippy: zero diagnostics reference the
  new files or `jackin-usage`.
- Full `--lib` suite: 3 failures, all in `host::tests` (provider-order/surface-inventory),
  owned by the concurrently editing host-discovery lane; additive re-exports cannot cause
  them. Clippy deny-level findings likewise all in `jackin-config`/`jackin-core` (other lanes).

## Deferred / follow-ups (not in lane scope)

1. `UsageSurface` has no Antigravity/Gemini/Cursor variants (lane constrained to mod lines +
   re-exports in `usage.rs`), so snapshots build via `UsageSurface::Unsupported` and patch
   `account.provider_label` — marked in code; surface wiring + tab strip is a follow-up.
   NOTE: other lanes ARE editing `usage.rs` concurrently (amp/claude/codex re-export hunks)
   despite the "untouched" premise — merged cleanly so far, but the premise is false.
2. Cursor `totalSpend`/summary dollar scale is unverified → labels only; Antigravity
   `/usage|/credits` exact live schema still wants one authenticated capture (audit §2.4).
3. Gemini has no pinned live reporting endpoint → honest `Unsupported`, no fetch.
