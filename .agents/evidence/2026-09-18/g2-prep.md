# G2 CLOSE-OUT PREP — bastion campaign (read-only design, no execution)

Status: **prep only** — author designs, verifier executes at G2 gate.
Authority: `plans/bastion-three-provider-ci/spec.md` (§1–§9).
Steps: `plans/bastion-three-provider-ci/work-plan.md` STEP G2.
Acceptance: `plans/bastion-three-provider-ci/checklist.md` G2 row.
Inputs: `plans/bastion-three-provider-ci/evidence.md` (revalidation only).
Gate precondition: G1 signed off (transitively A0–F2). Hard order Velnor → Jackin → ChainArgos → onboarding.

Conventions used below:
- `$V`, `$J`, `$C` = clean checkouts of `tailrocks/velnor`, `jackin-project/jackin`,
  `ChainArgos/java-monorepo` at their final qualified SHAs (recorded per §4.1 of this doc).
- `$B` = bastion `root@37.27.110.241` over SSH. Read-only unless a step says LIVE-WRITE.
- `$PIN` = final published generator pin; `$VERSION` = final installed `velnor-runner` version.
- Every proof command below is **fail-closed**: exit nonzero = gate FAIL. `rg` = ripgrep.
- Verifier ≠ author for every section. Verifier reruns, not rereads.

---

## 1. Mechanical no-regression proofs across all three trees

Goal (checklist G2): prove on every final tree — no VM/Go/quota/reservations/legacy/hand-YAML.
Run §1.1–§1.7 in EACH of `$V`, `$J`, `$C`, then §1.8 once on `$B` (live quota proof).

Setup per tree (record SHAs first):

```sh
# in $V / $J / $C respectively; exactly one SHA each, no dirty tree
git rev-parse HEAD && git status --porcelain | tee /tmp/g2-status-<repo>.txt
test ! -s /tmp/g2-status-<repo>.txt   # must be empty: clean tree
```

### 1.1 No VM / libvirt / KVM / QEMU runner infra

Spec §1, §4, §7: kernel/rootfs *staging* is not a VM; genuine KVM-runtime tests stay
declared-separate, never provisioned on bastion.

```sh
# 1.1a source-wide: no VM runner-infra references (allowlist: docs + declared-separate KVM test decls only)
rg -n --no-heading -i 'libvirt|qemu|/dev/kvm|virsh|virt-install|cloud-init|vagrant|vm\.spawn|create_vm|provision_vm' \
  --glob '!plans/**' --glob '!*.md' --glob '!*.mdx' . | tee /tmp/g2-vm-<repo>.txt
test ! -s /tmp/g2-vm-<repo>.txt

# 1.1b generated workflows: no VM steps (no exceptions)
rg -n --no-heading -i 'libvirt|qemu|kvm|vagrant|virtualbox|vmware|hyperv|macros\[.*vm' \
  .github/workflows/ | tee /tmp/g2-vm-gh-<repo>.txt
test ! -s /tmp/g2-vm-gh-<repo>.txt

# 1.1c bastion package surface: no libvirt dependency or unit (run with $B inventory from §1.8)
ssh root@37.27.110.241 'dpkg-query -W -f="${Depends}\n" velnor-runner | grep -ci "libvirt\|qemu\|kvm"; systemctl list-unit-files | grep -ci "libvirt\|qemu"'
# both counts must be 0
```

PASS = all three outputs empty / zero. Any hit → list file:line in report §3-row G2.1,
verifier judges allowlist (declared-separate KVM capability decl) or FAIL.

### 1.2 No Go controller / sidecar / scheduler

Spec §1, §5: one APT-installed Velnor Rust control plane; no Go runtime component.

