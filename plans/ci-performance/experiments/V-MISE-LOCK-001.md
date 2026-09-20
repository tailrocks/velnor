# V-MISE-LOCK-001: enforce declared tool locks across providers

Status: deterministic and actual-runtime probes passed; independent contract
review passed. Exact-push CI and immutable consumer rollout pending. Baseline `b6b4f2e14ab035def118612596df28e1f10d148b`.
Candidate generator revisions: schema 1 = 58, schema 2 = 60.

## Structural cause and alternatives

Hosted check profiles delegate installation to the pinned Mise action, which
adds locked installation when a lock exists. The Velnor profile renderer uses
a separate plain `mise install` command. Configuration validation also allowed
declared tools when the lock-key set was empty. The supposed shared tool
contract therefore had different enforcement across providers.

Alternatives: enforce a pinned lock at configuration and installer boundaries;
require every consumer task to implement its own lock checks; or trust Mise's
`settings.lockfile` alone. The first repairs the shared generator contract.
Consumer wrappers duplicate policy, and the runtime probe disproves the last
option. The existing pinned action behavior is documented in its
[primary source](https://github.com/jdx/mise-action/tree/c2a87611a18de5b3828c5652fe268e992400cb5c).

## Intervention

Both configuration schemas validate tool syntax and duplicates before requiring
non-empty lock keys and exact key membership. Both Velnor renderers add
`--locked`. A profile without a tool declaration remains valid. This unit does
not disable task auto-install: that depends on the separate transitive-tool
model in V-MISE-CLOSURE-001.

## Evidence and limits

Parent independently reproduced the actual installer boundary with Mise
2026.9.11 on macOS ARM64. The fixture declared Python 3.14.7, enabled lockfile
settings, supplied an empty lock, and isolated Mise directories. Offline mode
prevented network installation in both conditions:

- Plain `mise --yes install python` attempted installation and failed because
  offline mode was enabled.
- `mise --yes --locked install python` failed because Python was absent from
  the lock. The lockfile remained unchanged in both conditions.

Raw results: `observations/mise-locked-parent-probe.json`. These are behavior
probes, not a speed benchmark. They do not prove cache warmth or successful
installation of every platform's tools.

The isolated candidate passed 70 focused check-profile tests, all 1,787 library
tests, all-target Clippy, and formatting. Velnor regeneration changes ownership
revision only because it declares no affected check profile; temporary Jackin
regeneration succeeds with its existing auto-install override preserved.
Consumer delivery still depends on the reviewed immutable runtime rollout.

Independent contract review by `/root/parallax_inventory` confirmed both-schema
missing-lock and duplicate rejection, the emitted locked command, and exclusion
of the deferred auto-install change. Parent separately tested the isolated
complete patch and the real Mise boundary described above.

## Published identity

Local signed-off commit `64cef5ef93a90660983b3f2ec9dc42cfebda73c3` could not push
because the SSH agent failed signing. The independently reviewed API fallback
published identical tree `971c60e49840504b5948542dd232d9e2294b61ab` as
`057ed827ab487a8b7b818ab95a4b3c24dff4fe69`, preserving parent and trailers.
The clean default-feature build reports that exact published revision and
candidate closure `323c62fdd1423cd428411cd3a331c172c80a5d58b0eb526ba1cf6b0a8a97cf0b`.
A lean no-default-features build has a different, valid identity and cannot
satisfy the current default-feature candidate exception; its check rejection
is retained as a compatibility observation, not a generator-drift result.

Exact-revision runs started:
[PR 35490258957](https://github.com/tailrocks/velnor/actions/runs/35490258957) and
[policy 35490256957](https://github.com/tailrocks/velnor/actions/runs/35490256957).
PR run concluded cancelled after a prior generator test failure: integration
fixture `an_unknown_event_stops_generation` declared tools without a lock, so
strict lock validation preceded its expected unknown-event error. Job
`106023966065` failed at 05:00:06 UTC, before the next push. Raw complete jobs
and the failure excerpt are retained. Fixture repair and complete integration
target replay are required; this is not a successful performance baseline.
Separate policy outcome remains pending.

## Integration fixture repair

The canonical scheduled-check fixture now declares and locks ripgrep 14.1.1;
the unknown-event test first generates its valid baseline before changing the
event. Parent independently verified all 1,916 nextest tests with the initial
fixture prerequisite repair, then all seven scheduled-profile tests after the
canonical lock and explicit positive-control refinement. All-target Clippy
passes. No production validation was weakened. Exact-push CI of this repair
remains pending.
