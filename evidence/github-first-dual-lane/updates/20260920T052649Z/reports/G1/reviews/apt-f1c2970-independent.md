# Independent bounded APT delta review: f1c2970f4e4a419cee660071368f856ffa241ed5

Date: 2026-09-20

Scope: exact successor of the pinned `02c04180a61f23e74cf102cfcccd57adb0567d8d`
APT source review. Source-only, read-only. No source edits, release,
installation, runtime, MacDocker, Velnor, dispatch, publication, merge, or
approval was performed.

Review tree: `/private/tmp/velnor-apt-f1-review`, detached and clean. Remote
`origin/dual-lane-apt-schema2` resolved to the reviewed tip
`f1c2970f4e4a419cee660071368f856ffa241ed5`. The delta is exactly two commits:
`c536b74c2686ff18477379be80edd9b3eec58076` adds the native-product fixture
and parser test; `f1c2970f` corrects that commit's attribution. Production APT
logic, runtime wiring, and generated renderer logic are unchanged from 02c.

## Verdict

**BLOCKED. No publication approval.**

The delta is a useful bounded parser-compatibility test: it consumes a
captured native assembly `product-manifest.json`, validates its sidecar, and
projects its 18 artifact rows into the APT selection parser. It does not add
provider authentication, native producer handoff, package bytes, archive
extraction, attestation, or publication proof. All 02c authority blockers
remain.

The test's provider release metadata is explicitly synthetic (`provider_release_id`
12345, generated asset IDs 5000+, and synthetic asset sizes for subordinate
records). The test calls `read_discovery_selection` only. It must not be
described as a live provider/native release handoff or full archive/install
proof.

## Exact identity and hashes

```text
tip                                      f1c2970f4e4a419cee660071368f856ffa241ed5
parent                                   c536b74c2686ff18477379be80edd9b3eec58076
APT baseline                             02c04180a61f23e74cf102cfcccd57adb0567d8d
tip tree                                 72c8731c8a799f2f65af114d307197fb05aed4bf
apt.rs                                   9e37b7215700e18b6308d424411d874b7c7a91b2a166d813c9993c91bb2c1649
native product-manifest.json             87d27be4173b4f6e00eef05013ff725f11c604042ccc4db0e8f8fd034dfca6e6
native product-manifest sidecar          cd35df59e2eddc690ada562e1327f62355b1711c16f1ee1a6ee4069c5f1c72e3
generated f1 APT release.yml             65f2b6d24d21b6b4a66b23c60e5f28afc4aaaa9e4a654d9b64da126703d1d435
```

The fixture manifest is 5,684 bytes and contains 18 artifacts: 12 binaries,
2 APT packages, and 4 archives across 3 components. Its sidecar names the
canonical basename `product-manifest.json` and matches the exact fixture bytes.

## Delta evidence

The new test includes the captured bytes and sidecar with
`include_bytes!`/`include_str!` (`crates/velnor-workflow/src/apt.rs:7299-7306`).
It asserts native component fields, computes the exact manifest digest, and
parses the sidecar (`apt.rs:8053-8080`). It reads the 18 artifact names/sizes,
adds synthetic subordinate records and `.deb` sidecars, and fabricates 28
uploaded provider asset records with IDs beginning at 5000
(`apt.rs:8082-8139`). The selection explicitly sets
`provider_release_id: 12345` (`apt.rs:8145-8170`) and then only calls
`read_discovery_selection` (`apt.rs:8171-8197`). No asset bytes are fetched;
no `verify_discovery_incoming`, `verify_suite`, `gh attestation verify`,
archive extraction, signer verification, or publish path runs.

The native source comment identifies checkpoint `384e2e4e` and explicitly
labels provider metadata synthetic (`apt.rs:8055-8057`). That is honest test
scoping. The captured fixture is useful evidence that the APT schema can
consume the native manifest shape; it is not evidence that GitHub's release
object, selection, subordinate records, or archives are authentic.

## Inherited 02c blockers still apply

