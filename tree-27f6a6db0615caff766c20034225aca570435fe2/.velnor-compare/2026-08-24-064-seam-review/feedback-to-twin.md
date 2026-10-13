# Plan 064 seam arbitration — feedback to twin

Date: 2026-08-24. Arbiter: critical validator, static review only (no gates; tree mid-flight).
Compared: A = `fb4fdbc` as-reviewed, B = twin restructure (committed as `5ab7479`, on top of `fb4fdbc`; task brief said "uncommitted" — it is not).
Authority: `plans/velnorctl-migration/064-scaffold-workspace-and-cli.md`, AGENTS.md laws.

## Per-defect verdict table

| # | Defect | fb4fdbc | Twin tree | REQUIRED action |
|---|--------|---------|-----------|-----------------|
| 1 | CRITICAL: velnorctl missing serde_json dev-dep while boundary test used it (`tests/dependency_boundaries.rs` referenced `serde_json::Value`; `[dev-dependencies]` empty) | Present — suite does not compile | Fixed by deletion; no serde_json use remains in velnorctl | Remove the now-empty `[dev-dependencies]` section from `crates/velnorctl/Cargo.toml`. Re-add dep only when a real consumer lands. |
| 2 | CRITICAL: boundary test asserted REQUIRED `velnorctl → velnor-runner` edge absent from manifest (`crate_dependency_edges_match_the_plan` requires runner in ctl deps; manifest lists only model/control/client/render) | Present — test fails against own manifest | Fixed: asserts the four real required edges; legacy crates must not depend on new crates; runner edge neither required nor forbidden | Keep it that way. Plan says velnorctl MAY depend on the interim facade ("may depend", Target shape), never MUST. If a later task consumes the facade: add dep + assert ALLOWED, never assert presence as plan law. |
| 3 | MAJOR: parser_seams subprocess expectations contradicted lib wiring (expected bare argv → 2 and "unknown command 'cache'"; wired main returned 0 via `Outcome::Usage` and 3 via `LegacyRejected`) | Present — subprocess test fails against actual binary | Fixed: deleted contradictory suite; new subprocess smoke matches wired path exactly (--help=0, bare=2, cache=3, version=2) | Subprocess assertions must stay byte-synced to the wired dispatch path forever. Any taxonomy change updates both in one commit. |
| 4 | MAJOR: two parallel seam systems (lib.rs Outcome/Registry wired in main vs unwired cli.rs/dispatch.rs/commands/mod.rs with contradictory codes — everything→2 incl. MissingCommand→2 vs lib empty→0) | Present | Fixed: single seam survives (Outcome/Registry); orphan modules and their tests deleted; lib.rs declares no submodules for them | Do not reintroduce any parallel parse/dispatch surface. One composition point, C-tasks register into it. |
| 5 | MINOR: commit message overclaims ("every invocation … exits nonzero" — false: bare argv and -h/--help exited 0) | Present | Fixed by replacement message: honest (bare=2 usage-fail, explicit --help=0, matching clap required-subcommand) | Commit messages enumerate actual exit behavior. fb4fdbc's message was false; if history is rewritten before push, correct it there too. |

## Design-axis rulings

1. **Single vs dual seam system** → SINGLE (twin). Plan step 3 names global parser/dispatch composition points; two systems guarantee behavioral contradiction — defect #3 proved it live.
2. **Registry API shape** → Arc closure `Arc<dyn Fn(&[String]) -> Result<(), String> + Send + Sync>` (twin kept; fb4fdbc's fn-pointer `fn(&[String]) -> i32` dies with cli.rs). Closure carries state for future command modules; `Result` feeds central exit mapping instead of raw i32 bypassing the taxonomy.
3. **Exit-code taxonomy** → adopt twin table: 0 help/handled · 1 handler failure · 2 no-command/unimplemented · 3 legacy-rejected. Matches clap required-subcommand convention (usage error = 2) and keeps old-binary mental model coherent. Legacy rejection stays distinct (3): zero-alias law is a different failure class than unknown-command.
4. **Boundary-test placement** → single home in `velnor-client/tests/dependency_boundaries.rs` (twin). Exactly one authority beats duplication; client is permanent so suite survives velnorctl churn; workspace-wide laws may be asserted from any member crate. Never duplicate into velnorctl again.
5. **Facade extraction** → fb4fdbc shape is correct and twin correctly left it untouched: `scaffold::execute()` inside velnor-runner (strict-capability admission → manifest integrity → Cli::parse → telemetry dir selection → dispatch), thin `main.rs` adapter with bounded runtime shutdown. Old binary behavior unchanged. Facade unconsumed at 064 is acceptable — first consumer is C001. No speculative consumption now.

## MUST / FIX list (before declaring 064 done)

1. MUST delete empty `[dev-dependencies]` header from `crates/velnorctl/Cargo.toml`.
2. MUST keep exactly one parser/dispatch seam (`velnorctl::dispatch` over `compose()` registry). Any second classification path is a defect regardless of tests passing.
3. MUST keep boundary assertions equal to the real resolved manifest at every commit; run the client boundary suite before committing.
4. MUST keep subprocess smoke expectations identical to wired-path outcomes: `--help/-h`=0, bare=2, unregistered=2, legacy name=3, handler-fail=1 once handlers exist.
5. MUST NOT require or forbid the interim `velnorctl → velnor-runner` edge in tests; treat as optional per plan until consumed, then assert allowed-only.
6. MUST keep commit messages factual about exit codes and coverage; no claim without a corresponding assertion.
7. SHOULD (non-blocking): drop redundant `self` in `use velnorctl::{self, Outcome}`; consider collapsing `Help(String)`/`NoCommand(String)` into one usage-carrying variant distinguished by exit code when the signature is next touched.
8. Async-readiness note (non-blocking, record for C001): sync closure seam is fine for scaffold; when the first async leaf lands, widen `Handler` deliberately (async trait or block_on inside handler with an owned runtime) in its own task — do not pre-widen speculatively now.

## Verdict

**ACCEPT-BASE**: `5ab7479` is the base of record going forward. It resolves all five defects in fb4fdbc, which alone contained a non-compiling suite (#1), a self-failing boundary test (#2), and a binary contradicting its own integration test (#3) — FIX-THEN-COMMIT applied to fb4fdbc, satisfied by the twin restructure. Remaining items are MUST-list hygiene items 1–6 (item 1 blocking the next touch, none requiring a new commit today).
