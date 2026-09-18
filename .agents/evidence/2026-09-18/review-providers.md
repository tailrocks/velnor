# Provider-lane review vs contracts (read-only)

Branch `feat/multi-account-support`, uncommitted diffs. Contracts: `plans/multi-account/001-t02-domain-contracts.md` (T02),
`jackin-provider-research.md`, F-checklist (`jackin-implementation-and-verification.md` §F), G-items where UI-owned.
`cargo check -p jackin-usage` passes (verified this session). Out of scope / not reviewed: `usage/openrouter.rs`
(740-line untracked file, another lane) — needs its own review.

## Per-file verdicts

### `usage/claude.rs` — ACCEPT (1 minor note)
- Scope-restriction classifier correctly excludes 401/transport; only applied to `oauth_error`; `is_active` non-gating
  is evidence-backed (repo fixtures) and locked by test. Verified `claude_resolved_view` (`claude.rs:231-285`):
  the new `provider_error` only feeds the both-failed path, so no behavior change on OAuth-fail/CLI-success.
- Minor: on Fresh-via-CLI-fallback, `last_error` uses the raw `oauth_error` (`claude.rs:278`), not the normalized
  inference-only message — the explicit scope text never surfaces there. Suggest routing through
  `claude_provider_error_label`.

### `usage/codex.rs` — FIX-REQUIRED (minor)
- `CodexWindowSnapshot::used_percent_clamped` (`codex.rs:~487`) clamps >100 to 100 and the test locks `140→100`.
  T02 §6 requires over-100% raw values kept with only bar geometry clamped (Muse lane does this correctly).
  Suggest: preserve raw in `used_label` (e.g. `142% used`), clamp only `remaining_percent`.
- Wham relative reset (absolute-wins, negatives ignored, saturating), tolerant `used_percent`, defaulted RPC
  shapes all match F12. No `#[deny_unknown]`. Good.

### `usage/amp.rs` — ACCEPT (notes)
- Tier/legacy parsing, renewal anchoring, Orb independence, bold-strip, `Spend`-slot Money, Daily-headline
  isolation all correct; month≈30d approximation disclosed in pace label. No dollars→tokens invention (F13 units kept).
- Gap (evidence-limited, lane-disclosed): no linked-subscription/billed-via line parser — F13's "linked external
  subscription routing" stays partial until a live capture (H06/H09) evidences the shape. Plan name as funding
  route is the right interim.

### `usage/kimi.rs` — FIX-REQUIRED (1 moderate, 1 minor)
- MODERATE `kimi.rs` `KimiExtraUsage` aliases: unitless keys (`remaining`, `used`, `balance`, `total`, `cap`,
  `limit`, `monthly_cap`, `monthly_used`) are assumed to be minor units (exponent 2 `Money` on the `Spend` slot).
  Research §10 evidences only cent-denominated fields. A major-unit value under an invented alias renders 100× off
  in the headline. Fix: keep only evidenced `*_cents`/`balance`/`total` keys; treat unitless-alias hits as
  unknown-scale labels, not `Money`. (F06 currency precision.)
- Minor: `KimiPool::used_percent` clamps used to limit — same T02 over-100% point as Codex.
- `used_ratio` ≤1-fraction/>1-percent heuristic, pool-supersedes-summary (no double render, F10), version-gated
  membership names, identity fallback chain, in-band error rejection all good. `fetch_kimi_local_usage` unwired
  pending broker discovery — acknowledged, correct to not probe.

### `usage/zai.rs` — ACCEPT (2 minor notes)
- `CREDIT_LIMIT`/`TOKENS_LIMIT` slotting by explicit duration (F02), `success:false` distinct state, empty-windows
  error (never empty-but-fresh), team scope pure + tested, `epoch_seconds_from_maybe_ms` fix — all match F17/F29.
- Minor: `TIME_LIMIT` → `Web search` split at ≥28d (`zai.rs:~233`) is a window-size guess; a 28d+ MCP window
  mislabels. No sharper signal exists; keep but note residual risk.
- Minor: `zai_is_peak` hardcodes Mon–Fri 06:00–10:00 UTC. Pace-note only (no quota math), UTC-explicit (F07 OK),
  but will go stale silently on plan change. Suggest a named constant + comment pointing at the plan doc.

### `usage/minimax.rs` — ACCEPT (1 note)
- Token Plan vs PAYG routing, region pinning (no cross-region retry — research §17 honored), boost scaling past
  100 with raw kept (F06 ✓), status 2/3 exhausted/unlimited, `remains_time` ms fix, balance amounts as labels-only
  with region currency (F04/F09 ✓). Good.
