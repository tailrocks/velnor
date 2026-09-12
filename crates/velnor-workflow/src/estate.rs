use std::collections::BTreeMap;
use std::env;
use std::path::Path;

use crate::{
    local_github_repository, AnalysisSummary, CacheSpec, GeneratorError, ProjectConfig,
    ReleaseSpec, RunnerMode, Unit, UnitKind,
};

// TODO(rearch): consumer holla still on legacy table.
// TODO(rearch): consumer holla-apt still on legacy table.
// TODO(rearch): consumer jackin still on legacy table.
// TODO(rearch): consumer jackin-the-architect still on legacy table.
// TODO(rearch): consumer jackin-role-action still on legacy table.
// TODO(rearch): consumer jackin-agent-smith still on legacy table.
// TODO(rearch): consumer jackin-agent-brown still on legacy table.
// TODO(rearch): consumer jackin-sentinel still on legacy table.
// TODO(rearch): consumer jackin-dev still on legacy table.
// TODO(rearch): consumer jackin-marketplace still on legacy table.
// TODO(rearch): consumer jackin-github-terraform still on legacy table.
// TODO(rearch): consumer homebrew-tap still on legacy table.
// TODO(rearch): consumer homebrew-holla still on legacy table.
// TODO(rearch): consumer homebrew-parallax still on legacy table.
// TODO(rearch): consumer homebrew-ruxel still on legacy table.
// TODO(rearch): consumer homebrew-tablerock still on legacy table.
// TODO(rearch): consumer termrock still on legacy table.
// TODO(rearch): consumer parallax still on legacy table.
// TODO(rearch): consumer parallax-telemetry-playground still on legacy table.
// TODO(rearch): consumer ruxel still on legacy table.
// TODO(rearch): consumer tablerock still on legacy table.
// TODO(rearch): consumer schemalane still on legacy table.
// TODO(rearch): consumer ChainArgos/blockchain-nodes still on legacy table.
// TODO(rearch): consumer ChainArgos/github-terraform still on legacy table.
// TODO(rearch): consumer ChainArgos/java-monorepo still on legacy table.
//
/// The runner group the apt surfaces select their trust-gated self-hosted
/// lane by. The repo-owned generation config declares its own group; this
/// catalog value covers the apt repositories that have no config yet.
// TODO(rearch): consumer holla-apt still on legacy table.
pub(crate) const APT_VELNOR_RUNNER_GROUP: &str = "velnor-trusted";

/// The `actionlint.yaml` body the catalog surfaces rendered before the labels
/// moved into each repository's generation config. Byte-exact, so a takeover
/// can only adopt that one documented wording.
pub(crate) const LEGACY_ACTIONLINT_CONFIG: &str =
    "self-hosted-runner:\n  labels:\n    - velnor-target-mvp\n";

/// The runner selector the catalog surfaces embedded before the group and the
/// labels moved into each repository's generation config. Static-template
/// adoption replaces it with the declaring repository's own selector.
pub(crate) const LEGACY_VELNOR_RUNNER_SELECTOR: &str =
    "fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]')";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RepositoryProfile {
    Generic,
    RustWorkspaceNative,
    AgentImage,
    HomebrewTap,
    AgentContent,
    OpenTofu,
    CompositeAction,
    PolyglotMonorepo,
    DockerFleet,
    RustCrate,
    RustLibraryDocs,
    SkillsContent,
    RustWorkspaceCrates,
    RustCli,
    RustObservabilityCli,
    RustPlayground,
    AptRepository,
    OpenTofuRust,
}

impl RepositoryProfile {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::RustWorkspaceNative => "rust-workspace-native",
            Self::AgentImage => "agent-image",
            Self::HomebrewTap => "homebrew-tap",
            Self::AgentContent => "agent-content",
            Self::OpenTofu => "opentofu",
            Self::CompositeAction => "composite-action",
            Self::PolyglotMonorepo => "polyglot-monorepo",
            Self::DockerFleet => "docker-fleet",
            Self::RustCrate => "rust-crate",
            Self::RustLibraryDocs => "rust-library-docs",
            Self::SkillsContent => "skills-content",
            Self::RustWorkspaceCrates => "rust-workspace-crates",
            Self::RustCli => "rust-cli",
            Self::RustObservabilityCli => "rust-observability-cli",
            Self::RustPlayground => "rust-playground",
            Self::AptRepository => "apt-repository",
            Self::OpenTofuRust => "opentofu-rust",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EstateProfile {
    pub(crate) repository: &'static str,
    pub(crate) profile: RepositoryProfile,
    /// The self-hosted runner labels this surface renders. Catalog
    /// entries carry their own, because the generator has no default.
    // TODO(rearch): every entry here still renders labels from the
    // catalog; each moves into that repository's generation config.
    pub(crate) labels: &'static [&'static str],
    pub(crate) verified: bool,
    pub(crate) release: Option<&'static str>,
}

/// The self-hosted runner labels every catalog surface renders today.
const RUNNER_LABELS: &[&str] = &["self-hosted", "velnor-target-mvp"];

