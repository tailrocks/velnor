# Lane review-fix report — provider usage lanes (fixes 1–13 + openrouter/grok/opencode sweep)

Branch `feat/multi-account-support`. Owned files only:
`crates/jackin-usage/src/usage/{cursor,antigravity,gemini,kimi,hermes,codex,claude,zai,minimax,openrouter,opencode,grok}.rs`
(+ inline tests). `usage/tests.rs` untouched; all new tests inline per-file. No commit.

## Fix list (review §Fix list order)

1. **cursor double-count** (`cursor.rs`): `parse_cursor_credit_grants` now returns the
   itemized `grants[]` sum when non-empty, else the top-level total — never both.
   Test: total+breakdown → 150 (was 300); empty `grants[]` → top-level.
2. **cursor `Debug` on secrets** (`cursor.rs`): dropped `Debug` from `CursorAuth`
   (`access_token`) and `CursorEnterpriseScope` (`admin_token`); redaction comments added.
3. **antigravity absent fraction** (`antigravity.rs`): summary entry without quota signal
   → `remaining_percent: None` + "No data" detail row (legacy-path convention).
   Locking test renamed/updated (`…_is_unknown_not_depleted`); explicit `0.0` still → 0.
4. **gemini migration gate** (`gemini.rs`): `gemini_migration_action` now takes only the
   entitlement and fires solely on `consumer_unsupported`; the bare-OAuth+clock branch
   is gone (managed Standard/Enterprise logins no longer misclassified). Snapshot passes
   `None` (no entitlement endpoint wired) → generic reporting-gap message. Test updated.
   `GOOGLE_API_KEY` alias confirmed S1-recorded (`accounts/discovery.rs:41-44`) — cited.
5. **kimi unitless-cent aliases** (`kimi.rs`): `KimiExtraUsage` keeps only evidenced keys
   (`*_cents`, bare `balance`/`total`); unitless hits (`remaining`, `used`, `cap`,
   `limit`, `monthly_cap`, `monthly_used`) land in flattened `other` and render as a
   label-only `Extra usage` row (`{key} · unknown scale`) — no `Money`, no slot, no bar.
   Note: bare `monthly_used`/`monthly_cap` needed serde `rename` (field names bind
   otherwise); caught by test, fixed.
6. **over-100% raw** (codex+kimi): `CodexWindowSnapshot::used_percent_raw` (rounded,
   unclamped) feeds `used_label` (`142% used`); `used_percent_clamped` untouched (shared
   `tests.rs` lock `140→100` stays green). `KimiPool`/`KimiUsageDetail` refactored around
   `used_percent_raw`; over-cap prefixes the pace line (`140% used · …`); bars clamp to 0.