- Note: `sk-api-*` → PAYG key-shape rule (`minimax.rs:99`) is load-bearing routing from `ref-contracts-B §3`,
  which was not in my review inputs — I could not verify its evidence. Confirm citation before freeze.

### `usage/antigravity.rs` — FIX-REQUIRED (1 moderate, 2 minor)
- MODERATE `antigravity.rs:215`: summary entry with absent `remainingFraction` maps to depleted `Some(0)` and the
  test `summary_absent_fraction_is_depleted_not_missing` locks it. Research §1.5: missing quota is typed
  unavailable, never 0% remaining; F05 keeps unknown vs exhausted distinct. The `/usage` schema is unauthenticated
  and unverified (lane admits), so absence ⇒ exhausted invents data. Fix: `None` + "No data" detail row (the legacy
  path already does this correctly); only an explicit depleted marker should yield 0.
- Minor `antigravity.rs:610-620`: missing/unparseable `agy` binary → `NeedsLogin`. A login cannot install a binary;
  Cursor/Gemini lanes use `NeedsSecret` for missing local auth. Use `NeedsSecret` (keep `Unsupported` for predates-JSON).
- Minor `antigravity.rs:630-648`: parsed-but-zero-pool response yields view status `Fresh` containing a `Stale`
  placeholder bucket. Make the view `Stale` when no pool parsed.
- Version gate, exact bucket-ID match, legacy worst-fraction collapse, availability-only drop, credits minor-unit
  gating, identity-may-omit origin — all match F14/research §9.

### `usage/gemini.rs` — FIX-REQUIRED (1 moderate, 1 minor)
- MODERATE `gemini.rs:153-166,292`: `gemini_migration_action(None, has_oauth && !has_api_key, now)` tells EVERY
  OAuth-only user past 2026-06-18 (i.e. all of them now) to reconnect with Standard/Enterprise — including managed
  Standard/Enterprise logins that are already eligible. Research §9 requires the targeted notice + actual
  entitlement, "don't classify every 403 as migration" — here there isn't even a 403. Fix: show migration only on
  entitlement `consumer_unsupported`; otherwise show the generic reporting-gap message.
- Minor `gemini.rs:46-52`: `GOOGLE_API_KEY` alias assumed; T02 §1 says S1 decides alias support. Confirm S1 recorded it.
- Tier-over-plan preference, missing-tier-is-unknown, no-denominator-invention, typed `Unsupported` gap — good (F15).

### `usage/cursor.rs` — FIX-REQUIRED (2 moderate, 3 minor)
- MODERATE `cursor.rs:289-319`: `parse_cursor_credit_grants` sums top-level `grantTotal`-family AND `grants[]`
  itemized amounts. If the server returns both a total and its breakdown (the usual shape), credits double-count.
  Fix: use itemized sum when `grants[]` is non-empty, else the top-level total.
- MODERATE `cursor.rs:37-41` + `cursor.rs:700-703`: `CursorAuth` (holds `access_token`) and `CursorEnterpriseScope`
  (holds `admin_token`) derive `Debug`. Any future `{:?}` log/error renders live secrets. Claude credentials
  deliberately omit `Debug` for this reason. Fix: drop the derives or hand-write redacting `Debug`.
- Minor: `format_currency` hardcodes `$` and credits become `Money(USD)` — Cursor currency is research-unverified.
  USD is near-certain; still, note as assumption (F06).
- Minor `cursor.rs:459-484`: request usage takes the FIRST model entry with a counter; multi-model responses pick
  arbitrarily. Prefer an explicit model or document.
- Minor: `scope_urls_never_cross` test reads live `CURSOR_API_ENDPOINT`; a set env var breaks it. Make hermetic.
- Personal/Enterprise host separation verified (`cursor_snapshot` never touches `api.cursor.com`), Spend-slot
  label-only rows correctly excluded from headline (`spend_headline_label` requires `used_money`), pooled/zero
  Grok Bot → no meter, events never overview-polled, `overall` raw. Matches F19/F29.

### `usage/muse.rs` — ACCEPT
- Omitted usage → `Ok(None)`, raw over-100% in `used_label` with remaining clamped to 0 (T02-exact), duration-based
  window label, `observedAtMs` freshness preservation, key-exchange `polling_enabled()==false` with
  whitelist/denylist disjointness tested. Matches F21/research §12. No issues.

### `usage/omp.rs` — ACCEPT
- Verbatim attribution, pool routing → zero buckets, pool-file-is-not-authorization. Matches F24. No issues.

### `usage/hermes.rs` — FIX-REQUIRED (minor)
- `hermes.rs:99-119`: Portal `cycle_ends_at` (a renewal date) feeds `timed_bucket.reset_at`, rendering as
  "Resets in N days" (`format.rs:118-137`). F07: reset ≠ renewal. Fix: `None` reset + pace/detail "renews <date>".
