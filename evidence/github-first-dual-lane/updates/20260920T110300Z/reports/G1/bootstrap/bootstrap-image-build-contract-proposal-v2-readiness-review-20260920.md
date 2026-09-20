# Bounded readiness review — bootstrap image proposal v2

Date: 2026-09-20 (UTC/Asia/Ho_Chi_Minh)

Canonical evidence paths verified from the repository workspace:

```text
/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap/bootstrap-image-build-contract-review-20260920.md
/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap/bootstrap-image-build-contract-proposal-20260920.md
/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap/bootstrap-image-build-contract-proposal-v2-20260920.md
```

Verified SHA-256:

```text
review (restored original): 6f5ce0d29e9bcfda5bc20115e9a4ab3ea687645031a8bfc9ddfde463eca277ce
original proposal:          d08a0468066d2bf2331a6417f0c930806b5a384fa4de81a753b36f505c2aadb5
proposal v2:                2aa209303a86a8fc099bd9d4667345e56089ef18c1ceabeacc451602ba47e545
```

The amended v2 bytes are preserved separately. The restored review remains
independent evidence; no owner amendment is treated as reviewer approval.
No Docker/OrbStack/Velnor operation, image pull/build/push, dispatch,
publication, source mutation, or host mutation occurred.

## Verdict

**NOT READY; CHANGES REQUIRED.**

The v2 state graph is materially better: it separates lock-input acquisition
from candidate execution, rejects candidate code/tokens during networked
prefetch, separates dependency/toolchain/recipe digests, and specifies image
subject binding and measured copy quotas. It still has one demonstrated
execution blocker and several strict-contract defects. Keep both image digests
empty. This is a design review, not a G1 approval.

## Demonstrated blocker: manifest-only Cargo fetch fails

The v2 recipe at lines 390–468 copies workspace manifests and `Cargo.lock`,
omits all source, then runs:

```sh
cargo fetch --locked --manifest-path "$RUNNER_TEMP/bootstrap-lock-input/Cargo.toml"
```

I reproduced the exact operation read-only in a disposable `/tmp` directory
from source `3ed0023b038335d7b22dfa2758457e3808f777ee`:

```sh
git archive 3ed0023b038335d7b22dfa2758457e3808f777ee \
  Cargo.toml Cargo.lock rust-toolchain.toml \
  'crates/*/Cargo.toml' 'tools/*/Cargo.toml' | tar -x -C "$LOCK_INPUT"
CARGO_NET_OFFLINE=true cargo fetch --locked --offline \
  --manifest-path "$LOCK_INPUT/Cargo.toml"
```

Result: exit `101` before fetching. Cargo rejected the copied workspace member
`crates/velnor-model/Cargo.toml` because it has no target after its source tree
was omitted:

```text
failed to parse manifest at .../crates/velnor-model/Cargo.toml
no targets specified in the manifest
either src/lib.rs, src/main.rs, a [lib] section, or [[bin]] section must be present
```

