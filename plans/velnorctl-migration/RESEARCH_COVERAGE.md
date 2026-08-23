# Research coverage verification

## Result

The supplied research contained 79 executable leaf commands. A later operator
decision removes the complete five-command release family and requires Debian
native package/version management instead. The plan retains every other
research command and adds the service entry point needed to remove the old
binary:

```text
79 supplied leaf commands
-5 removed release commands
74 retained supplied leaf commands
 1 velnorctl daemon command required for complete velnor-runner removal
75 command tasks total
```

Every retained leaf command has exactly one implementation task. No task owns
two commands.

## Command-tree coverage

| Research surface | Command tasks |
|---|---|
| `get hosts|instances|slots|runners|jobs|runs|queue|events|reservations|leases` | C006–C015 |
| `describe` | C016 |
| `logs` | C017 |
| `events` | C018 |
| `top` | C019 |
| `wait` | C020 |
| `doctor` | C021 |
| `preflight` | C022 |
| `reconcile runners|jobs|docker|storage` | C023–C026 |
| `cordon|uncordon|drain|resume|restart|recycle|scale` | C027–C033 |
| `run list|view|watch|cancel|rerun|logs|download|dispatch|open` | C034–C042 |
| `storage status|paths|du|gc|history|reservations|leases|explain-pressure` | C043–C050 |
| `config view|validate|diff|sources` | C051–C054 |
| `context list|current|use|set|delete` | C055–C059 |
| `auth status|check` | C060–C061 |
| `instance init|install|apply|delete` | C062–C065 |
| `capability list|explain|check|export` | C066–C069 |
| `adapter list|describe|check` | C070–C072 |
| `workflow check` | C073 |
| `diagnostics bundle` | C074 |
| `version|api-resources|explain|completion|man` | C001–C005 |
| Architecture-required `daemon`/`--once` | C075 |

## Operator-directed removal

The five supplied release status/verify/activate/rollback/history leaves have no
command tasks and no replacement namespace. Plan 076 deletes their old runner
handlers and custom domain machinery. Debian-native equivalents remain normal
host operations:

| Need | Authoritative native surface |
|---|---|
| installed/candidate version | `dpkg-query` and `apt-cache policy` |
| installed-file integrity | `dpkg -V` |
| install/upgrade | exact signed apt package transaction |
| rollback/downgrade | exact signed predecessor through apt's downgrade path |
| transaction history | apt/dpkg logs and package database |

These are not Velnor CLI commands or API resources. General inspection and
diagnostics may report bounded read-only Debian results without managing them.

## Non-command research coverage

| Research requirement | Owning plan(s) |
|---|---|
| Precise host/instance/slot/runner/job/run/queue/event/reservation/lease/capability/adapter nouns | 065, 069 |
| Stable slot versus ephemeral JIT runner distinction | 065, 069, 073, C008, C009, C032 |
| Versioned serializable resources, conditions/reason/message/transition time | 065 |
| Global output/watch/context/filter/time/color/verbosity conventions | 065 and every command task |
| Mutation dry-run/yes/force/reason/timeout conventions | 065 and mutating command tasks |
| Unix-socket API, read/admin authorization, streams | 067 |
| Thin Clap handlers; shared business logic crates | 064 and every command task |
| Sanitized SQLite operational history at `/var/lib/velnor/state.db` | 066 |
| GitHub authority for runs, queues, cancellation, artifacts, completed logs | 069, 070, 074 |
| Active local logs, completed Results Service logs, artifact fallback | 070, C017, C039 |
| Normalized lifecycle/health events and timing metrics | 066, 071 |
| Read-only doctor and explicit repairs | 072, C021–C026 |
| Graceful drain, cordon, recycle, dynamic scale, signal/watchdog behavior | 073, C027–C033, C075 |
| Canonical storage accounting, leases, reservations, pressure, safe GC | 075, C043–C050 |
| Configuration precedence, contexts, auth checks, protected secrets | 068, C051–C065 |
| Debian-only package/version management and removal of custom release domain | 076, 079 |
| Strict capability/native adapter/transitive workflow analysis | 077, C066–C073 |
| Sanitized diagnostics archive | 078, C074 |
| Remote HTTPS/mTLS contexts and multi-host views | 080 |
| MVP/next/later prioritization | Priority fields in command index and dependency sequence in main README |
| Existing-command migration and final removal | C021–C026, C043–C050, C062–C075, Plan 079 |

## Verified deviations

Research statements changed only where requested terminal architecture or later
operator direction requires it:

1. `velnor-runner` does not remain worker/daemon. C075 provides `velnorctl
   daemon`; Plan 079 deletes the old crate/binary/package.
2. No compatibility cache or command aliases remain.
3. Old single-worker `run` becomes service-only `daemon --once`; `velnorctl run`
   remains the GitHub workflow-run namespace.
4. `instance install` never installs the Velnor package; it materializes an
   instance from the already apt-installed package.
5. All five Velnor release commands are removed. No release resource, API,
   event, state table, active-target pointer, history store, activation, or
   rollback service remains. Signed apt/dpkg are the only installed-version
   management path.
6. Package build/sign/publish may remain maintainer CI/tooling, but it cannot
   select or mutate a host's installed version.
7. `diagnostics bundle` uses `--archive <path>` because global
   `-o/--output` already selects render format. Keeping both `--output`
   meanings would make the Clap contract ambiguous.

## Research prioritization preserved

### MVP

`version` C001; `completion` C004; `get instances|slots|runners|jobs` C007–C010;
`describe` C016; `logs` C017; `doctor` C021; `preflight` C022; `run
list|view|watch` C034–C036; `storage status|du` C043/C045; and `config
view|validate` C051/C052.

### Safe management

`events` C018; `wait` C020; `reconcile` C023–C026;
`drain|resume|restart|recycle` C029–C032;
`storage gc|reservations|leases` C046/C048/C049; and diagnostics bundle C074.
Package transitions are deliberately outside `velnorctl` and follow Plan 076's
signed apt/dpkg validation.

### Later control plane

`cordon|uncordon` C027/C028; dynamic `scale` C033; remote/fleet `get hosts` C006
plus Plan 080; declarative `config diff` C053 and `instance apply` C064; durable
history Plan 066; fleet `top` C019 plus Plan 080; workflow compatibility C073;
and adapter inspection C070–C072.

Other retained supplied commands remain individually planned according to their
owning service dependency; this stage map does not remove them.

## Initial operator center preserved

- `velnorctl get slots`: C008
- `velnorctl get jobs --active`: C010
- `velnorctl describe job/<id>`: C016
- `velnorctl logs job/<id> -f`: C017
- `velnorctl doctor`: C021
- `velnorctl drain instance/<name>`: C029
- `velnorctl reconcile runners --dry-run`: C023
- `velnorctl run watch <run-id>`: C036
- `velnorctl storage status`: C043

## Coverage audit procedure

Before accepting plan edits:

1. Extract every `# Command Task C...: Implement ...` heading.
2. Assert IDs are exactly C001–C075 with no gaps or duplicates.
3. Assert the original 79-command set, after removing exactly the five release
   leaves, equals task headings after removing C075 daemon.
4. Assert no command/help/completion/man/API-resource plan introduces a Velnor
   release namespace.
5. Assert every task contains one command heading, required behavior, focused
   test gate, mandatory fixture integration, done criteria, and STOP conditions.
6. Assert every command-index link resolves and Plan 079 depends on all 75
   command tasks.