pub(crate) const ESTATE_PROFILES: &[EstateProfile] = &[
    EstateProfile {
        repository: "ChainArgos/blockchain-nodes",
        profile: RepositoryProfile::DockerFleet,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("docker-fleet"),
    },
    EstateProfile {
        repository: "ChainArgos/github-terraform",
        profile: RepositoryProfile::OpenTofu,
        verified: false,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "ChainArgos/jackin-agent-brown",
        profile: RepositoryProfile::AgentImage,
        verified: false,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "ChainArgos/java-monorepo",
        profile: RepositoryProfile::PolyglotMonorepo,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("docker-fleet"),
    },
    EstateProfile {
        repository: "jackin-project/homebrew-tap",
        profile: RepositoryProfile::HomebrewTap,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "jackin-project/jackin",
        profile: RepositoryProfile::RustWorkspaceNative,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("rust-binary"),
    },
    EstateProfile {
        repository: "jackin-project/jackin-agent-smith",
        profile: RepositoryProfile::AgentImage,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("agent-image"),
    },
    EstateProfile {
        repository: "jackin-project/jackin-dev",
        profile: RepositoryProfile::AgentContent,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "jackin-project/jackin-github-terraform",
        profile: RepositoryProfile::OpenTofu,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "jackin-project/jackin-marketplace",
        profile: RepositoryProfile::AgentContent,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "jackin-project/jackin-role-action",
        profile: RepositoryProfile::CompositeAction,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "jackin-project/jackin-sentinel",
        profile: RepositoryProfile::AgentImage,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("agent-image"),
    },
    EstateProfile {
        repository: "jackin-project/jackin-the-architect",
        profile: RepositoryProfile::AgentImage,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("agent-image"),
    },
    EstateProfile {
        repository: "tailrocks/cloudflare-tofu",
        profile: RepositoryProfile::OpenTofuRust,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "tailrocks/github-terraform",
        profile: RepositoryProfile::OpenTofu,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "tailrocks/holla",
        profile: RepositoryProfile::RustCli,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("rust-binary"),
    },
    EstateProfile {
        repository: "tailrocks/holla-apt",
        profile: RepositoryProfile::AptRepository,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("apt"),
    },
    EstateProfile {
        repository: "tailrocks/homebrew-holla",
        profile: RepositoryProfile::HomebrewTap,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "tailrocks/homebrew-parallax",
        profile: RepositoryProfile::HomebrewTap,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "tailrocks/homebrew-ruxel",
        profile: RepositoryProfile::HomebrewTap,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "tailrocks/homebrew-tablerock",
        profile: RepositoryProfile::HomebrewTap,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "tailrocks/parallax",
        profile: RepositoryProfile::RustObservabilityCli,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("rust-binary"),
    },
    EstateProfile {
        repository: "tailrocks/parallax-telemetry-playground",
        profile: RepositoryProfile::RustPlayground,
        verified: true,
        labels: RUNNER_LABELS,
        release: None,
    },
    EstateProfile {
        repository: "tailrocks/pg-bigdecimal",
        profile: RepositoryProfile::RustCrate,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("crates"),
    },
    EstateProfile {
        repository: "tailrocks/ruxel",
        profile: RepositoryProfile::RustCli,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("rust-binary"),
    },
    EstateProfile {
        repository: "tailrocks/schemalane",
        profile: RepositoryProfile::RustWorkspaceCrates,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("crates"),
    },
    EstateProfile {
        repository: "tailrocks/tablerock",
        profile: RepositoryProfile::RustWorkspaceNative,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("rust-binary"),
    },
    EstateProfile {
        repository: "tailrocks/tailrocks-skills",
        profile: RepositoryProfile::SkillsContent,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("pages"),
    },
    EstateProfile {
        repository: "tailrocks/termrock",
        profile: RepositoryProfile::RustLibraryDocs,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("crates"),
    },
    EstateProfile {
        repository: "tailrocks/tracing-request-level",
        profile: RepositoryProfile::RustCrate,
        verified: true,
        labels: RUNNER_LABELS,
        release: Some("crates"),
    },
];

pub(crate) const FOUR_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

pub(crate) fn estate_profile(repository: &str) -> Option<&'static EstateProfile> {
    ESTATE_PROFILES
        .iter()
        .find(|profile| profile.repository == repository)
}

pub(crate) fn estate_workflow_files(profile: &EstateProfile) -> Vec<String> {
    if !profile.verified {
        return Vec::new();
    }
    // A catalog entry without a scanned checkout owns no units, so it emits
    // no estate-default workflows.
    if profile.profile == RepositoryProfile::Generic {
        return Vec::new();
    }
    let mut files = vec![
        "ci-pr.yml".to_owned(),
        "ci-policy.yml".to_owned(),
        "ci-main.yml".to_owned(),
    ];
    if matches!(
        profile.profile,
        RepositoryProfile::RustWorkspaceNative
            | RepositoryProfile::RustCli
            | RepositoryProfile::RustObservabilityCli
    ) {
        files.push("preview.yml".to_owned());
    }
    if release_spec(profile).is_some() {
        files.push("release.yml".to_owned());
    }
    if matches!(
        profile.profile,
        RepositoryProfile::RustWorkspaceNative
            | RepositoryProfile::RustObservabilityCli
            | RepositoryProfile::SkillsContent
            | RepositoryProfile::PolyglotMonorepo
            | RepositoryProfile::DockerFleet
    ) {
        files.push("nightly.yml".to_owned());
    }
    files.push("maintenance.yml".to_owned());
    files
}