```sh
# 1.2a no Go sources anywhere in tree (no exceptions)
rg --files --glob '*.go' --glob 'go.mod' --glob 'go.sum' . | tee /tmp/g2-go-<repo>.txt
test ! -s /tmp/g2-go-<repo>.txt

# 1.2b no Go toolchain references in generator, packaging, workflows
rg -n --no-heading -i '\bgo (build|run|install|test)\b|golang|go-toolchain|setup-go[^h]' \
  --glob '!plans/**' --glob '!content/**' . | tee /tmp/g2-go-ref-<repo>.txt
test ! -s /tmp/g2-go-ref-<repo>.txt

# 1.2c no second controller/sidecar/scheduler component (Rust side: single control-plane product)
rg -n --no-heading -i 'sidecar|second.?controller|external.?scheduler|go-controller' \
  --glob '!plans/**' crates/ .github/ packaging/ 2>/dev/null | tee /tmp/g2-sidecar-<repo>.txt
test ! -s /tmp/g2-sidecar-<repo>.txt
```

Note: in `$J`/`$C`, `crates/`/`packaging/` globs may not exist — the command must still
exit 0 with empty output (no hits), not error on missing dirs (`2>/dev/null` + `test ! -s`).

### 1.3 No quota ceilings (configs + package + live inspection)

Spec §4.3: no Docker NanoCpus/quota/cpuset/memory ceilings, no systemd CPUQuota/MemoryMax/
MemoryHigh on workload ancestry, no slot-divided build budgets, no per-job disk/PID quotas.
Config proof here; live proof in §1.8 (same gate, both required).

```sh
# 1.3a no quota knobs in generator sources, packaging, systemd units, workflows
rg -n --no-heading 'NanoCpus|CpuQuota|CPUQuota|MemoryMax|MemoryHigh|MemorySwap|CpusetCpus|CpusetMems|PidsLimit|BlkioWeight|memory\.max|memory\.high|cpu\.max[^i]|CARGO_BUILD_JOBS|MBX_BUILD_JOBS|BUILDKIT_PARALLELISM|GRADLE_WORKERS|MAVEN_THREADS.*slot|heap.*partition|slot.*heap' \
  --glob '!plans/**' --glob '!content/**' . | tee /tmp/g2-quota-<repo>.txt
test ! -s /tmp/g2-quota-<repo>.txt

# 1.3b no quota drop-ins shipped (unit files + drop-in dirs)
rg --files --glob '*.service' --glob '*.slice' --glob '*.scope' . | xargs -r grep -lEi 'CPUQuota|MemoryMax|MemoryHigh' 2>/dev/null | tee /tmp/g2-dropin-<repo>.txt
test ! -s /tmp/g2-dropin-<repo>.txt

# 1.3c Rust compile-time: quota-related cfg/const gone (velnor tree only; skip in $J/$C with N/A + reason)
rg -n --no-heading -i 'cpu_quota|mem_quota|memory_quota|slot_budget|per_slot|resource_class|CpuBudget' \
  crates/ | tee /tmp/g2-quota-rs.txt
test ! -s /tmp/g2-quota-rs.txt
```

`cpu.max` note: the literal appears in *inspection* scripts (C2 proof reads `cpu.max`).
Allowlist = read-only inspection paths (`*_test.sh`, `live_host_doctor.sh`, docs);
verifier confirms every hit is a read (`cat`, `--format`, `systemctl cat`), never a write.

### 1.4 No per-repo / org / engine reservations, weights, pools

Spec §4.1: one host-wide `max_jobs=N`; registration = authorization, not capacity.

```sh
# 1.4a no reservation/weight/pool/fair-share capacity model in sources or config
rg -n --no-heading -i 'reservation|reserved_pool|per_?repo_?(pool|quota|limit|weight)|per_?org_?(pool|quota|limit)|per_?engine_?(pool|quota)|engine_weight|repo_weight|fair.?share|priority_class|capacity_pool|hardware_pool' \
  --glob '!plans/**' --glob '!content/**' . | tee /tmp/g2-reserve-<repo>.txt
test ! -s /tmp/g2-reserve-<repo>.txt

# 1.4b exactly one max_jobs authority (velnor tree): single definition site
rg -n --no-heading 'max_jobs' crates/ --glob '*.rs' | tee /tmp/g2-maxjobs.txt
# verifier asserts: one canonical ledger/authority definition; all acquisition paths reference it;
# no second semaphore/counter that gates jobs (see D1 allocator proof for the reference list)
```

