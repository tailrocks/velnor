# Independent source review — bootstrap prefetch prototype b24989d3

Date: 2026-09-20 (UTC/Asia/Ho_Chi_Minh)

Reviewed exact commit b24989d33c3cdae7b02c8b6479eb9d920c7591d7 on
codex/bootstrap-prefetch-prototype in:

~~~text
/private/tmp/bootstrap-prefetch-prototype
~~~

Commit tree: cbe52df2a88da70f346cfcfcba40157125b235dc.

Raw SHA-256:

~~~text
tools/bootstrap_prefetch.py      37f599fbedcae522e24f85fdacbc612d97701f23882de6c28a7380cd04593f29
tools/test_bootstrap_prefetch.py d9f7ddae8ec11db577eede89c02ba0e998d41315d73d6d722a1cff9f992b3628
~~~

No Docker/OrbStack/Velnor execution, untrusted network fetch, image
publication, dispatch, source edit, or shared-worktree mutation occurred.

## Verdict

**CHANGES REQUIRED; prototype is not bootstrap acceptance evidence.**

The prototype has useful bounded pieces: target manifests are rewritten to
empty .prefetch-targets/*.rs stubs; candidate Rust/build/test/script/config
bytes are not copied; manifest/lock parsing is strict in several places;
self-excluding manifest hashing and numeric-field validation exist; Git cache
and copy helpers reject many extras; and all seven owner tests pass.

The implementation does not yet bind the produced bundle to the reviewed
source, does not derive the closure count, leaves manifest source controls
incomplete, and accepts filesystem entries/inputs that the contract says must
be closed. Its network policy is explicitly declarative only, and the
network-fetch command fails closed. There is no image or network proof.

## Verification performed

~~~text
python3 -m unittest tools.test_bootstrap_prefetch -v
Ran 7 tests in 1.105s — OK
~~~

The tests use offline Cargo metadata and temporary fixtures. They do not run
Cargo network fetch, Docker, a final image, or a hostile candidate payload.

The prototype HEAD tree is:

~~~text
HEAD b24989d... tree cbe52df2a88da70f346cfcfcba40157125b235dc
requested 3ed0023b... tree 45be601efb57e8d9da424a07e9115beee93a1564
~~~

The prototype test still calls build_bundle with requested source
3ed0023b... while reading the current prototype worktree.

## Critical findings

### C1 — source identity is not bound to bytes

At bootstrap_prefetch.py:545-550, the code checks whether the requested
object exists:

~~~python
git rev-parse <source_head_sha>^{commit}
git rev-parse <source_head_sha>^{tree}
~~~

It does not require HEAD == source_head_sha, compare the working tree
to that tree, or read manifests from the requested Git tree. It then reads
the current filesystem at lines 556 onward.

Reproduction: build_bundle accepted the prototype worktree while passed
source_head_sha=3ed0023b... and source_tree_sha=45be601e...; the current
HEAD/tree were different. A second reproduction archived exact 3ed without
.git and passed all-zero/all-one fake identities; the function accepted them
because the identity check is skipped when .git is absent.

This permits a caller to mint a reviewed identity over different manifest
bytes. Fix by extracting the allowlisted blobs from the immutable requested
Git/API tree into a fresh directory and hashing those bytes, or require a
clean checkout whose HEAD, tree, index, tracked-file census, and no-untracked
state exactly match the reviewed object. Missing Git metadata must fail closed,
not accept caller-supplied identity fields.

### C2 — closure count is caller-minted

build_bundle(... producer_closure_nodes=...) only checks that the value is a
non-negative integer (534). validate_bundle checks the copied integer, not
Cargo output. Reproduction:

~~~text
build_bundle(... producer_closure_nodes=0)
validate_bundle(...)
accepted_closure_claim=0
~~~

The separate measure_closure helper is not linked to bundle creation or
manifest evidence. A caller can therefore mint the claimed 115-node contract.
Compute the count in a trusted, Linux/offline measurement, bind its canonical
Cargo-tree output/hash and exact full Git revision to the manifest, and reject
caller-supplied claims. The verifier must independently recompute it.

### C3 — workspace dependency source controls are incomplete

_validate_manifest_sources inspects top-level dependency tables but does not
inspect nested [workspace.dependencies] (or all other Cargo source-bearing
tables). Reproduction with a temporary manifest containing:

~~~toml
[workspace.dependencies]
evil = { git = "https://evil.invalid/other.git", rev = "<40-hex>" }
~~~

and an otherwise reviewed lock census was accepted:

~~~text
accepted_unreviewed_workspace_git=true
~~~

The implementation must validate every source-bearing dependency table,
including workspace dependencies and target-specific forms, against the
base-owned source allowlist. The expected_git allowlist is also caller
controlled by CLI --git-url/--git-rev; bind it to reviewed policy, not
operator-provided arbitrary values.

### H1 — dependency digest is not reusable dependency identity

At lines 653-660, dependency_contract_digest includes source_head_sha and
source_tree_sha. A source-only PR therefore changes the dependency digest and
cannot reuse the trusted builder cache. Source identity must be a separate
field/attestation; dependency identity must cover only reviewed
dependency/toolchain declarations and their exact bytes.

The toolchain_input_digest at lines 662-668 covers only the selected
toolchain file and target triple. It omits the base image, installer/archive
checksums, mise/build recipe, and direct toolchain resolution required by the
image contract. Bind those inputs separately and canonically.

### H2 — bundle field/census semantics are loose

_file_record calls its SHA-256 field blob_sha, although it is not a Git blob
SHA. Rename or define the field unambiguously.

validate_bundle's actual census (lines 825-837) records only regular files.
A temporary FIFO placed at the bundle root was accepted:

~~~text
accepted_extra_fifo=true
~~~

Unexpected special files and directories must fail closed. Add directory-count
and total-path limits, reject every non-directory/non-regular entry, reject
hardlinks in the bundle, and enforce bounds while building—not only after all
candidate manifests/stubs have already been written.

The source-side _lstat_regular check does not reject hardlinked manifest or
toolchain inputs. A temporary hardlinked Cargo.toml was accepted. Reject
hardlinks for source declarations or read immutable API/tree blobs instead.

### H3 — Git cache census misses working-tree integrity

git_census checks DB/checkout counts, revision, and commit tree equality, but
does not require the checkout working tree to be clean, contain no untracked
files, or have no modified files. A fetched checkout's HEAD can match while
Cargo reads modified files. Require a clean index/worktree and an exact
tracked-file census, or materialize the exact commit tree read-only. Bind the
actual public URL separately; matching a revision alone does not prove the
repository URL.

### H4 — target-neutral boundary is not yet generic

measure_closure defaults to crates/velnor-workflow/Cargo.toml, excludes only
package name velnor-workflow, and tests only the first eight characters of the
termrock revision. The package, feature set, target, closure selection, and
full revision must be explicit reviewed inputs. Do not hardcode Velnor package
names in a reusable boundary.

### H5 — sanitized bundle validation is not fully reconstructive

The builder rewrites target paths, but validate_bundle does not recompute the
expected sanitized manifest/stub set from the manifests. An attacker can
add/remove empty files under a permitted .prefetch-targets path while
remaining inside the loose census. Reconstruct the sanitizer output and
require exact equality. Bound target count and validate every path-bearing
Cargo field used by the fetch operation.

## Network and credential boundary

fetch_environment returns a scrubbed dictionary and the tests verify selected
variables are absent. This is only a data structure; it does not execute Cargo
under env -i. network_policy()["enforced"] == false by design, and
network-fetch always raises a gated error. The prototype therefore honestly
has no network-fetch proof.

The future job still needs an actual container/firewall egress allowlist,
credential-file census, no proxy/SSH helper, bounded DNS/hosts, and a
no-build/no-proc-macro proof. RUSTC=/bin/false is a useful fail-closed guard,
but it also means this prototype cannot serve as a successful Cargo fetch/image
builder until a trusted prefetch environment proves the exact Cargo behavior.
No image digest follows.

## Test fixture gaps

The seven passing owner tests do not cover:

- HEAD/tree mismatch, missing Git metadata, dirty/untracked worktree, or
  candidate manifest bytes changed after identity input;
- workspace dependency Git/path/custom-registry declarations;
- caller-controlled expected Git policy;
- source manifest/toolchain hardlinks and symlink parent/root cases;
- extra FIFOs/devices/sockets, extra directories, hardlinks, directory/path
  quotas, aggregate source/member/target limits;
- full Git checkout working-tree modifications and remote/source binding;
- exact full termrock revision (the test checks only rev[:8]);
- closure-count mismatch or candidate-supplied closure evidence;
- generic package/feature/target selection;
- sanitizer reconstruction and omitted/extra target stubs;
- credential files/config under HOME/CARGO_HOME and actual egress enforcement;
- clean Cargo cache/image-layer copy, Linux ELF build, final image config,
  runtime PID/env/proc/quota isolation, or hostile canary behavior.

## Disposition

Keep this commit as a static prototype only. Before using it in a trusted
refresh or image build, fix C1-C3 and H1-H5, add the missing negative tests,
bind closure evidence to independent Linux measurement, and implement the
external egress boundary. No candidate acceptance, image publication, digest
population, Docker execution, or G1 approval is justified by the current
prototype.

