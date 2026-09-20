# Schema-2 APT hardening review: 83e7ab4 and 91bdf6c

Date: 2026-09-20

Scope: source-only, detached worktrees. No source edits, release, install,
dispatch, merge, Velnor, or MacDocker execution.

Reviewed in order:

- `83e7ab4807824417734ffd96b6f0277c2a21234a` (`fix(apt): confine deb archive extraction`)
- `91bdf6cc1d0a5c429c5c01f17bf15dbb153c661b`, including `f8b0e22c2836eba7a03666a708dc15f83f058831`

Worktrees:

- `/private/tmp/velnor-apt83-review`, detached, clean
- `/private/tmp/velnor-apt91-review`, detached, clean

## Verdict

**BLOCKED.** The 83 implementation fixes the prior generated shell parse/
undefined-output defects, ordinary same-size candidate replacement with a
stale sentinel, basic archive traversal/type attacks, non-UTF-8 handling, and
the provider-scope extra-value gate. The 91 commits add honest platform
coverage and a real same-size candidate replacement regression only; they do
not close authority provenance, FD-bound verify-to-publish identity, or
extraction confinement races. No publication approval.

## Exact hashes

```text
83 apt.rs                         0bc68db60ca5fb2e21b2aaa1b3904f6c731586a767671ace24c73f6cf7dbc5dd
83 release.rs                     397f0220b0989174f7563e1c39ebda3e9eaccf3da15894e82f6e4ba8f4762265
83 s2/runtime.rs                  6c55c2e7d28819cd53ab132e0ed7a014ea976d4a2d7920f0904ab2702c3bf1f8
91 apt.rs                         e89661bf1fa9fee697666dcc7787a211b93c11f06cec56b14bf77a73eea0447d
91 s2/config/mod.rs               cd72513e3584fe21e3bfaa626c4bcc18ad81fb231784bee64cb92e72f343940d
91 release.rs                     397f0220b0989174f7563e1c39ebda3e9eaccf3da15894e82f6e4ba8f4762265
91 s2/runtime.rs                  6c55c2e7d28819cd53ab132e0ed7a014ea976d4a2d7920f0904ab2702c3bf1f8
91 generated APT release.yml      66fb018596ccf1a3c6597c95e0a0c47ed8661d8d441c1fd9745d658c16d31683
83..91 apt.rs diff                5b74464fb678d39ea9c95b5e6bf44d52aeb1fac43237f1869d0d6e47a9fb456b
```

The 83..91 diff is one file, `apt.rs`, with 39 insertions and 19 deletions:
the real invalid-byte test is Unix/non-macOS only; the same-size candidate
replacement test writes a same-length replacement and rejects it via the
armed sentinel.

## Blocking findings

1. **Provider/release/source authority remains self-declared.**
   `parse_discovery_selection` accepts a positive `provider_release_id`, a
   grammar-valid `release_id`, canonical-looking URLs, and field consistency
   (`crates/velnor-workflow/src/apt.rs:2054-2225`), but does not query or
   compare the provider release object. `run_fetch_selection` calls only
   `gh api repos/{source}/releases/assets/{numeric-id}` and checks byte length
   (`apt.rs:2425-2495`). It does not bind live release ID/tag/name/state/URL,
   repository, or asset metadata to the submitted selection. Generated shell
   does shallow `jq` checks and attests downloaded `.deb` subjects only
   (`s2/primitives/release.rs:4289-4327`); canonical manifest, subordinate
   records, release identity, and selection are not independently attested.
   A producer can therefore author a coherent-looking selection that names an
   unrelated provider release or assets.

2. **Verify-to-publish identity is still path/mutable-state based.**
   83 binds the sentinel to selection bytes and every selected asset digest
   (`apt.rs:1145-1229`), and 91 proves a real same-size candidate replacement
   is rejected (`apt.rs:6000-6021`). This closes the exact old attack where a
   candidate changed while an old selection-only sentinel remained.
   It does not hold verified directory/file descriptors through publication:
   `publish_suite` checks the sentinel and re-reads the incoming tree
   (`apt.rs:3765-3778`), then `stage_package` reopens paths and consumes
   metadata/debs (`apt.rs:3914-3965`, `4498-4515`, `4665-4690`). A concurrent
   replacement after the last check can change subordinate record/manifest
   inputs before `emit_publication_record`; replacing both mutable asset and
   sentinel or swapping the incoming tree is likewise not capability-bound.
   Fix needs an atomic handoff or private immutable snapshots/held descriptors
   consumed through publication, not another pathname recheck.

3. **Archive extraction still has a destination re-resolution race.**
   `prepare_extraction_destination` creates the directory relative to a
   no-follow parent and opens it, but immediately drops that descriptor
   (`apt.rs:2810-2842`). `deb_extract_data` then invokes external tar with
   pathname `-C dest_name` (`apt.rs:2845-2891`). A concurrent replacement of
   the freshly-created destination with a symlink can redirect extraction
   outside the intended root. Control extraction has the same path-based
   `tar -C scratch` behavior (`apt.rs:2643-2670`). Existing tests cover
   pre-existing destination symlinks, archive traversal members, and archive
   symlink/hardlink members, but no concurrent post-create swap.

