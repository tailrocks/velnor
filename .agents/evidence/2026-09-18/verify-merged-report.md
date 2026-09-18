# Verify-merged-slices report (READ-ONLY)

Worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor2` (dirty, untouched — no file modified).
Scope: slices 1–2, G, F, D, E, H8a as merged in the working tree vs HEAD `33688938`.
Method: gate re-runs, full read of new modules, validator-by-validator adversarial review,
byte-level diff of all render-string changes, pin-ledger reconciliation. No fixes, no commits.

## Gate outputs (all green)

- `cargo test -p velnor-workflow`: **all pass** — lib 561 + integration suites
  1, 5, 2, 6, 5, 4, 5, 10, 33 (12 `test result: ok` lines, 0 failed).
- `cargo clippy --all-targets` (workspace): exit 0, no warnings.
- `cargo fmt --all -- --check`: exit 0.
- `generic_surface_literals`: 2/2 pass (law holds over new code + new fixtures).

## Per-slice verdicts

### Slice 1 (Swift/macOS collapsed routing) — SOUND
- `ir.rs:3984-4031`: collapsed jobs sample `runner_for_unit(lane, members[0])`.
  Homogeneity is structural: `render_kind_unit_workflow` filters by kind (`ir.rs:3901`),
  and `runner_for_unit` on Github depends only on kind (`ir.rs:5541-55`).
  Rust/Github and Velnor-plain render byte-identical to before; velnor-trusted
  already used `runner_for_unit` (pre-existing). `[0]` indexing guarded by `is_empty`.
  Test `collapsed_swift_kind_lands_on_macos_while_rust_stays_on_linux` + e2e
  `migration_contract::swift_kind_renders_macos_while_rust_stays_linux` pin it.

### Slice 2 (FFI→Swift selection) — SOUND
- No generator change (correct per ledger F2): new unit test
  `runtime.rs:1158 affected_closure_follows_cross_kind_depends_on_edges` (absent at HEAD)
  + e2e `migration_contract::ffi_change_selects_the_swift_consumer` (real git history,
  asserts `swift_matrix` non-empty). Fixture `swift-ffi-consumer` is neutral
  (`example/*`, `packages/`, `clients/apple/`), exercises `lanes="both"` too.

### Slice G (check_profiles) — SOUND
- New `primitives/check_profiles.rs` (748 lines) + `scheduled_check_profiles.rs` e2e.
- Validators (`config/mod.rs:2011-54, 2315-2488`) close every render sink:
  id = GitHub-job-id charset (bare `needs: [..]` + job key safe), tasks = no-whitespace/
  no-shell charset with non-flag first byte (`mise run {task}` safe), env keys =
  shell identifiers, tools = `valid_mise_tool_id` + mise.lock membership + dupes,
  artifacts = single-line (block-scalar safe), timeout ≥ 1 and u32, needs =
  membership + within-file + acyclic (cycle DFS is correct), status allowlist,
  exactly-once file coverage (`primitives/mod.rs:1334-64`, dup + uncovered both refuse).
- Notes (not issues): timeout has no upper bound (GitHub caps ~35790; oversize fails
  at runtime, not generation); artifact upload sets no `retention-days` (default 90d).

### Slice F (docs_site) — ISSUE (2, one family)
**F-1 (high): result-reuse accepts by artifact name+expiry alone; required check goes
green without running checks.** `docs_site.rs:227-273` (`render_gate_job`):
a non-expired artifact named `docs-result-v1-<digest>` sets `hit=true` and every
local job (`source-links`, `spell`, `site`) skips — the artifact is never downloaded
or content-verified. The digest covers consumer `docs_paths` ONLY: not build/check
commands, not toolchain, not generator/pipeline version. Consequences:
(a) change a check command with identical docs inputs → checks skipped, green;
(b) `publish_guard` (`docs_site.rs:186-190`) lets **same-repo PRs publish** reuse
artifacts, so a PR can publish a result that makes a later default-branch push skip
`source-links`/`spell` (their `if:` has no deploy-gate exception, `:288-289`).
This is exactly what goal §C forbids ("never green from artifact name/existence
alone") and what ledger F3 says Velnor must not do. Mitigating: reuse self-limits
(hit runs don't republish, 7-day retention). Assumes `docs-required` is a required check.
**F-2 (medium): stale-site deploy via the same hole.** `render_site_job` (`:349-400`):
on a default-branch push with a lookup hit, build steps skip but the restored site is
still staged and deployed. A command-only change (or a same-repo-PR-published site
artifact) deploys bytes the current contract never built — a branch-protection bypass
shape for Pages. Trigger: PR weakens `build_commands`, its run publishes
`docs-site-v1-<digest>`; default push with same docs inputs deploys it unmerged.
- Rest of F is SOUND: URL/cron/command/path/glob/stage validators tight
  (`config:1910-2006, 2227-80`); `bash_escape` correct order; `hash_files_call`
  quote-doubling defeats expression breakout (attacker `'` always stays in-string);
  retry ladder (3 attempts + fail-closed gate), verify curl loop, fork-exclusion in
  publish guard all correct. (Cron charset is 5-field-count only, but values render
  via `yaml_scalar` where newlines fold, quotes escape — contained, no breakout.)

### Slice D (docker publisher) — ISSUE (1, pre-existing class extended) + pin LEGITIMATE
**D-1 (medium-low): `release.image` reaches YAML unquoted at 3 NEW sites.**
`release.rs:3023, 3041, 3099`: `GHCR_IMAGE: {image}` with `image = release.image`
raw (no `yaml_scalar`), while every neighboring interpolation is quoted. `image` is
validated only non-empty (both entries). Trigger: `image = "x\n      INJECTED: 1"`
→ injected env key / workflow corruption at generation-success. (Shell uses of
`${GHCR_IMAGE}` are safely double-quoted; `docker_cache_scope` sanitizes.)
Pre-existing: HEAD has the same flaw at 4 native-lane sites; D copies it into the
new publisher instead of fixing the class.
- Rest of D is SOUND: platform allowlist enforced at both entries
  (`valid_docker_platforms` + `release_contract_complete`); `docker_platform_arches`
  silently `continue`s unknown platforms but is unreachable post-validation;
  `verify-digests` strict (exact file set, 4 KiB, single token, `sha256:` + 64
  lowercase hex); admission reconcile (absent/resume-with-explicit-digest/conflict)
  + per-ref concurrency; single manifest publisher; SBOM+provenance; digest-format
  strictness varies by layer (platform job strict, shell `case` arms accept
  uppercase — cosmetic inconsistency).
- **Render-pin `920ae6e4` LEGITIMATE.** Only the native-identity `release.yml` pin
  moved (`f0e44f87`→`920ae6e4`); `preview.yml` pin `b2e2a977` unchanged. Exact
  string-diff of the native manifest render shows PURE INSERTS: `{mode_gate}`
  (E, renders empty when unbound) + the platform-set jq check (D). Self-surface
  regen is exactly +8 lines (id-token/attestations perms, provenance/sbom,
  platform-set check) + state hashes. The `config` state-hash move (`951b…`→`1f88…`)
  is schema evolution, not content drift: new `docs: DocsSection` has
  `#[serde(default)]` without `skip_serializing_if`, so canonical JSON (FNV input)
  changed shape with the same file. Policy stays consistent (same revision renders
  and checks).

### Slice E (release bindings/modes) — ISSUE (2) + byte-identity VERIFIED
**E-1 (medium): `binary`/`targets` interpolated raw into shell in 2 NEW sites.**
`release.rs:2318-29` (preview) and `:2758-67` (stable) build
`assemble-manifest --subjects "<binary>-…-<target>.tar.gz"` with zero quoting or
generation-time charset validation (config: non-empty only; targets: suffix check
only). Trigger: `targets = ["x-unknown-linux-gnu\"; touch /tmp/pwned; echo \"-unknown-linux-gnu"]`
(suffix ✓) → arbitrary command in the publish job (`contents: write`). Runtime
validation (`valid_subject_name`) can't help — the shell parses first. This extends
a pre-existing class (unquoted `cargo build --package/--bin`, `cp` globs) that E
should have closed; the codebase's own convention (`shell_quote` for
`manifest_schema`, `verify-tag --branch/--package`) shows the intended bar, and
runtime already has the exact charsets (`valid_package/valid_binary/valid_target`,
`runtime.rs:3265-84`) generation should mirror. (Same-trust config input —
defense-in-depth/accident-guardrail severity, not a privilege boundary break.)
**E-2 (medium-low): tarball-only bindings on other kinds render a success-with-omission.**
`release.rs:2347-51`: modes/producer/archive/credentials on `docker|crates|pages|
homebrew|apt` render `# Release omitted: …` and generation SUCCEEDS. A typo'd
`modes` on a docker contract silently drops the whole publisher. `validate_release`
checks binding shapes but never kind-compatibility — this should be a usage error.
- Rest of E is SOUND: `resolve-mode` event×mode matrix correct incl. all refusals
  (PR/schedule→validate, dispatch-publish refused, tag→publish, rolling-default→publish,
  workflow_run needs admitted producer+success, unknown event/mode refused) with
  single-token stdout safe for `GITHUB_OUTPUT`; `admit-producer` exact-match;
  `resolve-source` 40-hex (allows uppercase hex — note vs lowercase-strict digests
  elsewhere); `assemble-manifest` strict (schema `/`, 40-hex commit, subject charset
  ⇒ no traversal, re-hash + re-verify corpus from disk); `package-binary` strict +
  `Command`-based (no shell), GNU-tar gate, member≠binary, `.`/`..` excluded;
  credential pairing (trap+unmount+`always()` restore) covers success/failure/cancel/
  timeout with sanitized function names; `publish` never a dispatch option;
  `declared_*` parity with config validation holds field-by-field (modes, conclusion,
  members, checksum, retention incl. clamp-then-validate ordering, which is safe
  because strict `validate()` runs mandatorily in `scan_target`, `lib.rs:1283`).
  `manifest_schema` unvalidated on both paths but `shell_quote`d + runtime-checked.
- Inject-surgery notes (structural, not issues): all `replacen` anchors exist in
  current renders and are covered by tests, but a future render change that drops an
  anchor fails OPEN (silent no-inject) rather than erroring — same for
  `inject_native_preview_bindings` source-rewiring.
- Test-strength note: `resolve_mode_*` tests assert `Ok`/`Err` but never capture
  stdout, so the actual printed mode token (the whole point) is unasserted.
- **Default-byte-identity claim VERIFIED.** All four rust-binary pins
  (`fe8814a3/3164356d/dec062b1/63e76d5e`) and the native-preview pin (`b2e2a977`)
  are unchanged from HEAD with the suite green; every E gate renders `""`/identical
  bytes when unbound (verified each: `:1279, :1348, :1393, :1550, :2713, :2789,
  :2806, :3231` + all `inject_*` no-ops + `inject_native_dispatch` else-branch
  byte-identical to HEAD's tail); the two moved native tests are md5-identical.

### Slice H8a (renovate lanes) — ISSUE (1, low-medium)
**H8a-1: `fromJSON('…')` single-quote breakout.** `renovate.rs`: `writer_runs_on`
`Both` arm renders `fromJSON('[…]')` with `json_string` escaping `"`, `\`, controls —
but NOT `'`. `velnor_labels` and `velnor_trusted_label` are validated non-empty only
(`config:1380-91, 1442-65`), while every other labels sink uses double-quote
`yaml_scalar`. Trigger: `velnor_labels = ["self-hosted", "o'brien"]` +
`lanes = "both"` → broken/injected `runs-on` expression. Fix: escape `'` (e.g.
`\u0027`) or switch to double-quoted JSON.
- Rest of H8a is SOUND: lanes allowlist + `both`+group rejection + trusted-label
  requirements (`config:2090-2135`); `both` expression defaults to Velnor, schedule
  stays Velnor, dispatch input exists only when declared (no escalation);
  velnor-only renders byte-identical (static labels, bare dispatch — verified);
  `secrets.{token}` safe via strict token-name validator.

## Genericity / NOS

- Law test passes over all new code and fixtures. All four new fixtures inspected:
  neutral `example/*` names, no consumer paths/pins/grants. New modules name only
  families, kinds, and generator-owned paths.
- NOS spot-check: zero hits for `jackin|holla|aggregate-needs|download-ci-xtask|
  download-codebook|sign-capsule|capsule|desktop-cadence|jackin-dev|cache-cleanup|
  reuse-compliance|lychee|codebook|boltffi|xcframework` across new modules, new
  tests, and new fixtures. No PR-shape copying found (docs reuse is name-keyed
  artifact lookup, a new weaker construction — see F-1, not a copy).

## Deny-list-derived genericity test soundness

The derived test is `migration_contract::generated_output_names_no_consumer` (F has no
own deny test; `docs_site_pipeline.rs` asserts `scripts/generate-docs.ts` absence only).
It `include_str!`s the law, parses `DENY_LIST` entries + the `.replace` rewrite, and
probes all generated bytes. Verdict: SOUND for its purpose, with minor fragility:
(a) line-based quote parser — a quoted string in a DENY_LIST-region comment would
become a bogus probe (fail-closed: none exist today); (b) couples to the FIRST
`.replace(` line in the law file (correct today, line 151); (c) plain-substring
matching is weaker than the law's normalized scan (would miss split-across-fragments
literals) but adequate for generated YAML output. It also usefully asserts the probe
list is non-empty (no silent vacuity).

## Exact unverified items

1. Live execution semantics (no runners/credentials in this read-only pass):
   `imagetools inspect --format '{{json .}}'` shape (`.manifest.digest`), docker
   `push-by-digest` `outputs.digest`, Pages deploy/verify round-trip, credential
   `trap EXIT` under SIGTERM-cancel, docs-reuse race outcomes, `imagetools create`
   with attestation-manifest filtering counts.
2. Ledger's Jackin re-render claim (one-line `macos-26` Swift diff) — Jackin repo
   not in scope; not re-run.
3. `resolve-mode` printed tokens — tests assert status, not stdout.
4. `docs-required` failure SKIP analysis relies on GitHub's documented step semantics
   (plain-`if` steps skip after a failed step); not executed.
5. `docs_paths` default (`**/*.md`, `mkdocs.yml`, `docs/**`) adequacy per consumer —
   consumer-declared override exists; semantic coverage is consumer-side.