1. **Native handoff: BLOCKER.** The native producer contract still names
   `tailrocks/velnor-apt`, but the producer workflow explicitly does not push
   or dispatch the consumer (`.github-gen/velnor-workflow.toml:65-75`,
   `.github/workflows/release.yml:4381-4383`). The APT workflow remains
   schedule/manual-dispatch driven. This test does not change that.

2. **Provider/source authority: HIGH/BLOCKER.** The APT parser validates
   self-declared selection metadata and `run_fetch_selection` downloads by
   self-declared asset ID/URL while checking response length only
   (`crates/velnor-workflow/src/apt.rs:2210-2269,2291-2550,2767-2787`). The
   f1 fixture strengthens shape compatibility, not independent provider
   authentication.

3. **Rollback identity: PASS from 02c.** Signed-live `Packages` digest binding,
   held rollback bytes, and hostile tamper tests remain unchanged and passed in
   the pinned 02c review (`apt.rs:5348-5467`; `s2/runtime.rs:4047-4118`).
   The separate live signer-fingerprint pin gap remains: `gpgv` accepts the
   supplied keyring without comparing the record signer to configured signer
   (`apt.rs:5394-5416,6752-6758`; `s2/runtime.rs:4047-4055`).

4. **Optional legacy selection seam: HIGH.** `PublishInputs.selection` and
   `selection_path` remain optional (`apt.rs:4601-4645`), and legacy
   `runtime.rs:3014-3165` still constructs both as `None`. The current
   `publish_suite` boundary rejects that route (`apt.rs:4772-4787`), but the
   legacy API has not been removed. f1 does not alter it.

5. **No native attestation claim.** The generated APT job's `.deb`
   attestations do not attest the native product manifest, provider release
   object, or cross-repository handoff. This fixture has no attestation call.

## Archive/Homebrew evidence boundary

The captured APT manifest advertises archive rows and hashes only. It does not
carry archive bytes or execute the Homebrew consumer. Independent exact native
384 evidence reports that the rendered archive members are all mode 0644 and
the real Homebrew consumer rejects them (`archive member is not executable:
velnorctl`). The canonical cross-check is
`G0/homebrew-contract/native-384-exact-crosscheck.md`, SHA-256
`ff6f37e380efc7eeb5721b654436d533ab5e854f179a824c01456a4a93f146a0`; the
native review is `G0/native-review/native-384e2e4e-independent-review.md`,
SHA-256 `c4c9732899be0c8d799277a72ea70f890103eca9cd94971fc0b5afa426af05a1`.

I independently inspected the preserved rendered fixture at
`/private/tmp/velnor-native-apt-fixture.dgFWFR`: `tar -tvzf` listed all
archive binaries as `-rw-r--r--`, not executable. Therefore this APT
manifest-only test must not be used to claim full native archive handoff,
Homebrew acceptance, installation, or runtime operation.

## Verification

```text
apt::tests                                      85 passed, 0 failed
native_assembly_manifest_is_consumable...        1 passed, 1760 filtered out
cargo check -p velnor-workflow                  passed
cargo fmt --all -- --check                      passed
git diff --check 02c..f1                        passed
generated APT actionlint + ShellCheck           passed (exit 0)
cargo clippy -p velnor-workflow --lib           0 errors, 12 baseline warnings
cargo clippy ... -- -D warnings                12 baseline non-APT errors
```

The f1 generated workflow was rendered into
`/private/tmp/apt-f1-generated.nzw7QI`; its only diff from the 02c generated
fixture is the expected runtime/policy revision pin. Actionlint with explicit
ShellCheck exited 0. No generated workflow or remote feed was executed.

The six unrelated full-library failures and 12 non-APT clippy diagnostics
recorded in the pinned 02c review are unchanged by this test/fixture-only
delta; the full library was not rerun for this bounded delta.

## Disposition

Accept f1 only as a bounded source test addition. Keep publication blocked:
the synthetic fixture does not repair provider/source authority, native
producer handoff, live signer pinning, or legacy optional-selection removal;
the independent native 384 archive-mode failure also prevents any claim of
full archive/Homebrew handoff. **No publication approval.**

