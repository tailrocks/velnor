# Bootstrap hostile producer fixture

This is an untrusted candidate fixture for the G1 hosted-container canary. It is
independent of the generator owner and must never become a generator contract,
producer manifest, provenance source, or policy input.

The fixture deliberately probes nine hostile surfaces:

| ID | Probe | Safe expected result in the approved sandbox |
| --- | --- | --- |
| H1 | Process environment and `/proc/*/environ` token/name exposure | Only synthetic allow-list names; no values or credential names are emitted |
| H2 | GitHub command-file discovery/write (`GITHUB_ENV`, `GITHUB_PATH`, `GITHUB_OUTPUT`, `GITHUB_STATE`, `GITHUB_STEP_SUMMARY`) | Variables absent; any disposable command path is only a write probe |
| H3 | Direct artifact-service upload using runtime URLs | Real token values are refused; only an explicit synthetic canary token can authorize the fixed request, and the hosted boundary supplies no endpoint/token |
| H4 | Runner workspace, source, and Docker-socket writes | Fixed workspace/socket writes and `/input`/`/candidate` writes fail |
| H5 | DNS, IPv4/IPv6, metadata, proxy, and Unix-socket network paths | Connect/resolve attempts fail under `network=none` |
| H6 | Symlink and hardlink creation | Attempts are observable; trusted wrapper rejects the resulting output entries |
| H7 | Output abuse: sparse oversize file, inode flood, traversal, fake handoff/manifest | Disposable quota or post-copy validator rejects it; no candidate bytes become authority |
| H8 | PID namespace, status hardening, and bounded child/fork pressure | Candidate is PID 1, private process view, non-root/no-new-privileges/seccomp, bounded PIDs |
| H9 | Source/.git/cache mutation, cache image visibility, source rewrite/delete, and contract minting | Read-only source/cache paths reject mutation; fake output metadata remains untrusted |

## Files

* `probe.rs` is the malicious probe. It uses only Rust `std`; no network/client
  or credential dependency is compiled in.
* `build.sh` performs a deterministic local `rustc` build into a caller-owned
  disposable path. It has no package/network/bootstrap step.
* `fixture-contract.json` describes the fixture and expected containment checks.
  It is documentation, not trusted runtime input.
* `trusted-contract.schema.json` fixes the base-owned handoff fields and measured
  hash/image/resource limits. It is admitted only after the trusted wrapper
  verifies its exact SHA-256.
* `trusted-archive-check.py` rejects archive traversal, symlink/hardlink/device
  members, duplicate/ambiguous names, oversized members, and unsafe disposable
  output trees. It is base-owned wrapper code, not candidate code.
* `trusted-harness.sh` is a future Linux-hosted wrapper. It verifies the handoff,
  fixture files, final image index/platform/config digests, image `Config.Env`,
  Docker preflight, bounded execution, structured result, output rejection, and
  cleanup. Its success is diagnostic containment evidence only; it is not policy
  approval or provenance.
* `trusted-harness-negative-tests.sh` exercises schema duplicate/float/extra-key
  rejection, unreadable output-subtree rejection, unexpected regular-file
  rejection, and the non-Linux host gate. It never invokes Docker or the probe.

The approved sandbox base identity is recorded as the raw digest-pinned
`ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b`.
The future harness must admit the final image's exact platform-manifest and config
digests separately; this fixture never accepts a mutable tag or mints those
contract fields.

## Build and execution contract

Compile locally or in the trusted producer with:

```sh
bash build.sh /path/inside/a/disposable/staging-dir/velnor-hostile-producer
```

Static compilation is allowed locally. Do not execute the probe on the macOS
host, in a normal shell, under Docker/OrbStack, or in a unit test. The probe
refuses execution unless all of these are true:

* `--canary` is supplied;
* `HOSTILE_PROBE_ISOLATED=1` is supplied by the fixed hosted wrapper;
* the probe itself is PID 1; and
* `--output /output` is used.

The future hosted canary invokes only the measured binary in the approved
digest-pinned Docker boundary, for example:

```text
/candidate/velnor-hostile-producer --canary --output /output
```

The wrapper may set `HOSTILE_PROBE_WRITE_ROOT=/output` so command-file probes can
write only to disposable output. It must pass no credential, token, action
runtime URL, or runner environment deliberately. The probe reads an artifact
runtime token only to detect its name; it sends an `Authorization` header only
when the value is exactly the non-secret synthetic canary token and the explicit
synthetic-upload test flag is present. Any other token is refused before DNS or
connect. If GitHub injects endpoint variables, the probe records only fixed
status fields and never records values.

