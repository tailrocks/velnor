Audit complete. Read-only held (renders + probes ran from `/tmp/mx/*`; no repo files written). Hint verdicts first, then findings.

**Hint "tag shapes": partly confirmed.** Odd tags fail closed (`verify-tag` refuses `v9beta/vbeta/v1/v1.2.3/x`, probed), but moved-tag TOCTOU is real on two lanes (M-4). **Hint "curl retry": confirmed as uniformity gap, refuted as safety hole** — every site fails closed via `--fail` + sha256 check (M-5).

# Findings

**M-1 (medium) — Feature-branch dispatch rehearsal builds nothing.**
`release.rs:3374` (`trusted_release_runner_gate`: dispatch arm requires `ref==main`), applied to stable build (`release.rs:2682`) and tarball preview build (`release.rs:2232`); native preview identity/publish use ref-only gates (`release.rs:1602`, `release.rs:1681`).
Input: config with `modes=[validate,build,rehearse]` (fixture `tests/fixtures/release-bindings/velnor-workflow.toml`), event `workflow_dispatch` on `refs/heads/feature`, input `rehearse`. Rendered bytes (`/tmp/mx/base-out`, release.yml:173, preview.yml:123): build `if:` is false → build skipped → publish skipped. Only CI unit jobs run; no release build/package/attest happens, run goes green.
Required: rehearsal finishes declared work on the feature branch (`runtime.rs:3598-3603` doc; test docstring `release_modes_gate_tag_check_and_publish` "build must run the declared work in every mode"). Rehearsal only works from main.
Coverage: **uncovered** — adjacent tests pass vacuously (`release_modes_gate_tag_check_and_publish` checks absence of `outputs.mode`; `preview_dispatch_modes_resolve_without_waiting` checks absence of `sleep`).

