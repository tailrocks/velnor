# A1 Policy repro — run 35129353335 `generated-tree` failure

## Verdict

- (1) **Reproduces exactly** via the pinned generator: same 3 files, same
  `policy: 11 rules, 1 failed` report as CI.
- (2) **Root cause:** fleet sync PR #903 (`3353310c`, pin stamped `b9c3156c`)
  rendered the tree with a generator containing the **in-flight
  ruleset-403-fallback change** (pre-merge state of branch
  `fix/ruleset-403-fallback-pin`, landed on mainline as `a70d5bdb` in PR #907
  a day later). The pinned generator provably lacks that template.
  The drift landed because the PR **merged with Policy + regen-gate red**
  (merge-path bypass) and because nothing binds the stamped pin to the
  generator that actually rendered the tree.
- (3) **Proposed fix:** none needed now — already healed on mainline by
  `a70d5bdb` + `cb905653` (pin advance to `a70d5bdb` with regen); current pin
  `7341ef4b` descends from that lineage and HEAD is 0-diff. Structural:
  fleet sync must prove render-generator ≡ stamped pin; fleet merges must
  honor Policy/ci-required. **No edits made** (this report only).

## (1) Repro (CI-equivalent, pinned generator — not HEAD)

CI context from run 35129353335 (`main CI / Main`, workflow_dispatch):
`HEAD_SHA = BASE_SHA = 3353310c7648fca22698b6c0f4a69ab245127786`,
`VELNOR_WORKFLOW_POLICY_REVISION = b9c3156cdb88e63c11b9e595a3e694b02238c09a`,
`RULESET_CONTEXTS = DCO,Policy,ci-required`. CI installs the validator with
`cargo install --git … --rev b9c3156c…` and runs `velnor-workflow policy
--workflow-root … --head-sha … --base-sha … --ruleset-contexts …`.

Local equivalent (all under `/tmp`; workspace untouched):

```sh
git worktree add --detach /tmp/repro-335 3353310c7648fca22698b6c0f4a69ab245127786
git worktree add --detach /tmp/gen-b9 b9c3156cdb88e63c11b9e595a3e694b02238c09a
cargo build -p velnor-workflow          # in /tmp/gen-b9
VELNOR_WORKFLOW_POLICY_REVISION=b9c3156cdb88e63c11b9e595a3e694b02238c09a \
/tmp/gen-b9/target/debug/velnor-workflow policy \
  --workflow-root /tmp/repro-335 \
  --head-sha 3353310c7648fca22698b6c0f4a69ab245127786 \
  --base-sha 3353310c7648fca22698b6c0f4a69ab245127786 \
  --ruleset-contexts DCO,Policy,ci-required
```

Result — byte-for-byte the CI report (`/tmp/policy-fail.log` holds the CI log):

```text
FAIL generated-tree         the tree differs from the render of velnor-workflow at b9c3156c…
       - .github/ci/.github-actions-generator-state: differs from the pinned render
       - .github/workflows/ci-main.yml: differs from the pinned render
       - .github/workflows/ci-policy.yml: differs from the pinned render
policy: 11 rules, 1 failed
```

True content diff (pinned render in a symlink-preserving worktree,
`/tmp/repro-335b`, `git diff`): committed tree **has** the
`DECLARED_RULESET_CONTEXTS` + `HTTP 403|Upgrade to GitHub Team` fallback block
in the `Resolve required status checks` step of `ci-main.yml` and
`ci-policy.yml`; the pinned render lacks it. The state file differs **only**
in those two output hashes — `scan`/`config` lines are identical
(`scan bbf16ddf3612a65e` both sides).

Repro-environment trap (documented so nobody re-trips it): `cp -r` of a
worktree materializes the tracked symlink `CLAUDE.md -> AGENTS.md` as a
regular file; the scan skips tracked symlinks but counts the materialized
file, so renders into `cp -r` copies report a bogus `scan 99e4e556…`.
Renders must run in real worktrees (as CI does). The CI diff has no scan
component.

Control experiment: the same `b9c3156c`-built binary run against the pre-sync
tree (`/tmp/repro-b9`, pin `92c34786`) reports
`PASS generated-tree … byte-identical`, `11 rules, 0 failed`. The binary and
method are sound; only the #903 tree is pin-false.