The one-line `VELNOR_HOSTILE_RESULT {...}` record contains fixed field names,
booleans, bounded counts, and fixed status words only. It never includes an env
value, URL, path, response body, token, source byte, or error string. The record
is emitted on stdout and is an untrusted diagnostic. If the normal sandbox uses
`--log-driver=none`, the canary may use a separately admitted bounded log sink
(for example one local log file capped at 1 MiB) solely to collect this line; it
must never enable an unbounded log driver. The trusted wrapper must separately inspect mounts,
exit/OOM state, output tree, image/config digest, and `/proc`; it must not accept
the record as provenance or as a policy contract.

The probe intentionally leaves hostile output entries when the disposable output
mount permits them. A successful symlink/hardlink/fake-contract/oversize attempt
is evidence for the wrapper's rejection test, not fixture success. The wrapper
must copy/inspect within its bounded policy and fail closed on symlinks, special
files, traversal, extra/missing files, quota breach, timeout, or cleanup failure.

The probe's artifact attempt uses a fixed cache-service `POST` path and a fixed
synthetic JSON body. It never sends a real token, cookie, environment, source
data, or candidate output. The future wrapper supplies no endpoint or token and
requires `network=none`; the endpoint/token branches are static hostile coverage,
not a request to enable network. DNS, IPv4/IPv6 loopback, metadata, proxy,
Unix-socket, fixed command-file, source-cache, and Docker-socket probes are all
bounded and emit status words only.

## Base-owned handoff and image contract

The wrapper receives every path and expected hash below from base-owned workflow
metadata, never from `fixture-contract.json`, candidate JSON, artifact names, or
probe output:

* handoff JSON and SHA-256; the strict schema validator rejects duplicate keys,
  wrong integer types, missing fields, and additional properties;
* clean source archive and SHA-256, producer artifact archive SHA-256,
  probe/build/fixture/schema/checker/harness SHA-256 values, and the measured
  Linux ELF binary SHA-256;
* exact source head/tree/closure, target/head repository names and numeric IDs,
  producer workflow/event/run/job/artifact IDs and exact names, artifact service
  digest (which must equal `sha256:` plus the re-hashed producer archive), exact
  binary member, archive digest, profile/features/platform. These API
  observations are supplied by the trusted acquire job; the fixture cannot mint
  or select them;
* final sandbox image name, raw multi-platform index digest, exact `linux/amd64`
  platform-manifest digest, config digest, base digest, and image-build revision.

`trusted-harness.sh` rejects a mutable tag, a platform/config mismatch, extra
image `Config.Env`, nonempty image entrypoint/cmd/workdir/volume/port/healthcheck,
unmeasured layer size, wrong labels, stale local RepoDigests, or any Docker
preflight mismatch. It uses only read-only `/input` and `/candidate` binds plus
64 MiB/4096-inode disposable `/tmp` and `/output` tmpfs mounts. The only other
mounts permitted are Docker's per-container generated `/etc/hosts`,
`/etc/hostname`, and `/etc/resolv.conf` files; arbitrary host paths, volumes,
devices, and extra tmpfs mounts fail preflight. The source archive is extracted
only after the trusted archive checker accepts it. Resource limits are 128 PIDs,
512 MiB memory with equal swap, one CPU, 64 MiB file size, 1024 file
descriptors, 16 MiB `/dev/shm`, a 300-second timeout, and a 1 MiB container log
cap. Any image, quota, timeout, copy, result, or cleanup failure is red; no
host-shell fallback exists.

The hostile result parser asserts every H1-H9 field, not just field presence:
zero forbidden names/tokens, absent command files/endpoints/proxies/sockets,
read-only source/workspace/cache writes (only `missing` or `denied` is safe),
bounded process pressure, no connected network path, expected hostile output
attempts, exact base-owned cache presence and readability masks, and
PID/capability/seccomp/no-new-privileges values. The
post-copy census has an empty allow-list for this hostile fixture: any regular
file/directory, symlink, hardlink, special file, missing required entry, unreadable
subtree, quota breach, or traversal error is rejected and counted in structured
evidence. No output is transport authority.

Local checks may run `bash -n`, JSON parsing, Python syntax/help, Rust static
compilation, and formatting. They must not invoke Docker/OrbStack/Velnor or run
the probe. Only a fresh GitHub-hosted Linux x86-64 canary may invoke
`trusted-harness.sh`.