- Decimal-verbatim credits, `current:null` → `Ok(None)`, exclusive profiles, tracker → zero buckets, 401/403 →
  `NeedsLogin` fail-closed. Match F24. Good.

### `usage/tests.rs` (lane-A additions) — ACCEPT
- 10 tests assert real decoded values (inactive-flag rendering, scope labels, relative resets, tolerant percents,
  sparse RPC, tier/orb/renewal math incl. `1234.50/2000→62%`). No tautologies; fixtures sanitized. The two
  `codex::` qualification removals + 5 `buckets(now)` updates are mechanical and correct.

### `console/.../usage.rs`, `usage/tests.rs`, `input/list.rs`, `console/adapter/run.rs` — ACCEPT (1 disclosed gap)
- Stable-ID selection + Overview fallback with appended notice (G12 ✓), `meter_percent` used→remaining mirroring
  with `None` → no bar (F03/F04 ✓), freshness ages + `refreshing…` (G13 ✓), heartbeat 2min with completion-reset
  timer (G04 ✓, no sleep burst), `r` joins in-flight work, errors advance timer (G05 ✓), worker-thread broker work
  (G17 ✓ — render thread only polls/applies), narrow/wide render tests. Tests substantive, no tautologies.
- Gap (lane-disclosed, loop-owner work): refresh is keypress-driven; G03 "updates with no user input" needs the
  tick-owned drain (`poll_refresh`/`heartbeat_due`/`begin_refresh` API is already shaped for it). Phase-2, not this lane.

## Contract-trace gaps (cross-lane, not lane faults)
1. T02 §6 `metric_groups` typed groups: all lanes still project into `QuotaBucketView`/`windows`. Expected —
   T10 broker work; no lane should have freelanced canonical types.
2. `UsageSurface` has no Antigravity/Gemini/Cursor/Muse/omp/Hermes variants; lane-C/E snapshots build via
   `Unsupported` + patched labels and lane-E adapters are entirely unwired from dispatch. Integration debt for the
   surface-owning lane (acknowledged in both reports).
3. `usage/openrouter.rs` (new, 740 lines) is outside this review's file list — needs independent review before freeze.
4. F13 Amp linked-route parser and Kimi local-server snapshot wiring both await live evidence/discovery (H06/H09);
   current honest-gap handling is correct.

## Fix list, ordered by severity
1. (Mod) cursor.rs:289 — credit grants total+breakdown double-count → prefer itemized sum when present.
2. (Mod) cursor.rs:37,700 — `Debug` on token-holding structs → redact or drop derive.
3. (Mod) antigravity.rs:215 — absent fraction ⇒ 0% → map to unknown/"No data"; update locking test.
4. (Mod) gemini.rs:153,292 — migration text for all OAuth → gate on entitlement `consumer_unsupported`.
5. (Mod) kimi.rs `KimiExtraUsage` — drop unitless-cent aliases or render them as unknown-scale labels.
6. (Minor) codex.rs `used_percent_clamped`, kimi.rs `KimiPool::used_percent` — keep raw over-100 in `used_label`.
7. (Minor) hermes.rs:99 — `cycle_ends_at` renewal shown as "Resets…" → pace/detail "renews <date>".
8. (Minor) antigravity.rs:613 — missing binary → `NeedsSecret`, not `NeedsLogin`.
9. (Minor) antigravity.rs:644 — zero-pool parse → view `Stale`, not `Fresh`.
10. (Minor) claude.rs:278 — CLI-fallback `last_error` should use the normalized scope message.
11. (Minor) cursor.rs — record USD/scale assumption; make `scope_urls_never_cross` env-hermetic; pin request-model pick.
12. (Minor) zai.rs — document 28d `Web search` heuristic + peak-window source; minimax.rs — cite key-shape evidence.
13. (Nit) `· team` (zai) vs `· Team` (cursor) casing differs.

## Challenge-area sweep
- Invented quota / dollars→tokens: none found. Worst cases are #3 (0%-from-absence) and #5 (cent assumption).
- Scope confusion: none — personal/team, key/management, consumer/managed separations hold except #4's message.
- Reset/expiry/renewal labels: clean except #7 (and Amp's disclosed month approximation).
- Over-100%: Muse/MiniMax correct; Codex/Kimi clamp (fix #6).
- Secrets in fixtures/logs: fixtures sanitized (synthetic JWT, `sk-api-test`); live tokens never logged; latent
  risk is #2 (`Debug` derives).
- Test tautologies: none; `polling_enabled()==false`-style asserts are valid regression tripwires.