Legitimate `reserved` lifecycle-state uses (spec §4.1: "Reserved, acquiring, provisioning…"
are *counted states* in the one ledger) are allowlisted ONLY in the allocator module;
verifier diffs hits against the D1 allocator file list.

### 1.5 No legacy provider model / aliases / shims / dual parsers

Spec §2: exactly `github-hosted`, `github-self-hosted`, `velnor`; no aliases, no deprecation
branches, no dual parsers, no inference from `runner.environment`/labels.

```sh
# 1.5a canonical IDs only: every provider literal resolves to the set of three
rg -n --no-heading -o '"[a-z0-9-]*hosted[a-z0-9-]*"|"[a-z0-9-]*self_hosted[a-z0-9-]*"|"[a-z-]*velnor[a-z-]*"' \
  crates/velnor-workflow/src/ .github/workflows/ 2>/dev/null | sort -u | tee /tmp/g2-prov-lit-<repo>.txt
# verifier asserts output ⊆ {"github-hosted","github-self-hosted","velnor"} (plus documented display-name composites)

# 1.5b no alias/deprecation/compat provider paths
rg -n --no-heading -i 'provider.?alias|alias.?provider|deprecated.?provider|legacy.?provider|compat.?provider|provider.?fallback|default_provider|infer.?provider|provider.?inference|runner\.environment|runner_environment' \
  --glob '!plans/**' --glob '!content/**' . | tee /tmp/g2-legacy-<repo>.txt
test ! -s /tmp/g2-legacy-<repo>.txt

# 1.5c no dual parsers: single provider-set schema definition
rg -n --no-heading -i 'enum Provider|struct ProviderSet|Provider::from|parse_provider|providers\s*=\s*\[' \
  crates/ | tee /tmp/g2-prov-schema.txt
# verifier asserts: one schema site (velnor tree), consumed everywhere; $J/$C assert N/A (no generator sources) + typed-config-only check:
rg -n --no-heading 'providers\s*=' <jackin|chainargos-typed-config-path> | tee /tmp/g2-prov-cfg-<repo>.txt
# asserted values ⊆ canonical three

# 1.5d generated workflows: runs-on uses only canonical selectors (per-repo: exact file list from ownership inventory)
rg -n --no-heading 'runs-on:' .github/workflows/ | tee /tmp/g2-runson-<repo>.txt
# verifier asserts every selector ∈ {canonical three + disjoint dedicated local selectors + Apple-Silicon macOS ($J only)}
```

### 1.6 No hand-edited generated YAML (regeneration exact everywhere)

Spec §3.1: `velnor-workflow` sole owner; generator → regenerate → verify, always.