pub(crate) fn catalog_unit(
    id: &str,
    label: &str,
    kind: UnitKind,
    watch: &[&str],
    pr_commands: &[&str],
    full_commands: &[&str],
    cache: Option<CacheSpec>,
) -> Unit {
    Unit {
        id: id.to_owned(),
        label: label.to_owned(),
        kind,
        root: ".".to_owned(),
        pinned_lockfile: false,
        watch: watch.iter().map(|value| (*value).to_owned()).collect(),
        pr_commands: pr_commands
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        full_commands: full_commands
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        github_pr_commands: None,
        github_full_commands: None,
        velnor_pr_commands: None,
        velnor_full_commands: None,
        depends_on: Vec::new(),
        cache,
        tool_version: None,
    }
}

pub(crate) fn unit_with_dependencies(mut unit: Unit, dependencies: &[&str]) -> Unit {
    unit.depends_on = dependencies
        .iter()
        .map(|dependency| (*dependency).to_owned())
        .collect();
    unit
}

pub(crate) fn cargo_cache() -> CacheSpec {
    CacheSpec {
        key_files: vec![
            ".cargo/**".to_owned(),
            "Cargo.toml".to_owned(),
            "Cargo.lock".to_owned(),
            "rust-toolchain.toml".to_owned(),
            "rust-toolchain".to_owned(),
        ],
        paths: vec!["~/.cargo/registry".to_owned(), "~/.cargo/git".to_owned()],
        purpose: crate::CachePurpose::CargoSources,
        mbx_output_cache_justification: None,
    }
}

pub(crate) fn catalog_rust_unit(label: &str, full_command: &str) -> Unit {
    let full_commands = [full_command, crate::PER_CRATE_TEST_COMMAND];
    let mut unit = catalog_unit(
        "rust",
        label,
        UnitKind::Rust,
        &[
            "Cargo.toml",
            "Cargo.lock",
            "**/*.rs",
            "**/Cargo.toml",
            ".cargo/**",
            "rust-toolchain*",
        ],
        &[
            "cargo fmt --all -- --check",
            "cargo clippy --workspace --locked --all-targets --all-features -- -D warnings",
            crate::PER_CRATE_TEST_COMMAND,
        ],
        &full_commands,
        Some(cargo_cache()),
    );
    unit.pinned_lockfile = true;
    unit
}

