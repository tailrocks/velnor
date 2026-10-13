//! Primary cache keys, restore keys, and cache paths of every hosted
//! (unit, lane) job as the generator rendered them BEFORE the kind reusables
//! were parameterized by `workflow_call` inputs (captured from the generated
//! workflows at `origin/plan/ci-workflow-and-cache` @ 85e6568e; compatibility digests re-captured at 284f1091 after the mbx 1.11.1 bump;
//! `rust-velnor-workflow`'s freshness list gained `crates/velnor-workflow/build.rs`
//! when that crate acquired a build script — the same scan fact the literal
//! tree would have rendered). D9 of
//! `plans/2026-09-16-ci-workflow-and-cache-plan.md` requires the resolved
//! keys of the parameterized callee to equal these literal forms.
//!
//! Regenerate this table only when a key format changes on purpose; the test
//! that consumes it then documents the migration.
//!
//! R2m migration (schema-2 flip): caller jobs moved `github-*` to
//! `github-hosted-*` and callee jobs `verify-github` to
//! `verify-github-hosted`; the `bundle`, `docker_seed`, and `mbx` key
//! formats gained the `provider-platform-trust` segments
//! (`github-hosted-linux-x64-untrusted-ok`, `trusted-only` for the
//! trust-gated docker seed); the snapshot compatibility digests
//! rotated because the digest facts gained those same three fields
//! (s1 facts had no provider/platform/trust). The `cargo_bin`,
//! `mold`, and `rustup` layers are provider-agnostic by design and
//! their keys are byte-identical. Every other key byte — hashFiles
//! lists, paths, restore structure — is unchanged.

pub(crate) struct CacheKey {
    pub(crate) layer: &'static str,
    pub(crate) paths: &'static [&'static str],
    pub(crate) primary: &'static str,
    pub(crate) restore_keys: &'static [&'static str],
}

pub(crate) struct UnitLaneKeys {
    pub(crate) callee: &'static str,
    pub(crate) caller_job: &'static str,
    pub(crate) job: &'static str,
    pub(crate) keys: &'static [CacheKey],
}

