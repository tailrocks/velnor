# F1 Swift-routing PREP (bastion campaign, READ-ONLY — F gate not reached)

Collected: 2026-09-17 (UTC). No repo writes; live reads via `gh api` on
`jackin-project/jackin` main + local generator source in velnor3.

## 1. Inputs

### Spec §9.1 (plans/bastion-three-provider-ci/spec.md:319-329)

- 40-unit baseline: 38 Linux-oriented units → 114 three-provider executions,
  plus **2 genuine hosted macOS units**. "120 Linux executions are never
  counted by pretending Swift runs on Linux."
- "Generic native routing to actual Apple Silicon macOS is fixed. Same-job
  local XCFramework production followed by Swift build/test is preserved."
- Historic tools re-resolved; "Apple-only mise tools are never installed
  on Linux."
- Generator/Velnor fix first, then Jackin continues; no per-Jackin host arch.

### Run-3 (/tmp/a0-runs.md, jackin `35114867283`, push main, attempt 1)

Policy + Planning both `success`; 3 unit failures (all reproduce verbatim):

| unit | step | signature | exit |
|---|---|---|---|
| `swift-package-native` | `mise run desktop-xcframework` → `cargo xtask desktop xcframework` | `error: desktop xcframework requires macOS (Apple Silicon)` | 1 |
| `swift-package-native-design-prototypes-unifiedagentusage` | `swift build` | `bash: line 1: swift: command not found` | 127 |
| `rust-jackin-xtask` | nextest `desktop::tests::release_workflow_invokes_canonical_mise_tasks` | reads deleted `.github/workflows/release.yml`, panic at `desktop/tests.rs:159:33` | 100 |

(xtask failure is the separate `release.yml`-assertion reconciliation; not this fix.)

## 2. Live jackin main evidence (default_branch `main`, pushed 2026-09-16)

### Swift unit definitions

`.github-gen/velnor-workflow.toml` (50 lines total) declares:

```toml
[scan]
exclude = ["native/Package.swift", "native/.swiftpm/**"]

[workflow]
runners = "github"
automatic = "github"
default_dispatch_runner = "github"
automatic_lanes = "github"
github_runner = "ubuntu-26.04"
macos_runner = "macos-26"            # <-- declared but unused by swift jobs
velnor_labels = ["self-hosted", "velnor-target-mvp"]

[[units]]
id = "swift-package-native"
kind = "swift"
root = "native"
watch = ["native/**/*.swift", "native/Package.swift", "native/Package.resolved",
         "native/.swiftpm/Package.resolved", "crates/jackin-xtask/**"]
ci_tasks = ["swift-package-native-ci"]
mise_tools = ["cargo-binstall", "rust", "cargo:sccache", "cargo:boltffi_cli"]
```

Comment in file: "XCFramework must exist before `swift build` resolves the
binary target. Run desktop-xcframework in the same macOS job as
swift-package-native so the binary target path is populated on one runner
(separate jobs do not share target/xcframework)."

Second unit `swift-package-native-design-prototypes-unifiedagentusage` is
scan-derived (`swift-package:native/Design/Prototypes/UnifiedAgentUsage` in
project.toml `[analysis].detected`), root
`native/Design/Prototypes/UnifiedAgentUsage`, no `[[units]]` entry.

Generated `.github/ci/project.toml`:

- `swift-package-native-design-prototypes-unifiedagentusage`: kind `swift`,
  `github_pr/full_commands = ["cd -- '…' && swift build",
  "cd -- '…' && swift test --parallel"]` (same 4 lane arrays).
- `swift-package-native`: kind `swift`,
  all 4 command arrays = `["mise run swift-package-native-ci"]`.

### XCFramework production + Swift build/test chain (same job, must preserve)

`mise.toml` on main:

```toml
[tasks.desktop-xcframework]
description = "Build the arm64 static JackinUsageFFI XCFramework"
run = "cargo xtask desktop xcframework"

[tasks.swift-package-native-ci]
description = "CI gate for native SwiftPM package (xcframework then build/test)"
run = '''
set -euo pipefail
mise run desktop-xcframework
cd native && swift build && swift test --parallel
'''
```

`crates/jackin-xtask/src/desktop.rs:538-544`:

```rust
pub(super) fn require_macos(action: &str) -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        bail!("{action} requires macOS (Apple Silicon)")
    }
}
```

`cfg!(target_os)` is a compile-time check of the xtask runner host → the
whole `swift-package-native-ci` chain (xcframework + `swift build` +
`swift test`) must execute on one macOS host. Splitting across jobs is
wrong (no shared `target/xcframework`, comment above). No command changes
needed — only placement.

### Current runner labels (why Ubuntu runs them)

Live `.github/workflows/ci-unit-swift.yml` (generated): single job
`verify-github`, `runs-on: ubuntu-26.04`. Both ci-main.yml callers
(`github-swift-package-native`, `github-swift-package-native-design-…`)
`uses: ./.github/workflows/ci-unit-swift.yml` with `lane: github`, so both
Swift units land on Ubuntu: xtask bails (exit 1), `swift` missing (exit 127).
`macos_runner = "macos-26"` in the TOML reaches nothing in this file.

## 3. Root cause in the generator (velnor3, exact lines)

The generator already knows Swift is Apple-only — three places agree:

