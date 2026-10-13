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