## (2) Root cause

**Generator-side change:** `fix(velnor-workflow): fall back to declared
ruleset contexts on API 403` — mainline `a70d5bdb`, PR #907, branch
`fix/ruleset-403-fallback-pin`, merged **2026-09-17 05:13 UTC**, i.e. *after*
sync #903 merged (**2026-09-16 15:29 UTC**). The #903 tree was rendered with
the branch's pre-merge generator state (branch refs since deleted) while
stamping the mainline pin `b9c3156c`.

Evidence chain:

1. `DECLARED_RULESET_CONTEXTS` occurs **0×** in the `b9c3156c` generator
   source — the pinned generator cannot emit the committed bytes.
2. The committed fallback block (both workflows) is **byte-identical**
   (`diff`-clean) to the block the `a70d5bdb` generator renders, including the
   `BTreeSet`-sorted literal `DCO,Policy,ci-required` produced by
   `declared_ruleset_contexts_literal`.
3. `git log 92c34786..b9c3156c -- crates/velnor-workflow/` is **empty**: no
   mainline generator change could explain the bytes either.
4. All **19/19** state-file output hashes verify (FNV-1a) against the committed
   bytes: one coherent generator render, not a hand edit.
5. Rendering the 335 tree with the mainline `a70d5bdb` generator does **not**
   reproduce it either (21 files, incl. newer `ci-runtime-products.yml`) —
   the sync used the Sep-16 branch state, not the later mainline squash.

**Why the architecture allowed this drift class** (two independent holes;
either alone would have stopped it):

1. **Pin is a claim, not a binding.** The sync pipeline renders with generator
   X and stamps pin Y in `.github-gen/velnor-workflow.toml` with no step
   proving X ≡ Y (no rendering-generator closure recorded, no post-render
   pin-render self-check on the sync path). The D19 invariant
   ("tree == render of generator AT the pin") is enforced only by downstream
   validators, which brings us to —
2. **Merge-path bypass of red validators.** PR #903 (`velnor/fleet-b9c3156c…`)
   shows `Policy: FAILURE`, `Rust · velnor-workflow: FAILURE` on both lanes
   (the regen-gate `--check`, which at `b9c3156c` already wires the
   `verify_declared_pin_renders_tree` D19 guard), and `ci-required: FAILURE`
   — state `MERGED`. Both the pre-merge policy validator and the `--check`
   gate fired correctly and the merge overrode them. Post-merge mainline
   policy then failed exactly as designed (run 35129353335).

## (3) Proposed fix (not applied)

At `3353310c` the tree was unrenderable by **any** mainline pin (`b9c3156c`
lacks the template; `a70d5bdb` renders everything else differently), so the
only correct resolutions were (i) revert the 3 files to the `b9c3156c` render,
or (ii) land the generator change first, then pin-advance + regen. History
took (ii): PR #907 (`a70d5bdb`) + `cb905653` ("bump D19 pin to `a70d5bdb`
after ruleset 403 fallback"). The workspace pin (`7341ef4b`) descends from
that lineage and HEAD is already 0-diff, so **no source change is required
for this failure** — it is healed.

Structural (for whoever owns the fleet path):

1. Fleet sync must verify the rendering generator's source closure equals the
   pin it is about to stamp, before committing (render with X ⇒ stamp X).
2. Fleet merges must not bypass `Policy` / `ci-required`: the red checks on
   #903 were true positives — honoring them is the entire enforcement model.

## Artifacts (all `/tmp`, workspace unmodified, nothing committed)

- `/tmp/a1-policy.md` — this report.
- `/tmp/policy-fail.log` — CI failed-job log (run 35129353335).
- `/tmp/repro-335` — pristine `3353310c` worktree (policy repro target).
- `/tmp/repro-335b` — `3353310c` worktree + true pinned render (`git diff` =
  CI-equivalent 3-file diff).
- `/tmp/repro-b9` — pristine `b9c3156c` worktree (passing control).
- `/tmp/gen-b9`, `/tmp/gen-a70` — generator worktrees + debug binaries used
  for the renders.
- `/tmp/block-committed.txt`, `/tmp/block-a70.txt` — byte-identical fallback
  blocks.
