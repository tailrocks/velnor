# F1 release-reconciliation PREP (jackin, read-only design)

Date: 2026-09-17. Status: PREP ONLY — F gate not reached, zero writes to
`jackin-project/jackin`. All source facts below via live-`main` `gh api` reads
(SHA `0be3fcf9`) unless noted.

Authority: spec §9.1 ("The obsolete desktop `release.yml` assertion is
reconciled with generator-owned disabled/enabled release policy; legacy YAML
is not restored and releases are not enabled merely to pass it");
`/tmp/a0-consumers.md` §2.4; `/tmp/a0-runs.md` run-3.

## 1. Failing assertion (live main, verbatim shape)

File: `crates/jackin-xtask/src/desktop/tests.rs`, lines 223–245
(`repo_text` helper lines 154–160):

```rust
#[test]
fn release_workflow_invokes_canonical_mise_tasks() {
    let release = repo_text(".github/workflows/release.yml");
    for task in [
        "mise run desktop-build",
        "mise run desktop-verify",
        "mise run desktop-sign-notarize",
        "mise run desktop-release-state",
    ] {
        assert!(release.contains(task), "release.yml must invoke `{task}`");
    }
    for restated in [
        "cargo xtask desktop build",
        "cargo xtask desktop verify",
        "cargo xtask desktop sign-notarize",
        "cargo xtask desktop release-state",
    ] {
        assert!(
            !release.contains(restated),
            "release.yml must not restate `{restated}` beside the mise task"
        );
    }
}
```

Panic path: `repo_text` does `read_to_string(...).unwrap_or_else(panic!("reading …"))`
at line 158–159 — exactly run-3's `tests.rs:159:33 … release.yml: No such file
or directory` (1 of 303 xtask tests fails, exit 100; Policy + Planning both
`success` in that run, so this is purely a source-side stale assertion).

Why the file is gone (not an accident): PR #992 ("Clean-room velnor-workflow
regeneration (Class C omission)") deliberately deleted `preview.yml` and
`release.yml` (velnor-actions package-signer Class C omission). `release.yml`
is 404 at both audited (`92f347ac`) and live (`0be3fcf9`) SHAs. Restoring it
by hand would contradict the generator-owns-workflows rule (spec: every
`.github/workflows` file is generator-owned) and the deletion's reviewed
rationale.

## 2. Generator-owned disabled-release policy (the authority the test must bow to)

- `.github/ci/project.toml` lines 20–22 (byte-identical audited → live):
  `[release] enabled = false`, reason = "Release is fail-closed. Enable only
  after declaring immutable artifact, registry, provenance, and tag-protection
  policy."
- Spec §9.1 + §9.2: application release-disabled policy stays unchanged;
  never enable releases merely to pass the assertion.

## 3. Source-side release toolchain (intact — only the workflow consumer is gone)

| Module | Size | Role |
| --- | --- | --- |
| `desktop/release_state.rs` | 197 ln | `release-state`: independent publication state, `KEY=value` lines for `GITHUB_OUTPUT` |
| `desktop/sign_notarize.rs` | 332 ln | `sign-notarize`: Developer ID sign + notarize + staple |
| `release_archive.rs` | 380 ln | multi-target release archive production |
| `release_verify.rs` | 186 ln | archive verify: sha256 + attestation + cosign bundle |
| `mise.toml` | — | all four canonical tasks still defined (`desktop-build` L175, `desktop-verify` L183, `desktop-sign-notarize` L331, `desktop-release-state` L348), each delegating to the matching `cargo xtask desktop …` subcommand |

No other source file references `release.yml` (grep over all fetched
modules + `mise.toml`: only `tests.rs`). Available in-test deps: `toml`
(main deps, usable from `#[cfg(test)]` code), `serde_yaml_ng`; sibling tests
already assert on generated-tree content (`generated_ci_delegates_the_native_lane`)
and on the mise task graph (`cadence_tasks_define_the_canonical_graph`).

## 4. Designed fix (source-side, lands at F1 — NOT before F)

Rewrite `release_workflow_invokes_canonical_mise_tasks` as a **policy-driven**
test whose authority is the generator-owned `[release]` flag, keeping the
same test name (preserves run-3 signature continuity) and the canonical-task
intent:

1. **Read policy**: parse `.github/ci/project.toml` `[release].enabled`
   (use the existing `toml` dep; fail the test if the key is missing —
   fail-closed, never default-to-enabled).
2. **Disabled branch (current, must pass at live main)**:
   - assert `.github/workflows/release.yml` does NOT exist (fail-closed:
     nothing hand-restores a generator-owned path while policy says disabled);
   - positive anti-rot pin (preserves the test's original intent without a
     workflow): assert `mise.toml` still defines all four canonical tasks
     (`desktop-build`, `desktop-verify`, `desktop-sign-notarize`,
     `desktop-release-state`) and each task body delegates to its matching
     `cargo xtask desktop …` subcommand. Reuse the existing
     `task_block`/`assert_subsequence` helpers.
3. **Enabled branch (future, currently dead code path)**:
   - assert `release.yml` exists AND invokes the four canonical mise tasks in
     order AND does not restate raw `cargo xtask desktop …` commands (i.e. the
     current test body, kept verbatim as the enabled leg).

Pseudocode:

```rust
#[test]
fn release_workflow_invokes_canonical_mise_tasks() {
    let project = repo_text(".github/ci/project.toml");
    let enabled = project
        .parse::<toml::Table>()
        .expect("project.toml parses")
        .get("release")
        .and_then(|r| r.get("enabled"))
        .and_then(toml::Value::as_bool)
        .expect("[release].enabled must be explicit (fail-closed)");
    let mise = repo_text("mise.toml");
    // Canonical toolchain pinned in BOTH branches: the four tasks exist and
    // delegate 1:1 to `cargo xtask desktop …`.
    for (task, xtask) in [("desktop-build", "cargo xtask desktop build"), …] {
        assert!(task_block(&mise, task).contains(xtask), …);
    }
    let workflow = manifest_dir().join("../../.github/workflows/release.yml");
    if enabled {
        let release = repo_text(".github/workflows/release.yml");
        // … current body verbatim (four `mise run …` present, four restatements absent)
    } else {
        assert!(!workflow.exists(), "release disabled: no hand-restored release.yml");
    }
}
```

## 5. Explicit non-goals (forbidden alternatives)

- NO restoring `release.yml` (legacy YAML; contradicts #992 + generator ownership).
- NO setting `[release] enabled = true` (release-enabling to pass a test).
- NO deleting the test outright (loses the canonical-task pinning; the
  toolchain would rot silently while disabled).
- NO touching `preview.yml` (also deleted in #992; out of §9.1 scope).
- NO workflow regeneration needed for this change (source-side only).

## 6. F1 verification (at gate, not now)

- `cargo nextest run --locked --all-features --package jackin-xtask` (or the
  repo's `mbx nextest` wrapper): all 303 tests pass, including the rewritten
  test's disabled branch.
- Negative check: temporarily creating a stub `release.yml` must fail the
  disabled branch (proves the absence assertion is live, not vacuous).
- No `velnor-workflow` regen / no Policy impact: change touches only
  `crates/jackin-xtask/src/desktop/tests.rs` in the jackin repo.
