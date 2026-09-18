# Rejection-path audit: wrong-product checks and their tests

Scope: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor-bootstrap` (read-only; no repo writes made).
Checked-in vs source: `cmp` confirms `.github-gen/sources/actions/setup-velnor-workflow/action.yml`
MATCHES `.github/actions/setup-velnor-workflow/action.yml` (both 188 lines; line numbers below apply to both).
Checked-in `.github/workflows/ci-policy.yml` L54-113 matches the `policy_candidate_step` template
(`crates/velnor-workflow/src/lib.rs:3602-3665`) by inspection; no test diffs the checked-in bytes
(see gap G6).

Legend: (a) setup action, (b) Velnor provisioner = `workflow_pinned_policy_runtime_velnor`
(`crates/velnor-workflow/src/lib.rs:4220-4222`, one `format!` string; fragments cited as `lib.rs:4222/<fragment>`),
(c) ci-policy acquire (`ci-policy.yml:54-113` / template `lib.rs:3602-3665`) + `policy.rs`
`render_with_candidate` (`crates/velnor-workflow/src/policy.rs:1469-1545`) and `load_candidate_manifest`
(`policy.rs:1213-1245`).

## 4. Wrong products rejected

### 4.1 Wrong digest (binary bytes differ from manifest)

- (a) Download path `action.yml:136-142`: recompute sha256 (sha256sum/shasum fallback) of
  `$temporary/$asset`, compare to `jq -er .products[$platform].binary`; mismatch →
  `::error::runtime digest mismatch`, exit 1. Cached path `action.yml:162-168`: same recompute
  against `$runtime/bin/velnor-workflow` → `cached runtime digest mismatch`. Asset attestation
  `action.yml:122-125` is a second, independent tripwire for tampered bytes.
- (b) `lib.rs:4222/"expected=$(jq -er ... .products[$platform].binary"` + recompute of slot
  (`sha256sum "$binary"` / `shasum -a 256 "$binary"`) + reuse gate
  `if [[ "$reported" != "$closure" || "$existing" != "$expected" ]]`; slow path re-downloads and
  gates install on `[[ "$actual" == "$expected" ]]` → `policy runtime digest mismatch`.
- (c) Acquire `ci-policy.yml:100-106`: recompute, compare to `jq -er .binary_sha256` →
  `candidate digest mismatch`. `policy.rs:1524-1529`: `sha256_file(&binary)` compared to
  `bound.binary_sha256` BEFORE any execution; mismatch → `continue` (skip, never runs `--closure`).
- Tests: (a) NONE pins the two `[[ actual == expected ]]` gates or the mismatch strings
  (gap G1). (b) `lib.rs:7297 velnor_provisioner_reuses_the_slot_only_on_manifest_digest_match`
  (AND-gate, both-toolchain recompute, attest<gate<install order, slow-path mismatch string);
  also `lib.rs:959 regen_gate_unit_provisions_the_pinned_policy_runtime_on_the_velnor_lane_only`
  in `tests/velnor_first_ci.rs` (end-to-end rendered unit contains the gate).
  (c-acquire) NONE pins `candidate digest mismatch` (gap G4).
  (c-policy.rs) `policy/tests.rs:1021 lying_binary_whose_digest_misses_the_manifest_is_rejected`
  (digest miss → `None` AND `--closure` sentinel never touched: proves digest-before-exec).

### 4.2 Wrong platform (e.g. Linux asset on macOS, X64 bytes on ARM64)

- (a) `action.yml:114-115` asset name contains `${RUNNER_OS}-${RUNNER_ARCH}`; `action.yml:130-135`
  jq filter requires `.products[$platform].binary` to be 64-hex AND `.products[$platform].asset == $asset`;
  `action.yml:141` extracts the digest for the running platform only; cache key `action.yml:99`
  includes `runner.os-runner.arch`. A foreign-platform manifest entry simply has no digest under
  the running key → `jq -er` fails closed. asymmetry: the Verify-step filter `action.yml:157-161`
  drops the `.asset == $asset` clause (download path still enforces it; digest stays per-platform).
- (b) Same jq filter + per-platform `expected` inside `lib.rs:4222` (filter string is
  `MANIFEST_ACCEPT_FILTER`, `runtime_products.rs:74`).
- (c) Acquire `ci-policy.yml:75` artifact name embeds `${RUNNER_OS}-${RUNNER_ARCH}`;
  `ci-policy.yml:99` requires `.profile == "debug" and .platform == $platform and .repository == $repo
  and .run_id == $run`. `policy.rs` has NO platform check by design: `CandidateManifest`
  (`policy.rs:1204-1208`) binds only `revision`/`closure`/`binary_sha256` (serde ignores the rest);
  the byte digest is the platform-independent binding.
- Tests: (a) `runtime_products.rs:938 manifest_shape_matches_the_consumer_contract` asserts
  `action.contains(MANIFEST_ACCEPT_FILTER)` (covers closure+profile+features+digest+asset clauses
  jointly); cache-key platform component untested (gap G1, partial). (b) same test asserts
  `velnor.contains(MANIFEST_ACCEPT_FILTER)` (`runtime_products.rs:966-970`).
  (c-acquire) `.platform == $platform` clause untested (gap G4). (c-policy.rs) n/a by design
  (no check → no test needed; not a gap).

### 4.3 Wrong closure (product built from other sources)

- (a) `action.yml:130-135` (download) and `action.yml:157-161` (verify) require
  `.closure == $closure` where `$CLOSURE` is computed locally from `git ls-tree` bytes + footer
  (`action.yml:81-90`); final `action.yml:179-180` requires the binary's `--closure` to equal it.
- (b) `lib.rs:4222` jq `.closure == $closure` (closure resolved from local `ls-tree` at the pin,
  fetching the pin commit when shallow) + reuse AND-gate + final
  `[[ "$reported" == "$closure" ]]` gate.
- (c) Acquire `ci-policy.yml:74` computes `pin_candidate` locally
  (`velnor-workflow closure --rev=$pin --candidate`); `ci-policy.yml:108-109` requires
  `manifest_closure == pin_candidate` (full 64-hex equality, not prefix).
  `policy.rs:1496-1502`: `manifest.closure != wanted` (wanted = `candidate_closure_of_tree`
  of the audited checkout, `policy.rs:1483-1485`) → loud `usage` error naming both closures.
- Tests: (a) `MANIFEST_ACCEPT_FILTER` containment (same as 4.2) + `lib.rs:7419
  setup_action_acquires_a_product_and_never_compiles` (closure-resolution fragments: local
  `ls-tree` pathspec, trees-API fallback, `LC_ALL=C sort`, footer) + `runtime_products.rs:822
  closure_shell_matches_the_setup_action` (producer/consumer byte-stream agreement).
  (b) reuse-gate half of `lib.rs:7297`; final `reported == closure` gate has NO direct
  assertion (gap G2, partial — `velnor_first_ci.rs:959` only asserts `--closure` presence).
  (c-acquire) `lib.rs:12786 policy_candidate_step_binds_manifest_to_pin_and_exports_it`
  (full-equality fragment + both-digest error text + manifest export + early-exit clearing).
  (c-policy.rs) `policy/tests.rs:1055 candidate_manifest_for_another_tree_is_rejected`
  (self-consistent manifest+binary for CLOSURE_A rejected with error naming both digests) and
  `tests/velnor_first_ci.rs:746 candidate_manifest_env_fallback_binds_the_env_slot_candidate`
  (end-to-end: env manifest mismatch fails loudly through the `policy` CLI).

### 4.4 Lying `--closure` self-report (bytes echo a closure they were not built from)

- (a) `action.yml:179-180`: `reported=$("$runtime/bin/velnor-workflow" --closure)`;
  `[[ "$reported" == "$CLOSURE" ]]` else fail. Runs AFTER the digest proof (Download→Verify→PATH
  order), so a liar must also defeat the manifest digest.
- (b) `lib.rs:4222` final `[[ "$reported" == "$closure" ]]` after install; reuse also requires
  the report (`||` gate). Doc comment `lib.rs:4190-4218` names the threat model (manifest-first
  closes the self-report-alone planted-binary hole).
- (c) Acquire `ci-policy.yml:110-111`: `reported == manifest_closure` else
  `candidate reports closure ..., manifest claims ...`. `policy.rs:1530-1535`: after the digest
  gate, `binary_closure(&binary) != wanted` → `continue` (final tripwire; doc `policy.rs:35`
  and `policy.rs:1460-1468`: "`--closure` echo is an assertion by untrusted bytes, not proof").
- Tests: (a) `lib.rs:7419` asserts `--closure` invocation presence; equality gate string
  untested (gap G1, partial). (b) gap G2 (same final gate). (c-acquire) untested (gap G4).
  (c-policy.rs) `policy/tests.rs:1214 candidate_render_rejects_a_binary_claiming_another_closure`
  (valid manifest+digest binding, binary echoes CLOSURE_A → `None`); deleted-test note at
  `policy/tests.rs:915-922` documents that self-report-alone acceptance WAS the vulnerability.

### 4.5 Manifest/binary swap (manifest of X paired with binary of Y, incl. cross-closure mix)

- (a) Joint: filter binds manifest→expected closure (`action.yml:134`) while digest equality binds
  binary bytes→manifest entry (`action.yml:142`); a swapped pair fails one of the two. Attestation
  (`action.yml:122-129`) authenticates each file individually but does NOT bind the pair — the
  digest equality is the pair binding. Ordering pinned by test below.
- (b) Same joint structure in `lib.rs:4222` (filter → `expected` → digest compare).
- (c) Acquire `ci-policy.yml:106` digest equality is the pair binding; `policy.rs:1524-1529`
  digest-before-exec is the second, independent pair binding inside the validator.
- Tests: (a) `lib.rs:7481 setup_action_verifies_the_manifest_before_trusting_it`
  (asset-attest < manifest-attest < filter < digest order; exactly 2 attestations; manifest
  attestation pins owner/signer-repo/signer-workflow) + `runtime_products.rs:989
  attestation_covers_assets_and_manifest_in_the_pinned_workflow` (producer attests both subjects;
  both consumers verify both subjects). No test feeds a literally swapped pair through shell
  (shell is string-pinned, not executed — inherent, not a gap). (b) same two tests' velnor
  halves. (c-policy.rs) `policy/tests.rs:1021` IS the swap test (manifest bound to decoy bytes,
  binary echoes wanted closure → rejected, never executed).

### 4.6 Manifest for another tree (valid manifest, wrong tree)

- Same checks as 4.3 (closure equality vs locally computed value), with a distinct failure mode:
  (c-acquire) `ci-policy.yml:109` exit 1 naming both; (c-policy.rs) `policy.rs:1496-1502` loud
  error (not silent skip — "fails closed loudly ... not a skip", `policy.rs:1487-1488`).
  (a)/(b): `.closure == $closure` rejects; the tag locator (`velnor-workflow-runtime-v1-<16>`)
  is explicitly NOT trusted (`action.yml` header comment).
- Tests: same as 4.3, i.e. `policy/tests.rs:1055` + `velnor_first_ci.rs:746` for (c);
  (a)/(b) covered by filter-containment tests, not by a dedicated wrong-tree case.

### 4.7 Missing manifest (absent file / absent product)

- (a) `gh release download --pattern $asset --pattern manifest.json` fails closed with the
  producer-naming error (`action.yml:118-121`); `jq -e` on a missing file fails under
  `set -euo pipefail` (`action.yml:130`, `:157`); `test -s manifest` (`action.yml:178`).
- (b) `gh release download --pattern manifest.json` fails closed (`lib.rs:4222` "no policy
  runtime product for revision ..."); subsequent `jq` fails on missing file.
- (c) Acquire: `gh run download` fails when the artifact is absent; `jq -e` on missing
  `candidate-manifest.json` fails; `policy.rs:1489-1491` (`None` → no binding) +
  `policy.rs:1519-1521` (env-slot binary without manifest → `continue`, never executed).
  `policy.rs:1213-1245` additionally rejects unreadable/unparseable/misshapen manifests loudly.
- Tests: (a) `lib.rs:7419` asserts the "no runtime product for revision" + "mainline
  runtime-product publisher" strings; `test -s` untested (gap G1, trivial). (b) the
  "no policy runtime product" string is NOT asserted (gap G3).
  (c-policy.rs) `policy/tests.rs:1083 unbound_env_candidate_is_rejected_without_manifest`
  (`None` + never-executed sentinel) + `policy/tests.rs:1112
  malformed_candidate_manifest_is_rejected` (truncated JSON, short revision/closure/digest).

### Gap list

- G1 (a, setup action): several exact gate strings untested — `runtime digest mismatch` /
  `cached runtime digest mismatch` + `[[ "$actual" == "$expected" ]]` (both paths), `--closure`
  equality gate, `test -s`/`test -x`, Verify-step filter occurrence, cache-key `runner.os/arch`
  component. Only presence/order/filter-containment is pinned (`lib.rs:7419,7481,7523,10580`;
  `runtime_products.rs:822,938,989`). Severity: low-moderate — string-presence tests already
  force review of the whole file on change, but a gate could be deleted while its neighbors
  keep the tests green.
- G2 (b): final `[[ "$reported" == "$closure" ]]` self-report gate has no direct assertion
  (`lib.rs:7297` pins the reuse AND-gate and slow-path digest; `velnor_first_ci.rs:959` pins
  `--closure` presence only).
- G3 (b): "no policy runtime product for revision" fail-closed string unasserted.
- G4 (c-acquire): `candidate digest mismatch` gate, `reported == manifest_closure` gate, and
  the `.platform/.repository/.run_id` jq clauses are unasserted; only the
  `manifest_closure == pin_candidate` equality + export + early-exit are pinned (`lib.rs:12786`).
- G5 test-but-no-check (stale harness, not a security hole): `scripts/test-setup-velnor-workflow-action.sh`
  FAILS (15 assertions) against the current action — it expects `copy_with_mode`, GNU-isms, and
  `$temporary/runtime/...` paths that no longer exist. The script is stale, not the action.
- G6: no test diffs checked-in `.github/workflows/ci-policy.yml` against `generated_ci_policy()`
  output (acquire body could drift from the template; live CI policy `--check` is the only
  conformance enforcement). Contrast: setup action source-vs-generated IS compared (by the
  stale script in G5 and, effectively, by `cmp` MATCH today).
- No test-but-no-check gaps in the Rust rejection tests: every candidate/policy test names a
  real gate (verified by the revert experiment below for T1; T2/T3 by construction).

## 9. Security-critical tests are non-vacuous

- T1 `policy::tests::lying_binary_whose_digest_misses_the_manifest_is_rejected`
  (`policy/tests.rs:1020-1052`): fake binary echoes the wanted closure but the manifest binds
  decoy bytes; asserts `render_with_candidate` returns `None` AND a sentinel file (touched if
  `--closure` ever runs) does not exist. PERFORMED (see below).
- T2 `policy::tests::candidate_manifest_for_another_tree_is_rejected`
  (`policy/tests.rs:1054-1080`): self-consistent binary+manifest for CLOSURE_A against a fixture
  whose wanted closure differs; asserts loud `Err` naming both digests. Revert experiment
  (described): delete the `manifest.closure != wanted` early-`Err` block at `policy.rs:1496-1502`;
  the test's `must_fail` then panics with "manifest for another tree: expected an error"
  (render would return `Ok(None)` after the digest/self-report gates skip the foreign binary).
  Non-vacuous: asserts `Err` + two digest substrings, so both silent-skip and silent-accept
  regressions fail it.
- T3 `velnor_provisioner_reuses_the_slot_only_on_manifest_digest_match` (`lib.rs:7296-7361`):
  asserts manifest-first download+attestation, expected-digest-from-manifest, both-toolchain
  slot recompute, the `|| "$existing" != "$expected"` AND-gate, attest<gate<install order, and
  slow-path mismatch. Revert experiment (described): restore the old self-report-only gate
  (`if [[ "$reported" != "$closure" ]]; then`) in the `lib.rs:4222` template; the test fails on
  the AND-gate `contains` assertion AND on the negative assertion forbidding the old gate shape.
  Non-vacuous: it pins the exact gate that closed the planted-binary hole (doc `lib.rs:4190-4218`).

### Performed revert experiment (T1, /tmp only)

1. `tar --exclude=./target` copy of the repo to `/tmp/audit-revert` (original untouched).
2. Break: `policy.rs:1528` `if digest != bound.binary_sha256 {` →
   `if false && digest != bound.binary_sha256 {` (digest gate disabled, self-report tripwire intact).
3. `cargo test -p velnor-workflow --lib policy::tests::lying_binary_whose_digest_misses_the_manifest_is_rejected`
   → FAILED as required:
   `test policy::tests::lying_binary_whose_digest_misses_the_manifest_is_rejected ... FAILED`,
   `panicked at crates/velnor-workflow/src/policy/tests.rs:1037:5: a binary whose digest misses
   the manifest is not the candidate` (the lying binary was accepted as the candidate: `Some`
   instead of `None`). Proves the test fails iff the digest gate is removed (remaining tripwire
   passes since the liar echoes the wanted closure — exactly the attack the gate exists for).
4. Restored the line in `/tmp/audit-revert`, re-ran → `ok. 1 passed`.