pub(crate) fn catalog_units(profile: &EstateProfile) -> Vec<Unit> {
    match profile.profile {
        RepositoryProfile::RustWorkspaceNative => vec![
            catalog_rust_unit(
                "Rust workspace",
                "cargo check --workspace --locked --all-targets",
            ),
            unit_with_dependencies(
                catalog_unit(
                    "native",
                    "macOS native",
                    UnitKind::Rust,
                    &["native/**", "**/*ffi*/**"],
                    &["cargo check --locked --all-targets"],
                    &["cargo check --locked --all-targets"],
                    None,
                ),
                &["rust"],
            ),
        ],
        RepositoryProfile::RustObservabilityCli => vec![
            catalog_rust_unit(
                "Rust workspace",
                "cargo check --workspace --locked --all-targets",
            ),
            unit_with_dependencies(
                catalog_unit(
                    "storage",
                    "Storage integration",
                    UnitKind::Rust,
                    &["crates/**/storage/**", "tests/storage/**"],
                    &[crate::PER_CRATE_TEST_COMMAND],
                    &[crate::PER_CRATE_TEST_COMMAND],
                    None,
                ),
                &["rust"],
            ),
        ],
        RepositoryProfile::RustLibraryDocs => vec![
            catalog_rust_unit(
                "Rust workspace",
                "cargo check --workspace --locked --all-targets",
            ),
            catalog_unit(
                "docs",
                "Documentation",
                UnitKind::Docs,
                &["docs/**", "**/*.md"],
                &[
                    "npx --yes markdownlint-cli2@0.20.0 \"**/*.md\" \"#node_modules\" \"#**/AGENTS.md\" \"#**/CLAUDE.md\" \"#target\" \"#**/target/**\" \"#dist\" \"#coverage\" \"#**/.cache/**\"",
                ],
                &[
                    "npx --yes markdownlint-cli2@0.20.0 \"**/*.md\" \"#node_modules\" \"#**/AGENTS.md\" \"#**/CLAUDE.md\" \"#target\" \"#**/target/**\" \"#dist\" \"#coverage\" \"#**/.cache/**\"",
                ],
                None,
            ),
        ],
        RepositoryProfile::RustWorkspaceCrates => {
            vec![catalog_rust_unit(
                "Rust workspace",
                "cargo check --workspace --locked --all-targets",
            )]
        }
        RepositoryProfile::RustCli => vec![catalog_rust_unit(
            "Rust CLI",
            "cargo check --workspace --locked --all-targets",
        )],
        RepositoryProfile::RustCrate => vec![catalog_rust_unit(
            "Rust crate",
            "cargo package --workspace --locked",
        )],
        RepositoryProfile::RustPlayground => vec![catalog_rust_unit(
            "Rust playground",
            "cargo check --workspace --locked --all-targets",
        )],
        RepositoryProfile::AgentImage => vec![
            catalog_unit(
                "role",
                "Role contract",
                UnitKind::Docker,
                &[
                    "Dockerfile",
                    "jackin.role.toml",
                    ".pre-commit-config.yaml",
                    "**/*.toml",
                    "**/*.md",
                ],
                &["git diff --check"],
                &["git diff --check"],
                None,
            ),
            unit_with_dependencies(
                catalog_unit(
                    "image",
                    "Agent image",
                    UnitKind::Docker,
                    &["Dockerfile", "**/*"],
                    &["docker build --file Dockerfile --tag local-ci:agent ."],
                    &["docker build --file Dockerfile --tag local-ci:agent ."],
                    None,
                ),
                &["role"],
            ),
        ],
        RepositoryProfile::HomebrewTap => vec![catalog_unit(
            "homebrew",
            "Homebrew",
            UnitKind::Homebrew,
            &["Formula/**", "Casks/**", "Aliases/**", "Brewfile"],
            &["brew audit --strict --online"],
            &["brew audit --strict --online"],
            None,
        )],
        RepositoryProfile::AgentContent | RepositoryProfile::SkillsContent => vec![catalog_unit(
            "content",
            "Content",
            UnitKind::Bun,
            &[
                "skills/**",
                "scripts/**",
                "**/*.json",
                "**/*.md",
                "package.json",
                "bun.lock",
            ],
            &[
                "bun install --frozen-lockfile",
                "bun test",
                "git diff --check",
            ],
            &[
                "bun install --frozen-lockfile",
                "bun test",
                "git diff --check",
            ],
            Some(CacheSpec {
                key_files: vec!["package.json".to_owned(), "bun.lock".to_owned()],
                paths: vec!["~/.bun/install/cache".to_owned()],
                    purpose: crate::CachePurpose::Generic,
                    mbx_output_cache_justification: None,
            }),
        )],
        RepositoryProfile::CompositeAction => vec![catalog_unit(
            "action",
            "Composite action",
            UnitKind::Docs,
            &["action.yml", "action.yaml", "scripts/**", "**/*.sh"],
            &[
                "git diff --check",
                "find . -type f -name '*.sh' -print0 | xargs -0 -r -n1 bash -n",
            ],
            &[
                "git diff --check",
                "find . -type f -name '*.sh' -print0 | xargs -0 -r -n1 bash -n",
            ],
            None,
        )],
        RepositoryProfile::OpenTofu => vec![catalog_unit(
            "opentofu",
            "OpenTofu",
            UnitKind::OpenTofu,
            &["**/*.tf", "**/*.tofu", "**/.terraform.lock.hcl"],
            &[
                "tofu fmt -check -recursive -no-color",
                "TF_IN_AUTOMATION=1 TF_INPUT=0 tofu init -backend=false -input=false -no-color",
                "TF_IN_AUTOMATION=1 TF_INPUT=0 tofu validate -no-color",
            ],
            &[
                "tofu fmt -check -recursive -no-color",
                "TF_IN_AUTOMATION=1 TF_INPUT=0 tofu init -backend=false -input=false -no-color",
                "TF_IN_AUTOMATION=1 TF_INPUT=0 tofu validate -no-color",
            ],
            Some(CacheSpec {
                key_files: vec!["**/.terraform.lock.hcl".to_owned()],
                paths: vec!["~/.terraform.d/plugin-cache".to_owned()],
                    purpose: crate::CachePurpose::Generic,
                    mbx_output_cache_justification: None,
            }),
        )],
        RepositoryProfile::OpenTofuRust => vec![
            catalog_rust_unit("Rust sidecar", "cargo check --locked"),
            catalog_unit(
                "opentofu",
                "OpenTofu",
                UnitKind::OpenTofu,
                &["**/*.tf", "**/*.tofu", "**/.terraform.lock.hcl"],
                &[
                    "tofu fmt -check -recursive -no-color",
                    "TF_IN_AUTOMATION=1 TF_INPUT=0 tofu validate -no-color",
                ],
                &[
                    "tofu fmt -check -recursive -no-color",
                    "TF_IN_AUTOMATION=1 TF_INPUT=0 tofu validate -no-color",
                ],
                Some(CacheSpec {
                    key_files: vec!["**/.terraform.lock.hcl".to_owned()],
                    paths: vec!["~/.terraform.d/plugin-cache".to_owned()],
                    purpose: crate::CachePurpose::Generic,
                    mbx_output_cache_justification: None,
                }),
            ),
        ],
        RepositoryProfile::PolyglotMonorepo => vec![
            catalog_rust_unit(
                "Rust workspace",
                "cargo check --workspace --locked --all-targets",
            ),
            catalog_unit(
                "gradle",
                "Gradle backend",
                UnitKind::Gradle,
                &["backend/**", "**/*.gradle", "**/*.gradle.kts"],
                &["./gradlew --build-cache --parallel --configuration-cache check"],
                &["./gradlew --build-cache --parallel --configuration-cache check"],
                Some(CacheSpec {
                    key_files: vec![
                        "backend/settings.gradle.kts".to_owned(),
                        "backend/gradle.lockfile".to_owned(),
                    ],
                    paths: vec![
                        "~/.gradle/caches".to_owned(),
                        "~/.gradle/wrapper".to_owned(),
                    ],
                    purpose: crate::CachePurpose::Generic,
                    mbx_output_cache_justification: None,
                }),
            ),
        ],
        RepositoryProfile::DockerFleet => vec![catalog_rust_unit(
            "Build tooling",
            "cargo check --workspace --locked --all-targets",
        )],
        RepositoryProfile::AptRepository => vec![catalog_unit(
            "apt",
            "APT contract",
            UnitKind::Docs,
            &["conf/**", "scripts/**", "package-state.json", "*.gpg"],
            &[
                "git diff --check",
                "find scripts -type f -name '*.sh' -print0 | xargs -0 -r -n1 bash -n",
            ],
            &[
                "git diff --check",
                "find scripts -type f -name '*.sh' -print0 | xargs -0 -r -n1 bash -n",
            ],
            None,
        )],
        RepositoryProfile::Generic => Vec::new(),
    }
}

