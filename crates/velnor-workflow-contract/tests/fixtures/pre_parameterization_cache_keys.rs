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
        caller_job: "github-bun-velnor",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.bun/install/cache"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-bun-${{ hashFiles('package.json', 'bun.lock') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-bun-",
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-docker.yml",
        caller_job: "github-docker",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "docker_seed",
                paths: &[".velnor-docker-cache"],
                primary: "velnor-docker-seed-v3-9c97194a807b-${{ runner.os }}-${{ runner.arch }}-docker-${{ hashFiles('Cargo.lock', 'Dockerfile', 'docker/build-mise.lock', 'docker/build-mise.toml', 'rust-toolchain.toml') }}-${{ hashFiles('Cargo.lock', 'deny.toml') }}",
                restore_keys: &[
                    "velnor-docker-seed-v3-9c97194a807b-${{ runner.os }}-${{ runner.arch }}-docker-${{ hashFiles('Cargo.lock', 'Dockerfile', 'docker/build-mise.lock', 'docker/build-mise.toml', 'rust-toolchain.toml') }}-",
                    "velnor-docker-seed-v3-9c97194a807b-${{ runner.os }}-${{ runner.arch }}-docker-",
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-docs.yml",
        caller_job: "github-docs",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.npm"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-docs-${{ hashFiles('package-lock.json', 'bun.lock') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-docs-",
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-opentofu.yml",
        caller_job: "github-opentofu",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.terraform.d/plugin-cache"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-opentofu-${{ hashFiles('**/.terraform.lock.hcl') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-opentofu-",
                ],
            },
        ],
    },
    UnitLaneKeys {
        callee: "ci-unit-rust.yml",
        caller_job: "github-rust-policy",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "cargo_bin",
                paths: &["~/.cargo/bin"],
                primary: "velnor-cargo-bin-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('mise.lock', 'mise.toml') }}",
                restore_keys: &[
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-947413fdb95d-${{ runner.os }}-${{ runner.arch }}-rust-policy-${{ hashFiles('.cargo/**', 'Cargo.lock', 'deny.toml') }}-${{ hashFiles('.cargo/audit.toml', '.cargo/deny.toml', 'Cargo.lock', 'audit.toml', 'deny.toml') }}",
                restore_keys: &[
                    "velnor-mbx-v3-947413fdb95d-${{ runner.os }}-${{ runner.arch }}-rust-policy-${{ hashFiles('.cargo/**', 'Cargo.lock', 'deny.toml') }}-",
                    "velnor-mbx-v3-947413fdb95d-${{ runner.os }}-${{ runner.arch }}-rust-policy-",
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
        caller_job: "github-rust-unit-collector",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-ad16fc81795d-${{ runner.os }}-${{ runner.arch }}-rust-unit-collector-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('mise.lock', 'tools/unit-collector/**/*.rs', 'tools/unit-collector/benches/**', 'tools/unit-collector/examples/**', 'tools/unit-collector/src/**', 'tools/unit-collector/tests/**', 'tools/unit-collector/tests/fixtures/dependency-bump.jsonl', 'tools/unit-collector/tests/fixtures/fresh.jsonl', 'tools/unit-collector/tests/fixtures/touched-source.jsonl') }}",
                restore_keys: &[
                    "velnor-mbx-v3-ad16fc81795d-${{ runner.os }}-${{ runner.arch }}-rust-unit-collector-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-ad16fc81795d-${{ runner.os }}-${{ runner.arch }}-rust-unit-collector-",
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
        caller_job: "github-rust-velnor-bench",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-d779917f73e7-${{ runner.os }}-${{ runner.arch }}-rust-velnor-bench-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('config/fleet/**', 'crates/velnor-bench/**/*.rs', 'crates/velnor-bench/benches/**', 'crates/velnor-bench/examples/**', 'crates/velnor-bench/src/**', 'crates/velnor-bench/tests/**', 'crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-runner/**/*.rs', 'crates/velnor-runner/benches/**', 'crates/velnor-runner/build.rs', 'crates/velnor-runner/debian/postinst', 'crates/velnor-runner/debian/postrm', 'crates/velnor-runner/debian/preinst', 'crates/velnor-runner/debian/prerm', 'crates/velnor-runner/debian/velnor-control.slice', 'crates/velnor-runner/debian/velnor-controller@.service', 'crates/velnor-runner/debian/velnor-daemon.service', 'crates/velnor-runner/debian/velnor-daemon@.service', 'crates/velnor-runner/debian/velnor-doctor.service', 'crates/velnor-runner/debian/velnor-doctor@.service', 'crates/velnor-runner/debian/velnor-guardian.service', 'crates/velnor-runner/debian/velnor-job@.service', 'crates/velnor-runner/debian/velnor-jobs.slice', 'crates/velnor-runner/debian/velnor-runner.tmpfiles', 'crates/velnor-runner/debian/velnor-slot@.service', 'crates/velnor-runner/debian/velnor.env', 'crates/velnor-runner/examples/**', 'crates/velnor-runner/src/**', 'crates/velnor-runner/src/cache.rs', 'crates/velnor-runner/src/execution/mod.rs', 'crates/velnor-runner/src/executor.rs', 'crates/velnor-runner/src/leftover_disk.rs', 'crates/velnor-runner/src/node/complete.rs', 'crates/velnor-runner/src/node/guardian.rs', 'crates/velnor-runner/src/node/job.rs', 'crates/velnor-runner/src/node/slot.rs', 'crates/velnor-runner/src/release.rs', 'crates/velnor-runner/src/runner.rs', 'crates/velnor-runner/tests/**', 'crates/velnor-runner/tests/fixtures/backend-parity.yml', 'crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.service', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.timer', 'docker/job-mise.lock', 'docker/job-ubuntu.Dockerfile', 'microvm/**', 'microvm/kernel.config', 'microvm/manifest.json', 'microvm/pins.json', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-d779917f73e7-${{ runner.os }}-${{ runner.arch }}-rust-velnor-bench-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-d779917f73e7-${{ runner.os }}-${{ runner.arch }}-rust-velnor-bench-",
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
        caller_job: "github-rust-velnor-client",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-5e23f22451d2-${{ runner.os }}-${{ runner.arch }}-rust-velnor-client-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-client/**/*.rs', 'crates/velnor-client/benches/**', 'crates/velnor-client/examples/**', 'crates/velnor-client/src/**', 'crates/velnor-client/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-5e23f22451d2-${{ runner.os }}-${{ runner.arch }}-rust-velnor-client-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-5e23f22451d2-${{ runner.os }}-${{ runner.arch }}-rust-velnor-client-",
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
        caller_job: "github-rust-velnor-control",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-41ffe5cc6df5-${{ runner.os }}-${{ runner.arch }}-rust-velnor-control-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-41ffe5cc6df5-${{ runner.os }}-${{ runner.arch }}-rust-velnor-control-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-41ffe5cc6df5-${{ runner.os }}-${{ runner.arch }}-rust-velnor-control-",
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
        caller_job: "github-rust-velnor-model",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-0cdf3f658900-${{ runner.os }}-${{ runner.arch }}-rust-velnor-model-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-0cdf3f658900-${{ runner.os }}-${{ runner.arch }}-rust-velnor-model-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-0cdf3f658900-${{ runner.os }}-${{ runner.arch }}-rust-velnor-model-",
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
        caller_job: "github-rust-velnor-render",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-2e39b420a6ed-${{ runner.os }}-${{ runner.arch }}-rust-velnor-render-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-render/**/*.rs', 'crates/velnor-render/benches/**', 'crates/velnor-render/examples/**', 'crates/velnor-render/src/**', 'crates/velnor-render/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-2e39b420a6ed-${{ runner.os }}-${{ runner.arch }}-rust-velnor-render-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-2e39b420a6ed-${{ runner.os }}-${{ runner.arch }}-rust-velnor-render-",
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
        caller_job: "github-rust-velnor-runner",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-c9c99df41d5a-${{ runner.os }}-${{ runner.arch }}-rust-velnor-runner-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('config/fleet/**', 'crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-runner/**/*.rs', 'crates/velnor-runner/benches/**', 'crates/velnor-runner/build.rs', 'crates/velnor-runner/debian/postinst', 'crates/velnor-runner/debian/postrm', 'crates/velnor-runner/debian/preinst', 'crates/velnor-runner/debian/prerm', 'crates/velnor-runner/debian/velnor-control.slice', 'crates/velnor-runner/debian/velnor-controller@.service', 'crates/velnor-runner/debian/velnor-daemon.service', 'crates/velnor-runner/debian/velnor-daemon@.service', 'crates/velnor-runner/debian/velnor-doctor.service', 'crates/velnor-runner/debian/velnor-doctor@.service', 'crates/velnor-runner/debian/velnor-guardian.service', 'crates/velnor-runner/debian/velnor-job@.service', 'crates/velnor-runner/debian/velnor-jobs.slice', 'crates/velnor-runner/debian/velnor-runner.tmpfiles', 'crates/velnor-runner/debian/velnor-slot@.service', 'crates/velnor-runner/debian/velnor.env', 'crates/velnor-runner/examples/**', 'crates/velnor-runner/src/**', 'crates/velnor-runner/src/cache.rs', 'crates/velnor-runner/src/execution/mod.rs', 'crates/velnor-runner/src/executor.rs', 'crates/velnor-runner/src/leftover_disk.rs', 'crates/velnor-runner/src/node/complete.rs', 'crates/velnor-runner/src/node/guardian.rs', 'crates/velnor-runner/src/node/job.rs', 'crates/velnor-runner/src/node/slot.rs', 'crates/velnor-runner/src/release.rs', 'crates/velnor-runner/src/runner.rs', 'crates/velnor-runner/tests/**', 'crates/velnor-runner/tests/fixtures/backend-parity.yml', 'crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.service', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.timer', 'docker/job-mise.lock', 'docker/job-ubuntu.Dockerfile', 'microvm/**', 'microvm/kernel.config', 'microvm/manifest.json', 'microvm/pins.json', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-c9c99df41d5a-${{ runner.os }}-${{ runner.arch }}-rust-velnor-runner-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-c9c99df41d5a-${{ runner.os }}-${{ runner.arch }}-rust-velnor-runner-",
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
        caller_job: "github-rust-velnor-tools",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-15d708957f00-${{ runner.os }}-${{ runner.arch }}-rust-velnor-tools-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('config/fleet/**', 'crates/velnor-client/**/*.rs', 'crates/velnor-client/benches/**', 'crates/velnor-client/examples/**', 'crates/velnor-client/src/**', 'crates/velnor-client/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-runner/src/manifest.rs', 'crates/velnor-tools/**/*.rs', 'crates/velnor-tools/benches/**', 'crates/velnor-tools/examples/**', 'crates/velnor-tools/src/**', 'crates/velnor-tools/tests/**', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-15d708957f00-${{ runner.os }}-${{ runner.arch }}-rust-velnor-tools-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-15d708957f00-${{ runner.os }}-${{ runner.arch }}-rust-velnor-tools-",
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
        caller_job: "github-rust-velnor-workflow",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-434caa7ff878-${{ runner.os }}-${{ runner.arch }}-rust-velnor-workflow-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('Dockerfile', 'config/fleet/**', 'crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-runner/**/*.rs', 'crates/velnor-runner/benches/**', 'crates/velnor-runner/build.rs', 'crates/velnor-runner/debian/postinst', 'crates/velnor-runner/debian/postrm', 'crates/velnor-runner/debian/preinst', 'crates/velnor-runner/debian/prerm', 'crates/velnor-runner/debian/velnor-control.slice', 'crates/velnor-runner/debian/velnor-controller@.service', 'crates/velnor-runner/debian/velnor-daemon.service', 'crates/velnor-runner/debian/velnor-daemon@.service', 'crates/velnor-runner/debian/velnor-doctor.service', 'crates/velnor-runner/debian/velnor-doctor@.service', 'crates/velnor-runner/debian/velnor-guardian.service', 'crates/velnor-runner/debian/velnor-job@.service', 'crates/velnor-runner/debian/velnor-jobs.slice', 'crates/velnor-runner/debian/velnor-runner.tmpfiles', 'crates/velnor-runner/debian/velnor-slot@.service', 'crates/velnor-runner/debian/velnor.env', 'crates/velnor-runner/examples/**', 'crates/velnor-runner/src/**', 'crates/velnor-runner/src/cache.rs', 'crates/velnor-runner/src/execution/mod.rs', 'crates/velnor-runner/src/executor.rs', 'crates/velnor-runner/src/leftover_disk.rs', 'crates/velnor-runner/src/node/complete.rs', 'crates/velnor-runner/src/node/guardian.rs', 'crates/velnor-runner/src/node/job.rs', 'crates/velnor-runner/src/node/slot.rs', 'crates/velnor-runner/src/release.rs', 'crates/velnor-runner/src/runner.rs', 'crates/velnor-runner/tests/**', 'crates/velnor-runner/tests/fixtures/backend-parity.yml', 'crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.service', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.timer', 'crates/velnor-workflow/**/*.rs', 'crates/velnor-workflow/README.md', 'crates/velnor-workflow/benches/**', 'crates/velnor-workflow/build.rs', 'crates/velnor-workflow/examples/**', 'crates/velnor-workflow/src/**', 'crates/velnor-workflow/src/runtime.rs', 'crates/velnor-workflow/templates/**', 'crates/velnor-workflow/templates/release-package-signer.yml', 'crates/velnor-workflow/tests/**', 'docker/build-mise.*', 'docker/job-mise.lock', 'docker/job-ubuntu.Dockerfile', 'microvm/**', 'microvm/kernel.config', 'microvm/manifest.json', 'microvm/pins.json', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-434caa7ff878-${{ runner.os }}-${{ runner.arch }}-rust-velnor-workflow-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-434caa7ff878-${{ runner.os }}-${{ runner.arch }}-rust-velnor-workflow-",
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
        caller_job: "github-rust-velnor-workflow-contract",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'crates/velnor-workflow-contract/Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-2544c2ae90ab-${{ runner.os }}-${{ runner.arch }}-rust-velnor-workflow-contract-${{ hashFiles('.cargo/**', 'crates/velnor-workflow-contract/Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('crates/velnor-workflow-contract/**/*.rs', 'crates/velnor-workflow-contract/Cargo.lock', 'crates/velnor-workflow-contract/benches/**', 'crates/velnor-workflow-contract/examples/**', 'crates/velnor-workflow-contract/src/**', 'crates/velnor-workflow-contract/tests/**', 'mise.lock') }}",
                restore_keys: &[
                    "velnor-mbx-v3-2544c2ae90ab-${{ runner.os }}-${{ runner.arch }}-rust-velnor-workflow-contract-${{ hashFiles('.cargo/**', 'crates/velnor-workflow-contract/Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-2544c2ae90ab-${{ runner.os }}-${{ runner.arch }}-rust-velnor-workflow-contract-",
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
        caller_job: "github-rust-velnorctl",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain.toml', 'rust-toolchain') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-8123d20d5b6a-${{ runner.os }}-${{ runner.arch }}-rust-velnorctl-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-${{ hashFiles('config/fleet/**', 'crates/velnor-client/**/*.rs', 'crates/velnor-client/benches/**', 'crates/velnor-client/examples/**', 'crates/velnor-client/src/**', 'crates/velnor-client/tests/**', 'crates/velnor-control/**/*.rs', 'crates/velnor-control/benches/**', 'crates/velnor-control/examples/**', 'crates/velnor-control/src/**', 'crates/velnor-control/tests/**', 'crates/velnor-model/**/*.rs', 'crates/velnor-model/benches/**', 'crates/velnor-model/examples/**', 'crates/velnor-model/src/**', 'crates/velnor-model/tests/**', 'crates/velnor-render/**/*.rs', 'crates/velnor-render/benches/**', 'crates/velnor-render/examples/**', 'crates/velnor-render/src/**', 'crates/velnor-render/tests/**', 'crates/velnor-runner/**/*.rs', 'crates/velnor-runner/benches/**', 'crates/velnor-runner/build.rs', 'crates/velnor-runner/debian/postinst', 'crates/velnor-runner/debian/postrm', 'crates/velnor-runner/debian/preinst', 'crates/velnor-runner/debian/prerm', 'crates/velnor-runner/debian/velnor-control.slice', 'crates/velnor-runner/debian/velnor-controller@.service', 'crates/velnor-runner/debian/velnor-daemon.service', 'crates/velnor-runner/debian/velnor-daemon@.service', 'crates/velnor-runner/debian/velnor-doctor.service', 'crates/velnor-runner/debian/velnor-doctor@.service', 'crates/velnor-runner/debian/velnor-guardian.service', 'crates/velnor-runner/debian/velnor-job@.service', 'crates/velnor-runner/debian/velnor-jobs.slice', 'crates/velnor-runner/debian/velnor-runner.tmpfiles', 'crates/velnor-runner/debian/velnor-slot@.service', 'crates/velnor-runner/debian/velnor.env', 'crates/velnor-runner/examples/**', 'crates/velnor-runner/src/**', 'crates/velnor-runner/src/cache.rs', 'crates/velnor-runner/src/execution/mod.rs', 'crates/velnor-runner/src/executor.rs', 'crates/velnor-runner/src/leftover_disk.rs', 'crates/velnor-runner/src/node/canary.rs', 'crates/velnor-runner/src/node/complete.rs', 'crates/velnor-runner/src/node/guardian.rs', 'crates/velnor-runner/src/node/job.rs', 'crates/velnor-runner/src/node/slot.rs', 'crates/velnor-runner/src/release.rs', 'crates/velnor-runner/src/runner.rs', 'crates/velnor-runner/tests/**', 'crates/velnor-runner/tests/fixtures/backend-parity.yml', 'crates/velnor-runner/tests/fixtures/telemetry.golden.ndjson', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.service', 'crates/velnor-tools/debian/velnor-fleet-policy-audit.timer', 'crates/velnorctl/**/*.rs', 'crates/velnorctl/benches/**', 'crates/velnorctl/examples/**', 'crates/velnorctl/src/**', 'crates/velnorctl/tests/**', 'docker/job-mise.lock', 'docker/job-ubuntu.Dockerfile', 'microvm/**', 'microvm/kernel.config', 'microvm/manifest.json', 'microvm/pins.json', 'mise.lock', 'schemas/**', 'schemas/velnor.telemetry.v1.json') }}",
                restore_keys: &[
                    "velnor-mbx-v3-8123d20d5b6a-${{ runner.os }}-${{ runner.arch }}-rust-velnorctl-${{ hashFiles('.cargo/**', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') }}-",
                    "velnor-mbx-v3-8123d20d5b6a-${{ runner.os }}-${{ runner.arch }}-rust-velnorctl-",
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
        caller_job: "github-rust-production-topology",
        job: "verify-github",
        keys: &[
            CacheKey {
                layer: "bundle",
                paths: &["~/.cargo/registry", "~/.cargo/git"],
                primary: "ci-${{ runner.os }}-${{ runner.arch }}-rust-${{ hashFiles('Cargo.toml', 'Cargo.lock') }}",
                restore_keys: &[
                    "ci-${{ runner.os }}-${{ runner.arch }}-rust-",
                ],
            },
            CacheKey {
                layer: "mbx",
                paths: &[],
                primary: "velnor-mbx-v3-ed95a0c30f51-${{ runner.os }}-${{ runner.arch }}-rust-production-topology-${{ hashFiles('Cargo.lock', 'Cargo.toml') }}-${{ hashFiles('./**/*.rs', 'crates/velnor-runner/**', 'docker/**', 'microvm/**', 'mise.lock', 'scripts/check-release-feature-boundary.sh', 'scripts/test-check-release-feature-boundary.sh') }}",
                restore_keys: &[
                    "velnor-mbx-v3-ed95a0c30f51-${{ runner.os }}-${{ runner.arch }}-rust-production-topology-${{ hashFiles('Cargo.lock', 'Cargo.toml') }}-",
                    "velnor-mbx-v3-ed95a0c30f51-${{ runner.os }}-${{ runner.arch }}-rust-production-topology-",
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