4. **Temporary materialization and fetch-directory creation are not fully
   confined.** `scratch_dir` uses a predictable temp pathname and
   `create_dir_all` (`apt.rs:2677-2686`); a pre-existing/raced parent link can
   redirect materialized input or control extraction. `run_fetch_selection`
   uses `dir.exists`, `create_dir_all`, path `create_new`, and path `rename`
   without a held parent directory descriptor (`apt.rs:2437-2495`). The
   component-wise no-follow reader protects later reads, not these writes or
   their parent boundary. The requested parent-symlink/hardlink tests cover
   reads/extraction setup, not this fetch/create race.

5. **Native producer handoff remains absent.** The checked-in source still
   declares native release plus generic release declaration
   (`.github-gen/velnor-workflow.toml:65-78,293-295`); the native workflow
   explicitly says it does not push or dispatch `tailrocks/velnor-apt`
   (`.github/workflows/release.yml:4381-4383`). No trusted producer path emits
   the schema-2 APT selection/attestation. This is unchanged from c678.

6. **Generated APT workflow still fails default actionlint through ShellCheck
   warnings.** The external 91 fixture at
   `/private/tmp/apt91-generated/.github/workflows/release.yml` reports:

   - line 206: `SC2054` for the intentional `amd64,arm64` array argument;
   - line 254 twice: `SC2100` for the literal `last-publish`/
     `last-publish-preview` values.

   `actionlint` exits 1. Syntax-only `actionlint -shellcheck=` exits 0 and
   extracted run blocks pass `bash -n`, so the c678 hard parse failure is fixed;
   the generated lint gate still fails unless these warnings are suppressed or
   rendered without triggering ShellCheck.

## Fixed / verified against c678

- Renderer closes `verify_args` and exports `publish.outputs.channel`;
  generated APT syntax has no parse/undefined-output error
  (`s2/primitives/release.rs:4281-4287,4323-4327,4339,4447`).
- Provider gate is exact `''|github-hosted` case matching
  (`release.rs:4273-4277`).
- Asset hashes are included in the sentinel; 91 exercises a real same-size
  candidate replacement (`apt.rs:6000-6021`).
- Archive member paths/types are rejected before extraction; destination
  creation rejects existing symlink/hardlink paths
  (`apt.rs:2771-2842`), with passing hostile archive tests.
- Directory enumeration rejects invalid UTF-8. On this macOS host the direct
  conversion test runs; the real invalid-byte filesystem test is correctly
  gated to Unix non-macOS (`apt.rs:6082-6110`).
- Exact two-package Linux APT census and duplicate-target rejection pass
  (`apt.rs:1934-1970`, test `6048-6079`). Both `-amd64/_amd64` and
  `-arm64/_arm64` suffix forms are implemented (`apt.rs:1938-1945`); current
  fixture coverage uses the hyphen form, so add an underscore-form fixture.
- Schema-2 command wiring is active: `lib.rs:5691-5695` and
  `s2/dispatch.rs:53-81` route the generated workflow to `s2/runtime.rs`,
  whose APT fetch/verify/publish handlers consume `--selection` and pass the
  typed selection into publication (`s2/runtime.rs:3861-4000`). The legacy
  runtime's older option table is not the active schema-2 route.

## Verification evidence

```text
91 apt::tests                       76 passed, 0 failed
91 s2::runtime                      60 passed, 0 failed
91 s2::config::tests::apt_           2 passed, 0 failed
91 s2::config::tests::declared_apt_discovery_script_must_exist
                                     1 passed, 0 failed
91 s2::primitives::release::tests::a_declared_apt
                                     2 passed, 0 failed
cargo check -p velnor-workflow       passed
cargo fmt --all -- --check           passed
actionlint checked-in workflows      passed
git diff-tree --check 91^ 91         passed
```

Generated fixture checks:

```text
actionlint release.yml                exit 1 (SC2054 + 2x SC2100 above)
actionlint -shellcheck= release.yml   exit 0
extracted run blocks | bash -n        exit 0
```

`cargo clippy -p velnor-workflow --lib --locked --offline -- -D warnings`
fails with the same 12 baseline diagnostics recorded in
`G0/distribution/apt-schema2-followup-4c4b7a7-independent.md:143-148`:
`s2/mod.rs` (`needless_raw_string_hashes`, `too_many_arguments`,
`too_many_lines`, `no_effect_replace`, `single_char_pattern`) and
`s2/policy.rs` (`too_many_arguments`); no APT diagnostic appears.

Report is external to both review worktrees. No publication or approval is
granted.