pub(crate) fn release_spec(profile: &EstateProfile) -> Option<ReleaseSpec> {
    let kind = profile.release?;
    let mut spec = ReleaseSpec {
        kind: kind.to_owned(),
        package: String::new(),
        packages: Vec::new(),
        binary: String::new(),
        targets: Vec::new(),
        image: String::new(),
        source_repository: String::new(),
        consumer_repository: String::new(),
        artifact_path: String::new(),
        description: String::new(),
    };
    match profile.repository {
        "jackin-project/jackin" => {
            "jackin".clone_into(&mut spec.package);
            "jackin".clone_into(&mut spec.binary);
            spec.targets = FOUR_TARGETS
                .iter()
                .map(|target| (*target).to_owned())
                .collect();
            "jackin-project/homebrew-tap".clone_into(&mut spec.consumer_repository);
            "Agentic development environment and role runner".clone_into(&mut spec.description);
        }
        "tailrocks/tablerock" => {
            "tablerock-cli".clone_into(&mut spec.package);
            "tablerock".clone_into(&mut spec.binary);
            spec.targets = FOUR_TARGETS
                .iter()
                .map(|target| (*target).to_owned())
                .collect();
            "tailrocks/homebrew-tablerock".clone_into(&mut spec.consumer_repository);
            "Native database and table tooling".clone_into(&mut spec.description);
        }
        "tailrocks/ruxel" => {
            "ruxel-cli".clone_into(&mut spec.package);
            "ruxel".clone_into(&mut spec.binary);
            spec.targets = FOUR_TARGETS
                .iter()
                .map(|target| (*target).to_owned())
                .collect();
            "tailrocks/homebrew-ruxel".clone_into(&mut spec.consumer_repository);
            "Rust-native automation without YAML archaeology".clone_into(&mut spec.description);
        }
        "tailrocks/parallax" => {
            "parallax-cli".clone_into(&mut spec.package);
            "parallax".clone_into(&mut spec.binary);
            spec.targets = FOUR_TARGETS
                .iter()
                .map(|target| (*target).to_owned())
                .collect();
            "tailrocks/homebrew-parallax".clone_into(&mut spec.consumer_repository);
            "Telemetry and observability development toolkit".clone_into(&mut spec.description);
        }
        "tailrocks/holla" => {
            "holla-cli".clone_into(&mut spec.package);
            "holla".clone_into(&mut spec.binary);
            spec.targets = FOUR_TARGETS
                .iter()
                .map(|target| (*target).to_owned())
                .collect();
            "tailrocks/homebrew-holla".clone_into(&mut spec.consumer_repository);
            "Adaptive development environment CLI".clone_into(&mut spec.description);
        }
        "tailrocks/tracing-request-level" => {
            "tracing-request-level".clone_into(&mut spec.package);
            spec.packages = vec![spec.package.clone()];
        }
        "tailrocks/pg-bigdecimal" => {
            "pg-bigdecimal".clone_into(&mut spec.package);
            spec.packages = vec![spec.package.clone()];
        }
        "tailrocks/termrock" => {
            "termrock".clone_into(&mut spec.package);
            spec.packages = vec![spec.package.clone()];
        }
        "tailrocks/schemalane" => {
            "schemalane-cli".clone_into(&mut spec.package);
            spec.packages = [
                "schemalane-version",
                "schemalane-macros",
                "pg_query_fmt",
                "schemalane-core",
                "schemalane-cli",
            ]
            .iter()
            .map(|package| (*package).to_owned())
            .collect();
        }
        "tailrocks/tailrocks-skills" => {
            "docs".clone_into(&mut spec.artifact_path);
            "Skills documentation".clone_into(&mut spec.description);
        }
        "tailrocks/holla-apt" => {
            "tailrocks/holla".clone_into(&mut spec.source_repository);
            "holla".clone_into(&mut spec.package);
        }
        // Research identifies these as family candidates, but no immutable
        // image name or fleet DAG was verified. Do not emit a publisher.
        _ => return None,
    }
    crate::primitives::release::release_contract_complete(&spec).then_some(spec)
}

