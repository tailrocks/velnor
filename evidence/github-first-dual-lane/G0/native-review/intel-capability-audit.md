# Intel macOS capability audit

Date: 2026-09-20  
Reviewer: independent G2/G3 review  
Scope: existing Homebrew contract, Velnor producer targets, GitHub-hosted native macOS capability.

## Finding

The original Homebrew tap commit `7af1249f3d69c9f2e548583cdc9f3e737da41b81` has a generic source formula for `velnorctl`: it declares only a Rust build dependency, has no architecture exclusion, and builds with `cargo install` on the host. Its README says “Install the CLI on macOS” without an Intel caveat. It does not claim a native macOS Actions runner; Velnor execution remains Linux-only. Therefore an arm64-only replacement must not silently delete the existing Intel macOS CLI/source-build capability.

Velnor’s `rust-toolchain.toml` explicitly lists both `aarch64-apple-darwin` and `x86_64-apple-darwin`. The current release workflow, however, builds only Linux targets on `ubuntu-24.04`; Apple target installation in Linux jobs is cross-target setup, not native Apple build proof. The current macOS host guide has no Intel exclusion, but describes a Linux Docker VM and rejects `macos-*` execution labels.

## Hosted evidence

GitHub’s current hosted-runner documentation lists standard Intel labels `macos-15-intel` and `macos-26-intel`, alongside arm64 labels `macos-14`, `macos-15`, and `macos-26`:

- https://docs.github.com/en/actions/reference/runners/github-hosted-runners
- https://docs.github.com/en/actions/reference/runners/larger-runners

GitHub-maintained image readmes document the corresponding Intel toolchain images:

- https://github.com/actions/runner-images/blob/main/images/macos/macos-15-Readme.md — macOS 15.7.9, Clang/LLVM 17, Xcode Command Line Tools 16.4.
- https://github.com/actions/runner-images/blob/main/images/macos/macos-26-Readme.md — macOS 26.6.1, Clang/LLVM 21, Xcode Command Line Tools 26.6.

These are hosted-capability facts, not proof that Velnor’s products build or install correctly on Intel.

## Smallest typed producer correction

Keep Linux/Debian release rows separate. Add a typed native product row for `x86_64-apple-darwin` beside `aarch64-apple-darwin` (kind `homebrew-archive` or equivalent), built on an explicit Intel label, with canonical target, archive name, checksum, size, and provenance. Do not append Apple targets to the existing `velnor-runner` Debian matrix: its digest/archive logic accepts only Linux targets.

The Intel row needs a native build/install smoke lane: verify Mach-O x86_64, all shipped sibling binaries, version/source identity, install/upgrade behavior, and sibling resolution. Project both Apple rows into Homebrew’s typed `on_intel`/`on_arm` consumers. Until that artifact and test exist, retain the generic source-build path rather than publish an arm64-only formula. Do not call Rust/CLT support “Xcode” unless the lane actually selects and tests full Xcode.

