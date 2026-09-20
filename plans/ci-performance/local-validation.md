# Local validation contract

Normal commits run formatting and then Clippy through the existing single-purpose
`mise run fmt` and `mise run lint` tasks. Prek stops after the first failure.
Both checks run for every commit, including configuration-only changes, because
a Rust-file filter cannot prove coverage of manifests, locks, toolchain, Cargo
settings, generator policy, lint policy, and shared scripts.

## Manager decision

The evaluated releases were hk 2.0.1 and prek 0.5.3 on Darwin arm64. Disposable
Git repositories established that hk with explicit `stash = "git"` accepted bad
staged data when the unstaged fix restored HEAD bytes. It also accepted an
unstaged hk configuration replacing the check with `true`. Prek rejected both
cases, but left untracked and ignored dependency files visible. Neither manager
alone meets the complete index-snapshot requirement.

Prek is selected. In addition to those correctness differences, hk's
[official release matrix](https://github.com/jdx/hk/blob/v2.0.1/.github/workflows/release.yml)
omits Darwin x86_64, while this repository's tool lock includes macos-x64.
[Prek 0.5.3](https://github.com/j178/prek/releases/tag/v0.5.3) supplies native
artifacts for both Darwin architectures and the existing Linux/Windows tool
platforms. Building hk from source would conflict with the existing prebuilt
tool policy. Only prek is installed. Its seven platform entries are pinned by
version, artifact URL, and checksum in `mise.lock`.

Primary behavior references: [hk hooks and stashing](https://hk.jdx.dev/hooks),
[hk mise integration](https://hk.jdx.dev/mise_integration), and
[prek hook execution](https://prek.j178.dev/running-hooks/). Three warmed no-op
fixture runs measured hk at 62–64 ms and prek at 151–162 ms. These measure manager
overhead only and are not Rust workspace or cold-install performance claims.

## Snapshot boundary

Bootstrap installs a repository-local launcher under Git's common directory.
It embeds the already pinned Bun runtime and disables runtime loading of dotenv,
bunfig, package.json, and tsconfig. The shell shim also removes inherited runtime
preloads and task/compiler/skip overrides before starting the executable.
On macOS, bootstrap signs the locally generated binary with an ad-hoc signature;
Bun 1.4.0's embedded output otherwise has an invalid signature on the tested host.
No working-tree helper or task executes before snapshot creation.

The launcher reads the actual commit index, including `GIT_INDEX_FILE` supplied
by Git for partial commits. Each worktree has a stable isolated clone under an
exclusive directory lock. It shares Git objects, preserves source HEAD/history,
loads the staged tree, and removes every undeclared or ignored file. Raw staged
blob hashes are checked before validation; EOL/encoding transformations are
replaced with the actual staged bytes. Global/system Git configuration is
disabled. Staged deletions cannot retain HEAD files. The source index, worktree,
and untracked files are never stashed, reset, or copied into the snapshot.

The stable path and unchanged file timestamps retain valid Cargo fingerprints.
An initial disposable-path experiment recompiled an unchanged Rust fixture on
its second check; the stable snapshot regression now asserts that no such
recompile occurs. This is measured build reuse, not a whole-workspace speedup
claim. Interrupted runs release the lock on handled signals; after a forced
process kill, the diagnostic names the lock and requires verifying its owner
has stopped before manual recovery. Concurrent checkouts use separate locks.

Only then does prek load the staged hook configuration and invoke the staged
mise tasks. Cargo dependency paths and workspace membership escaping the
snapshot are rejected. Escaping or dangling symlinks and submodules are rejected
with diagnostics. This is source isolation, not a security sandbox for arbitrary
staged build scripts. All staged code must remain trusted local development code.

The launcher rejects tracked mutations by checks and rejects a source index
changed during validation. Compiler products and Cargo downloads live outside
the snapshot under Git's local hook directory. Cargo owns build locking and
fingerprint validation; no successful-check status is cached. User Cargo
configuration is not inherited. Global mise configuration is disabled while the
installed tool store remains available.

## Bootstrap and verification

Run `mise trust`, `mise install`, and `mise run bootstrap` after cloning. Run
bootstrap again after changing hook tooling. It is idempotent and supports
linked worktrees through Git's common hook directory. It refuses to overwrite
an existing foreign pre-commit hook or install into any configured `core.hooksPath`,
including a global/shared path, and reports the setting for explicit integration.
Missing pinned tools fail with the same setup instructions. Initial Rust checks
may download dependencies; after one successful check, a prepared toolchain and
Cargo cache support offline validation. Toolchain and dependency changes can
require preparation again.

Run `mise run test-hooks` for disposable-repository regression fixtures. Coverage
includes staged formatting/Clippy failures hidden by unstaged fixes, unstaged
task/hook/helper tampering, untracked and ignored dependencies, runtime preloads,
inherited environment overrides, configuration-only commits, alternate indexes,
normal commits, linked worktrees, staged deletions, outside paths, missing tools,
foreign hooks, exact source-content preservation, and real Rust checks in an
offline warm fixture.

Required CI must independently enforce formatting and Clippy even when Git
hooks are bypassed. This document does not claim that local hooks provide merge
protection or that macOS fixture results verify Windows/Linux execution.

## Aggregate task ordering

`mise run check` references each canonical task through an ordered `run` array.
The first three entries are formatting, Clippy, and tests; all previous auxiliary
checks remain, plus hook regressions. Canonical leaf tasks contain no nested
formatting/lint/test orchestration. [Mise documents](https://mise.jdx.dev/tasks/task-configuration.html#run)
that each entry finishes before the next starts. The regression fixture executes
the actual aggregate structure with timestamped leaf commands, deliberately
fails formatting and Clippy, and proves later stages never start.