pub(crate) fn catalog_config_with_default_branch(
    profile: &'static EstateProfile,
    runners: RunnerMode,
    default_branch: &str,
) -> ProjectConfig {
    let release = profile.verified.then(|| release_spec(profile)).flatten();
    let mut notes = vec![
        "Profile and workflow membership are Rust-owned catalog data derived from the repository inventory.".to_owned(),
    ];
    let mut limitations = vec![
        "Catalog metadata is explicit review input; it is not a substitute for source analysis."
            .to_owned(),
    ];
    if matches!(
        profile.profile,
        RepositoryProfile::PolyglotMonorepo | RepositoryProfile::DockerFleet
    ) {
        limitations.push(
            "Image build task is omitted until a source-backed Dockerfile or native builder command is detected; use a direct source scan or explicit profile input.".to_owned(),
        );
    }
    if !profile.verified {
        notes.push(
            "SOURCE PREFLIGHT BLOCKED: repository could not be verified; no workflows are emitted until reviewed."
                .to_owned(),
        );
    }
    if profile.release.is_some() && release.is_none() {
        notes.push(
            "RELEASE PREFLIGHT BLOCKED: artifact, registry, platform, or signing metadata is incomplete; publisher omitted."
                .to_owned(),
        );
    }
    let mut config = ProjectConfig {
        repository: profile.repository.to_owned(),
        profile: profile.profile.as_str().to_owned(),
        analysis: AnalysisSummary {
            method: "reviewed-catalog".to_owned(),
            detected: vec![format!("catalog-profile:{}", profile.profile.as_str())],
            limitations,
        },
        verified: profile.verified,
        workflow_files: estate_workflow_files(profile),
        notes,
        version_bump_units: Vec::new(),
        default_branch: default_branch.to_owned(),
        runners,
        github_runner: "ubuntu-24.04".to_owned(),
        velnor_labels: profile
            .labels
            .iter()
            .map(|label| (*label).to_owned())
            .collect(),
        release_enabled: release.is_some(),
        release_reason: if release.is_some() {
            "Release contract is explicitly cataloged; review credentials, environments, and tag protection before cutover.".to_owned()
        } else {
            "Release is fail-closed until source, artifact, registry, provenance, and credential contracts are verified.".to_owned()
        },
        release,
        units: if profile.verified {
            catalog_units(profile)
        } else {
            Vec::new()
        },
        workflow_templates: BTreeMap::new(),
        adopted_workflow_surface: false,
        actionlint_config_variables_null: false,
        ci_required: true,
        package_update_channels: None,
        velnor_runner_group: None,
        static_files: Vec::new(),
        declared_surface: true,
    };
    crate::enable_mr_boxington_commands(&mut config);
    config
}

pub(crate) fn render_apt_package_updater_template(
    template: &str,
    default_branch: &str,
    github_runner: &str,
    velnor_labels: &[String],
    velnor_group: Option<&str>,
) -> String {
    let Some((prefix, jobs)) = template.split_once("\n  verify:\n") else {
        return normalize_apt_package_updater_static_template(template, github_runner);
    };
    let Some((verify_body, mutate_body)) = jobs.split_once("\n  mutate:\n") else {
        return template.to_owned();
    };
    let trusted_gate = apt_velnor_trusted_gate(default_branch);
    let runners = AptLaneRunners {
        github_runner,
        velnor_labels,
        velnor_group,
    };
    let mut lanes = String::new();
    for (id, body, job, lane) in [
        (
            "verify-velnor",
            verify_body,
            AptPackageUpdaterJob::Verify,
            AptPackageUpdaterLane::Velnor,
        ),
        (
            "verify-github",
            verify_body,
            AptPackageUpdaterJob::Verify,
            AptPackageUpdaterLane::Github,
        ),
        (
            "mutate-velnor",
            mutate_body,
            AptPackageUpdaterJob::Mutate,
            AptPackageUpdaterLane::Velnor,
        ),
        (
            "mutate-github",
            mutate_body,
            AptPackageUpdaterJob::Mutate,
            AptPackageUpdaterLane::Github,
        ),
    ] {
        lanes.push_str(&render_apt_package_updater_job(
            id,
            body,
            job,
            lane,
            &trusted_gate,
            &runners,
        ));
        lanes.push('\n');
    }
    format!("{prefix}\n{}", lanes.trim_end_matches('\n'))
}

fn normalize_apt_package_updater_static_template(template: &str, github_runner: &str) -> String {
    let mut output = String::with_capacity(template.len());
    let mut github_job = false;
    for segment in template.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        if indentation == 2 && trimmed.ends_with(':') {
            github_job = matches!(trimmed, "verify-github:" | "mutate-github:");
        }
        if github_job
            && indentation == 4
            && trimmed.starts_with("runs-on:")
            && trimmed != "runs-on:"
        {
            output.push_str(&line[..indentation]);
            output.push_str("runs-on: ");
            output.push_str(&crate::yaml_scalar(github_runner));
            if segment.ends_with('\n') {
                output.push('\n');
            }
        } else {
            output.push_str(segment);
        }
    }
    output
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AptPackageUpdaterJob {
    Verify,
    Mutate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AptPackageUpdaterLane {
    Github,
    Velnor,
}

/// The runner placement every lane of a rendered `package-update.yml` selects
/// from: the hosted image the GitHub lane pins, and the self-hosted labels and
/// group the Velnor lane selects by.
struct AptLaneRunners<'a> {
    github_runner: &'a str,
    velnor_labels: &'a [String],
    velnor_group: Option<&'a str>,
}

fn render_apt_package_updater_job(
    id: &str,
    body: &str,
    job: AptPackageUpdaterJob,
    lane: AptPackageUpdaterLane,
    trusted_gate: &str,
    runners: &AptLaneRunners<'_>,
) -> String {
    let lane_name = match lane {
        AptPackageUpdaterLane::Github => "github",
        AptPackageUpdaterLane::Velnor => "velnor",
    };
    let trusted_condition = if lane == AptPackageUpdaterLane::Velnor {
        format!(" && {trusted_gate}")
    } else {
        String::new()
    };
    let mut body = body.to_owned();
    match job {
        AptPackageUpdaterJob::Verify => {
            body = body.replace(
                "    if: ${{ inputs.consumer-repository != '' }}",
                &format!(
                    "    if: ${{{{ inputs.consumer-repository != '' && inputs.lane == '{lane_name}'{trusted_condition} }}}}"
                ),
            );
        }
        AptPackageUpdaterJob::Mutate => {
            body = body.replace(
                "    needs: verify",
                &format!("    needs: verify-{lane_name}"),
            );
            body = body.replace(
                "    if: ${{ needs.verify.outputs.available == 'true' && inputs.writer }}",
                &format!(
                    "    if: ${{{{ needs.verify-{lane_name}.outputs.available == 'true' && inputs.writer{trusted_condition} }}}}"
                ),
            );
            // The mutate body can consume more than the availability output.
            // Rewrite every remaining dependency reference after renaming the
            // verify job so split lanes cannot retain a dangling needs.verify.
            body = body.replace("needs.verify.", &format!("needs.verify-{lane_name}."));
        }
    }
    format!(
        "  {id}:\n{}",
        replace_apt_package_updater_runner(&body, lane, runners)
    )
}