Per tree (using the tree's own pinned product — already-published `$PIN`, never source build):

```sh
# 1.6a exact regeneration: 0 files changed
./target/debug/velnor-workflow --plain --dry-run 2>&1 | tee /tmp/g2-regen-<repo>.txt
# assert: reports 0 files; exit 0
./target/debug/velnor-workflow --plain --force && git status --porcelain | tee /tmp/g2-regen-force-<repo>.txt
test ! -s /tmp/g2-regen-force-<repo>.txt

# 1.6b ownership inventory complete incl. unexpected files
# (generator-owned inventory command per repo docs; assert: every .github/workflows/*.yml +
#  referenced local actions + generated manifests listed; unexpected-file list empty)
rg --files .github/workflows/ | sort > /tmp/g2-wf-files-<repo>.txt
# verifier cross-checks against inventory output: set equality, no extras, no missing

# 1.6c generated-marker present on every workflow file
for f in .github/workflows/*.yml; do head -n 1 "$f" | grep -q 'Generated by velnor-workflow' || echo "UNMARKED: $f"; done | tee /tmp/g2-unmarked-<repo>.txt
test ! -s /tmp/g2-unmarked-<repo>.txt

# 1.6d structured policy + actionlint clean on final tree
# (repo's policy gate command, e.g. cargo test -p velnor-workflow policy + actionlint)
actionlint .github/workflows/*.yml 2>&1 | tee /tmp/g2-actionlint-<repo>.txt
test ! -s /tmp/g2-actionlint-<repo>.txt
```

`$J`/`$C` use their pinned-product regeneration path (setup action / published binary),
not `./target/debug/` — record the exact command per repo in the report.

### 1.7 Regression test gates per tree (prove the proofs are live, not stale greps)

Static greps rot. Each tree must also show its guarding tests green on the final SHA:

```sh
# velnor ($V): generator + contract + lints
cargo test -p velnor-workflow 2>&1 | tail -n 5 | tee /tmp/g2-test-V.txt
cargo test -p velnor-workflow-contract 2>&1 | tail -n 5 | tee -a /tmp/g2-test-V.txt
cargo clippy --all-targets -p velnor-workflow -- -D warnings 2>&1 | tail -n 3 | tee -a /tmp/g2-test-V.txt
cargo fmt --check 2>&1 | tee -a /tmp/g2-test-V.txt
# assert: all green, zero failures/warnings

# jackin ($J): full unit set incl. explicit docker-e2e + native Apple routing (final main SHA)
gh run list --repo jackin-project/jackin --branch main --limit 3
gh run view <final-main-run-id> --repo jackin-project/jackin --json conclusion,jobs  # all green, E2E present

# chainargos ($C): full 71-unit set incl. Testcontainers/RustFS/bake (final main SHA)
gh run list --repo ChainArgos/java-monorepo --branch main --limit 3
gh run view <final-main-run-id> --repo ChainArgos/java-monorepo --json conclusion,jobs  # all green
```

### 1.8 Live bastion quota-free re-proof (once, after all upgrades)

Repeat of the C2 inspection bundle on the FINAL package — §1.3 config proof is necessary
but not sufficient. Read-only SSH unless noted.

```sh
B=root@37.27.110.241
# 1.8a HostConfig of REAL running job containers: no NanoCpus/quotas/cpusets/memory ceilings
ssh $B 'for id in $(docker ps -q); do echo "== $id"; docker inspect --format "{{.Name}} {{.HostConfig}}" $id; done' | tee /tmp/g2-hostconfig.txt
# verifier asserts: no NanoCpus:<nonzero>, no Memory:<nonzero-ceiling>, no CpusetCpus, no Blkio*, no PidsLimit

# 1.8b effective cgroup ancestry incl. inherited limits
ssh $B 'for p in $(pgrep -f "velnor|runner" | head -20); do echo "== pid $p"; tr "\0" " " < /proc/$p/cgroup 2>/dev/null; echo; done; echo ---; cat /sys/fs/cgroup/system.slice/*/cpu.max /sys/fs/cgroup/system.slice/*/memory.max /sys/fs/cgroup/system.slice/*/memory.high 2>/dev/null | sort -u' | tee /tmp/g2-cgroup.txt
# verifier asserts: workload ancestry effectively max/unset; inherited limits inspected, not just emitted flags

# 1.8c package units/drop-ins: no quota directives
ssh $B 'systemctl cat $(systemctl list-units --type=service --state=running | grep -i velnor | awk "{print \$1}") 2>/dev/null | grep -Ei "CPUQuota|MemoryMax|MemoryHigh" | tee /tmp/g2-units.txt; test ! -s /tmp/g2-units.txt'

# 1.8d effective build env: no injected budgets
ssh $B 'docker inspect $(docker ps -q | head -5) --format "{{.Config.Env}}" | tr " " "\n" | grep -Ei "CARGO_BUILD_JOBS|MBX|BUILDKIT|GRADLE|heap|slot" | tee /tmp/g2-buildenv.txt; test ! -s /tmp/g2-buildenv.txt'

# 1.8e no raw-socket mount on any job container (re-proof of C2 §3)
ssh $B 'docker inspect $(docker ps -q) --format "{{.Name}} {{.Mounts}}" | grep -c "docker.sock\|docker-proxy"' | tee /tmp/g2-socket.txt
# count must be 0 for bastion-managed socket paths (job-private DinD socket proven distinct by daemon identity)
```

---

## 2. Fresh renamed-fixture onboarding end-to-end

Goal (spec §9.3, work-plan G2.2): prove onboarding is
trust→auth→config→published-pin→regen→qualify→inventory with **no new infra**,
and a new GitHub org adds **auth metadata only**.

### 2.1 Fixture design (renamed to catch name-based special cases)

- New repo `g2-onboarding-probe-<random>` under a FRESH GitHub org (not tailrocks/
  jackin-project/ChainArgos) — the fresh org is what proves the new-org claim.
- Renamed package/repo identity inside: fixture package name shares NO substring with
  `velnor`/`jackin`/`chainargos`; workflow/unit names use the renamed identity.
- Minimal surface: 1 Rust unit + 1 Docker unit + docs (enough to exercise planner,
  3-provider fanout, watchdog, strict-result aggregation; NOT a 4th full migration).
- Typed config ONLY (`providers = ["github-hosted","github-self-hosted","velnor"]`
  or the final-schema equivalent); no hand YAML at any point.

### 2.2 Step-by-step procedure (exact order, each step gated)

| # | Step | Action | Proof artifact |
|---|------|--------|----------------|
| 1 | trust/access | Verify fresh-org trust posture: fork-default-hosted, no privileged `pull_request_target`, controller-side checks cover the new org | trust-check log + negative fork-PR run (hosted-only, no bastion execution) |
| 2 | authorize | Add repo authorization (+ org registration metadata); add declarative GitHub scope ONLY if the controller requires it for the new org | auth diff (metadata-only: registration entries, zero capacity/infra fields) |
| 3 | typed config | Add typed generator config with canonical three providers | config file + schema-validation log |
| 4 | published pin | Select the ALREADY-PUBLISHED `$PIN` (record digest); never unpublished source | pin record (digest match vs §4.3) |
| 5 | regen/check | Regenerate full tree with pinned product; prove exactness (0 files on re-run), ownership inventory, local-ref resolution, structured policy, actionlint | regen logs + inventory + policy/lint logs |
| 6 | qualify | Run full qualification: affected/full planner → 3-provider fanout with identical inputs → strict expected-result set → hosted watchdog verdict; fail-fast disabled | run URL(s) + unit×provider matrix (engines, digests, JUnit counts, cleanup receipts) |
| 7 | inventory | Record health inventory: authenticated outbound health records bound to repo/source/run/attempt/provider, permits, provisioning progress | health-record sample + correlation check |

### 2.3 No-new-infra assertions (fail-closed)

After step 7, verifier proves ALL of:

```sh
# 2.3a same controller, same N: no new daemon/unit/pool
ssh root@37.27.110.241 'systemctl list-units --type=service --state=running | grep -ci velnor' | tee /tmp/g2-units-count.txt
# count EQUALS the pre-onboarding count recorded at G1 close (no new daemon)

# 2.3b no new host packages, VMs, scripts, or copied workflows for the fixture
ssh root@37.27.110.241 'dpkg-query -W | wc -l'  # equals pre-onboarding count
rg --files $FIXTURE --glob '*.sh' --glob '*.py' | tee /tmp/g2-fixture-scripts.txt  # only generated/typed-contract paths or empty
git -C $FIXTURE log --oneline | head -20  # no copied historical workflow bodies: every .yml generated-marker present (§1.6c procedure)

# 2.3c new-org diff is auth metadata only
git diff <pre-auth>..<post-auth> --stat  # touches ONLY authorization/registration metadata paths
rg -n --no-heading -i 'max_jobs|pool|reservation|quota|weight|capacity' <auth-diff-files> | tee /tmp/g2-auth-diff.txt
test ! -s /tmp/g2-auth-diff.txt
```

### 2.4 New-org = auth metadata only (explicit proof)

- The fresh org's onboarding diff (§2.2 step 2) is attached verbatim to the report.
- Verifier asserts: diff contains ONLY identity/authorization/scope-registration entries
  (org numeric ID, App installation refs, scope selectors); ZERO capacity, pool,
  reservation, quota, weight, hardware, or scheduling fields.
- Negative: a fork PR in the fixture org runs hosted-only; bastion admission logs show
  zero grants for the untrusted source (trust-denial excerpt attached).

---

## 3. Idempotent setup + reinstall + recovery verification matrix

Goal (spec §7 para 6, work-plan G2.4): prove reproducible infra WITHOUT compat shims.
Rule: **APT downgrade ONLY with a proven matching config/state snapshot restore;
else tested forward recovery. All recovery stays APT-only.**

### 3.1 Preconditions (record before any recovery test)

```sh
B=root@37.27.110.241
ssh $B 'dpkg-query -W velnor-runner; echo ---; velnorctl --help 2>&1 | head -30' | tee /tmp/g2-pkg-baseline.txt
# record: exact VERSION, available verbs (activation/drain/health derived HERE, never invented)
ssh $B 'ls -la /etc/velnor /var/lib/velnor /run/velnor /var/cache/velnor' | tee /tmp/g2-paths-baseline.txt
```

### 3.2 Matrix (each row: procedure → expected → evidence; verifier reruns marked ★)

| Row | Path | Procedure (exact) | Expected | Evidence |
|-----|------|-------------------|----------|----------|
| R1 ★ | idempotent setup re-run | Re-run full host setup from §C1 on live bastion (read-first inventory, SSH preserved, NVMe untouched, Docker/pinned tooling, cgroup v2 check, managed paths) | exit 0, zero changes to running jobs/config; second run byte-identical no-op where already converged | setup re-run log + pre/post `dpkg-query` + job-continuity excerpt |
| R2 ★ | package reinstall (same VERSION) | Drain per package procedure → `flock … apt-get install --reinstall "velnor-runner=${VERSION}"` → `dpkg-query -W` → `release verify-installed` → package-derived activation → health | identical identity pre/post; `verify-installed` passes pre-start; health green | transaction log + identity diff (empty) + health proof |
| R3 | forward upgrade (VERSION → VERSION+1) | Only if a newer signed candidate exists; else N/A with reason. Same locked path as C1/D3 | new identity; config/secrets preserved; quota-free re-proof (§1.8) passes | upgrade log + repeated §1.8 bundle |
| R4 ★ | forward recovery (corrupt config) | Corrupt a COPY-adjacent test config key (never prod secrets) → run package/documented forward-recovery (regenerate-or-restore-from-live-defaults per runbook) → `verify-installed` + health | service healthy WITHOUT downgrade, WITHOUT shim; recovery path is the documented one | corruption record + recovery log + health proof |
| R5 | downgrade WITH proven snapshot | Allowed ONLY if: (a) previous coherent package retained in feed, (b) matching config/state snapshot exists, (c) snapshot-restore is a TESTED procedure. Drain → `flock … apt-get install "velnor-runner=${PREV}"` → restore matching snapshot → `verify-installed` → activation → health | old identity + old schema state consistent; NO claim that old binary reads new schema | snapshot provenance + restore log + `verify-installed` + health |
| R6 | downgrade WITHOUT snapshot | PROHIBITED path — prove it is refused, not performed: attempt must be rejected by procedure (runbook says forward-recovery) or, if mechanically attempted in an isolated test window, must FAIL `verify-installed`/health rather than silently run mixed-version | explicit refusal or failed verification; no mixed-version state left behind | refusal log or failed-verification log + post-state identity (unchanged/forward) |
| R7 ★ | runbook-vs-help audit | Diff every command verb in ops/recovery runbook against `velnorctl --help` + package unit docs | 1:1 match; ZERO invented verbs; ZERO `dpkg -i`/sideload/signature-bypass instructions | verb-mapping table + `grep -c "dpkg -i\|apt install ./\|--allow-unauthenticated" runbook` = 0 |

### 3.3 Rules enforced by the matrix

1. Every recovery row ends with `release verify-installed` BEFORE start + health proof.
2. R5 requires the snapshot's provenance (which release, which schema version, restore test
   date); "previous package retained" alone does NOT authorize downgrade.
