# A1 FIX — Preview guest-payload symlink escape

- Branch: `fix/preview-guest-upload` (from `docs/bastion-final-plan` @ `38ffbfd7`)
- Commit: `6be652b5` — `fix(ci): upload explicit guest-payload file list, move build scratch out of publish dir` (signed off, pushed to origin)
- Diagnosis: `/tmp/a1-preview.md` (run 35159519217 EACCES scandir)

## Changes (5 files, +103/-8)

1. Generator (`crates/velnor-workflow/src/primitives/release.rs`, `render_guest_payload_job`):
   `path: dist/microvm/*` → explicit 5-file block using existing `agent_bin`
   (vmlinux, rootfs.ext4, rootfs.sha256, {agent_bin}, guest-agent.sha256).
2. Generator test (`native_guest_payload_uses_scanned_guest_bins`): asserts the exact
   upload block inside the `guest-payload` job of both preview and release, and
   rejects `dist/microvm/*` in both workflows.
3. Builder hardening (`crates/velnor-runner/src/bin/velnor-guest-image.rs`):
   `--work-dir` flag added; default is the `<out>-work` sibling (never nested
   under `--out`); default sibling removed on successful build (explicit
   `--work-dir` left for inspection; removal failure warns, never fails build).
   Fail-closed when `--out` has no file name. 3 new unit tests.
4. Regen via generator ONLY (`velnor-workflow . --plain --force`, no hand edits):
   `preview.yml` + `release.yml` diffs show only the upload-block change
   (`velnor-guest-agent` rendered from `agent_bin`); generator state hashes updated.

## Verification (all observed in this worktree)

- `cargo test --locked -p velnor-workflow`: 486 lib + 2/6/5/9/33 integration — 0 failed
- `cargo test --locked -p velnor-runner --bin velnor-guest-image`: 9 passed, 0 failed
- `cargo clippy --locked -p velnor-workflow -p velnor-runner --all-targets`: clean
- `cargo fmt --check`: clean
- `actionlint preview.yml release.yml`: clean (exit 0)
- `velnor-workflow . --plain --check`: "Generated files are current"
- `grep -rn 'dist/microvm/*'`: only the test comment + rejection assertion remain

## Residual

- Cold-cache Preview run on both arches still needed to confirm green end-to-end.
- Adjacent findings from diagnosis untouched (vmlinux.sha256 parity, rootfs.sha256 format).
