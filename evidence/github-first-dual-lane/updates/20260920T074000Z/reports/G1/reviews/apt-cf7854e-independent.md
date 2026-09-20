# Independent APT rereview: cf7854e842d3e587e7b1c524cd7ebee731a36366

Date: 2026-09-20

Scope: exact remote tip, source-only, read-only rereview of the delta over
e862e591. No source edits, test execution, release, install, publish,
dispatch, Docker/OrbStack/Velnor runtime, or hostile payload execution.

Remote evidence:

```text
git ls-remote origin refs/heads/dual-lane-apt-schema2
cf7854e842d3e587e7b1c524cd7ebee731a36366 refs/heads/dual-lane-apt-schema2

cf7854e842d3e587e7b1c524cd7ebee731a36366
e862e5913b7ead8c5bbb2111657dd697a6a23782
fix(apt): reject unverifiable provider and GPG statuses
```

The delta is one file: `crates/velnor-workflow/src/apt.rs` (281 additions,
18 deletions). The native producer workflow is byte-identical to e862:

```text
apt.rs at cf785                         fd8de70a9358b318b90354f224a455bc525feb8c
.github/workflows/release.yml at e862  5888aa8c78eaf5002806e1f0d648b5f30bcb300c
.github/workflows/release.yml at cf785  5888aa8c78eaf5002806e1f0d648b5f30bcb300c
```

## Verdict

**CHANGES REQUIRED / BLOCKED. No canary or publication approval.**

This tip closes important parts of the prior review: it rejects flat
pagination responses, binds repository owner ID/login/type through a provider
owner lookup, validates a bounded UTC timestamp shape, adds explicit GPG
status rejection, and asserts expected hostile-provider error classes. It is
not yet a complete acceptance proof. The GPG status set is incomplete for the
upstream status protocol; process exit/status combinations are not tested
through the new wrapper; the timestamp parser accepts trailing bytes after a
bare `Z`; provider raw schemas and attestation record cardinality remain
permissive; and the real native producer still does not publish the mandatory
`release-attestation.json` asset.

## Findings

### 1. High: GPG rejection still misses documented failure classes

The new `REJECTED_GPGV_STATUSES` contains:

```text
BADSIG ERRSIG EXPSIG EXPKEYSIG REVKEYSIG NO_PUBKEY NODATA FAILURE
```

`run_gpgv` scans status-fd stdout, then checks the process exit status; the
signer parser repeats the status scan before requiring exactly one pinned
`VALIDSIG` (`apt.rs:1658-1701,1726-1756`). This correctly rejects every listed
status whether the stub exits zero or nonzero.

