# Exact d9 G2 applicability hostile proof

## Scope

Read-only offline CLI review of checker commit
`d9a277938d54d93f06b85ce6e8d406ebb67468c3`, whose implementation parent is
`2ba66b116dd5511f0b4f2a6856cfbed6bd290152`. The detached tree was clean.
No source edit, release endpoint, package manager, host, Docker, or OrbStack
operation occurred. This fixture proves checker behavior only; its synthetic
release is not a product publication or G2 fleet claim.

The exact source has the relevant branch in
`crates/velnor-tools/src/evidence_check.rs:3006-3094`:

- record applicability is rejected against the reviewed row only when that
  row equals `Required`;
- a non-`Required` record with a nonempty justification returns before
  `check_canonical_release`, including `check_install_evidence`;
- `Applicable` is therefore not treated as an authoritative requirement.

## Reproducible fixture and command

Persisted harness and inputs:

- [harness.sh](</Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-g2-applicability-cli-d9/harness.sh>)
- [`fixtures/`](</Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-g2-applicability-cli-d9/fixtures/)
- case reports under `fixtures/results/` and the per-case copied inputs under
  `fixtures/results/<case>/`

The harness starts from one exact-schema passing G2 input, mutates only the
reviewed applicability or install operation, and invokes:

```sh
CHECKER=/private/tmp/g3-checker-g2-app-d9/target/debug/velnor-tools
CHECKER="$CHECKER" rtk proxy /bin/zsh \
  /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-g2-applicability-cli-d9/harness.sh
```

The canonical release document has an externally computed digest
`sha256:b477abf048b380b2c7e6cd661da42e0743df46491e173a4af33b462b0d003cb3`.
The exact checker binary SHA-256 is
`0ba5d0360dd84d741043df3cf8d8526953e6da2afbb54535398ab9bbe5362aa7`.

## Observed exact results

`fixtures/results/results.ndjson` records these CLI results:

| Case | Intended mutation | Exit/status | Findings |
| --- | --- | --- | --- |
| `baseline` | Valid typed release/install, with clean install, same-channel upgrade, channel switch, target, binaries, and service applicability | `0 / pass` | 0 |
| `applicable-install-na` | Reviewed release applicability `Applicable`; record install `NotApplicable` plus justification; operations/environment/installed proof removed | **`0 / pass`** | 0 |
| `applicable-release-install-na` | Reviewed `Applicable`; record release `NotApplicable` and install `Excluded`, both justified | **`0 / pass`** | 0 |
| `required-release-applicable` | Reviewed `Required`; record release `Applicable` plus justification | `1 / fail` | `release-applicability` |
| `required-install-applicable` | Reviewed `Required`; record install `Applicable` plus justification | `1 / fail` | `install-applicability` |
| `required-install-na` | Reviewed `Required`; record install `NotApplicable` plus justification | `1 / fail` | `install-applicability` |
| `required-install-excluded` | Reviewed `Required`; record install `Excluded` plus justification | `1 / fail` | `install-applicability` |
| `required-missing-upgrade` | Reviewed `Required`; remove both upgrade/switch predecessors | `1 / fail` | 2 × `install-operation` |
| `required-platform-mismatch` | Reviewed `Required`; change installer architecture to `arm64` while installed target remains `linux-x64` | `1 / fail` | `installed-identity-mismatch` |

The two bold rows are the defect: a reviewed `Applicable` release can waive
the entire install contract. The successful cases contain no clean-install
environment, predecessor identities, upgrade/channel-switch operations, or
installed binaries after mutation. The checker returns before those checks.
This is an actual d9 CLI result, not a source-only prediction.

The `Required` rows show the useful existing boundary: any record value other
than `Required` is rejected when the reviewed row is `Required`, including
`Applicable` and `Excluded`. The finding message says “not-applicable” but the
predicate rejects all non-`Required` values. This does not repair the
`Applicable` reviewed-row path.

## Required applicability contract

Applicability must be derived from reviewed product/source policy, not from a
result row. The corrected checker needs these invariants:

1. Reviewed release applicability `Required` **or** `Applicable` requires
   typed release evidence with `applicability=Required` and complete canonical
   producer/publication proof.
2. For an installable product release, reviewed install applicability
   `Required` **or** `Applicable` requires `install.applicability=Required`.
   A record may not change it to `Applicable`, `NotApplicable`, or `Excluded`,
   even with a nonempty justification.
3. Reviewed `NotApplicable`/`Excluded` can waive execution only when the
   authoritative reviewed policy itself carries that state and reason. A
   record-side reason is an observation of policy, not policy authority.
4. A required install runs the full contract for every authoritative
   applicable target/channel: clean install, same-channel upgrade from a
   distinct older release, channel switch from a distinct channel/version,
   package/binary target identity, and functional/service result. No early
   return may bypass these checks.
5. Release, install, and runtime-artifact applicability are separate typed
   policy dimensions. The current `ManifestRepository` exposes only
   `release_applicability`; it has no authoritative installability field. Add
   a reviewed product/package policy or typed `install_applicability` derived
   from the source release contract before allowing a record to claim a
   runtime artifact is non-installable.

### Runtime artifact versus product release

“Runtime artifact is not installable” must be a source-policy classification,
not a record escape hatch. A runtime-only helper or generator artifact may be
`NotApplicable`/`Excluded` only when the reviewed producer policy identifies
its product/component/target scope, says why no client install applies, and
binds that policy to the reviewed source/config digest. A product release that
is applicable to APT/Homebrew clients remains install-required even if a
separate runtime helper is runtime-only.

Required schema-next negative pair:

- mark the product release/installable package in authoritative policy as
  applicable, then submit record `install=NotApplicable` or `Excluded` with a
  plausible “runtime-only” reason; fail;
- mark only a distinct runtime helper artifact as non-installable in the
  authoritative policy, then submit its exact non-applicable record; accept
  only if the release/component scope, target, reason, and policy digest all
  match. A record cannot transfer that exemption to the installable product.

The current exact-schema fixture intentionally uses the first pair through
reviewed `release_applicability=Applicable`; it does not invent a new field
or claim that a future policy schema already exists.

## Stage boundary

These CLI cases are G2 only. At G0, `check_record` returns immediately after
`check_g0_record` (`evidence_check.rs:2242-2245`); release/install objects are
not execution evidence. Do not run these mutations at G0 or treat a G0 pass
as proof of release applicability, installation, upgrade, channels, targets,
or runtime artifact policy. No live G7 conclusion follows from this offline
proof.