This is the normal Cargo manifest contract, not a network/cache failure; see
the official [Cargo manifest target documentation](https://doc.rust-lang.org/cargo/reference/cargo-targets.html).

As a bounded control experiment, I generated empty **trusted** `src/lib.rs`
stubs in the disposable copy for each allowlisted workspace manifest, without
copying candidate source, then reran the same offline fetch. Result: exit `0`.
This proves a safe implementation direction, but it is not yet part of v2 and
is not evidence of a builder image.

### Required repair

Choose and specify one exact implementation before approval:

1. Generate a base-owned synthetic prefetch workspace. Parse only the
   allowlisted manifests, reject absolute/`..`/out-of-bundle target paths and
   unreviewed path/git/registry sources, and create empty trusted target stubs
   at validated paths. Never copy candidate `.rs`, `build.rs`, proc-macro,
   test, script, or example bytes. Run `cargo fetch --locked` only against
   that generated workspace, with build/compiler execution disabled and a
   bounded stub census; or
2. Generate a standalone base-owned fetch manifest containing exactly the
   selected package's normal/build dependency declarations, with a trusted
   target stub and a lock-compatibility check. Prove that its fetched graph
   contains every dependency required by the actual producer command; or
3. Use a separately reviewed lock/source resolver that does not ask Cargo to
   parse a workspace with missing targets.

The chosen path must be tested against this exact source and a hostile manifest
fixture. It must prove no candidate build script/proc-macro/source executes
while network access is enabled. A statement that the bundle contains only
manifests is insufficient because the shown command currently fails.

## Additional contract defects

### 1. Schema types and self-hash are underspecified

The v2 JSON example uses string placeholders for
`workspace_manifest_count` and `lock_package_count` (lines 415–416), while the
text requires observed integers. Define a strict schema with JSON numbers and
reject quoted values. Define `bundle_sha256` over a canonical payload that
excludes the `bundle_sha256` field itself (or hash a separate bundle byte
stream); otherwise the manifest hash is self-referential and cannot be
recomputed. Define whether `sha256:` prefixes are part of the stored digest
type consistently across dependency, toolchain, recipe, and bundle fields.

### 2. Network allowlisting is only prose

`env -i` removes ambient credentials, but it does not restrict egress. The
trusted fetch job needs an explicit network policy allowing only the reviewed
Cargo sparse/index and crate hosts plus the exact public Git source transport,
with DNS/proxy behavior bounded. Fail if Cargo follows any extra source. The
lock validator must bind all registry URLs and the one `termrock` URL/revision;
the network layer must enforce the same set. A public token-free path is
feasible, but `permissions: {}` and an empty environment are not an egress
firewall.

### 3. Git census check is incomplete

The proposed `termrock_db` command uses `find ... -print -quit`, validates only
the first `termrock-*` database, and does not reject extra Git databases. The
implementation must enumerate the complete Cargo git DB/checkouts, require
exactly the reviewed source set, reject zero/multiple matching revisions and
any unreviewed repository, then verify each accepted checkout's HEAD/tree and
the recorded URL/revision. Record a deterministic DB/source census; do not
trust a guessed directory name.

### 4. The v2 artifact contains a contradictory unsafe example

The earlier acceptance block still shows unbounded:

```sh
cp -R --dereference /opt/velnor/cargo-home/. /target/cargo-home/
```

Amendment B correctly says this must not be the cache proof and requires a
recipe-pinned wrapper with census/limits. Resolve the contradiction before
freezing the proposal: remove the old command or mark it explicitly invalid;
the only normative path must be the trusted wrapper, no-following copy method,
cache byte/file/path limits, and measured peak inequality.

### 5. Prefetch scope needs an explicit graph contract

`cargo fetch` at the workspace root can hydrate more than the producer's 115
normal/build nodes, while `Cargo.lock` has many workspace/dev packages. Define
whether the image intentionally contains the full workspace lock closure or
only the `velnor-workflow` normal/build closure. In either case, record the
selected package/features/target, expected node count, registry checksum
census, and exact `termrock` object. The later offline proof must assert the
same scope; “lock package count” alone is not the 115-node producer proof.

### 6. Base/API/artifact authority still needs an executable boundary

The base-owned workflow inputs (`head_sha`, `pr_number`, `base_sha`) are
untrusted values until API/tree responses bind repository identity, PR
head/base, tree, and immutable artifact bytes. Specify the read-only API
credential or public-read assumption, response hashes, artifact service
boundary, and TOCTOU handling. `permissions: {}` must not silently depend on
an ambient API or Actions runtime token. The Cargo process may receive no such
token; artifact transfer must be separately scoped and verified.

## Ready criteria

Proposal-v2 becomes reviewable again only after all of these are supplied:

- a tested source-free Cargo prefetch construction that succeeds for exact
  `3ed0023b...` and hostile manifests;
- strict numeric/self-hash schema and canonical bundle bytes;
- enforced egress/source allowlist, complete Git/registry census, and no-secret
  fetch logs;
- one normative bounded wrapper replacing the old `cp -R` example;
- explicit producer dependency scope and 115-node Linux proof;
- executable API/artifact identity and TOCTOU checks;
- fresh `linux/amd64` image manifest/config/platform/provenance evidence and
  hosted canary evidence, all still gated after independent review.

Until then: **CHANGES REQUIRED, no approval, no digest population, no
publication, no Docker/OrbStack/Mac runtime.**

