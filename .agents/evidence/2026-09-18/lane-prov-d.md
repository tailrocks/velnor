# Lane D report — OpenRouter / OpenCode Go / Grok usage

Branch `feat/multi-account-support`, shared workspace. No commit (per instructions).

## Files

- NEW `crates/jackin-usage/src/usage/openrouter.rs` (~750 lines + 9 inline tests)
- EXT `crates/jackin-usage/src/usage/opencode.rs` (tolerant parser, typed fetch errors, 5 inline tests)
- EXT `crates/jackin-usage/src/usage/grok.rs` (period rules, headers, taxonomy, precedence, 6 inline tests)
- `crates/jackin-usage/src/usage.rs`: exactly one added line — `mod openrouter;` after `mod opencode;` (line 52). No re-export touched. Other diffs in that file belong to other lanes.

All mapping targets are the existing `QuotaBucketView` / `FocusedUsageView` (+ `Money`, `StatusSlot`, `UsageSnapshotStatus`) types. No new surface, enum variant, or telemetry name.

## OpenRouter (new)

- `GET {base}/key` (base overridable via `OPENROUTER_API_URL` / `OPENROUTER_BASE_URL`): `Key Limit` meter (`used = limit - remaining`, `Spend` slot), `Spent today/week/month` money rows with no percentages (no denominator), separate `BYOK spend` row (never merged into key usage), optional `expires …` pace label, optional `reset_at` reset. Plan label from `is_free_tier` ("Free tier" / "Pay as you go").
- Null/non-positive cap → no `Key Limit` row at all (no cap, never infinite); cap without `limit_remaining` shows limit only.
- `GET {base}/credits`: typed `OpenRouterCreditsOutcome::{Available, ManagementScopeDenied, Unavailable}`; 403 classifies by status code (no string sniffing) and the snapshot keeps all `/key` rows with status `Fresh` plus a "needs a Management key" note. Balance `= total_credits - total_usage`; real `$0` renders; percent meter only when ceiling > 0.
- Catalog validation: pure `check_openrouter_model_in_catalog` (exact ID match) + `fetch_openrouter_model_check`; stale omission or fetch failure → `Unverified`, never a rejection, never an error.
- Snapshot uses `UsageSurface::Unsupported` + `provider: Some("OpenRouter")` (no OpenRouter surface exists and lane scope forbids adding one); missing key → `NeedsLogin`, `/key` 401 → `NeedsLogin`, else `Error`.

## OpenCode Go (extended)

- Tolerant window parser (same `parse_opencode_usage` signature): percent from `percent | usagePercent | usage_percent | used/limit`; reset from `resetsAt | resetAt | reset_at | resets_at | now + resetInSec*`. `1` means 1% (tested: → 99% remaining). Status stays strict (`ok` / `rate-limited`).
- Typed `OpenCodeUsageError::{Key, Entitlement, Http, Transport, Decode, Schema}`; `classify_opencode_http_error` lets the in-band `error.type` (`EntitlementError` / `AuthError`) win over status, else 401 → key, 403 → entitlement. Snapshot: key → `NeedsLogin`, entitlement → `Unsupported`, else `Error`. `fetch_opencode_usage` signature changed to the typed error (only caller is `opencode_profile_snapshot`; `load_opencode_api_key` untouched).
- Per-model pools and `zenBalanceUSD` parse to nothing: exactly 3 buckets (tested). No pace invented (monthly anchors subscription anniversary).

## Grok (extended)

- Headline: weekly percent meter only when `currentPeriod` type is not monthly; omitted `creditUsagePercent` with a valid weekly period = 0% used (100% remaining); explicitly monthly periods honestly blank without a monthly-limit fallback. Monthly fallback accepts `periodStart`/`periodEnd` aliases.
- Prepaid (`Extra usage credits`) and on-demand bounds unchanged in shape (positive-cap gate, `i64::MIN`-safe); covered by existing tests.
- `X-XAI-Token-Auth: xai-grok-cli` header added to billing + settings REST calls (contract E §1).
- Settings tier lookup centralized in `grok_tier_from_settings` with display-form priority (`subscriptionTierDisplay` / `subscription_tier_display` first).
- `GrokBillingErrorKind::{Auth, RateLimited, Timeout, Rpc, Decode, Transport}` pure classifier over this module's own error strings.
- Auth precedence: `resolve_grok_billing_auth` (`Subscription` > `EnvKeyOnly` > `None`); `grok_bearer_token` documented file-only (never env); env-key-only billing failure now reports "consumer billing needs subscription auth; XAI_API_KEY is inference-only" instead of a file/RPC error. `grok_snapshot_from_rpc_result` semantics untouched.

## Verification (honest record)

- `cargo test -p jackin-usage --lib openrouter` → 9 passed
- `cargo test -p jackin-usage --lib opencode` → 8 passed (5 new inline + 3 pre-existing incl. tests.rs contract)
- `cargo test -p jackin-usage --lib grok` → 20 passed (6 new inline + 14 pre-existing)
- `cargo test -p jackin-usage --lib` (full) → 412 passed, 0 failed
- `cargo clippy -p jackin-usage --lib --tests` → zero hits in the three lane files (crate-wide clippy still errors on other lanes' files: digit-grouping, `assert!(is_err)`, etc. — not mine, left alone)
- `rustfmt --check` clean on the three lane files (formatted only those; other files' fmt diffs belong to other lanes)
- Fixtures sanitized: no secrets/tokens in any fixture (grep verified); tests never touch network.

## Known limits / handoff notes

- `x-opencode-session` header not sent (no install-id source; inventing one would be dishonest). If the server enforces it, Go usage fails typed-`Http` — visible, not silent.
- `openrouter_snapshot` / `fetch_openrouter_*` are currently unreachable from production dispatch (no re-export added per lane constraint); the coordinator lane wires them.
- Grok `classify_grok_billing_error` is defined + tested but status mapping in `grok_snapshot_from_rpc_result` intentionally unchanged (Fresh/Error/NeedsLogin contract preserved for existing tests).