3. R6 is a NEGATIVE test: the gate passes when the prohibited path is demonstrably blocked
   or fails closed — never when it "seems to work".
4. No row may introduce a compat shim, dual schema reader, or alias: verifier greps the
   recovery diff for `compat|shim|alias|legacy|deprecated` (must be empty, same as §1.5b).

---

## 4. Acceptance report skeleton

One report, covering all three final default trees + every gate A0–G2.
Signed ONLY by the final independent verifier (not any step author).

### 4.1 Header (exact identities)

```text
Campaign: bastion three-provider CI — FINAL ACCEPTANCE REPORT
Branch/PR: docs/bastion-final-plan / tailrocks/velnor#912
Report date: <YYYY-MM-DD>   Report SHA: <git sha of report commit>
Final SHAs:
  velnor:      <40-hex>   (main @ <date>, run <url>)
  jackin:      <40-hex>   (main @ <date>, run <url>)
  chainargos:  <40-hex>   (main @ <date>, run <url>)
Generator pins per repo (digest + product record):
  velnor:     <pin>  velnor-runner N/A-separate-identity (see §4.3)
  jackin:     <pin>
  chainargos: <pin>
Deployed bastion: velnor-runner=<VERSION>  (dpkg-query excerpt attached)
Signer fingerprint: <fpr>  (independent reference: <url/doc>)
APT record: <candidate record id + InRelease digest>
Upstream protocol ref: actions/scaleset@<sha>  (+ runner image digests: <list>)
Final N: <int>  (mixed-engine, post-ChainArgos; conditions: <cold/warm, mix>)
```

