# Integration refresh supplied by the user

Recorded by this coordination workstream at 2026-09-22T23:52:14Z.
Source: the user's integration-context message in this session.
These are observed reports, not desired values. This workstream did not independently
query GitHub, either host, or the preview service.

## Product and preview

- Fresh remote `tailrocks/velnor` main: `6391a0ce53b7e666d1bfb391083011622158f4f3`.
- Live preview run: `35797775098`, reported `in_progress`.
- Run reference: https://github.com/tailrocks/velnor/actions/runs/35797775098
- The run's source commit, attempt, artifacts and conclusion were not supplied.
- Stale rolling preview targets `f7bc191`. This prefix must never be selected in
  the preview lock. A successful later run alone cannot approve that stale target.
- Main HEAD and a running workflow are not a published, verified preview release.

## Bastion

Observation time supplied: `2026-09-22T23:44Z`.

- Docker `29.8.1`; installed stable Velnor `0.1.274`.
- 16 slots reported; zero ready; GitHub unreachable.
- Quotas remain: `CPUQuota`, `MemoryHigh`, `MemoryMax`.
- Velnor daemon inactive.
- This refresh does not supply new binary/image hashes, exact quota values,
  routing configuration, service persistence or secondary-disk observations.

## Mac

Observation date supplied: `2026-09-23` MYT (UTC+08:00); exact time not supplied.

- Apple M5 Max, host `arm64`; OrbStack engine `arm64`.
- No live supervisor.
- State reports `N=1`; monitor reports `N=4`.
- Neither value is a desired or measured accepted capacity.
- No fresh installed version, artifact digest, workflow placement or qualification
  result was supplied.
