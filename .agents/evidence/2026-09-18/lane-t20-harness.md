# T20 test-harness prep — report

Branch `feat/multi-account-support`. No commit (per instructions).
Changed: `crates/jackin-test-support/src/lib.rs` (+6 lines: 3 `mod` + 3 `pub use`,
existing convention). Everything else is NEW files under
`crates/jackin-test-support/src/`. No `Cargo.toml` change — all three modules
are **std-only** (no new deps), so provider/adapter crates inherit zero
dependency cost.

## New APIs

### `fixture_http` — instrumented HTTP fixture server
- `ScriptedResponse::new(status, body)` + builders `.with_header(k, v)`,
  `.with_retry_after_secs(n)`, `.with_latency(d)`. `Content-Length` and
  `Connection: close` added automatically; reason phrases for common codes,
  `Unknown` fallback.
- `FixtureHttpServer::start(script)` → binds `127.0.0.1:0`, serves one
  connection per script entry **sequentially** (deterministic order).
  `addr()`, `url()`, `request_count()`, `requests()`,
  `push_response()` (extend live script for multi-phase tests),
  `script_remaining()`. Empty script → `500` until extended; last entry
  sticks (repeats). Drop = clean shutdown + thread join.
- `RecordedRequest { method, path, headers, body }` + `.header(name)`
  (case-insensitive), `.body_text()`, `.assert_no_secrets(&[&str])`
  (panics naming match index/location, never the secret; empty entries skipped).
- `redact_secrets(text, secrets) -> String` — replaces hits with `<redacted>`.

Example: script `[429 + Retry-After: 2, 200 + JSON]`, drive adapter at
`server.url()`, assert `request_count() == 2` and recorded paths/headers.

### `fixture_process` — fake TUI/CLI process harness
- `FakeProcessHarness::new()` → unique temp dir (removed on drop);
  `.binary(name, &script)` writes an executable `sh` fake + fresh log.
  `name` must be a plain file name (else panic). Harness must outlive binaries.
- `ProcessScript::new().on_exact([args], stdout, stderr, code)`
  `.on_prefix([args], …)` (trailing flags allowed; empty prefix matches all)
  `.with_default(…)` + conveniences `version_stub(name, ver)`
  (`--version`/`version`/`-V`), `help_stub(usage)`
  (`--help`/`help`/`-h`).
- Dispatch is generated `if/elif` on `$#` + per-position literal `[ = ]`
  comparisons (`${N}` form) — **no `case` globs**, so args containing
  `*?[]`, backslashes, quotes, `$`, spaces match exactly. Bodies emitted via
  `printf '%s'` + single-quote escaping (no shell expansion).
- `FakeBinary::path()`, `.command()` (`sh <script>` fallback on non-Unix),
  `.invocations()` → `Vec<Invocation>` in spawn order (`argv` incl. `argv[0]`,
  env pairs; empty vec before first spawn).
- `Invocation::args()`, `.env_get(k)`, `.assert_no_secrets(&[&str])`.

Example: `version_stub` + `on_prefix(["account","list"])`; spawn, assert
stdout/exit; loop `invocations()` asserting no secrets in argv/env.

### `time` — controllable clock (was missing; nothing duplicated)
- `ManualClock::new(start)` / `::epoch()` / `Default` (= epoch).
  Shared `Arc<Mutex<…>>`: clones see one time (broker thread + driver thread).
- `.now()` (poison → epoch, never panics), `.set(t)` (backwards ok),
  `.advance(d) -> SystemTime`, `.elapsed_since(t)` (saturating),
  `.deadline(from, interval)`, `.is_due(from, interval)`.

## Secrets / redaction (documented in each module's docs)
- Fixtures must use fake placeholders only; never real credentials.
- HTTP: `assert_no_secrets` over path+headers+body; `redact_secrets` before
  logging/snapshotting recorded traffic.
- Process: `assert_no_secrets` over captured argv+env; panic messages are
  index/location only, secret never echoed (covered by `should_panic` tests).

## Verification
- Harness self-tests: 9 http + 10 process + 7 time = **26 unit tests**, plus
  **3 doctests** (one runnable example per module).
- `cargo fmt --check`: clean on real files.
- `cargo test -p jackin-test-support` **could not run in-workspace**: blocked
  by another lane's in-progress `crates/jackin-config` edit (69
  `unreachable_pub` errors; `jackin-manifest → jackin-config` is in the dep
  chain). Untouched per scope. Earlier this session, before that breakage,
  the real command compiled and ran these tests.
- Equivalent verification in `/tmp/t20-crate` (byte-identical sources, same
  crate name, full workspace `[lints]` copied): **26/26 unit + 3/3 doctests
  pass; `cargo clippy --all-targets`: 0 errors, 0 warnings; `cargo doc`:
  clean** (intra-doc links verified).
- Notable bugs found & fixed during self-test: (1) prefix `case` pattern
  emitted `*))` + bash-3.2 rejects quoted-backslash escapes in `case` →
  replaced `case` with literal `[ = ]` dispatch; (2) emits joined with space
  sent all output to stderr → joined with `;`.

## Files
- `src/fixture_http.rs` (399) + `src/fixture_http/tests.rs` (182)
- `src/fixture_process.rs` (419) + `src/fixture_process/tests.rs` (192)
- `src/time.rs` (101) + `src/time/tests.rs` (67)
- `src/lib.rs` (+6)