### 4.2 Per-gate evidence table (one row per gate; attach pointers, not prose)

```text
Gate | Verdict | Author | Verifier | Evidence pointers (run URLs, SHAs, digests, logs) | Signoff
A0   | PASS    | <name>| <name>   | ledger <path>; main SHAs <3×sha>; runs <4 urls>; … |
A1   | PASS    | …     | …        | repro→fix→regression <paths>; regen 0-files <log>; …|
A2   | PASS    | …     | …        | negative suite <log>; cold-consumer <log>; atomic promotion <commit>; zero-cargo <log> |
A3   | PASS    | …     | …        | 3× main run urls + SHAs + plan digests + timings |
B1   | PASS    | …     | …        | generator/IR tests <log>; renamed fixture <log>; negatives <log> |
B2   | PASS    | …     | …        | atomic velnor-apt commit <sha>; ownership/policy/lint <logs> |
B3   | PASS    | …     | …        | tag/commit/version; amd64+arm64 digests; manifests/record; attestations |
B4   | PASS    | …     | …        | pre-mutation log; live feed chain proof; candidate identity; retained prev version |
C1   | PASS    | …     | …        | flock transaction log; dpkg-query; verify-installed; activation/health |
C2   | PASS    | …     | …        | quota-free bundle; no-socket proof; smoke url; occupancy/cleanup; provisional N |
D1   | PASS    | …     | …        | fixtures + LIVE canary; digest pins; private-Docker proof; allocator races; restart proofs |
D2   | PASS    | …     | …        | no-legacy proof; fanout proof; strict-result negatives; watchdog outage proof; trust denials |
D3   | PASS    | …     | …        | new release/feed chain; upgrade log; repeated quota-free proof |
E1   | PASS    | …     | …        | 17-unit ×3 matrix; engine identities; ownership proof; exceptions |
E2   | PASS    | …     | …        | per-row fault records; ≤N ledger; mixed N; 3× triple-green urls + PR url |
F1   | PASS    | …     | …        | migrated tree; 40-unit ledger; routing/release diffs; explicit E2E proof |
F2   | PASS    | …     | …        | parity set; PR+main urls; N data (if any); Velnor non-regression proof |
G1   | PASS    | …     | …        | migrated tree; 71/213 ledger; DB/Testcontainers/RustFS/Docker/browser proofs; PR+main |
G2.1 | PASS    | <prep>| <final>  | §1 logs: /tmp/g2-{vm,go,quota,reserve,legacy,regen,actionlint,test,hostconfig,cgroup,units,buildenv,socket}-* |
G2.2 | PASS    | <prep>| <final>  | §2 logs: fixture repo url; auth diff; regen logs; qualification matrix; health inventory; fork-denial |
G2.3 | PASS    | <prep>| <final>  | §3 logs: R1–R7 table + transaction/recovery/health logs; verb-mapping |
G2.4 | PASS    | <prep>| <final>  | this report + pin/identity record (§4.3) + runbooks (§4.4) + final N bundle (§4.5) |
```

