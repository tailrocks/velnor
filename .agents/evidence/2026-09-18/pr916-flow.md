# Generator-landing flow: --check semantics, #907 precedent, green sequence

Date: 2026-09-17. Scope: read-only flow investigation (no repo edits).
Inputs: /tmp/v-a2-split2.md, /tmp/pr916-red.md, code at main checkout,
measured runs with purpose-built binaries, `gh` CI history.

## (1) What `--plain --check` compares, and its exit codes

Two sequential stages in `run()` (`crates/velnor-workflow/src/lib.rs:4852-4900`).
`main.rs:5-10` maps `Ok(())` -> exit 0, any `Err` (printed as `error: ...`) -> exit 1.

**Stage 1 — current-binary render vs tree** (`write_generated_with_options`
-> `apply_generated_write_plan`, lib.rs:5856-5879; verdict builder
`generated_check_error`, lib.rs:5817-5850). The RUNNING binary renders the
tree in memory and diffs every generated file plus the ownership state.
Mismatch -> `error: generated files differ: <paths>; rerun generate`, exit 1.
Match-but-inputs-moved -> `error: generated files match but generation
inputs changed: ...`, exit 1. Match -> `WriteOutcome::Unchanged`, continue.

**Stage 2 — D19 pin guard** (`policy::verify_declared_pin_renders_tree`,
policy.rs:544-589, via `regenerate_and_compare`, policy.rs:1472-1508).
Resolves a renderer proving the DECLARED pin (`resolve_pinned_binary`,
policy.rs:1185-1241: self if `SOURCE_CLOSURE` in `expected_closures` =
release+debug+candidate closures of the pin tree, else
`$VELNOR_WORKFLOW_PINNED_BINARY`, else PATH, else install-root cache, else
fail-closed unless `--pin-build`), renders to scratch, byte-compares
(`render_and_compare`, policy.rs:1599-1682, state file included). Pin render
matches -> `Ok(())`, silent. Else the candidate exception
(`render_with_candidate`, policy.rs:1521-1597) tries `[env-slot?, current_exe]`
(the running binary is manifest-exempt; others need digest+manifest binding)
for a binary reporting `wanted = candidate_closure_of_tree(HEAD)` whose render
matches -> `Ok(())` + stderr `notice: the tree matches the candidate render
(<closure>), not the render of the declared pin <pin>; ... after merge`.
Neither matches -> `error: the declared generator pin <pin> renders the tree
differently; ...`, exit 1.

**Measured with the 1bee4f23 binary** (built in scratch worktree @1bee4f23;
`--revision`=1bee4f23, `--closure`=7dcabc83ea2c2a6b...):

| tree shape | `--plain --check` | `--plain --dry-run` |
|---|---|---|
| A: 1bee4f23 as-is (old pin 7341ef4b + NEW render) | exit 0 + candidate notice (7dcabc83...) | exit 0, "0 files would change" |
| B: source-only old-render (same HEAD, 2 regen files reverted to 33688938 base) | exit 1: `generated files differ: .github/workflows/ci-runtime-products.yml, .github/ci/.github-actions-generator-state` | exit 0, "2 files would change" |

Notes: (a) shape-A exit 0 needed a cached pin renderer
(`$TMPDIR/velnor-workflow-policy-7341ef4b.../bin/velnor-workflow`, reports
rev 7341ef4b / closure 9f236b40... = `closure --rev 7341ef4b`); with an empty
cache dir the same command exits 1 fail-closed (`no ... renderer for the
declared pin ... is provisioned and building one is forbidden here`).
(b) `--dry-run` always exits 0; it only reports.
(c) The closure-input set is `crates/velnor-workflow Cargo.toml Cargo.lock
rust-toolchain[.toml] .cargo` only — regen outputs under `.github/` are NOT
inputs, so shapes A and B share candidate closure 7dcabc83.

## (2) How render-changing generator PR #907 landed: RED-merged

PR #907 "Fix/ruleset 403 fallback pin", MERGED 2026-09-16T22:13:33Z by donbeave
as true merge d714a96b (`Merge: 73bb3ff9 cb905653`), ~2 min after head push.
Two commits: a70d5bdb (generator source only — ruleset-403 fallback + 37-line
lib.rs render change) then cb905653 (`chore(ci): bump D19 pin to a70d5bdb` +
full regen). Head pin = a70d5bdb = unmerged branch commit; base pin 14c1c69a.

CI at merge (head cb905653) — all red, bypass-merged (ruleset `protect-main`
active with `required_status_checks`, bypass actor RepositoryRole; legacy
branch protection 404):
- `Control / Planning` FAIL: `no runtime product for revision a70d5bdb...
  (closure fd45efac853b6570); the mainline runtime-product publisher builds
  it after merge` — identical chicken-and-egg signature as #916@f4d46b3b.
