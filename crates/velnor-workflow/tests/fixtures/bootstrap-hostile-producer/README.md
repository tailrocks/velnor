# Bootstrap hostile producer fixture

This is an untrusted candidate fixture for the G1 hosted-container canary. It is
independent of the generator owner and must never become a generator contract,
producer manifest, provenance source, or policy input.

The fixture deliberately probes nine hostile surfaces:

| ID | Probe | Safe expected result in the approved sandbox |
| --- | --- | --- |
| H1 | Process environment and `/proc/*/environ` token/name exposure | Only synthetic allow-list names; no values or credential names are emitted |
| H2 | GitHub command-file discovery/write (`GITHUB_ENV`, `GITHUB_PATH`, `GITHUB_OUTPUT`, `GITHUB_STATE`, `GITHUB_STEP_SUMMARY`) | Variables absent; any disposable command path is only a write probe |
| H3 | Direct artifact-service upload using runtime URLs | No runtime token is read or sent; network connect/upload fails under `network=none` |
| H4 | Runner workspace and authoritative source writes | Workspace paths are absent/unreachable; `/input` and `/candidate` writes fail |
| H5 | Direct network connect | Connect fails under `network=none` |
| H6 | Symlink and hardlink creation | Attempts are observable; trusted wrapper rejects the resulting output entries |
| H7 | Output abuse: sparse oversize file, inode flood, traversal, fake handoff/manifest | Disposable quota or post-copy validator rejects it; no candidate bytes become authority |
| H8 | PID namespace, status hardening, and bounded child/fork pressure | Candidate is PID 1, private process view, non-root/no-new-privileges/seccomp, bounded PIDs |
| H9 | Source/.git mutation and contract minting | Read-only source rejects mutation; fake output metadata remains untrusted |

## Files

* `probe.rs` is the malicious probe. It uses only Rust `std`; no network/client
  or credential dependency is compiled in.
* `build.sh` performs a deterministic local `rustc` build into a caller-owned
  disposable path. It has no package/network/bootstrap step.
* `fixture-contract.json` describes the fixture and expected containment checks.
  It is documentation, not trusted runtime input.

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
runtime URL, or runner environment deliberately. If GitHub injects any endpoint
variables, the probe records only fixed status fields and never records values.

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

The probe's network attempt sends at most a synthetic unauthenticated empty-body
HTTP `POST`; it never sends `Authorization`, token, cookie, environment, or
source data. It is only reachable in the future `network=none` hosted canary;
do not run it with network access.
