# VERDICT: CERTIFIED — a2-provmech repair (G3+G5+G8+G9)

Repair branch `feat/a2-provision-promotion`, commit
`5838b9fe00c29a85085deafc545134dc7ea66a73` (fetched from origin;
`origin/feat/a2-provision-promotion` = 5838b9fe, parent = a05b0e2a,
fast-forward, original commit NOT amended, signed off). Base comparison
unchanged. All proof in a FRESH clone from origin (`/tmp/v-a2-rv2` @
5838b9fe, clean) plus a synthetic consumer (`/tmp/v-a2-rv2-consumer`).
No repo edits, no merges, no pushes.

## Disproof 1 (stale ownership state) — FIXED, re-proven

- Committed `.github/ci/.github-actions-generator-state` now carries
  `scan 5774aa62dffb375e` — exactly the value the MISMATCH verdict
  predicted for a clean checkout; config `951b0f534335b78c` and
  generator `50` unchanged.
- Fresh-clone binary: `--plain --dry-run` → "0 files would change",
  exit 0; `--plain --check` → "Generated files are current", exit 0
  (via the candidate-render exception naming `17baa98f…` vs declared
  pin `7341ef4b…` — expected for a generator change in flight, same as
  the repair evidence). `git status` clean after both runs.

## Disproof 2 (promote cannot promote) — FIXED, re-proven independently

- `promote.rs` writer call now passes positional
  `dry_run=false, check=false, force=true, adopt=false` (parameter
  order confirmed in `lib.rs:5573-5584`; justification comment present).
- Own shell-driven repro (not the test harness): synthetic consumer
  with old pin → prior render committed (3 workflows in
  `.github/workflows`) → `promote --rev <own-rev=5838b9fe>` exits 0,
  pin advances `00000000 -> 5838b9fe`, promotion commit contains 10/10
  `M` (modified) entries — the Update path — tree clean.
- Binding intact: `promote --rev 96ccc0f1…` (full foreign SHA) refused
  naming render≡stamp ("renders with exactly the generator it
  stamps"), exit 1, tree untouched.
- New bundled test `promote_advances_a_committed_prior_render` passes
  as part of the suite (promote_atomic: 4/4).

## Full gates (fresh clone) — all green

- `cargo test --locked --all-features -p velnor-workflow`:
  495+0+2+6+4+5+9+33 = **554 passed, 0 failed** (+1 vs bundle = new test).
- `cargo clippy --locked --profile test --all-targets --all-features
  -p velnor-workflow -- -D warnings`: exit 0.
- `cargo fmt -p velnor-workflow -- --check`: exit 0.
- `actionlint` 1.7.12: exit 0.

## G3/G8/G9 spot-checks — still clean

- G3: provisioner uses `PRODUCT_REPOSITORY` + product trees API with
  truncation guard; the 3 remaining `git fetch` lines are the
  pre-existing PR_HEAD/BASE/base_pin policy fetches — no consumer-repo
  pin fetch. All 4 provisioner tests pass incl. the executable one.
- G8: `platforms()` takes no config (fixed constants);
  `ci-runtime-products.yml` untouched by the repair
  (empty a05b0e2a..5838b9fe diff); hostile-label test passes.
- G9: `crate_inputs_stay_closure_complete` +
  `build_script_mirrors_the_closure_spec` pass.

## Repair scope

`a05b0e2a..5838b9fe` touches exactly 3 files (+77/−2): ownership state
(scan line only), `promote.rs` (comment + `false`→`true`), and
`promote_atomic.rs` (new test). No sibling-surface drift.

Scratch kept: `/tmp/v-a2-rv2`, `/tmp/v-a2-rv2-consumer`,
`/tmp/v-a2-rv2-prior.log`, `/tmp/v-a2-rv2-fmt.log`.
