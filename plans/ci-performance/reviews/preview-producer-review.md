# Preview producer review

Status: source fix prepared in an isolated archive; consumer regeneration and
real CI validation remain pending.

## Evidence

- Preview run [35504500719](https://github.com/tailrocks/velnor/actions/runs/35504500719)
  from generator revision `9e5c0eb215d4169578d6f064806e89fe4c793e85` failed in
  ARM Debian job [106063528246](https://github.com/tailrocks/velnor/actions/runs/35504500719/job/106063528246):
  `aws-lc-sys` and `openssl-sys` invoked `aarch64-linux-gnu-gcc`, which was not
  installed on the x64 `ubuntu-24.04` runner.
- The same preview failed in x64 job
  [106063528303](https://github.com/tailrocks/velnor/actions/runs/35504500719/job/106063528303)
  because `velnor-runner/build.rs` rejected one dirty checkout path while
  downloaded release metadata was placed under `metadata/` in the workspace.
  This is a source identity failure, not evidence that the identity guard is
  unnecessary.
- PR [#960](https://github.com/tailrocks/velnor/pull/960), current inspected
  head `c8f7a2b353f9d2d6a60d3ad8bb2dc6299a108ceb`, changes both legacy and
  schema-2 release renderers. Its compatible scope is to download
  `release-metadata` or `preview-metadata` under `$RUNNER_TEMP` and resolve all
  metadata consumers from that directory. The clean-tree and source-commit
  checks remain intact.
- PR [#962](https://github.com/tailrocks/velnor/pull/962), current inspected
  head `43ba3b414f245bf3aa9176afce5bf97f2d1e5235`, addresses ARM with native
  `ubuntu-24.04-arm` routing and target linker preflight. Its provider-default
  change would remove existing Velnor release coverage, so it cannot be
  adopted wholesale.

## Prepared source change

`/tmp/velnor-pr960-metadata2.patch` is generated from clean source revision
`9e5c0eb215d4169578d6f064806e89fe4c793e85`. It changes only
`src/primitives/release.rs` and `src/s2/primitives/release.rs`:

1. Downloaded metadata goes to `${{ runner.temp }}/release-metadata`.
2. Each identity and package-record step initializes and quotes
   `metadata_dir="$RUNNER_TEMP/release-metadata"`.
3. The artifact release tool, manifest checks, checksum, and package-record
   inputs resolve through that directory.
4. The preview source-commit check remains emitted and tested.
5. Both schema renderers retain their pinned output assertions, refreshed for
   the exact `9e5` source.

The patch applies cleanly with `patch --dry-run -p1` to a fresh `9e5` archive.
In the patched archive, both schema variants passed the hostile `$RUNNER_TEMP`
path fixture and both pinned identity render checks. `cargo fmt --check` and
all-target, all-feature Clippy passed. The release test subset had 210 passes;
two unrelated schema-2 fixture tests failed because an archive has no `.git`
checkout and therefore reports generator revision `unknown`.

## ARM decision

The structural remedy is a typed target-to-runner mapping with native ARM
hosted producers and explicit C/C++ linker preflight. Cross compilation is a
valid fallback only with pinned sysroot/toolchain inputs and target-specific
Cargo/`cc` environment. A larger runner alone is not a generator speedup.

## Parent implementation review

Metadata candidate `/tmp/velnor-pr960-metadata2.patch`: HOLD.
It changes paths by replacing strings in already-rendered YAML and shell.
The old relative paths remain the source of truth; replacement depends on
indentation and statement spelling. This permits future metadata consumers to
escape the staging contract. Emit the external staging location directly from
the source renderer, sharing its path contract where appropriate. Remove the
post-render replacement helpers. Execute the actual emitted stage block in the
hostile-path fixture instead of reconstructing its copy command.

The clean-tree identity guard must remain. This finding does not dispute the
observed `metadata/` pollution or PR #960's intended correction.