The upstream GnuPG status contract also defines `KEYEXPIRED`, `KEYREVOKED`,
and generic `ERROR`; they are not in this list. GnuPG specifically says
`KEYEXPIRED` can be emitted for an expired key/subkey and `KEYREVOKED` for a
revoked used key, while `ERROR` is the generic failure status. See
[GnuPG's status protocol](https://github.com/gpg/gnupg/blob/master/doc/DETAILS)
and [gpgv's trust/expiry behavior](https://github.com/gpg/gnupg/blob/master/doc/gpgv.texi).

Therefore a synthetic output containing a valid `VALIDSIG` plus
`KEYEXPIRED`, `KEYREVOKED`, or `ERROR`, with exit status zero, passes both
`run_gpgv` and `gpgv_signer`. A nonzero exit still fails, but that does not
prove the required status gate. Add all policy-relevant classes and test the
matrix explicitly:

| stdout status | exit 0 | exit nonzero |
|---|---|---|
| listed rejected status | reject by status | reject by status |
| `KEYEXPIRED`, `KEYREVOKED`, generic `ERROR` | currently accepted if one `VALIDSIG` is present | reject by exit only |
| no `VALIDSIG`, no rejected status | reject later by signer cardinality | reject by exit |
| one valid `VALIDSIG`, no rejected status | accept | reject by exit |

The existing `gpgv_status_requires_one_pinned_primary` test feeds bytes only to
`gpgv_signer`. The command-shape test still calls `run_fixed`, not
`run_gpgv` (`apt.rs:10069-10122,10124-10204`), so it does not exercise either
the new status wrapper or exit-code combinations. No real Linux GPG primary,
subkey, expired, revoked, missing-key, malformed-data, or algorithm case was
run. This is source evidence only; it is not cryptographic acceptance.

### 2. Medium: the UTC timestamp validator accepts a suffix after bare `Z`

`valid_provider_timestamp` checks the first 20 bytes and returns true when
`bytes[19] == b'Z'`, without requiring `bytes.len() == 20`
(`apt.rs:2706-2751`). For example, all of these are accepted by the function:

```text
2026-09-20T00:00:00Zx
2026-09-20T00:00:00Zgarbage
```

Fractional timestamps do check the terminal `Z` and fraction length. The new
unit test covers ranges, offsets, missing fractions, and overlong fractions,
but not a suffix after a bare `Z` (`apt.rs:10042-10066`). Require exact length
20 for the no-fraction form and add NUL/control/trailing-byte negatives. The
UTC-only choice itself is coherent with GitHub's release response; the parser
must still enforce the complete string.

### 3. Blocker unchanged: native producer still cannot provide the required
release attestation

The parser still requires `release-attestation.json`, validates its exact
source/release/ref/commit/manifest/artifact bindings, and invokes the provider
attestation verifier for its raw bytes (`apt.rs:3150-3197,3270-3406`). The
native release workflow hash is unchanged from e862 and its `gh release create`
asset list still has no `release-attestation.json` (`.github/workflows/release.yml:4262-4379`).
It explicitly does not push or dispatch the APT consumer (`release.yml:4381-4383`).

The native assembly test still inserts synthetic provider IDs/assets and a
placeholder attestation only to parse a selection; it does not consume a real
native release or run a genuine attestation verification
(`apt.rs:9114-9160+`). Missing producer evidence therefore remains correctly
fail-closed, but there is no positive producer-to-APT handoff and no
publication/canary claim is possible.

### 4. Owner binding is structurally improved; User/Organization proof is
incomplete

`acquire_provider_release` now requires repository `owner.id`, `owner.login`,
and `owner.type`, then queries `users/{owner_login}` and requires equal
positive ID, login, and type (`apt.rs:3040-3073`). `validate_provider_facts`
accepts only `User` or `Organization` and binds the owner login to the first
component of the selected repository slug (`apt.rs:2890-2917`). The repository
ID itself is only required to be positive; it is not a configured expected
repository ID.

The endpoint choice is usable for public GitHub: the live raw response for
[`GET /users/github`](https://api.github.com/users/github) reports
`type: Organization`, while the organization-specific endpoint is
[`GET /orgs/github`](https://api.github.com/orgs/github). GitHub documents the
separate [users API](https://docs.github.com/en/rest/users/users#get-a-user)
and [organizations API](https://docs.github.com/en/rest/orgs/orgs#get-an-organization).
The implementation always uses `/users/{owner}` and does not validate the
owner response's canonical `url`/`html_url` or branch to `/orgs/{owner}` for an
organization. That is a contract/test gap, not evidence that `/users` cannot
return an organization.

The fixture covers only an Organization owner and only mutates its numeric ID
(`apt.rs:8840-8890,8968-8976`). There is no User-shaped fixture, wrong owner
login/type case, owner endpoint URL case, or real raw response capture. Add
both owner kinds and assert the selected route/response shape under the
intended provider contract.

### 5. Medium: provider raw REST schemas and attestation cardinality remain
permissive

The selection schema is exact at the top level and for selection assets. The
fresh provider responses are not exact schemas: repository, owner, release,
tag, compare, and asset objects are parsed by extracting required fields;
unknown fields and some wrong object shapes remain accepted. In particular:

- `provider_asset_rows(..., paginated = false)` accepts an array of arrays and
  flattens it, although the release endpoint's raw `assets` field is a flat
  array of asset objects (see the [GitHub release REST schema](https://docs.github.com/en/rest/releases/releases);
  `apt.rs:2793-2871`). Strict pagination is enforced only for the
  `--paginate --slurp` response. The new flat-pagination negative is good and
  rejects a flat top-level paginated response.
- No `X-GitHub-Api-Version` header is sent; current GitHub REST examples use a
  version header. The code relies on `gh` defaults and selected-field parsing,
  so a raw API shape drift is not pinned.
- Provider asset `digest` (present in current release API responses) is not
  checked. Downloaded raw bytes are independently hashed against the
  producer manifest and attestation, so this is not a substitute for those
  checks; it is still an unrecorded provider field.
- `verify_provider_attestation` accepts any one matching record from the
  returned JSON array and ignores extra conflicting/duplicate records
  (`apt.rs:3203-3270`). Missing/empty and all-wrong arrays fail, but one valid
  plus one wrong/duplicate record passes. The mock has no duplicate, missing,
  expired, or real-bundle cases.

The 17 hostile provider cases now assert expected error substrings for wrong
release/source/owner/tag, asset census and URL errors, duplicates, mixed and
flat pagination, draft state, invalid timestamp, forged attestation fields,
payload mutation, and invalid JSON (`apt.rs:8947-9112`). They still use the
forged `gh` stub; they do not prove cryptographic verification, owner User
semantics, exact raw REST shape, duplicate attestation rejection, or
GPG process status behavior.

## Verified improvements over e862

- GPG status policy was attempted with explicit status scanning, and the
  listed bad/error/expired/revoked/no-key/no-data/failure statuses are
  rejected when actually emitted.
- `gpgv` process exit status is now checked by the production wrapper after
  status scanning.
- Provider owner ID/login/type is reconciled between repository and owner
  endpoint, with `User`/`Organization` type restriction.
- Published timestamps are intended to be UTC RFC3339-shaped, with calendar
  and range checks.
- The `--paginate --slurp` provider response must be an array of page arrays;
  a flat page and mixed pagination shape now fail.
- Hostile provider assertions now check expected error classes instead of only
  checking that some error occurred.

## Required bounded acceptance work

No hostile or live command was run here. Before any G1/canary claim:

1. Extend the GPG rejection list/tests to cover `KEYEXPIRED`, `KEYREVOKED`,
   generic `ERROR`, and valid/invalid status-plus-exit combinations through
   `run_gpgv`; then run genuine Linux `gpgv` with primary/subkey, expiry,
   revocation, no-key, no-data, and malformed-input fixtures.
2. Fix the bare-`Z` timestamp length check; test trailing bytes and controls.
3. Decide and document the owner endpoint contract. Test both actual User and
   Organization response shapes, login/type/ID mismatches, and canonical owner
   URL/API fields; pin the REST API version if deterministic raw schema is
   required.
4. Make raw response shape and attestation record cardinality policy explicit:
   reject nested release assets, duplicate/conflicting attestation records,
   missing records, and unknown fields where the contract requires exactness.
5. Add a real native `release-attestation.json` release asset and consume it
   through a GitHub-hosted Linux canary. Verify every selected raw asset,
   genuine `gh attestation verify` bundle, source/ref/digest/signer binding,
   and all negative cases before persisting the handoff.

No canary, publication, or cryptographic approval is granted by this report.
