# XCFramework handoff re-review: `32f2acf`

Review target: `tailrocks/velnor`, branch
`codex/github-first-xcframework-policy`, exact detached HEAD
`32f2acf809b21c63ec65b3227e755a09e284291c`, parent
`acb17712cf8dba45e3e84f801a5dbb150365badf`. Remote branch resolved to the
same SHA. The detached tree was clean; no source, generated output, branch, or
remote state was changed.

## Verdict

Approve the source-only XCFramework validator change, subject to the existing
native-label fixture drift being tracked separately. The prior acb defects are
fixed: valid two-link framework chains now pass, canonical dot/empty path forms
fail closed, and symlink cycles are bounded and rejected. The new DAG lookup
also returns a typed usage error instead of panicking on an undeclared child.

No hosted macOS, Docker, rollout, or end-to-end GitHub claim is made here.

## Exact evidence

The exact generated archive validator was extracted from both:

- `crates/velnor-workflow/src/platform.rs:446-637`;
- `crates/velnor-workflow/src/s2/platform.rs:147-338`.

After raw-string indentation removal, the legacy and S2 validator scripts are
byte-identical. Bounded shell fixtures against the exact script produced:

- realistic chain accepted:
  `Foo.framework/Foo -> Versions/Current/Foo -> Versions/A/Foo`;
- nested relative escape rejected with
  `artifact archive symlink escapes its bundle or contains a cycle`;
- nested cycle rejected for `A -> dir/B -> ../A`;
- direct cycle rejected by the committed integration test;
- `file` plus `./file` rejected as
  `artifact archive contains a non-canonical member path`;
- a raw archive member containing an empty path component (`//`) rejected by
  the same non-canonical-path check.

The committed integration tests execute both paths. Targeted exact tests:

```text
artifact_shell_materializes_real_bytes_and_rejects_hostile_symlinks: 1 passed
schema_two_artifact_renderer_accepts_framework_symlink_chain: 1 passed
```

The full exact suite `cargo test -p velnor-workflow
--test platform_prerequisites --locked` reports **12 passed, 3 failed**. The
three failures are unchanged assertions expecting `runs-on: macos-15` at
`platform_prerequisites.rs:340,405,1116`; the current renderer emits
`macos-26`. The archive chain, duplicate, cycle, hardlink, traversal,
destination-parent, and DAG tests pass.

Additional exact checks:

- `cargo clippy -p velnor-workflow --all-features --all-targets --locked --
  -D warnings`: passed (`cargo clippy: No issues found`).
- `cargo fmt --all -- --check`: passed.
- legacy/S2 validator source parity: no diff.
- `git status --short`: clean.
- `git ls-remote origin refs/heads/codex/github-first-xcframework-policy`:
  `32f2acf809b21c63ec65b3227e755a09e284291c`.

## Source assessment

`artifact_resolve_path` follows the longest matching archive symlink prefix,
normalizes relative targets and suffixes, tracks visited link names, and caps
resolution at 128 links. Archive census rejects empty, doubled-slash, dot,
tab, and carriage-return path forms before duplicate comparison. The existing
root-kind, hardlink/special-member, archive-root, existing destination, and
parent-symlink checks remain in place. Legacy and schema-2 implementations are
mirrored exactly.

The DAG fix changes `indegree.get_mut(child).expect(...)` to an explicit
`GeneratorError::usage` for an undeclared child, eliminating the prior panic
path. No additional source defect was found in this bounded review.