### 4.3 Pin + deployed-identity record (exact values, no "latest")

```text
Generator products (per repo): pin sha, manifest digest, binary digest(s) per arch, signer workflow/ref, self-report
velnor-runner: exact VERSION, amd64/arm64 package hashes, manifest/record digests, APT publication record id
Images: official runner image digest, DinD image digest, toolchain image digests (per repo)
Protocol: actions/scaleset@sha, runner version (recorded separately), DinD version
Signer: APT key fingerprint + independent-reference URL
```

### 4.4 Runbook pointers

```text
Ops runbook: <path> — matches implemented package/help per R7 verb-mapping (attached)
Recovery runbook: <path> — forward-recovery default; downgrade-with-snapshot conditions; prohibited-path refusal (R6)
Drain/activation/health commands: quoted verbatim from --help (attached), never invented
```

### 4.5 Final N + pool/provenance/fault bundle

```text
Final N: <int> with full metric set (jobs/min, time-to-green, queue/setup/compile/test/cache/cleanup,
  CPU, mem pressure/OOM, IO/disk/inodes, contention) at tested values; cold+warm; post-ChainArgos mix
Shared-pool proof: single max_jobs=N ledger excerpt; occupancy ledger sample; monotonic ≤N statement
Provenance bundle: source SHAs → plan digests → run urls → engine identities → image digests → JUnit counts
Fault bundle: E2 per-row records index + G2 R1–R7 recovery records index
```

