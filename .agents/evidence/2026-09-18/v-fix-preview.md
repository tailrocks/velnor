# V-FIX-PREVIEW — independent verification of fix/preview-guest-upload

Verdict: **MATCH / CERTIFIED**

- Branch: `origin/fix/preview-guest-upload` @ `6be652b5`, single commit on base
  `38ffbfd7` (`origin/docs/bastion-final-plan`), `Signed-off-by` trailer present.
- Scope: exactly 5 files, +103/-8 — matches `/tmp/fix-preview.md` claim.
- No merges, no pushes performed. All checks run in scratch worktree
  `/tmp/vfix-worktree` (detached HEAD `6be652b5`), restored clean afterwards.

## Claims checked (all hold)

1. Generator upload block uses `agent_bin`, no hardcoded name:
   `release.rs:725` renders `dist/microvm/{agent_bin}` where `agent_bin` comes
   from `guest_payload_bins(config, &release.package)` (`:649`). Zero
   `velnor-guest-agent` literals in `release.rs`. Rendered YAML shows
   `velnor-guest-agent` purely as the expansion for this repo's package.
2. Test rejects wildcard: `native_guest_payload_uses_scanned_guest_bins`
   asserts the exact 5-file block inside the `guest-payload` job (via `yaml_job`
   scoping) for BOTH preview and release, loops over two package names
   (`widget`, `acme-box` → proves genericity), and asserts no
   `dist/microvm/*` in either workflow.
3. Builder hardening: `--work-dir` flag added; default is the `<out>-work`
   sibling (`dist/microvm` → `dist/microvm-work`, never nested); default
   sibling removed on success, explicit `--work-dir` kept; removal failure
   warns only; fail-closed when `--out` has no file name. 3 new unit tests.
4. YAML changed ONLY via regen: rebuilt the generator from the branch and ran
   `./target/debug/velnor-workflow . --plain --dry-run` → `0 files would
   change`, exit 0. `preview.yml`/`release.yml` hunks show only the upload
   block; state file only the two expected hash updates.
5. Fix matches `/tmp/a1-preview.md` proposal exactly: same 5-file list, same
   `agent_bin` requirement, same builder hardening, no rejected approaches
   (no chmod/chown, no sudo-wrap, no negative glob). Adjacent findings
   (vmlinux.sha256 parity, rootfs.sha256 format) correctly untouched.

## Disproof attempts (all failed = fix stands)

- Mutation: reverted generator line to `path: dist/microvm/*` in scratch
  worktree → new test FAILS (`guest payload upload must list the exact
  consumer files`); fix restored → passes. Test is non-vacuous.
- Wildcard remnants: `grep -rn 'dist/microvm/*'` → only the test comment
  (`:4532`) + rejection assertion (`:4544`). No YAML matches.
- Contract satisfiability: fresh-build step writes all 5 files
  (`test -s` guarded); seed-reuse path also produces all 5
  (plus extra `vmlinux.sha256`, now simply not uploaded — no consumer reads it).
  Explicit list cannot starve on either path.
- CI takes the safe builder path: no workflow passes `--work-dir`
  (grep: no matches), so CI always uses the `<out>-work` sibling default.

## Rerun results (observed in scratch worktree)

- `cargo test --locked -p velnor-workflow`: 486 lib + 2/6/5/9/33 — 0 failed
  (exact counts as claimed); targeted `native_guest_payload_uses_scanned_guest_bins` ok
- `cargo test --locked -p velnor-runner --bin velnor-guest-image`: 9 passed, 0 failed
- `cargo clippy --locked -p velnor-workflow -p velnor-runner --all-targets`: exit 0, 0 warnings
- `cargo fmt --check`: exit 0
- `actionlint preview.yml release.yml`: exit 0

## Note (environmental, not a fix defect)

`--plain --check` fails in THIS sandbox under the D19 pin guard (stale system
`velnor-workflow` shims lack `--closure`, no pinned renderer provisioned) —
not content drift. `--plain --dry-run` exit 0 with `0 files would change` is
the authoritative no-hand-edit proof and it passes.

## Residual (unchanged from fix report)

Cold-cache Preview run on both arches still needed to confirm green end-to-end.