fn replace_apt_package_updater_runner(
    body: &str,
    lane: AptPackageUpdaterLane,
    runners: &AptLaneRunners<'_>,
) -> String {
    let mut output = String::with_capacity(body.len() + 96);
    let mut replaced = false;
    for segment in body.split_inclusive('\n') {
        let runner_line = segment.strip_suffix('\n').unwrap_or(segment);
        if runner_line.trim_start().starts_with("runs-on:")
            && runner_line.contains("${{")
            && runner_line.contains("inputs.lane")
        {
            match lane {
                AptPackageUpdaterLane::Github => {
                    output.push_str("    runs-on: ");
                    output.push_str(&crate::yaml_scalar(runners.github_runner));
                }
                AptPackageUpdaterLane::Velnor => {
                    output.push_str("    runs-on:");
                    output.push_str(&velnor_lane_selector(
                        runners.velnor_labels,
                        runners.velnor_group,
                    ));
                }
            }
            if segment.ends_with('\n') {
                output.push('\n');
            }
            replaced = true;
        } else {
            output.push_str(segment);
        }
    }
    // A generated workflow can be adopted again. In that case the runner
    // selector is already static, so preserve it instead of treating the
    // absence of a replacement as a malformed source template.
    if !replaced {
        return body.to_owned();
    }
    output
}

/// The static `runs-on:` value the updater's Velnor lane renders: the group
/// and the labels the repository declares for its self-hosted lane.
fn velnor_lane_selector(labels: &[String], group: Option<&str>) -> String {
    let labels = labels
        .iter()
        .map(|label| crate::yaml_scalar(label))
        .collect::<Vec<_>>()
        .join(", ");
    match group {
        Some(group) => format!(
            "\n      group: {}\n      labels: [{labels}]",
            crate::yaml_scalar(group)
        ),
        None => format!("[{labels}]"),
    }
}

/// The owner blocks the rendered `package-update.yml` declares, in template
/// order. The repo-owned channel grants are validated against this set, so a
/// typo'd block name is a usage error instead of a narrowed publish lane; the
/// renderer below walks the same shape to attribute each channel matrix to its
/// block.
pub(crate) fn apt_package_update_owner_blocks(template: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut in_jobs = false;
    for segment in template.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        if indentation == 0 {
            in_jobs = trimmed == "jobs:";
            continue;
        }
        if in_jobs && indentation == 2 && trimmed.ends_with(':') {
            blocks.push(trimmed.strip_suffix(':').unwrap_or(trimmed));
        }
    }
    blocks
}

pub(crate) fn render_apt_package_update_template(config: &ProjectConfig, template: &str) -> String {
    let mut output = String::with_capacity(template.len());
    let mut replaced = false;
    let mut block: Option<&str> = None;
    let mut in_jobs = false;
    for segment in template.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        // Owner blocks are the top-level jobs of this workflow; the channel
        // matrix inside each one is rewritten from catalog data below.
        if indentation == 0 {
            in_jobs = trimmed == "jobs:";
        }
        if in_jobs && indentation == 2 && trimmed.ends_with(':') {
            block = Some(trimmed.strip_suffix(':').unwrap_or(trimmed));
        }
        let is_channel_matrix = indentation == 8 && trimmed.starts_with("channel:");
        if let Some(block) = block.filter(|_| is_channel_matrix) {
            output.push_str(&line[..indentation]);
            output.push_str(&apt_package_update_channel_matrix(
                &package_update_channel_grant(config, block),
            ));
            if segment.ends_with('\n') {
                output.push('\n');
            }
            replaced = true;
        } else if trimmed.starts_with("runs-on:")
            && line.contains("${{")
            && line.contains("inputs.lanes")
            && line.contains("velnor")
        {
            let indent = &line[..indentation];
            output.push_str(indent);
            output.push_str("runs-on: ");
            output.push_str(&crate::yaml_scalar(&config.github_runner));
            if segment.ends_with('\n') {
                output.push('\n');
            }
            replaced = true;
        } else {
            output.push_str(segment);
        }
    }
    // A generated workflow can be adopted again. In that case the barrier
    // selector is already static, so preserve it instead of panicking while
    // checking an otherwise valid generated file.
    if !replaced {
        return template.to_owned();
    }
    output
}