7. **hermes renewal** (`hermes.rs`): `cycle_ends_at` no longer feeds `reset_at`; renders
   `renews 2026-10-17` pace note (UTC-explicit, chrono; `local_timestamp_label` is
   unreachable from this module — `mod format` is private and it isn't re-exported).
8. **antigravity missing binary** (`antigravity.rs`): new pure `antigravity_version_error_status`
   — predates-JSON → `Unsupported`, missing/unparseable → `NeedsSecret`. Tested.
9. **antigravity zero-pool** (`antigravity.rs`): new pure `antigravity_snapshot_status` —
   `Fresh` only with pools or a credits row; parsed-but-empty → `Stale`. Tested.
10. **claude CLI-fallback error** (`claude.rs`): fallback `last_error` now uses normalized
    `provider_error` (scope text surfaces); extracted pure `claude_resolved_last_error`,
    first inline test module in file. Shared `claude_provider_error_label` tests untouched.
11. **cursor notes/hermetic/pick** (`cursor.rs`): USD/exponent-2 assumption recorded on
    `cursor_credits_bucket`; `cursor_dashboard_url_with_base` pure seam + hermetic test
    (no live `CURSOR_API_ENDPOINT` read); multi-model request pick pinned to
    alphabetically-first counter key + documented + order-independent test.
12. **zai docs + minimax citation**: `time_label` 28d heuristic residual risk documented;
    `ZAI_PEAK_START/END_HOUR_UTC` consts + `ref-contracts-B §2` pointer; minimax `sk-api-*`
    routing citation VERIFIED (`/tmp/ref-contracts-B.md` §3 ll.122-126, `selectUsageEndpoint`,
    minimax-cli `endpoints.ts:50-84`) and cited on the enum.
13. **team casing**: zai `{plan} · team` → `{plan} · Team` (cursor's 4 usages already `Team`;
    no test locked the lowercase form).

## OpenRouter review verdict (740-line file, previously unreviewed): FIX-APPLIED, now ACCEPT

Sweep findings, all fixed inline: (a) BYOK fallback summed ANY `byok*` numeric — a
`byok_limit` cap would fabricate spend; now only usage/spend/cost-named keys count.
(b) Omitted `is_free_tier` asserted "Pay as you go"; now `None` (unknown), label only on
explicit true/false. (c) `openrouter_base_url_prefers_api_url_override` was vacuous under
live env; pure `openrouter_base_url_from` + hermetic test. Clean on re-check: null cap →
no row, no percent without denominator, /credits 403 typed `ManagementScopeDenied` never
suppressing /key rows, money labels raw, no secret-holding structs, no tautologies left.
Notes (no change): missing-key/401 → `NeedsLogin` (kimi/minimax use `NeedsSecret`; left —
status taxonomy outside sweep); `/key` error classification string-sniffs 401 inside
`get_json_bearer` errors (typed-status refactor belongs to that helper's owner).

## Grok review verdict: FIX-APPLIED, now ACCEPT

- **Omitted `creditUsagePercent` + valid weekly period → FIXED to unknown/No-data**
  (was 0% used / 100% remaining). Justification for fix over justify: no research evidences
  proto3-zero semantics; the `{val}` wrapper shape is custom JSON, not proto3-canonical
  (real protobuf wrappers serialize bare), and the first-party client is Rust where the
  field is plausibly `Option<f64>` — omission = unknown. F05 + this task's own antigravity
  precedent (#3) demand unknown≠full. Known period end still anchors reset; no slot.
  Locking test rewritten; explicit `0.0` still → full meter.
- Same-class adjacent fixes: fallback omitted `used` → No-data (was 0%); on-demand omitted
  `used` → limit-only row (was $0); `checked_cent_magnitude` `abs()` REMOVED — negative
  limits/caps/balances no longer mirror into positive bounds (`positive_cent_value`;
  negative prepaid is a deficit, not credit). All covered by new inline tests.
- Rest of sweep clean: monthly-period guard correct (`creditUsagePercent` is weekly-scoped;
  research §14 "legacy monthly fields cannot relabel a current weekly response"),
  subscription-over-env-key precedence, EnvKeyOnly honest gap, tier display-first chain,
  error taxonomy (note: `classify_grok_billing_error` is currently exercised only by tests —
  snapshot maps billing errors directly; broker-lane wiring opportunity, not a defect).

## OpenCode review verdict: FIX-APPLIED, now ACCEPT

- `1 means 1%`: RESEARCH-EVIDENCED (research §16: Go route reports 0–100 percents) — kept,
  citation added. Entitlement-vs-Key taxonomy EVIDENCED (distinct 403, research §16) — kept,
  citation added; in-band-type-wins + bare-401/403 mapping sound.
- `zenBalanceUSD`/per-model dropping CORRECT (research §16: per-model fields unverified
  until live capture; "no verified Zen source, not zero balance") — honest gap, locked test kept.
- Sweep fixes: (a) `OpenCodeAuthEntry` (`key`) dropped `Debug` (same latent-secret class as
  cursor #2). (b) `percent > 100` was a whole-parse `Err` (one over-cap window blanked all
  three — T02 violation); now raw `101% used` label + remaining 0, siblings survive.
  Locking test honestly rewritten (negative still errors, messages asserted).

## Verification (observed)

- `cargo test -p jackin-usage --lib usage::` → **265 passed, 0 failed** (174 filtered out).
  Includes all new/updated inline tests; shared `usage/tests.rs` untouched and green
  (codex `140→100` clamp lock holds by design — clamp helper preserved, raw added alongside).
- `rustfmt --check` on all 12 owned files → clean.
- `cargo clippy -p jackin-usage`: my lines clean (one `map_unwrap_or` in new code fixed).
  Full-clippy pass currently IMPOSSIBLE: deny-level `unnecessary_closure` error in
  `jackin-config/src/accounts.rs:757` (another lane's file) fails the dependency build;
  remaining `jackin-usage` warnings (cursor `map().unwrap_or`, gemini `find_map`, hermes
  doc backticks, cursor:24 `expect`, antigravity `assert_is_err`) are all pre-existing
  lines I did not author — left for owning lanes, none in my added code.
- Build was transiently blocked twice by other lanes' in-flight edits (`jackin-config`
  15→0 errors; `host/discovery.rs` `xdg_roots`); resolved without touching their files.
- Parent follow-up re S2 `xdg_roots` breakage at "kimi.rs:689 / openrouter.rs:35": NOT
  APPLICABLE — zero `AccountCredential` references in any owned file; those coordinates
  were my own earlier compile errors (already fixed). No `xdg_roots: None` needed in scope.
- `git status` confirms: edits confined to the 12 owned files; `usage/tests.rs`, `amp.rs`,
  `usage.rs`, `host/*` diffs are other lanes' content (spot-checked, no fmt noise from my
  `cargo fmt -p` run — it was idempotent on their files).