pub(crate) const PRE_PARAMETERIZATION_KEYS: &[UnitLaneKeys] = &[
    UnitLaneKeys {
        callee: "ci-unit-bun.yml",
        caller_job: "github-hosted-bun-velnor",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.bun/install/cache"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-bun-${{ hashFiles('package.json', 'bun.lock') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-bun-",
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-docker.yml",
        caller_job: "github-hosted-docker",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "docker_seed",
                paths: &[".velnor-docker-cache"],
                primary: "velnor-docker-seed-v3-7e67216c9e23-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-trusted-only-docker-${{ hashFiles('Cargo.lock', 'Dockerfile', 'docker/build-mise.lock', 'docker/build-mise.toml', 'rust-toolchain.toml') }}-${{ hashFiles('Cargo.lock', 'deny.toml') }}",
                restore_keys: &[
                    "velnor-docker-seed-v3-7e67216c9e23-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-trusted-only-docker-${{ hashFiles('Cargo.lock', 'Dockerfile', 'docker/build-mise.lock', 'docker/build-mise.toml', 'rust-toolchain.toml') }}-",
                    "velnor-docker-seed-v3-7e67216c9e23-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-trusted-only-docker-",
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-docs.yml",
        caller_job: "github-hosted-docs",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.npm"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-docs-${{ hashFiles('package-lock.json', 'bun.lock') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-docs-",
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-opentofu.yml",
        caller_job: "github-hosted-opentofu",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.terraform.d/plugin-cache"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-opentofu-${{ hashFiles('**/.terraform.lock.hcl') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-opentofu-",
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-policy",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "cargo_bin",
                paths: &["~/.cargo/bin"],
                primary: "velnor-cargo-bin-v2-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('mise.lock', 'mise.toml') }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-ec62b8b58bab-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-policy-${{ hashFiles('.cargo/**', 'Cargo.lock', 'deny.toml') }}-${{ hashFiles('.cargo/audit.toml', '.cargo/deny.toml', 'Cargo.lock', 'audit.toml', 'deny.toml') }}",
                restore_keys: &[
                    "velnor-mbx-v3-ec62b8b58bab-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-policy-${{ hashFiles('.cargo/**', 'Cargo.lock', 'deny.toml') }}-",
                    "velnor-mbx-v3-ec62b8b58bab-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-policy-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-unit-collector",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-f75156e9e834-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-unit-collector-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('mise.lock', 'tools/unit-collector/**/*.rs', 'tools/unit-collector/benches/**', 'tools/unit-collector/examples/**', 'tools/unit-collector/src/**', 'tools/unit-collector/tests/**', 'tools/unit-collector/tests/fixtures/dependency-bump.jsonl', 'tools/unit-collector/tests/fixtures/fresh.jsonl', 'tools/unit-collector/tests/fixtures/touched-source.jsonl') }}",
                restore_keys: &[
                    "velnor-mbx-v3-f75156e9e834-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-unit-collector-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-f75156e9e834-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-unit-collector-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-bench",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-9874c4732ec4-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-bench-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('config/fleet/**', 'crates/velnor-bench/**/*.rs', 'crates/velnor-bench/benches/**', 'crates/velnor-bench/examples/**', 'crates/velnor-bench/src/**', 'crates/velnor-bench/tests/**', 'crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-runner/**/*.rs', 'crates/velnor-runner/benches/**', 'crates/velnor-runner/build.rs', 'crates/velnor-runner/debian/postinst', 'crates/velnor-runner/debian/postrm', 'crates/velnor-runner/debian/preinst', 'crates/velnor-runner/debian/prerm', 'crates/velnor-runner/debian/velnor-control.slice', 'crates/velnor-runner/debian/velnor-controller@.service', 'crates/velnor-runner/debian/velnor-daemon.service', 'crates/velnor-runner/debian/velnor-daemon@.service', 'crates/velnor-runner/debian/velnor-doctor.service', 'crates/velnor-runner/debian/velnor-doctor@.service', 'crates/velnor-runner/debian/velnor-guardian.service', 'crates/velnor-runner/debian/velnor-job@.service', 'crates/velnor-runner/debian/velnor-jobs.slice', 'crates/velnor-runner/debian/velnor-runner.tmpfiles', 'crates/velnor-runner/debian/velnor-slot@.service', 'crates/velnor-runner/debian/velnor.env', 'crates/velnor-runner/examples/**', 'crates/velnor-runner/src/**', 'crates/velnor-runner/src/cache.rs', 'crates/velnor-runner/src/execution/mod.rs', 'crates/velnor-runner/src/executor.rs', 'crates/velnor-runner/src/leftover_disk.rs', 'crates/velnor-runner/src/node/complete.rs', 'crates/velnor-runner/src/node/guardian.rs', 'crates/velnor-runner/src/node/job.rs', 'crates/velnor-runner/src/node/slot.rs', 'crates/velnor-runner/src/release.rs', 'crates/velnor-runner/src/runner.rs', 'crates/velnor-runner/tests/**', 'crates/velnor-runner/tests/fixtures/backend-parity.yml', 'crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.service', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.timer', 'docker/job-mise.lock', 'docker/job-ubuntu.Dockerfile', 'microvm/**', 'microvm/kernel.config', 'microvm/manifest.json', 'microvm/pins.json', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-9874c4732ec4-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-bench-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-9874c4732ec4-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-bench-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-client",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-d7ddd7df48d1-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-client-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-client/**/*.rs', 'crates/velnor-client/benches/**', 'crates/velnor-client/examples/**', 'crates/velnor-client/src/**', 'crates/velnor-client/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-d7ddd7df48d1-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-client-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-d7ddd7df48d1-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-client-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-control",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-fed6b5542fa2-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-control-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-fed6b5542fa2-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-control-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-fed6b5542fa2-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-control-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-model",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-6caac7864631-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-model-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-6caac7864631-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-model-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-6caac7864631-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-model-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-render",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-81e65c6ca644-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-render-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-render/**/*.rs', 'crates/velnor-render/benches/**', 'crates/velnor-render/examples/**', 'crates/velnor-render/src/**', 'crates/velnor-render/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-81e65c6ca644-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-render-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-81e65c6ca644-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-render-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-runner",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-91fc926cb724-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-runner-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('config/fleet/**', 'crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-runner/**/*.rs', 'crates/velnor-runner/benches/**', 'crates/velnor-runner/build.rs', 'crates/velnor-runner/debian/postinst', 'crates/velnor-runner/debian/postrm', 'crates/velnor-runner/debian/preinst', 'crates/velnor-runner/debian/prerm', 'crates/velnor-runner/debian/velnor-control.slice', 'crates/velnor-runner/debian/velnor-controller@.service', 'crates/velnor-runner/debian/velnor-daemon.service', 'crates/velnor-runner/debian/velnor-daemon@.service', 'crates/velnor-runner/debian/velnor-doctor.service', 'crates/velnor-runner/debian/velnor-doctor@.service', 'crates/velnor-runner/debian/velnor-guardian.service', 'crates/velnor-runner/debian/velnor-job@.service', 'crates/velnor-runner/debian/velnor-jobs.slice', 'crates/velnor-runner/debian/velnor-runner.tmpfiles', 'crates/velnor-runner/debian/velnor-slot@.service', 'crates/velnor-runner/debian/velnor.env', 'crates/velnor-runner/examples/**', 'crates/velnor-runner/src/**', 'crates/velnor-runner/src/cache.rs', 'crates/velnor-runner/src/execution/mod.rs', 'crates/velnor-runner/src/executor.rs', 'crates/velnor-runner/src/leftover_disk.rs', 'crates/velnor-runner/src/node/complete.rs', 'crates/velnor-runner/src/node/guardian.rs', 'crates/velnor-runner/src/node/job.rs', 'crates/velnor-runner/src/node/slot.rs', 'crates/velnor-runner/src/release.rs', 'crates/velnor-runner/src/runner.rs', 'crates/velnor-runner/tests/**', 'crates/velnor-runner/tests/fixtures/backend-parity.yml', 'crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.service', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.timer', 'docker/job-mise.lock', 'docker/job-ubuntu.Dockerfile', 'microvm/**', 'microvm/kernel.config', 'microvm/manifest.json', 'microvm/pins.json', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-91fc926cb724-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-runner-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-91fc926cb724-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-runner-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-tools",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-64c5561b2930-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-tools-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('config/fleet/**', 'crates/velnor-client/**/*.rs', 'crates/velnor-client/benches/**', 'crates/velnor-client/examples/**', 'crates/velnor-client/src/**', 'crates/velnor-client/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-runner/src/manifest.rs', 'crates/velnor-tools/**/*.rs', 'crates/velnor-tools/benches/**', 'crates/velnor-tools/examples/**', 'crates/velnor-tools/src/**', 'crates/velnor-tools/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-64c5561b2930-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-tools-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-64c5561b2930-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-tools-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-workflow",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-a1e6d261b1d9-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-workflow-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('Dockerfile', 'config/fleet/**', 'crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-runner/**/*.rs', 'crates/velnor-runner/benches/**', 'crates/velnor-runner/build.rs', 'crates/velnor-runner/debian/postinst', 'crates/velnor-runner/debian/postrm', 'crates/velnor-runner/debian/preinst', 'crates/velnor-runner/debian/prerm', 'crates/velnor-runner/debian/velnor-control.slice', 'crates/velnor-runner/debian/velnor-controller@.service', 'crates/velnor-runner/debian/velnor-daemon.service', 'crates/velnor-runner/debian/velnor-daemon@.service', 'crates/velnor-runner/debian/velnor-doctor.service', 'crates/velnor-runner/debian/velnor-doctor@.service', 'crates/velnor-runner/debian/velnor-guardian.service', 'crates/velnor-runner/debian/velnor-job@.service', 'crates/velnor-runner/debian/velnor-jobs.slice', 'crates/velnor-runner/debian/velnor-runner.tmpfiles', 'crates/velnor-runner/debian/velnor-slot@.service', 'crates/velnor-runner/debian/velnor.env', 'crates/velnor-runner/examples/**', 'crates/velnor-runner/src/**', 'crates/velnor-runner/src/cache.rs', 'crates/velnor-runner/src/execution/mod.rs', 'crates/velnor-runner/src/executor.rs', 'crates/velnor-runner/src/leftover_disk.rs', 'crates/velnor-runner/src/node/complete.rs', 'crates/velnor-runner/src/node/guardian.rs', 'crates/velnor-runner/src/node/job.rs', 'crates/velnor-runner/src/node/slot.rs', 'crates/velnor-runner/src/release.rs', 'crates/velnor-runner/src/runner.rs', 'crates/velnor-runner/tests/**', 'crates/velnor-runner/tests/fixtures/backend-parity.yml', 'crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.service', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.timer', 'crates/velnor-workflow/**/*.rs', 'crates/velnor-workflow/README.md', 'crates/velnor-workflow/benches/**', 'crates/velnor-workflow/build.rs', 'crates/velnor-workflow/examples/**', 'crates/velnor-workflow/src/**', 'crates/velnor-workflow/src/runtime.rs', 'crates/velnor-workflow/templates/**', 'crates/velnor-workflow/templates/release-package-signer.yml', 'crates/velnor-workflow/tests/**', 'crates/velnor-workflow/tests/generic_surface_literals.rs', 'docker/build-mise.*', 'docker/job-mise.lock', 'docker/job-ubuntu.Dockerfile', 'microvm/**', 'microvm/kernel.config', 'microvm/manifest.json', 'microvm/pins.json', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-a1e6d261b1d9-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-workflow-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-a1e6d261b1d9-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-workflow-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnor-workflow-contract",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'crates/velnor-workflow-contract/Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-3b2aa59909ca-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-workflow-contract-${{ hashFiles('.cargo/**', 'crates/velnor-workflow-contract/Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-workflow-contract/**/*.rs', 'crates/velnor-workflow-contract/Cargo.lock', 'crates/velnor-workflow-contract/benches/**', 'crates/velnor-workflow-contract/examples/**', 'crates/velnor-workflow-contract/src/**', 'crates/velnor-workflow-contract/tests/**', 'mise.lock') }}",
                restore_keys: &[
                    "velnor-mbx-v3-3b2aa59909ca-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-workflow-contract-${{ hashFiles('.cargo/**', 'crates/velnor-workflow-contract/Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-3b2aa59909ca-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnor-workflow-contract-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-velnorctl",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-f9bd0478a352-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnorctl-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('config/fleet/**', 'crates/velnor-client/**/*.rs', 'crates/velnor-client/benches/**', 'crates/velnor-client/examples/**', 'crates/velnor-client/src/**', 'crates/velnor-client/tests/**', 'crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-render/**/*.rs', 'crates/velnor-render/benches/**', 'crates/velnor-render/examples/**', 'crates/velnor-render/src/**', 'crates/velnor-render/tests/**', 'crates/velnor-runner/**/*.rs', 'crates/velnor-runner/benches/**', 'crates/velnor-runner/build.rs', 'crates/velnor-runner/debian/postinst', 'crates/velnor-runner/debian/postrm', 'crates/velnor-runner/debian/preinst', 'crates/velnor-runner/debian/prerm', 'crates/velnor-runner/debian/velnor-control.slice', 'crates/velnor-runner/debian/velnor-controller@.service', 'crates/velnor-runner/debian/velnor-daemon.service', 'crates/velnor-runner/debian/velnor-daemon@.service', 'crates/velnor-runner/debian/velnor-doctor.service', 'crates/velnor-runner/debian/velnor-doctor@.service', 'crates/velnor-runner/debian/velnor-guardian.service', 'crates/velnor-runner/debian/velnor-job@.service', 'crates/velnor-runner/debian/velnor-jobs.slice', 'crates/velnor-runner/debian/velnor-runner.tmpfiles', 'crates/velnor-runner/debian/velnor-slot@.service', 'crates/velnor-runner/debian/velnor.env', 'crates/velnor-runner/examples/**', 'crates/velnor-runner/src/**', 'crates/velnor-runner/src/cache.rs', 'crates/velnor-runner/src/execution/mod.rs', 'crates/velnor-runner/src/executor.rs', 'crates/velnor-runner/src/leftover_disk.rs', 'crates/velnor-runner/src/node/canary.rs', 'crates/velnor-runner/src/node/complete.rs', 'crates/velnor-runner/src/node/guardian.rs', 'crates/velnor-runner/src/node/job.rs', 'crates/velnor-runner/src/node/slot.rs', 'crates/velnor-runner/src/release.rs', 'crates/velnor-runner/src/runner.rs', 'crates/velnor-runner/tests/**', 'crates/velnor-runner/tests/fixtures/backend-parity.yml', 'crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.service', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.timer', 'crates/velnorctl/**/*.rs', 'crates/velnorctl/benches/**', 'crates/velnorctl/examples/**', 'crates/velnorctl/src/**', 'crates/velnorctl/tests/**', 'docker/job-mise.lock', 'docker/job-ubuntu.Dockerfile', 'microvm/**', 'microvm/kernel.config', 'microvm/manifest.json', 'microvm/pins.json', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-f9bd0478a352-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnorctl-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-f9bd0478a352-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-velnorctl-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-hosted-rust-production-topology",
        job: "verify-github-hosted",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-${{ hashFiles('Cargo.toml', 'Cargo.lock') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-84d1e28baf02-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-production-topology-${{ hashFiles('Cargo.lock', 'Cargo.toml') }}-${{ hashFiles('./**/*.rs', 'crates/velnor-runner/**', 'docker/**', 'microvm/**', 'mise.lock', 'scripts/check-release-feature-boundary.sh', 'scripts/test-check-release-feature-boundary.sh') }}",
                restore_keys: &[
                    "velnor-mbx-v3-84d1e28baf02-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-production-topology-${{ hashFiles('Cargo.lock', 'Cargo.toml') }}-",
                    "velnor-mbx-v3-84d1e28baf02-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-production-topology-",
                ],
            },
            CacheKey {
                layer: "mold",
                paths: &["~/.cache/velnor/mold/2.42.0"],
                primary: "velnor-mold-2.42.0-${{ runner.os }}-${{ runner.arch }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "rustup",
                paths: &["~/.rustup"],
                primary: "velnor-rustup-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                ],
            },
        ],
    },
];