**M-2 (medium) — Native identity lane without modes publishes the OCI version index on dispatch-on-tag.**
`release.rs:3239` renders dispatch with no mode gate; image-platform gate is only `existing != 'true'` (`release.rs:1345-1355`); image job has no mode gate without modes (`release.rs:1410`); neither transitively needs the gated `build` job (metadata needs `[verify]` only — render bytes line 313).
Input: `kind=native` + consumer + `release-build` feature + `image`, no modes (my `/tmp/mx/natimg` variant; same shape as this repo's own `.github/workflows/release.yml`), `gh workflow run release.yml --ref v1.2.3` (API accepts tag refs). Render bytes: image-platform (line 451) and image (line 623) have no event gate → staging pushes + `imagetools create --tag VERSION` run; GitHub Release publish is skipped (needs gated `build`).
Observed: OCI version index published on dispatch, orphaned (no Release); the later tag-push run's admission then refuses to adopt it without a recovery digest (`release.rs:1329` block), which tag-push runs cannot supply → release flow bricked. Required: publish never on dispatch (`runtime.rs:3687`, `lib.rs:928`).
Coverage: **uncovered**.

**M-3 (medium) — Native preview without producer publishes on dispatch-to-main (missing event check).**
`release.rs:1681`: publish `if: github.ref == 'refs/heads/main'` — no `event_name` check; identity job same (`release.rs:1602`). Tarball preview has the check (`release.rs:2189`: `push && ref==main`).
Input: `kind=native` + consumer + `release-build`, no producer/modes (`/tmp/mx/nomode-natprev`); dispatch on `main`. Render bytes (preview.yml:472): publish runs → `gh release delete/create preview` replaces the rolling release. With a producer bound, the same dispatch can never publish (mode gate) — shapes contradict.
Coverage: **uncovered** (`native_preview_produces_debs_with_rolling_identity` asserts `needs:`/assets, never the `if:`).

**M-4 (medium) — Binary + native-non-debian publish lacks tag-immutability re-verification (moved-tag TOCTOU).**
`release.rs:2662`: `gh release create "${{ ref_name }}" dist/* --verify-tag` — and `gh` documents `--verify-tag` as existence-only ("abort if the tag doesn't already exist", verified via `gh release create --help`). Native-debian re-checks via ls-remote + commit compare (`release.rs:1573` block, "Verify release tag stayed immutable", render bytes line 896).
Input: push `v1.2.3` at main tip; tag force-moved (or maintainer re-cuts it) between verify and publish. Observed: release binds the moved tag while artifacts were built from the event SHA — silent release↔commit mismatch on immutable releases. Required: refuse like the debian lane. Accidental trigger needs no attacker.
Coverage: **uncovered** for these lanes (required behavior pinned for debian lane only, by `native_identity_publish_assembles_and_binds_the_release_record`).

**M-5 (low) — curl retry/time-bound non-uniformity across rendered scripts.**
`lib.rs:5131` (mold, rendered ci-unit-rust.yml:293): `--retry 3 --retry-delay 2`, no `--max-time`/`--connect-timeout` — a stalled connection hangs to job timeout. `runtime.rs:3381` (guest kernel): `--connect-timeout 30 --max-time 900`, no `--retry` — single attempt vs Firecracker's `--retry 20` (`release.rs:723`). Both fail closed (`--fail` + strict sha check), so robustness-only.
Coverage: **uncovered** (no test asserts curl flags).

**M-6 (low) — `resolve-mode` workflow_run arm admits any producer name (latent).**
`runtime.rs:3697-3710`: requires non-empty producer + `success`, never compares against the trusted producer. Probed: `--event workflow_run --producer EVIL --conclusion success` → `publish`. Currently unreachable from renders (rolling gate calls `admit-producer`, `release.rs:1966`; stable has no `workflow_run` trigger), but violates the function's own "total matrix / refuses untrusted" contract — a trap for any future caller.
Coverage: **uncovered** (`resolve_mode_refuses_untrusted_publish` covers empty-producer + failure only; name check tested only for `admit-producer`).

# Correct samples (cell + evidence)
1. push `refs/tags/v1.2.3` → `publish` + verify-tag prints `1.2.3` (binary probes).
2. push `v9beta/vbeta/v1/v1.2.3/x/1.2.3/V1.2.3` → verify-tag refuses "semver v* tag" (binary probes).
3. dispatch + `input=publish` → refused "never dispatched" even if choice bypassed (probe; `resolve_mode_refuses_untrusted_publish`).
4. PR/`pull_request_target` + publish → refused; PR otherwise → `validate` (probe; same test).
5. crates publisher: `tags: ["v*"]` only, no dispatch surface (render bytes).
6. binary+modes: mode resolver + tag-check/publish gated on `mode==publish` (render bytes; `release_modes_gate_tag_check_and_publish`).
7. rolling gate: workflow_run→admit-producer, drill resolve, no wait loop (render bytes; `preview_dispatch_modes_resolve_without_waiting`).
8. `workflow_run` trigger: `workflows:[CI], types:[completed], branches:[main]` (render bytes; `preview_producer_binding_renders_source_gate_and_wired_publish`).
9. admit-producer: exact, case-sensitive name + success (probed `ci`≠`CI`; `admit_producer_refuses_name_and_conclusion_mismatch`).
10. resolve-source: workflow_run→producer head SHA, 40-hex enforced (probed; `resolve_source_binds_the_producer_revision`).
11. docs PR render: deploy excludes PR/schedule; verify-deployed needs deploy success (render bytes; `pull_request_runs_never_publish_or_satisfy_trusted_reuse`).
12. scheduled checks: schedule+dispatch, `contents:read`, checks-only jobs incl. velnor lane (render bytes).
13. docker dispatch-on-tag: deliberate recovery path, exact-digest-gated adoption (render bytes + `release.rs:3038`).
14. producer-outcome fetch: `--retry 0` + bounded Rust loop + `--max-time 30` (source `runtime.rs:866`).
15. sitemap verify: 5× shell loop + `--max-time 30`, fail-closed (render bytes, docs.yml:320).

# Coverage statement
Drove 8 fixture renders (rust-binary ±bindings, native ±debian ±image, docker, pages, crates, docs, scheduled, velnor-provider) + 25 runtime probes through the current-tree binary. Did not examine: homebrew/apt feed contents beyond trigger shape; maintenance.yml's `pull_request:closed` + `actions:write` (out of matrix); `policy.rs` validator internals; prepared-tool install flow; guest-payload internals; live GitHub behavior. Note: tree is mid-flight by siblings — `cargo test -p velnor-workflow --lib` shows 695 pass + 1 pre-existing failure (`checked_in_workflows_match_the_generator_byte_for_byte`: checked-in release.yml drifted), unrelated to this audit; all cited line numbers re-verified after the final build.