/// Update channels an owner block of the rendered `package-update.yml` matrix
/// may consult. A channel is granted only where the consumer-side
/// `package-updater.yml` implements that channel's arm: a granted-but-
/// unimplemented channel renders a scheduled job that can never succeed. The
/// grant is per owner block, because one rendered file is shared by every
/// owner: jackin's updater predates the preview lane and already carries a
/// preview arm, and no `ChainArgos` consumer implements one, so both blocks are
/// consumer-independent. The tailrocks block is granted per estate — only
/// velnor-apt's own package-updater.yml verifies and publishes the preview
/// channel today, so the other tailrocks consumers keep `channel: [stable]`.
/// Never returns an empty grant.
pub(crate) fn apt_package_update_channels(block: &str) -> &'static [&'static str] {
    match block {
        "jackin_project" => &["stable", "preview"],
        _ => &["stable"],
    }
}

/// The update channels one owner block of the rendered `package-update.yml`
/// matrix may consult. The repo-owned config grants channels explicitly; the
/// catalog grant applies only while the repository has not adopted a config. A
/// declared grant table is total before it reaches this renderer —
/// `config::validate` rejects a table that leaves an owner block without a row
/// or a `default` — so the catalog table below is the only fallback.
pub(crate) fn package_update_channel_grant(config: &ProjectConfig, block: &str) -> Vec<String> {
    if let Some(grants) = &config.package_update_channels {
        return grants
            .get(block)
            .or_else(|| grants.get("default"))
            .cloned()
            .unwrap_or_else(|| vec!["stable".to_owned()]);
    }
    apt_package_update_channels(block)
        .iter()
        .map(|channel| (*channel).to_owned())
        .collect()
}

fn apt_package_update_channel_matrix(channels: &[String]) -> String {
    let rendered = channels
        .iter()
        .map(|channel| crate::yaml_scalar(channel))
        .collect::<Vec<_>>()
        .join(", ");
    format!("channel: [{rendered}]")
}

fn apt_velnor_trusted_gate(default_branch: &str) -> String {
    format!(
        "github.ref == 'refs/heads/{default_branch}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')"
    )
}

/// Apply the estate's runner contract to a repository the catalog still
/// serves and that has no generation config of its own. Everything the three
/// migrated repositories used to take from here is declared in their configs
/// now; this path is the boundary that remains.
pub(crate) fn apply_legacy_runner_profile(root: &Path, mut config: ProjectConfig) -> ProjectConfig {
    let Some(repository) = local_github_repository(root) else {
        return config;
    };
    let Some(profile) = estate_profile(&repository) else {
        return config;
    };
    config.repository = repository;
    profile.profile.as_str().clone_into(&mut config.profile);
    config.velnor_labels = profile
        .labels
        .iter()
        .map(|label| (*label).to_owned())
        .collect();
    if profile.profile == RepositoryProfile::AptRepository {
        config.velnor_runner_group = Some(APT_VELNOR_RUNNER_GROUP.to_owned());
    }
    config
}

/// The runner group a repository selects its self-hosted lane by while it has
/// no generation config of its own.
pub(crate) fn legacy_velnor_runner_group(repository: &str) -> Option<&'static str> {
    estate_profile(repository)
        .is_some_and(|profile| profile.profile == RepositoryProfile::AptRepository)
        .then_some(APT_VELNOR_RUNNER_GROUP)
}

pub(crate) fn run_estate(cli: &crate::Cli) -> Result<(), GeneratorError> {
    let root = env::current_dir()
        .map_err(|error| GeneratorError::usage(format!("resolve current directory: {error}")))?
        .canonicalize()
        .map_err(|error| {
            GeneratorError::usage(format!("canonicalize current directory: {error}"))
        })?;
    let output_root = match cli.output.as_deref() {
        Some(path) => crate::resolve_output_path(path)?,
        None => root.join("repositories"),
    };
    let mut generated = 0usize;
    let default_branch = match cli.default_branch.as_deref() {
        Some(branch) => crate::validate_default_branch(branch)?.to_owned(),
        None => "main".to_owned(),
    };
    for entry in ESTATE_PROFILES {
        let profile = estate_profile(entry.repository).ok_or_else(|| {
            GeneratorError::usage(format!("missing catalog profile: {}", entry.repository))
        })?;
        let (owner, repository) = profile.repository.split_once('/').ok_or_else(|| {
            GeneratorError::usage(format!(
                "invalid catalog repository: {}",
                profile.repository
            ))
        })?;
        let destination = crate::output_root_for_repository(&output_root, owner, repository)?;
        let mut config = catalog_config_with_default_branch(profile, cli.runners, &default_branch);
        if cli.adopt {
            config = crate::adopt_existing_workflow_templates(&destination, config)?;
        }
        let files = crate::generated_files(&config)?;
        // Catalog generation is a function of code-owned profiles, not of a
        // scanned shape or a repo-owned config, so its recorded inputs are the
        // canonical "no config, no scan" form.
        let inputs = crate::GenerationInputs::parts(0, 0);
        let outcome = crate::write_generated_with_options(
            &destination,
            &files,
            &inputs,
            cli.dry_run,
            cli.check,
            cli.force,
            cli.adopt,
        )?;
        match &outcome {
            crate::WriteOutcome::Written { .. } | crate::WriteOutcome::Unchanged => generated += 1,
            crate::WriteOutcome::DryRun(changed) => {
                if !changed.is_empty() {
                    println!(
                        "{}: {}",
                        profile.repository,
                        crate::display_paths(changed.iter())
                    );
                }
            }
        }
    }
    if cli.dry_run {
        println!("Estate dry-run inspected {generated} catalog profiles");
    } else {
        println!(
            "Generated {generated} catalog profiles in {}",
            output_root.display()
        );
    }
    Ok(())
}
