# GitHub Actions runner policy

Every executable workflow uses one canonical YAML shape on all lanes:

- The dispatch selector is the plural `lanes` choice input with exactly
  `velnor | github | both` (`lanes: type: choice, default: velnor,
  options: [velnor, github, both]`); never use a singular dispatch selector.
  Callable reusable workflows keep their singular `lane` input; callers derive
  it from `inputs.lanes`.
- Defaults are organization-derived, exactly as the marked contract states:
  every listed `jackin-project/*` repository defaults to `github`; every
  listed `tailrocks/*` and `ChainArgos/*` repository defaults to `velnor`.
  There is no universal Velnor default.
- The `github` lane is the pinned comparison and fleet-recovery lane and runs
  on `ubuntu-26.04`; never use `ubuntu-latest` or an unpinned Ubuntu label.
- `both` executes identical jobs and steps on both lanes.

Velnor jobs are admitted through runner group `velnor-trusted`; individual
runner selection uses label `velnor-target-mvp`. Do not conflate the group
with the selection label. Use the canonical inline `matrix.config` expression.
Only `matrix.config.writer` may gate mutating steps; it must guarantee exactly
one writer. Never branch step semantics by lane.

Final binary/package ownership after cutover is the `velnorctl` binary and the
signed-apt `velnorctl` package; the interim product binary/package remains
`velnor-runner` until Plan 079 removes it completely (no alias or shim
survives).

Rust compile jobs use mold and local-only sccache v0.16.0 with a 20 GiB bound.
The native adapter owns cache reporting. Do not combine target-directory
caches with sccache, compile CI tooling, or enable a remote cache backend.

Every job has a measured `timeout-minutes`; every workflow has concurrency and
an intentional cancellation policy. Checkouts are shallow and disable
credential persistence unless a documented writer step requires otherwise.

The GitHub lane is retained permanently so releases remain possible when the
Velnor fleet is unavailable. Changes to runner labels, lane matrices, actions,
or cache behavior must pass `velnor`, `github`, and `both` verification.

Never add a repository-controlled execution-backend input. Operator selection
is `[execution] backend = "docker"` or `"microvm"` in `execution.toml` per
daemon/pool, with no fallback.
