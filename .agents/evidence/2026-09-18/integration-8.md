# Integration batch 8: provmech + b1 merges (bastion campaign)

- Role: DESIGNATED INTEGRATION subagent. Read `/tmp/v-a2-provmech2.md` +
  `/tmp/v-b1.md` first; both sources INDEPENDENTLY CERTIFIED, SHAs verified
  after `git fetch origin`, source branches untouched.
- Start: `docs/bastion-final-plan` @ `34c44fa7` (clean tree, verified).
- Order: provmech first, then b1. Both based at old `96ccc0f1`.

## Merge 1: origin/feat/a2-provision-promotion @ 5838b9fe

- `git merge` → 3 conflicts (all expected old-base overlap):
  - `crates/velnor-workflow/src/lib.rs`: provisioner string — HEAD added
    `--source-ref refs/heads/main` x2 + revision jq predicate + `--revision`
    self-report check; theirs replaced fetch with `PRODUCT_REPOSITORY` +
    trees-API listing. Resolved by script: theirs' listing + HEAD's 3
    additions, verified by diff (vs HEAD: only env+listing regions differ;
    vs theirs: only the 3 additions differ).
  - `crates/velnor-workflow/src/primitives/runtime_products.rs`: HEAD added
    `all_consumers_pin_the_same_producer_ref` above the renamed test;
    theirs renamed `platforms_cover_the_consumer_lanes` (base body == HEAD
    body, theirs rewrote it) → kept both (HEAD test + renamed test).
  - `.github/ci/.github-actions-generator-state` (generated): took theirs,
    regen recomputed. Non-conflicting output-hash lines auto-took HEAD
    (base == theirs there, HEAD moved them via later regens).
- `git add -A` BEFORE regen per standing rule; rebuilt
  (`cargo build --locked -p velnor-workflow`); regen
  `./target/debug/velnor-workflow --plain --force .` → "Generated 21 files",
  only state changed (`scan 5774aa62dffb375e` → `8dbe18a3ecbc51b7`).
  Auto-merged YAML == regen output (disjoint shell regions); merged YAML
  carries both contracts (PRODUCT_REPOSITORY x2, source-ref x2,
  manifest_revision x1 per file).
- 2 semantic test conflicts (union contract), fixed in test sources only:
  - lib.rs `velnor_provisioner_executes_product_repo_resolution` (a2):
    fixture manifest lacked `revision`, asset lacked `--revision` → added
    `"revision": pin` + revision/closure dispatching asset.
  - consumer_negatives `velnor_provisioner_resolves_fetched_pin_...` (HEAD):
    fetch-remote premise obsolete → stub `gh` gained a fail-closed `api`
    trees handler, `run_velnor_provisioner` sets PRODUCT_REPOSITORY
    (mirrors rendered env), new `serve_trees()` helper serves the pin's
    committed tree; test renamed `..._resolves_remote_pin_to_its_true_closure`
    + asserts the product-repo endpoint; `..._fails_closed_when_pin_unresolvable`
    simplified (dead fetch scaffolding removed); stale fetch docs updated.
- Intermediate gates: 531 lib + 2/6/4/5/9/33, 0 failed; clippy `-D warnings`
  clean; `cargo fmt` applied + clean; dry-run 0 + check current.
- Commit: `499a7c034f2770be6e968dbf65987c5911c42731`
  "Merge feat/a2-provision-promotion into docs/bastion-final-plan" (+ signoff).

## Merge 2: origin/feat/b1-apt-primitives @ ed442855

- `git merge` → CLEAN (ort, no conflicts). Diff stat 5 files +7473/−41
  matches the CERTIFIED record exactly.
- Rebuilt; pre-regen `--check` showed exactly the predicted B1 F1 churn
  (`config 951b0f534335b78c` → `79f5e562a86f2cee`, outputs match).
- Regen `--plain --force` → only state changed
  (`config ...→79f5e562a86f2cee`, `scan 8dbe18a3ecbc51b7` → `f9bd3415a85dff7a`);
  zero output changes (apt feeds render only in apt-configured consumers).
- No hand edits to generated YAML at any step.

## Gates at pushed bytes (`bde4d4ec`)

- `--plain --dry-run`: `0 files would change`, exit 0
- `--plain --check`: `Generated files are current`, exit 0
- `cargo test --locked -p velnor-workflow`: **591 lib** + 2/6/4/5/9/33
  integration (0/0 doc+bin), 0 failed, exit 0
- `cargo clippy --locked -p velnor-workflow --all-targets -- -D warnings`:
  exit 0
- `cargo fmt --all -- --check`: clean, exit 0
- `actionlint` (repo-wide): exit 0, no findings
- `git diff --check`: clean

## Push

- Amended b1 auto-merge: staged regen'd state + `git commit --amend -s`
  with conventional message (+ signoff).
- Merge commit: `bde4d4ecb1e4ddc40c6426b93e2c19e9d927a70d`
  "Merge feat/b1-apt-primitives into docs/bastion-final-plan".
- `git push origin docs/bastion-final-plan`: `34c44fa7..bde4d4ec`, exit 0.