- `Policy` FAIL: `no same-repository PR run published candidate
  velnor-workflow-candidate-a12f08d0eab291a4-Linux-X64` (starvation: units
  skipped, nothing published).
- All units skipped; `ci-required`, `Control / Required` FAIL; only DCO passed.

Tree-vs-pin (measured: a70d5bdb-built binary `--plain --check` on cb905653
tree, plus `--output` render diff): every rendered output byte-identical to
the pin render EXCEPT the state file's `scan` line (committed b445147d vs
fresh-checkout 9f2b61f7, deterministic across two independent fresh checkouts;
a stray untracked file does not move it — the author's regen ran in a
shape-dirty checkout). Consequence: even WITH a candidate, Policy's
byte-compare (state file included) would have failed on the state file.
#907 could not have gone green in one PR under any timing.

History pattern (all render-changing generator landings went around green PR CI):
- #911 (self-bump): Planning passed ONLY via pre-guard branch publish
  (`ci-runtime-products.yml` success on branch @14c1c69a 21:58, release
  `...-1ff514a2852c82d3` created pre-merge); Policy red (candidate
  ef844396... WAS published — head/pin candidate closures verified equal
  locally — but after Policy's poll exited: timing race). Merged red.
- #914 (self-bump, render-NEUTRAL comment-only change): Planning+Policy(units)
  green ONLY via pre-guard branch publish (@7341ef4b 22:35); the one live proof
  the candidate rendezvous works when names match. Not reproducible post-guard.
- Gate-lanes 2773e123 (huge render-changing generator + regen single commit):
  direct push to main (merge f16cc51a, no PR). 386ccc16 pin-only follow-up:
  direct push to main.

## (3) EXACT green sequence post-guard (guard = the #916 producer change)

Key mechanics (code): the unit regen-gate (config `regen-gate` command:
`mbx run -- ... --plain --check ../..`) builds the binary from PR source, so
the unit is green IFF tree == PR-source render. Policy same-closure path
(Acquire: `closure --rev $pin` == `closure --rev $BASE_PIN`, lib.rs:3950-3956)
is green IFF tree == old-pin render. For a RENDER-CHANGING generator these
contradict, and the candidate path additionally requires a pin release product
(pin merged) plus head_closure == pin_candidate (no closure-path delta
pin..head) — jointly unsatisfiable while a generator change is in flight.
So: phase 1 lands red by bypass (like #907); phase 2 (pin-only) goes fully green.

**Phase 1 — producer change, old-pin + new-render (the 1bee4f23 shape).**
Push 1bee4f23-equivalent (generator source + regen, pin reverted to old).
Predicted CI: Planning PASS (old-pin product exists) / all GitHub units PASS
(velnor-workflow unit: stage-1 match + candidate-exception notice via self;
candidate publishes, harmless) / Policy FAIL (`generated-tree`: the 2 regen
files; same-closure empty manifest, exception dead on base product) / DCO PASS.
Land via role bypass (red-merge). Velnor-lane admission failures are
pre-existing infra noise, out of scope (identical on #911/#914/#916).
Why this shape over source-only: the unit stays green so CI actually validates
the new generator; source-only flips it (unit red exit-1 at stage 1, Policy
green) and main still needs phase 2 either way.

**Gate — wait for + verify the post-merge publisher.** After the phase-1 merge
M1, `Velnor workflow runtime products` (now default-branch-guarded) publishes
release `velnor-workflow-runtime-v1-<C1:16>` with manifest `revision == M1`,
`closure == C1`. Verify `gh release view` BEFORE opening phase 2 — without it
phase-2 Planning starves exactly like #907.

**Phase 2 — pin-only bump (fast-follow, green).** Branch from new main; in a
CLEAN checkout (scan-hash lesson from #907: commit only what a fresh checkout
renders; verify with a second fresh-checkout render diff) run
`velnor-workflow promote --rev M1` (stamps pin + regenerates atomically, cf.
TOML comment "bump it in a single pin commit after the last generator change").
Net diff: pin + regen files ONLY, zero generator source. Predicted CI, all
green: Planning PASS (M1 product exists) / all units PASS (PR-source binary ==
pin-source binary: stage-1 match, stage-2 `TreeComparison::Pin`) / publisher
emits candidate (head_closure=new source differs from base_pin=old source;
name head16 == pin_candidate16 since pin..head touches no closure paths — the
#914 rendezvous, verified live) / Policy candidate path: acquire matches,
manifest+digest+self-report bind, render == tree -> `generated-tree` Pin PASS.
Merge green. DCO PASS.

**Sequence to return:** (1) land 1bee4f23-shape phase-1 red via bypass
(Planning+units green, Policy red expected); (2) verify post-merge release
for M1 exists; (3) land pin-only `promote --rev M1` phase-2 fully green via
the candidate path.