### 4.6 Verifier signoff blocks

```text
Per-gate verifier signoff (A0–G1; each gate's own verifier):
  Gate ___: I reran the critical checks (not just read the author's logs) and the
  evidence above supports PASS. Name: ______ Date: ______ Sig: ______

Final independent verifier signoff (G2 + whole campaign):
  I am not an author of any campaign step. I reran §1 (all three trees + live §1.8),
  witnessed §2 onboarding, reran §3 rows R1/R2/R4/R5-or-R6/R7, and verified §4.1–§4.5
  identities against live state (dpkg-query, APT feed, run urls). The campaign meets
  every checklist item. Name: ______ Date: ______ Sig: ______

Exceptions / waivers: NONE PERMITTED. Any exception fails the gate; record as FAIL with
  finding + required remediation, never as PASS-with-note.
```

---

## Appendix: execution order + file manifest

1. Record final SHAs (§4.1) → 2. §1.1–§1.7 per tree ($V, $J, $C) → 3. §1.8 live bastion →
   4. §2 fixture onboarding (fresh org) → 5. §3 matrix R1–R7 → 6. assemble §4 report →
   7. final verifier signs §4.6.
2. All `/tmp/g2-*` logs are evidence; attach by path + sha256 to the report commit.
3. Prep author performed NO execution: no checkouts created, no SSH, no runs triggered.
   First live command runs at G2 gate under verifier supervision.