- `crates/velnor-workflow/src/primitives/ir.rs:5451-5454` `runner_for_unit`:
  `Swift + Github → yaml_scalar(&self.macos_runner)`.
- `crates/velnor-workflow/src/lib.rs:4269-4271` `lane_supports_unit_kind`:
  Velnor lane cannot run Swift (so no `verify-velnor` job is emitted for the
  kind — matches live file).
- `crates/velnor-workflow/src/lib.rs:5510-5544` `report_unit_runners`:
  reports `github: <macos_runner>` for Swift units. The emitted workflow
  contradicts the generator's own report.

But the collapsed reusable-workflow path bypasses `runner_for_unit`:

- `render_kind_unit_workflow` (ir.rs:3842-3857) filters members by
  `unit.kind == kind` → all members share one kind — then calls
- `render_collapsed_kind_verify_job` (ir.rs:3924), whose Github lane uses
  `self.runner_for(RunnerMode::Github)` (ir.rs:3936) → always
  `github_runner` (ubuntu). The Swift→macos carve-out never fires.
- Contrast: the trusted-Velnor lane in the same function already does the
  right thing — `runner_for_unit(RunnerMode::Velnor, sample)` with
  `sample = velnor_trusted[0]` (ir.rs:3963-3965). And the non-collapsed
  per-unit surface uses `runner_for_unit(lane, unit)` at ir.rs:4702
  (covered by test `macos_runner_label_overrides_the_apple_lane`,
  lib.rs:8864, which is why that test passes while live output is wrong).

So: **one call site** — ir.rs:3936 — routes the shared `verify-github` job
of `ci-unit-swift.yml` to Ubuntu.

## 4. Generator fix design (F1 execution, after gate)

1. In `render_collapsed_kind_verify_job` (ir.rs:3933-3946), mirror the
   trusted-Velnor pattern:
   ```rust
   if !github_members.is_empty() {
       let sample = github_members[0];
       let runs_on = self.runner_for_unit(RunnerMode::Github, sample);
       ...
   }
   ```
   Sound because `render_kind_unit_workflow` guarantees single-kind
   membership; for `ci-unit-swift.yml` the sample is always Swift →
   `runs-on: macos-26`. Non-Swift kinds resolve identically to today
   (`runner_for_unit` falls through to `runner_for` for non-Swift).
2. No new config surface: `[workflow] macos_runner` already exists,
   validated non-empty (config/mod.rs:1119), plumbed through scan
   (scan/mod.rs:214, default `macos-15`) and generation
   (lib.rs:1784-1786). Jackin already declares `macos-26`. Default stays.
3. No command changes: `mise run swift-package-native-ci` keeps running
   xcframework → `swift build` → `swift test` in one job, now on a
   macOS-26 (Apple Silicon) runner. Prototype unit's raw `swift build/test`
   likewise lands on macOS via the same shared `verify-github` job.
4. Nothing else moves: `render_unit_runtime` already special-cases Swift
   to source-build the runtime (ir.rs:4926-4932, "Apple jobs cannot consume
   a Linux-built plan artifact"); actionlint allowlist already includes
   `macos_runner` when Swift units exist (lib.rs:5174-5181); cache keys use
   `runner.os`/`runner.arch` so macOS-ARM keys separate automatically;
   mise-action + the declared `mise_tools` (cargo-binstall, rust,
   cargo:sccache, cargo:boltffi_cli) install on macOS; Policy
   trusted-runners/action-pins unaffected (hosted label, no new actions).
5. Regenerate Jackin at the fixed pin: expect `ci-unit-swift.yml`
   `verify-github` → `runs-on: macos-26`, everything else identical;
   Policy `generated-tree` then passes against the new pin (F1 owns the
   pin bump + regen; the `release.yml`-assertion reconciliation in
   `rust-jackin-xtask` is separate work per §9.1 — legacy YAML not
   restored, releases not enabled merely to pass it).

### Tests to add with the fix

- Collapsed-path Apple routing test (mirror of
  `macos_runner_label_overrides_the_apple_lane` but through
  `render_kind_unit_workflow(UnitKind::Swift)`): `verify-github` block
  contains `runs-on: macos-26` (custom) / `macos-15` (default), and no
  `verify-velnor` job. Suggested home: `crates/velnor-workflow/tests/lane_pairing.rs`
  next to `both_allows_swift_with_explicit_github_jobs` (which asserts the
  swift collapsed job's structure but not its `runs-on` — no existing test
  pins the buggy Ubuntu label; verified by search).
- Non-Swift regression: collapsed rust/bun/docker kind files still emit
  `runs-on: <github_runner>` (existing tests cover shape; assert label).

## 5. §9.1 count check after fix

38 Linux units × 3 providers = 114 executions; 2 Swift units run as genuine
hosted macOS (`macos-26`) executions, never counted as Linux. `docker-e2e`
with `e2e` enabled, capsule-from-source, and tool re-resolution are outside
this routing fix.

## 6. Explicit non-goals

- No `release.yml` restoration, no release enabling (separate §9.1 clause).
- No per-Jackin host architecture, no new runner labels, no Velnor-lane
  Swift (Velnor has no macOS fleet; `lane_supports_unit_kind` stays).
- No writes made by this prep.
