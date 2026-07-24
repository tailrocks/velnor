use std::collections::BTreeMap;
use std::fmt;

use anyhow::Result;
use serde::Serialize;

use crate::action::{
    string_inputs, unsupported_action_error, NativeActionAdapter, NATIVE_ACTION_REF,
};
use crate::cli::{CapabilitiesArgs, CapabilitiesCommand};
use crate::compiler_cache::CompilerCacheBackend;
use crate::job_message::{ActionReferenceType, AgentJobRequestMessage};

// Plan 009 introduced v6 (action subpaths + reusable-workflow schema). Plan 010
// adds source-SHA + crate-version identity to the exported manifest so a consumer
// can bind the compiled manifest to one release commit, bumping the schema to v7.
pub const MANIFEST_VERSION: u32 = 7;

#[derive(Debug, Clone, Copy)]
pub struct CapabilityManifest {
    pub version: u32,
    pub actions: &'static [ActionCapability],
    pub reusable_workflows: &'static [ReusableWorkflow],
}

#[derive(Debug, Clone, Copy)]
pub struct ActionCapability {
    pub repository: &'static str,
    pub adapter: NativeActionAdapter,
    pub allowed_refs: &'static [AllowedRef],
    /// Non-root action subpaths this repository exposes (for example
    /// `actions/cache` exposes `restore` and `save`). The root action is always
    /// admissible; any subpath outside this set fails closed. Empty means the
    /// repository is only ever used at its root.
    pub allowed_subpaths: &'static [&'static str],
    pub inputs: &'static [InputRule],
    pub notes: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct AllowedRef {
    pub value: &'static str,
    pub release: &'static str,
}

/// A server-expanded reusable workflow (`jobs.<id>.uses`) that Velnor admits.
/// This is distinct from a runner-side action: GitHub resolves the workflow
/// server-side and dispatches an expanded job to Velnor, so admission cross-
/// checks the workflow's repository, path, and immutable full-SHA ref plus the
/// dispatched inputs — it never parses `jobs.<id>.uses` as a runner action.
#[derive(Debug, Clone, Copy)]
pub struct ReusableWorkflow {
    pub repository: &'static str,
    /// Workflow file path within the repository, e.g.
    /// `.github/workflows/publish.yml`.
    pub path: &'static str,
    pub allowed_refs: &'static [AllowedRef],
    pub inputs: &'static [InputRule],
    pub notes: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum InputRule {
    Any(&'static str),
    Literal(&'static str, &'static [&'static str]),
    RequiredLiteral(&'static str, &'static [&'static str]),
    Forbidden(&'static str),
    /// Value is admissible iff the pure predicate returns true. `accepted`
    /// documents the constraint in violation messages (the value itself stays
    /// redacted). Used for the strict mise date-version and `install_args`
    /// tool-key shape rules; membership against the committed lock is enforced
    /// at install time by `crate::mise`.
    Predicate(&'static str, fn(&str) -> bool, &'static [&'static str]),
}

impl InputRule {
    fn name(self) -> &'static str {
        match self {
            Self::Any(name)
            | Self::Literal(name, _)
            | Self::RequiredLiteral(name, _)
            | Self::Forbidden(name)
            | Self::Predicate(name, _, _) => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityViolation {
    pub step: String,
    pub repository: String,
    pub action_ref: String,
    pub field: String,
    pub received: String,
    pub accepted: Vec<String>,
    pub manifest_version: u32,
}

impl fmt::Display for CapabilityViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported capability in step '{}': action {}@{}, field '{}' received [redacted]; accepted: {}; manifest version {}",
            self.step,
            self.repository,
            self.action_ref,
            self.field,
            if self.accepted.is_empty() {
                "none".to_string()
            } else {
                self.accepted.join(", ")
            },
            self.manifest_version
        )
    }
}

impl std::error::Error for CapabilityViolation {}

const fn allowed(value: &'static str, release: &'static str) -> AllowedRef {
    AllowedRef { value, release }
}

const CHECKOUT_REFS: &[AllowedRef] = &[
    allowed(NATIVE_ACTION_REF, "broker-managed checkout"),
    allowed("9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0", "v7"),
    allowed("3d3c42e5aac5ba805825da76410c181273ba90b1", "v7"),
    allowed("df4cb1c069e1874edd31b4311f1884172cec0e10", "v6"),
    allowed("34e114876b0b11c390a56381ad16ebd13914f8d5", "v4"),
    allowed("v4", "fixture transition until plan 041"),
    allowed("v6", "fixture transition until plan 041"),
    allowed("v7", "fixture transition until plan 041"),
];
const CACHE_REFS: &[AllowedRef] = &[
    allowed("55cc8345863c7cc4c66a329aec7e433d2d1c52a9", "v6"),
    allowed("27d5ce7f107fe9357f9df03efb73ab90386fccae", "v5"),
];
const UPLOAD_REFS: &[AllowedRef] = &[
    allowed("043fb46d1a93c77aae656e7c1c64a875d1fc6a0a", "v7"),
    allowed("ea165f8d65b6e75b540449e92b4886f43607fa02", "v4"),
    allowed("v7", "fixture transition until plan 041"),
];
const DOWNLOAD_REFS: &[AllowedRef] = &[
    allowed("3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c", "v8"),
    allowed("v8", "fixture transition until plan 041"),
];
const MISE_REFS: &[AllowedRef] = &[
    allowed("dad1bfd3df957f44999b559dd69dc1671cb4e9ea", "v4.2.1"),
    allowed("e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d", "v4.2.0"),
    allowed("dba19683ed58901619b14f395a24841710cb4925", "v4.1.0"),
    allowed("v4", "fixture transition until plan 041"),
];
const SCCACHE_REFS: &[AllowedRef] = &[
    allowed("9e7fa8a12102821edf02ca5dbea1acd0f89a2696", "v0.0.10"),
    allowed("v0.0.10", "fixture transition until plan 041"),
];
const MOLD_REFS: &[AllowedRef] = &[
    allowed("9c9c13bf4c3f1adef0cc596abc155580bcb04444", "v1"),
    allowed("v1", "fixture transition until plan 041"),
];
const RUST_CACHE_REFS: &[AllowedRef] = &[
    allowed("42dc69e1aa15d09112580998cf2ef0119e2e91ae", "v2"),
    allowed("c19371144df3bb44fab255c43d04cbc2ab54d1c4", "v2"),
    allowed("e18b497796c12c097a38f9edb9d0641fb99eee32", "v2"),
    allowed("v2", "fixture transition until plan 041"),
];
const PATHS_REFS: &[AllowedRef] = &[
    allowed("7b450fff21473bca461d4b92ce414b9d0420d706", "v4"),
    allowed("v4", "fixture transition until plan 041"),
];
const RUNTIME_REFS: &[AllowedRef] = &[
    allowed("04d248b84655b509d8c44dc1d6f990c879747487", "v4"),
    allowed("v4", "fixture transition until plan 041"),
];
const GITHUB_SCRIPT_REFS: &[AllowedRef] =
    &[allowed("373c709c69115d41ff229c7e5df9f8788daa9553", "v9")];
const GITHUB_SCRIPT_INPUTS: &[InputRule] = &[
    InputRule::Any("github-token"),
    InputRule::Literal(
        "script",
        &[
            "core.setOutput('docs-xtask', process.env.CONTRACT)",
            "return await import(process.env.JACKIN_ACTION_RUNTIME).then(({ main }) => main())",
        ],
    ),
];
const RENOVATE_REFS: &[AllowedRef] = &[
    allowed("3064367f740a1a91cca218698a63902689cce200", "v46"),
    allowed("22e0a16091fc706b04affe6ae53d5e3358ac4023", "v44"),
    allowed("693b9ef15eec82123529a37c782242f091365961", "v43"),
];
const BUILDX_REFS: &[AllowedRef] = &[
    allowed("bb05f3f5519dd87d3ba754cc423b652a5edd6d2c", "v4"),
    allowed("v4", "fixture transition until plan 041"),
];
const LOGIN_REFS: &[AllowedRef] = &[
    allowed("af1e73f918a031802d376d3c8bbc3fe56130a9b0", "v4"),
    allowed("v4", "fixture transition until plan 041"),
];
const BAKE_REFS: &[AllowedRef] = &[
    allowed("d3418bd7d0e9324001bca92fa8ba175ea7e6dc9b", "v7"),
    allowed("v7", "fixture transition until plan 041"),
];

const CACHE_INPUTS: &[InputRule] = &[
    InputRule::Any("path"),
    InputRule::Any("key"),
    InputRule::Any("restore-keys"),
    InputRule::Literal("fail-on-cache-miss", &["true", "false"]),
    InputRule::Literal("lookup-only", &["true", "false"]),
];
const ARTIFACT_INPUTS: &[InputRule] = &[
    InputRule::Any("name"),
    InputRule::Any("path"),
    InputRule::Literal("if-no-files-found", &["warn", "error", "ignore"]),
    InputRule::Literal("include-hidden-files", &["true", "false"]),
    InputRule::Literal("overwrite", &["true", "false"]),
    InputRule::Literal("compression-level", &["0"]),
    InputRule::Literal("retention-days", &["1", "7", "14", "30", "90"]),
];
const DOWNLOAD_INPUTS: &[InputRule] = &[
    InputRule::Any("name"),
    InputRule::Any("pattern"),
    InputRule::Any("path"),
    InputRule::Literal("merge-multiple", &["true", "false"]),
];
const MISE_INPUTS: &[InputRule] = &[
    // Exact mise date-version only; omission (empty) resolves to the fleet pin.
    InputRule::Predicate(
        "version",
        crate::mise::is_valid_mise_version,
        &["exact YYYY.M.D mise version, or omitted for the fleet-pinned latest"],
    ),
    InputRule::Literal("install", &["true", "false"]),
    // Whitespace-separated bare tool keys; membership in the committed lock is
    // enforced fail-closed at install time (crate::mise).
    InputRule::Predicate(
        "install_args",
        crate::mise::is_valid_install_args_shape,
        &["whitespace-separated tool keys committed in mise.lock (no flags/@version/URLs/paths)"],
    ),
    InputRule::Any("working_directory"),
    InputRule::Any("github_token"),
    InputRule::Literal("cache_key_prefix", &["mise-v2"]),
    InputRule::Literal("cache_save", &["true", "false"]),
];
const SCCACHE_INPUTS: &[InputRule] = &[
    InputRule::Literal("version", &["v0.16.0"]),
    InputRule::Literal("disable_annotations", &["false"]),
    InputRule::Forbidden("token"),
];
const KACHE_INPUTS: &[InputRule] = &[
    InputRule::Literal("version", &["v0.10.0"]),
    InputRule::Literal("github-cache", &["false"]),
    InputRule::Literal("cache-executables", &["false"]),
    InputRule::Literal("pr-comment", &["false"]),
    InputRule::Literal("max-size", &["20GiB"]),
    InputRule::Forbidden("token"),
    InputRule::Forbidden("sync"),
    InputRule::Forbidden("warm"),
    InputRule::Forbidden("manifest-key"),
    InputRule::Forbidden("namespace"),
    InputRule::Forbidden("min-compile-ms"),
    InputRule::Forbidden("cache-key-prefix"),
];
const RUST_CACHE_INPUTS: &[InputRule] = &[
    InputRule::Any("shared-key"),
    InputRule::Any("cache-directories"),
    InputRule::Literal("cache-on-failure", &["true", "false"]),
];
const BUILDX_INPUTS: &[InputRule] = &[
    InputRule::Any("name"),
    InputRule::Literal("driver", &["docker-container"]),
    InputRule::Literal("install", &["true", "false"]),
    InputRule::Literal("cleanup", &["true", "false"]),
    InputRule::Literal("keep-state", &["true", "false"]),
    InputRule::Literal(
        "buildkitd-config-inline",
        &["[registry.\"docker.io\"]\n  mirrors = [\"mirror.gcr.io\"]"],
    ),
];
const LOGIN_INPUTS: &[InputRule] = &[
    InputRule::Any("registry"),
    InputRule::Any("username"),
    InputRule::Any("password"),
];
const BUILD_PUSH_INPUTS: &[InputRule] = &[
    InputRule::Any("context"),
    InputRule::Any("file"),
    InputRule::Any("platforms"),
    InputRule::Any("tags"),
    InputRule::Any("labels"),
    InputRule::Any("build-args"),
    InputRule::Any("secrets"),
    InputRule::Any("cache-from"),
    InputRule::Any("cache-to"),
    InputRule::Any("outputs"),
    InputRule::Literal("push", &["true", "false"]),
    InputRule::Literal("load", &["true", "false"]),
];

macro_rules! capability {
    ($repo:literal, $adapter:ident, $refs:expr, $inputs:expr) => {
        capability!($repo, $adapter, $refs, $inputs, subpaths: &[])
    };
    ($repo:literal, $adapter:ident, $refs:expr, $inputs:expr, subpaths: $subpaths:expr) => {
        ActionCapability {
            repository: $repo,
            adapter: NativeActionAdapter::$adapter,
            allowed_refs: $refs,
            allowed_subpaths: $subpaths,
            inputs: $inputs,
            notes: "native Rust adapter; estate pin sweep 2026-07-18",
        }
    };
}

pub static ACTIONS: &[ActionCapability] = &[
    ActionCapability {
        repository: "jackin-project/jackin-role-action",
        adapter: NativeActionAdapter::ApprovedComposite,
        allowed_refs: &[
            allowed(
                "80a1acd07257a23b441c546e6fcad12239ef7626",
                "estate-pinned composite",
            ),
            allowed(
                "889e01e1fec152cc68271385f8976319244d9251",
                "latest-build artifact API lookup (#80)",
            ),
        ],
        allowed_subpaths: &[],
        inputs: &[
            InputRule::Any("path"),
            InputRule::Any("jackin-version"),
            InputRule::Literal("skip-build", &["true", "false"]),
            InputRule::Any("registry-cache-image"),
        ],
        notes: "pinned remote composite; expanded into strictly validated native adapters",
    },
    capability!(
        "actions/checkout",
        Checkout,
        CHECKOUT_REFS,
        &[
            InputRule::Any("repository"),
            InputRule::Any("ref"),
            InputRule::Any("token"),
            InputRule::Literal("persist-credentials", &["true", "false"]),
            InputRule::Any("path"),
            InputRule::Literal("clean", &["true", "false"]),
            InputRule::Any("fetch-depth"),
            InputRule::Literal("fetch-tags", &["true", "false"]),
            InputRule::Literal("lfs", &["true", "false"]),
        ]
    ),
    capability!(
        "actions/cache",
        Cache,
        CACHE_REFS,
        CACHE_INPUTS,
        subpaths: &["restore", "save"]
    ),
    capability!(
        "actions/attest-build-provenance",
        AttestBuildProvenance,
        &[allowed(
            "0f67c3f4856b2e3261c31976d6725780e5e4c373",
            "v4.1.1"
        )],
        &[InputRule::RequiredLiteral(
            "subject-path",
            &["dist/*.tar.gz"]
        )]
    ),
    capability!(
        "actions/upload-artifact",
        UploadArtifact,
        UPLOAD_REFS,
        ARTIFACT_INPUTS
    ),
    capability!(
        "actions/github-script",
        GitHubScript,
        GITHUB_SCRIPT_REFS,
        GITHUB_SCRIPT_INPUTS
    ),
    capability!(
        "actions/download-artifact",
        DownloadArtifact,
        DOWNLOAD_REFS,
        DOWNLOAD_INPUTS
    ),
    capability!(
        "actions/upload-pages-artifact",
        UploadPagesArtifact,
        &[
            allowed("fc324d3547104276b827a68afc52ff2a11cc49c9", "v5"),
            allowed("v5", "fixture transition until plan 041")
        ],
        &[InputRule::Any("path"), InputRule::Any("name")]
    ),
    capability!(
        "actions/configure-pages",
        ConfigurePages,
        &[allowed(
            "45bfe0192ca1faeb007ade9deae92b16b8254a0d",
            "v6.0.0"
        )],
        &[
            InputRule::Any("token"),
            InputRule::Literal("enablement", &["false"]),
            InputRule::Forbidden("static_site_generator"),
            InputRule::Forbidden("generator_config_file")
        ]
    ),
    capability!(
        "actions/deploy-pages",
        DeployPages,
        &[
            allowed("cd2ce8fcbc39b97be8ca5fce6e763baed58fa128", "v5"),
            allowed("v5", "fixture transition until plan 041")
        ],
        &[
            InputRule::Any("token"),
            InputRule::Any("timeout"),
            InputRule::Any("error_count"),
            InputRule::Any("reporting_interval"),
            InputRule::Literal("preview", &["true", "false"]),
            InputRule::Any("artifact_name")
        ]
    ),
    capability!(
        "dorny/paths-filter",
        PathsFilter,
        PATHS_REFS,
        &[
            InputRule::Any("filters"),
            InputRule::Any("base"),
            InputRule::Any("ref"),
            InputRule::Any("list-files"),
            InputRule::Any("working-directory"),
            // An explicitly empty token is the upstream action's supported
            // way to force local git classification without API calls.
            InputRule::Literal("token", &[""])
        ]
    ),
    capability!("jdx/mise-action", Mise, MISE_REFS, MISE_INPUTS),
    capability!(
        "mozilla-actions/sccache-action",
        Sccache,
        SCCACHE_REFS,
        SCCACHE_INPUTS
    ),
    capability!(
        "kunobi-ninja/kache-action",
        Kache,
        &[allowed("49398d37113c616fdb61be434cb497e3c2c8f3e6", "v1")],
        KACHE_INPUTS
    ),
    capability!("rui314/setup-mold", SetupMold, MOLD_REFS, &[]),
    capability!(
        "extractions/setup-just",
        SetupJust,
        &[allowed("53165ef7e734c5c07cb06b3c8e7b647c5aa16db3", "v4")],
        &[]
    ),
    capability!(
        "swatinem/rust-cache",
        RustCache,
        RUST_CACHE_REFS,
        RUST_CACHE_INPUTS
    ),
    capability!(
        "crazy-max/ghaction-github-runtime",
        GitHubRuntimeExport,
        RUNTIME_REFS,
        &[]
    ),
    capability!(
        "renovatebot/github-action",
        Renovate,
        RENOVATE_REFS,
        &[
            InputRule::Any("token"),
            InputRule::Any("renovate-version"),
            InputRule::Any("renovate-image")
        ]
    ),
    capability!(
        "docker/setup-buildx-action",
        DockerSetupBuildx,
        BUILDX_REFS,
        BUILDX_INPUTS
    ),
    capability!("docker/login-action", DockerLogin, LOGIN_REFS, LOGIN_INPUTS),
    capability!(
        "docker/metadata-action",
        DockerMetadata,
        &[
            allowed("dc802804100637a589fabce1cb79ff13a1411302", "v6"),
            allowed("v6", "fixture transition until plan 041"),
        ],
        &[InputRule::Any("images"), InputRule::Any("tags")]
    ),
    capability!(
        "docker/build-push-action",
        DockerBuildPush,
        &[
            allowed("53b7df96c91f9c12dcc8a07bcb9ccacbed38856a", "v7"),
            allowed("v7", "fixture transition until plan 041"),
        ],
        BUILD_PUSH_INPUTS
    ),
    capability!(
        "docker/bake-action",
        DockerBake,
        BAKE_REFS,
        &[
            InputRule::Any("files"),
            InputRule::Any("set"),
            InputRule::Literal("push", &["true", "false"]),
            InputRule::Any("targets")
        ]
    ),
    capability!(
        "hadolint/hadolint-action",
        Hadolint,
        &[allowed(
            "2332a7b74a6de0dda2e2221d575162eba76ba5e5",
            "v3.3.0"
        )],
        &[
            InputRule::Any("dockerfile"),
            InputRule::Any("config"),
            InputRule::Literal("recursive", &["true", "false"]),
            InputRule::Any("output-file"),
            InputRule::Literal("no-color", &["true", "false"]),
            InputRule::Literal("no-fail", &["true", "false"]),
            InputRule::Literal("verbose", &["true", "false"]),
            InputRule::Any("format"),
            InputRule::Any("failure-threshold"),
            InputRule::Any("override-error"),
            InputRule::Any("override-warning"),
            InputRule::Any("override-info"),
            InputRule::Any("override-style"),
            InputRule::Any("ignore"),
            InputRule::Any("trusted-registries")
        ]
    ),
    capability!(
        "docker/setup-qemu-action",
        SetupQemu,
        &[allowed("c7c53464625b32c7a7e944ae62b3e17d2b600130", "v3")],
        &[
            InputRule::Any("image"),
            InputRule::Any("platforms"),
            InputRule::Literal("reset", &["true", "false"])
        ]
    ),
    capability!(
        "sigstore/cosign-installer",
        CosignInstaller,
        &[allowed(
            "6f9f17788090df1f26f669e9d70d6ae9567deba6",
            "v4.1.2"
        )],
        &[
            InputRule::Any("cosign-release"),
            InputRule::Any("install-dir")
        ]
    ),
];

/// Inputs dispatched to the approved publish reusable workflow via
/// `jobs.<id>.with`. These mirror the composite `jackin-role-action` surface the
/// estate already admits; the exact `on.workflow_call.inputs` schema of
/// `publish.yml@80a1acd0…` MUST be reconciled against that immutable file before
/// real in-order execution (plan 009 STOP: "exact current workflow schema cannot
/// be derived without new approval").
const PUBLISH_WORKFLOW_INPUTS: &[InputRule] = &[
    InputRule::Any("jackin-version"),
    InputRule::Literal("skip-build", &["true", "false"]),
    InputRule::Any("registry-cache-image"),
];

pub static REUSABLE_WORKFLOWS: &[ReusableWorkflow] = &[ReusableWorkflow {
    repository: "jackin-project/jackin-role-action",
    path: ".github/workflows/publish.yml",
    allowed_refs: &[allowed(
        "80a1acd07257a23b441c546e6fcad12239ef7626",
        "estate-pinned publish reusable workflow",
    )],
    inputs: PUBLISH_WORKFLOW_INPUTS,
    notes: "server-expanded reusable workflow; identity/full-SHA/inputs admitted, jobs.<id>.uses never parsed as a runner action",
}];

pub static MANIFEST: CapabilityManifest = CapabilityManifest {
    version: MANIFEST_VERSION,
    actions: ACTIONS,
    reusable_workflows: REUSABLE_WORKFLOWS,
};

/// Exact `(repository, mutable-tag)` identities that are deliberately retained as
/// a documented N2 exception while pre-plan-041 fixtures still reference mutable
/// tags. `assert_manifest_integrity` accepts these and ONLY these mutable refs;
/// every other non-`__native` ref must be a full 40-hex SHA. Remove this
/// allowlist when plan 041 migrates the fixture to immutable refs.
const PLAN_041_FIXTURE_TRANSITION_ALLOWLIST: &[(&str, &str)] = &[
    ("actions/checkout", "v4"),
    ("actions/checkout", "v6"),
    ("actions/checkout", "v7"),
    ("actions/upload-artifact", "v7"),
    ("actions/download-artifact", "v8"),
    ("jdx/mise-action", "v4"),
    ("mozilla-actions/sccache-action", "v0.0.10"),
    ("rui314/setup-mold", "v1"),
    ("swatinem/rust-cache", "v2"),
    ("dorny/paths-filter", "v4"),
    ("crazy-max/ghaction-github-runtime", "v4"),
    ("docker/setup-buildx-action", "v4"),
    ("docker/login-action", "v4"),
    ("docker/bake-action", "v7"),
    ("actions/upload-pages-artifact", "v5"),
    ("actions/deploy-pages", "v5"),
    ("docker/metadata-action", "v6"),
    ("docker/build-push-action", "v7"),
];

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_unsafe_subpath(subpath: &str) -> bool {
    subpath.is_empty()
        || subpath.starts_with('/')
        || subpath
            .split('/')
            .any(|segment| segment == ".." || segment.is_empty())
}

/// Startup integrity gate for the compiled manifest. Run once before any work is
/// accepted; a violation is a programming error in the compiled manifest, so it
/// fails hard (`bail!`). It rejects any non-`__native` action/workflow ref that
/// is neither a full 40-hex SHA nor an explicit plan-041 transition tag,
/// duplicate action/workflow identities, duplicate or unsafe subpaths, and
/// `__native` used for anything but broker-managed checkout.
pub fn assert_manifest_integrity() -> Result<()> {
    assert_manifest_integrity_of(ACTIONS, REUSABLE_WORKFLOWS)
}

fn assert_manifest_integrity_of(
    actions: &[ActionCapability],
    reusable_workflows: &[ReusableWorkflow],
) -> Result<()> {
    let mut seen_repositories: Vec<String> = Vec::new();
    for capability in actions {
        let repository = capability.repository.to_ascii_lowercase();
        if seen_repositories.contains(&repository) {
            anyhow::bail!("manifest integrity: duplicate action repository '{repository}'");
        }
        seen_repositories.push(repository.clone());

        let mut seen_refs: Vec<&str> = Vec::new();
        for allowed_ref in capability.allowed_refs {
            if seen_refs.contains(&allowed_ref.value) {
                anyhow::bail!(
                    "manifest integrity: duplicate ref '{}' for '{}'",
                    allowed_ref.value,
                    capability.repository
                );
            }
            seen_refs.push(allowed_ref.value);

            if allowed_ref.value == NATIVE_ACTION_REF {
                // `__native` authorizes broker-managed checkout only; it must
                // never stand in for a metadata-fetched action ref.
                if capability.adapter != NativeActionAdapter::Checkout {
                    anyhow::bail!(
                        "manifest integrity: '__native' ref is only valid for broker-managed checkout, found on '{}'",
                        capability.repository
                    );
                }
                continue;
            }
            if is_full_sha(allowed_ref.value) {
                continue;
            }
            if PLAN_041_FIXTURE_TRANSITION_ALLOWLIST
                .iter()
                .any(|(repo, tag)| {
                    repo.eq_ignore_ascii_case(capability.repository) && *tag == allowed_ref.value
                })
            {
                continue;
            }
            anyhow::bail!(
                "manifest integrity: mutable ref '{}' for '{}' is neither a 40-hex SHA nor a plan-041 transition tag",
                allowed_ref.value,
                capability.repository
            );
        }

        let mut seen_subpaths: Vec<&str> = Vec::new();
        for subpath in capability.allowed_subpaths {
            if is_unsafe_subpath(subpath) {
                anyhow::bail!(
                    "manifest integrity: unsafe subpath '{}' for '{}'",
                    subpath,
                    capability.repository
                );
            }
            if seen_subpaths.contains(subpath) {
                anyhow::bail!(
                    "manifest integrity: duplicate subpath '{}' for '{}'",
                    subpath,
                    capability.repository
                );
            }
            seen_subpaths.push(subpath);
        }
    }

    let mut seen_workflows: Vec<(String, String)> = Vec::new();
    for workflow in reusable_workflows {
        let identity = (
            workflow.repository.to_ascii_lowercase(),
            workflow.path.to_ascii_lowercase(),
        );
        if seen_workflows.contains(&identity) {
            anyhow::bail!(
                "manifest integrity: duplicate reusable workflow '{}/{}'",
                workflow.repository,
                workflow.path
            );
        }
        seen_workflows.push(identity);
        if is_unsafe_subpath(workflow.path) {
            anyhow::bail!(
                "manifest integrity: unsafe reusable workflow path '{}'",
                workflow.path
            );
        }
        // Reusable-workflow admission is new in manifest v6; it grants no
        // plan-041 transition exception — every ref must be a full 40-hex SHA.
        for allowed_ref in workflow.allowed_refs {
            if !is_full_sha(allowed_ref.value) {
                anyhow::bail!(
                    "manifest integrity: reusable workflow '{}/{}' ref '{}' is not a 40-hex SHA",
                    workflow.repository,
                    workflow.path,
                    allowed_ref.value
                );
            }
        }
    }
    Ok(())
}

/// Look up a reusable workflow capability by repository and workflow path.
pub fn find_reusable_workflow(repository: &str, path: &str) -> Option<&'static ReusableWorkflow> {
    let path = path.trim_start_matches('/');
    REUSABLE_WORKFLOWS.iter().find(|workflow| {
        workflow.repository.eq_ignore_ascii_case(repository)
            && workflow.path.trim_start_matches('/') == path
    })
}

pub fn find(repository: &str) -> Option<&'static ActionCapability> {
    ACTIONS
        .iter()
        .find(|capability| capability.repository.eq_ignore_ascii_case(repository))
}

pub fn validate_resolved_action(
    step: &str,
    repository: &str,
    action_ref: &str,
    source_path: Option<&str>,
    inputs: &BTreeMap<String, String>,
) -> Result<()> {
    let capability = find(repository).ok_or_else(|| {
        violation(
            step,
            repository,
            action_ref,
            "uses",
            repository,
            ACTIONS
                .iter()
                .map(|item| item.repository.to_string())
                .collect(),
        )
    })?;
    if !capability
        .allowed_refs
        .iter()
        .any(|candidate| candidate.value == action_ref)
    {
        return Err(violation(
            step,
            repository,
            action_ref,
            "ref",
            action_ref,
            capability
                .allowed_refs
                .iter()
                .map(|candidate| candidate.value.to_string())
                .collect(),
        )
        .into());
    }
    if let Some(error) = subpath_violation(step, repository, action_ref, source_path, capability) {
        return Err(error.into());
    }
    let mut found = Vec::new();
    validate_inputs(
        &mut found,
        step,
        repository,
        action_ref,
        capability.inputs,
        inputs,
    );
    if let Some(error) = found.into_iter().next() {
        return Err(error.into());
    }
    Ok(())
}

/// Validate an action subpath against the capability's admitted subpaths. Root
/// (absent/empty) is always admissible; a traversal/absolute path or any subpath
/// outside the declared set fails closed.
fn subpath_violation(
    step: &str,
    repository: &str,
    action_ref: &str,
    source_path: Option<&str>,
    capability: &ActionCapability,
) -> Option<CapabilityViolation> {
    let subpath = source_path
        .map(|value| value.trim().trim_matches('/'))
        .filter(|value| !value.is_empty())?;
    if is_unsafe_subpath(subpath)
        || !capability
            .allowed_subpaths
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(subpath))
    {
        let mut accepted = vec!["<root>".to_string()];
        accepted.extend(capability.allowed_subpaths.iter().map(|s| s.to_string()));
        return Some(violation(
            step, repository, action_ref, "path", subpath, accepted,
        ));
    }
    None
}

/// Validate a server-expanded reusable workflow identity and its dispatched
/// inputs. `workflow_ref` must be the immutable full-SHA ref carried by
/// `github.job_workflow_ref`; a mutable ref, unknown identity, or disallowed
/// input fails closed. The received value is never surfaced.
pub fn validate_reusable_workflow(
    step: &str,
    repository: &str,
    path: &str,
    workflow_ref: &str,
    inputs: &BTreeMap<String, String>,
) -> Result<()> {
    let identity = format!("{repository}/{path}");
    let workflow = find_reusable_workflow(repository, path).ok_or_else(|| {
        violation(
            step,
            &identity,
            workflow_ref,
            "uses",
            &identity,
            REUSABLE_WORKFLOWS
                .iter()
                .map(|item| format!("{}/{}", item.repository, item.path))
                .collect(),
        )
    })?;
    if !workflow
        .allowed_refs
        .iter()
        .any(|candidate| candidate.value == workflow_ref)
    {
        return Err(violation(
            step,
            &identity,
            workflow_ref,
            "ref",
            workflow_ref,
            workflow
                .allowed_refs
                .iter()
                .map(|candidate| candidate.value.to_string())
                .collect(),
        )
        .into());
    }
    let mut found = Vec::new();
    validate_inputs(
        &mut found,
        step,
        &identity,
        workflow_ref,
        workflow.inputs,
        inputs,
    );
    if let Some(error) = found.into_iter().next() {
        return Err(error.into());
    }
    Ok(())
}

/// Whether `input_name` is capability-affecting (constrained) for `repository`:
/// true unless the repository declares it as a free-form `Any` input. An unknown
/// repository or input counts as constrained (it is rejected outright), so an
/// unresolved `${{ … }}` expression in such an input must fail admission.
pub fn action_input_is_constrained(repository: &str, input_name: &str) -> bool {
    match find(repository) {
        Some(capability) => !capability.inputs.iter().any(
            |rule| matches!(rule, InputRule::Any(name) if name.eq_ignore_ascii_case(input_name)),
        ),
        None => true,
    }
}

pub fn violations(job: &AgentJobRequestMessage) -> Vec<CapabilityViolation> {
    violations_with_context(job, &[])
}

pub fn violations_with_context(
    job: &AgentJobRequestMessage,
    context_data: &[(String, serde_json::Value)],
) -> Vec<CapabilityViolation> {
    let mut violations = Vec::new();
    for (index, step) in job
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.enabled)
    {
        if step.reference_type() != Some(ActionReferenceType::Repository) {
            continue;
        }
        let Some(reference) = step.reference.as_ref() else {
            continue;
        };
        let Some(repository) = reference.name.as_deref() else {
            continue;
        };
        if repository.starts_with("./")
            || reference
                .path
                .as_deref()
                .is_some_and(|path| path.starts_with("./"))
        {
            continue;
        }
        let step_name = step
            .display_name_template()
            .or_else(|| step.name.clone())
            .unwrap_or_else(|| format!("step-{index}"));
        let action_ref = reference.git_ref.as_deref().unwrap_or(
            if repository.eq_ignore_ascii_case("actions/checkout") {
                NATIVE_ACTION_REF
            } else {
                "<missing>"
            },
        );
        let Some(capability) = find(repository) else {
            let accepted = unsupported_action_error(repository)
                .map(|message| vec![message.to_string()])
                .unwrap_or_else(|| {
                    ACTIONS
                        .iter()
                        .map(|item| item.repository.to_string())
                        .collect()
                });
            violations.push(violation(
                &step_name, repository, action_ref, "uses", repository, accepted,
            ));
            continue;
        };
        if !capability
            .allowed_refs
            .iter()
            .any(|candidate| candidate.value == action_ref)
        {
            violations.push(violation(
                &step_name,
                repository,
                action_ref,
                "ref",
                action_ref,
                capability
                    .allowed_refs
                    .iter()
                    .map(|item| format!("{} ({})", item.value, item.release))
                    .collect(),
            ));
        }
        if let Some(error) = subpath_violation(
            &step_name,
            repository,
            action_ref,
            reference.path.as_deref(),
            capability,
        ) {
            violations.push(error);
        }
        let inputs = match string_inputs(step) {
            Ok(inputs) => inputs
                .into_iter()
                .map(|(name, value)| {
                    (
                        name,
                        crate::executor::render_context_expressions(&value, context_data),
                    )
                })
                .collect(),
            Err(error) => {
                violations.push(violation(
                    &step_name,
                    repository,
                    action_ref,
                    "inputs",
                    &error.to_string(),
                    Vec::new(),
                ));
                continue;
            }
        };
        validate_inputs(
            &mut violations,
            &step_name,
            repository,
            action_ref,
            capability.inputs,
            &inputs,
        );
    }
    validate_compiler_cache_topology(job, &mut violations);
    validate_attestation_permissions(job, &mut violations);
    violations
}

fn validate_attestation_permissions(
    job: &AgentJobRequestMessage,
    violations: &mut Vec<CapabilityViolation>,
) {
    let uses_attestation = job.steps.iter().filter(|step| step.enabled).any(|step| {
        step.reference
            .as_ref()
            .and_then(|reference| reference.name.as_deref())
            .is_some_and(|repository| {
                repository.eq_ignore_ascii_case("actions/attest-build-provenance")
            })
    });
    if !uses_attestation {
        return;
    }
    let has_id_token_endpoint = job.system_connection().is_some_and(|endpoint| {
        endpoint.data.iter().any(|(name, value)| {
            matches!(
                name.to_ascii_lowercase().replace(['-', '_'], "").as_str(),
                "generateidtokenurl" | "actionsidtokenrequesturl"
            ) && !value.trim().is_empty()
        })
    });
    if !has_id_token_endpoint {
        violations.push(violation(
            "job preflight",
            "actions/attest-build-provenance",
            "permissions",
            "permissions.id-token",
            "absent",
            vec!["write".into()],
        ));
    }
    let parsed = job
        .variables
        .get("system.github.token.permissions")
        .and_then(|variable| variable.value.as_deref())
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .and_then(|value| value.as_object().cloned());
    let Some(permissions) = parsed else {
        violations.push(violation(
            "job preflight",
            "actions/attest-build-provenance",
            "permissions",
            "permissions",
            "absent or malformed",
            vec!["contents: read, attestations: write".into()],
        ));
        return;
    };
    for (scope, accepted) in [("contents", "read"), ("attestations", "write")] {
        let received = permissions
            .iter()
            .find(|(name, _)| name.to_ascii_lowercase().replace(['-', '_'], "") == scope)
            .and_then(|(_, value)| value.as_str())
            .unwrap_or("absent");
        if !received.eq_ignore_ascii_case(accepted) {
            violations.push(violation(
                "job preflight",
                "actions/attest-build-provenance",
                "permissions",
                &format!("permissions.{scope}"),
                received,
                vec![accepted.into()],
            ));
        }
    }
}

pub fn compiler_cache_backend(job: &AgentJobRequestMessage) -> CompilerCacheBackend {
    let mut sccache = false;
    let mut kache = false;
    for step in job.steps.iter().filter(|step| step.enabled) {
        let repository = step
            .reference
            .as_ref()
            .and_then(|reference| reference.name.as_deref());
        sccache |= repository
            .is_some_and(|name| name.eq_ignore_ascii_case("mozilla-actions/sccache-action"));
        kache |=
            repository.is_some_and(|name| name.eq_ignore_ascii_case("kunobi-ninja/kache-action"));
    }
    match (sccache, kache) {
        (true, false) => CompilerCacheBackend::Sccache,
        (false, true) => CompilerCacheBackend::Kache,
        _ => CompilerCacheBackend::Off,
    }
}

fn validate_compiler_cache_topology(
    job: &AgentJobRequestMessage,
    violations: &mut Vec<CapabilityViolation>,
) {
    let wrappers = job
        .steps
        .iter()
        .filter(|step| step.enabled)
        .filter_map(|step| {
            step.reference
                .as_ref()
                .and_then(|reference| reference.name.as_deref())
        })
        .filter(|repository| {
            repository.eq_ignore_ascii_case("mozilla-actions/sccache-action")
                || repository.eq_ignore_ascii_case("kunobi-ninja/kache-action")
        })
        .collect::<Vec<_>>();
    if wrappers
        .iter()
        .any(|repository| repository.eq_ignore_ascii_case("mozilla-actions/sccache-action"))
        && wrappers
            .iter()
            .any(|repository| repository.eq_ignore_ascii_case("kunobi-ninja/kache-action"))
    {
        violations.push(violation(
            "job preflight",
            "compiler-cache",
            "mixed",
            "compiler-cache.backend",
            "sccache+kache",
            vec!["off".into(), "sccache".into(), "kache".into()],
        ));
    }

    let mut environment = Vec::new();
    for value in &job.environment_variables {
        collect_environment_names(value, &mut environment);
    }
    for step in job.steps.iter().filter(|step| step.enabled) {
        if let Some(value) = &step.environment {
            collect_environment_names(value, &mut environment);
        }
    }
    environment.extend(job.variables.keys().cloned());
    environment.sort_unstable();
    environment.dedup();
    for name in environment {
        let upper = name.to_ascii_uppercase().replace('-', "_");
        let forbidden = upper == "SCCACHE_DIR"
            || upper.starts_with("SCCACHE_BUCKET")
            || upper.starts_with("SCCACHE_ENDPOINT")
            || upper.starts_with("SCCACHE_REGION")
            || upper.starts_with("SCCACHE_S3_")
            || upper.starts_with("SCCACHE_REDIS")
            || upper.starts_with("SCCACHE_MEMCACHED")
            || upper.starts_with("SCCACHE_GCS")
            || upper.starts_with("SCCACHE_AZURE")
            || upper.starts_with("SCCACHE_WEBDAV")
            || upper == "KACHE_CACHE_DIR"
            || upper.starts_with("KACHE_S3_")
            || upper.starts_with("KACHE_REMOTE")
            || upper.starts_with("KACHE_PLANNER");
        if forbidden {
            violations.push(violation(
                "job preflight",
                "compiler-cache",
                "environment",
                &format!("env.{name}"),
                "provided",
                vec!["variable must be absent; local stores are runner-owned".into()],
            ));
        }
    }
}

fn collect_environment_names(value: &serde_json::Value, names: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(name) = object
                .get("Key")
                .or_else(|| object.get("key"))
                .and_then(template_literal)
            {
                names.push(name.to_string());
            } else {
                names.extend(
                    object
                        .keys()
                        .filter(|key| !matches!(key.as_str(), "type" | "Type" | "map" | "Map"))
                        .cloned(),
                );
            }
            for nested in object.get("map").or_else(|| object.get("Map")).into_iter() {
                collect_environment_names(nested, names);
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                collect_environment_names(nested, names);
            }
        }
        _ => {}
    }
}

fn template_literal(value: &serde_json::Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .as_object()
            .and_then(|object| object.get("lit").or_else(|| object.get("Lit")))
            .and_then(serde_json::Value::as_str)
    })
}

fn validate_inputs(
    violations: &mut Vec<CapabilityViolation>,
    step: &str,
    repository: &str,
    action_ref: &str,
    rules: &[InputRule],
    inputs: &BTreeMap<String, String>,
) {
    for (name, value) in inputs {
        match rules
            .iter()
            .copied()
            .find(|rule| rule.name().eq_ignore_ascii_case(name))
        {
            Some(InputRule::Any(_)) => {}
            Some(InputRule::Literal(_, allowed) | InputRule::RequiredLiteral(_, allowed))
                if allowed
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(value.trim())) => {}
            Some(InputRule::Literal(_, allowed) | InputRule::RequiredLiteral(_, allowed)) => {
                violations.push(violation(
                    step,
                    repository,
                    action_ref,
                    &format!("with.{name}"),
                    value,
                    allowed.iter().map(|value| (*value).to_string()).collect(),
                ))
            }
            Some(InputRule::Predicate(_, check, _)) if check(value.trim()) => {}
            Some(InputRule::Predicate(_, _, accepted)) => violations.push(violation(
                step,
                repository,
                action_ref,
                &format!("with.{name}"),
                value,
                accepted.iter().map(|value| (*value).to_string()).collect(),
            )),
            Some(InputRule::Forbidden(_)) => violations.push(violation(
                step,
                repository,
                action_ref,
                &format!("with.{name}"),
                value,
                vec!["input must be absent".to_string()],
            )),
            None => violations.push(violation(
                step,
                repository,
                action_ref,
                &format!("with.{name}"),
                value,
                rules.iter().map(|rule| rule.name().to_string()).collect(),
            )),
        }
    }
    for rule in rules {
        if let InputRule::RequiredLiteral(name, allowed) = rule {
            if !inputs.keys().any(|input| input.eq_ignore_ascii_case(name)) {
                violations.push(violation(
                    step,
                    repository,
                    action_ref,
                    &format!("with.{name}"),
                    "absent",
                    allowed.iter().map(|value| (*value).to_string()).collect(),
                ));
            }
        }
    }
}

fn violation(
    step: &str,
    repository: &str,
    action_ref: &str,
    field: &str,
    received: &str,
    accepted: Vec<String>,
) -> CapabilityViolation {
    CapabilityViolation {
        step: step.to_string(),
        repository: repository.to_ascii_lowercase(),
        action_ref: action_ref.to_string(),
        field: field.to_string(),
        received: received.to_string(),
        accepted,
        manifest_version: MANIFEST_VERSION,
    }
}

pub fn validate_job_with_context(
    job: &AgentJobRequestMessage,
    context_data: &[(String, serde_json::Value)],
) -> Result<()> {
    if let Some(violation) = violations_with_context(job, context_data)
        .into_iter()
        .next()
    {
        return Err(violation.into());
    }
    Ok(())
}

#[derive(Serialize)]
struct ExportManifest<'a> {
    version: u32,
    /// Plan 010: the exact source commit this binary (and therefore this compiled
    /// manifest) was built from. `development` for non-release builds. Lets a
    /// consumer bind the manifest to one release record.
    source_sha: &'a str,
    /// The crate version compiled into this binary.
    crate_version: &'a str,
    actions: Vec<ExportAction<'a>>,
    reusable_workflows: Vec<ExportReusableWorkflow<'a>>,
}
#[derive(Serialize)]
struct ExportAction<'a> {
    repository: &'a str,
    adapter: String,
    allowed_refs: Vec<&'a str>,
    allowed_subpaths: Vec<&'a str>,
    inputs: Vec<&'a str>,
    notes: &'a str,
}
#[derive(Serialize)]
struct ExportReusableWorkflow<'a> {
    repository: &'a str,
    path: &'a str,
    allowed_refs: Vec<&'a str>,
    inputs: Vec<&'a str>,
    notes: &'a str,
}

pub fn to_json() -> Result<String> {
    let actions = MANIFEST
        .actions
        .iter()
        .map(|item| ExportAction {
            repository: item.repository,
            adapter: format!("{:?}", item.adapter),
            allowed_refs: item
                .allowed_refs
                .iter()
                .map(|reference| reference.value)
                .collect(),
            allowed_subpaths: item.allowed_subpaths.to_vec(),
            inputs: item.inputs.iter().map(|input| input.name()).collect(),
            notes: item.notes,
        })
        .collect();
    let reusable_workflows = MANIFEST
        .reusable_workflows
        .iter()
        .map(|item| ExportReusableWorkflow {
            repository: item.repository,
            path: item.path,
            allowed_refs: item
                .allowed_refs
                .iter()
                .map(|reference| reference.value)
                .collect(),
            inputs: item.inputs.iter().map(|input| input.name()).collect(),
            notes: item.notes,
        })
        .collect();
    Ok(serde_json::to_string_pretty(&ExportManifest {
        version: MANIFEST.version,
        source_sha: env!("VELNOR_SOURCE_SHA"),
        crate_version: env!("CARGO_PKG_VERSION"),
        actions,
        reusable_workflows,
    })?)
}

pub fn run(args: CapabilitiesArgs) -> Result<()> {
    match args.command {
        CapabilitiesCommand::Export => println!("{}", to_json()?),
        CapabilitiesCommand::Check { job_dump } => {
            let bytes = std::fs::read(&job_dump)?;
            let job: AgentJobRequestMessage = serde_json::from_slice(&bytes)?;
            let violations = violations(&job);
            if violations.is_empty() {
                println!(
                    "job is compatible with capability manifest version {}",
                    MANIFEST_VERSION
                );
            } else {
                for violation in &violations {
                    eprintln!("{violation}");
                }
                anyhow::bail!(
                    "job has {} capability violation(s) against manifest version {}",
                    violations.len(),
                    MANIFEST_VERSION
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(
        repository: &str,
        action_ref: Option<&str>,
        inputs: serde_json::Value,
    ) -> AgentJobRequestMessage {
        serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "manifest test",
            "jobName": "test",
            "requestId": 1,
            "variables": {
                "system.github.token.permissions": {
                    "value": "{\"Contents\":\"read\",\"Attestations\":\"write\"}"
                }
            },
            "resources": {
                "endpoints": [{
                    "name": "SystemVssConnection",
                    "data": { "GenerateIdTokenUrl": "https://oidc.actions.example/token" }
                }]
            },
            "steps": [{
                "type": "Action",
                "displayName": "target action",
                "reference": {
                    "type": "Repository",
                    "name": repository,
                    "ref": action_ref
                },
                "inputs": inputs
            }]
        }))
        .unwrap()
    }

    fn capability_with(
        repository: &'static str,
        allowed_refs: &'static [AllowedRef],
        allowed_subpaths: &'static [&'static str],
    ) -> ActionCapability {
        ActionCapability {
            repository,
            adapter: NativeActionAdapter::Cache,
            allowed_refs,
            allowed_subpaths,
            inputs: &[],
            notes: "synthetic",
        }
    }

    #[test]
    fn compiled_manifest_is_version_seven_and_structurally_immutable() {
        // Plan 010 bumped v6 -> v7 (source-SHA + crate-version identity in the
        // export). 009's structural integrity checks remain intact.
        assert_eq!(MANIFEST_VERSION, 7);
        assert_eq!(MANIFEST.version, 7);
        assert_manifest_integrity().expect("compiled manifest must pass integrity");
    }

    #[test]
    fn every_non_native_ref_is_full_sha_or_documented_transition_tag() {
        for capability in ACTIONS {
            for allowed_ref in capability.allowed_refs {
                if allowed_ref.value == NATIVE_ACTION_REF {
                    assert_eq!(
                        capability.adapter,
                        NativeActionAdapter::Checkout,
                        "__native only for checkout"
                    );
                    continue;
                }
                let is_sha = is_full_sha(allowed_ref.value);
                let is_transition =
                    PLAN_041_FIXTURE_TRANSITION_ALLOWLIST
                        .iter()
                        .any(|(repo, tag)| {
                            repo.eq_ignore_ascii_case(capability.repository)
                                && *tag == allowed_ref.value
                        });
                assert!(
                    is_sha || is_transition,
                    "{} ref '{}' is neither SHA nor transition tag",
                    capability.repository,
                    allowed_ref.value
                );
            }
        }
    }

    #[test]
    fn integrity_rejects_new_mutable_tag() {
        let actions = [capability_with(
            "acme/widget",
            &[AllowedRef {
                value: "v9",
                release: "not on the allowlist",
            }],
            &[],
        )];
        let error = assert_manifest_integrity_of(&actions, &[]).unwrap_err();
        assert!(error.to_string().contains("mutable ref"));
    }

    #[test]
    fn integrity_rejects_duplicate_identities_and_unsafe_subpaths() {
        const SHA_REF: &[AllowedRef] = &[AllowedRef {
            value: "0000000000000000000000000000000000000000",
            release: "",
        }];
        let duplicate = [
            capability_with("acme/dup", SHA_REF, &[]),
            capability_with("acme/dup", SHA_REF, &[]),
        ];
        assert!(assert_manifest_integrity_of(&duplicate, &[])
            .unwrap_err()
            .to_string()
            .contains("duplicate action repository"));

        let traversal = [capability_with("acme/trav", SHA_REF, &["../escape"])];
        assert!(assert_manifest_integrity_of(&traversal, &[])
            .unwrap_err()
            .to_string()
            .contains("unsafe subpath"));

        let absolute = [capability_with("acme/abs", SHA_REF, &["/etc"])];
        assert!(assert_manifest_integrity_of(&absolute, &[])
            .unwrap_err()
            .to_string()
            .contains("unsafe subpath"));
    }

    #[test]
    fn integrity_rejects_native_ref_outside_checkout() {
        let actions = [ActionCapability {
            repository: "acme/notcheckout",
            adapter: NativeActionAdapter::Cache,
            allowed_refs: &[AllowedRef {
                value: NATIVE_ACTION_REF,
                release: "",
            }],
            allowed_subpaths: &[],
            inputs: &[],
            notes: "synthetic",
        }];
        assert!(assert_manifest_integrity_of(&actions, &[])
            .unwrap_err()
            .to_string()
            .contains("__native"));
    }

    #[test]
    fn integrity_rejects_mutable_reusable_workflow_ref() {
        let workflows = [ReusableWorkflow {
            repository: "acme/flows",
            path: ".github/workflows/x.yml",
            allowed_refs: &[AllowedRef {
                value: "v1",
                release: "",
            }],
            inputs: &[],
            notes: "synthetic",
        }];
        assert!(assert_manifest_integrity_of(&[], &workflows)
            .unwrap_err()
            .to_string()
            .contains("not a 40-hex SHA"));
    }

    #[test]
    fn validate_resolved_action_enforces_admitted_subpaths() {
        let inputs = BTreeMap::from([
            ("path".to_string(), "target".to_string()),
            ("key".to_string(), "k".to_string()),
        ]);
        // Root, restore, and save are admitted.
        for subpath in [None, Some("restore"), Some("save")] {
            validate_resolved_action(
                "cache",
                "actions/cache",
                "55cc8345863c7cc4c66a329aec7e433d2d1c52a9",
                subpath,
                &inputs,
            )
            .unwrap();
        }
        // An unknown subpath fails on the `path` field.
        let error = validate_resolved_action(
            "cache",
            "actions/cache",
            "55cc8345863c7cc4c66a329aec7e433d2d1c52a9",
            Some("bogus"),
            &inputs,
        )
        .unwrap_err();
        let violation = error.downcast_ref::<CapabilityViolation>().unwrap();
        assert_eq!(violation.field, "path");

        // Traversal is rejected.
        assert!(validate_resolved_action(
            "cache",
            "actions/cache",
            "55cc8345863c7cc4c66a329aec7e433d2d1c52a9",
            Some("../etc"),
            &inputs,
        )
        .is_err());

        // A repository with no declared subpaths rejects any subpath.
        assert!(validate_resolved_action(
            "checkout",
            "actions/checkout",
            "9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
            Some("sub"),
            &BTreeMap::new(),
        )
        .is_err());
    }

    #[test]
    fn reusable_workflow_validation_enforces_identity_ref_and_inputs() {
        let publish_sha = "80a1acd07257a23b441c546e6fcad12239ef7626";
        // Approved identity + immutable ref + no inputs is admitted.
        validate_reusable_workflow(
            "wf",
            "jackin-project/jackin-role-action",
            ".github/workflows/publish.yml",
            publish_sha,
            &BTreeMap::new(),
        )
        .unwrap();

        // Unknown identity is rejected.
        assert!(validate_reusable_workflow(
            "wf",
            "acme/unknown",
            ".github/workflows/publish.yml",
            publish_sha,
            &BTreeMap::new(),
        )
        .is_err());

        // A mismatched ref is rejected.
        let error = validate_reusable_workflow(
            "wf",
            "jackin-project/jackin-role-action",
            ".github/workflows/publish.yml",
            "1111111111111111111111111111111111111111",
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<CapabilityViolation>().unwrap().field,
            "ref"
        );
    }

    #[test]
    fn constrained_input_classification() {
        // Free-form `Any` inputs are unconstrained; literals/predicates are.
        assert!(!action_input_is_constrained("actions/cache", "path"));
        assert!(action_input_is_constrained("actions/cache", "lookup-only"));
        // Unknown repositories and inputs count as constrained.
        assert!(action_input_is_constrained("acme/unknown", "whatever"));
        assert!(action_input_is_constrained(
            "actions/cache",
            "not-a-real-input"
        ));
    }

    #[test]
    fn manifest_covers_every_native_adapter() {
        let expected = [
            NativeActionAdapter::ApprovedComposite,
            NativeActionAdapter::Checkout,
            NativeActionAdapter::Cache,
            NativeActionAdapter::UploadArtifact,
            NativeActionAdapter::DownloadArtifact,
            NativeActionAdapter::UploadPagesArtifact,
            NativeActionAdapter::ConfigurePages,
            NativeActionAdapter::DeployPages,
            NativeActionAdapter::AttestBuildProvenance,
            NativeActionAdapter::PathsFilter,
            NativeActionAdapter::Mise,
            NativeActionAdapter::Sccache,
            NativeActionAdapter::Kache,
            NativeActionAdapter::SetupMold,
            NativeActionAdapter::SetupJust,
            NativeActionAdapter::RustCache,
            NativeActionAdapter::GitHubRuntimeExport,
            NativeActionAdapter::GitHubScript,
            NativeActionAdapter::Renovate,
            NativeActionAdapter::DockerSetupBuildx,
            NativeActionAdapter::DockerLogin,
            NativeActionAdapter::DockerMetadata,
            NativeActionAdapter::DockerBuildPush,
            NativeActionAdapter::DockerBake,
            NativeActionAdapter::Hadolint,
            NativeActionAdapter::SetupQemu,
            NativeActionAdapter::CosignInstaller,
        ];
        for adapter in expected {
            assert!(
                ACTIONS.iter().any(|item| item.adapter == adapter),
                "missing {adapter:?}"
            );
        }
    }

    #[test]
    fn manifest_exports_json() {
        let value: serde_json::Value = serde_json::from_str(&to_json().unwrap()).unwrap();
        assert_eq!(value["version"], MANIFEST_VERSION);
        assert_eq!(value["actions"].as_array().unwrap().len(), ACTIONS.len());
        // Plan 010: the export binds the compiled manifest to one source commit +
        // crate version. In the default (feature-off) build these are the
        // `development` sentinel from build.rs.
        assert_eq!(value["source_sha"], env!("VELNOR_SOURCE_SHA"));
        assert_eq!(value["crate_version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn pinned_role_composite_is_admitted_but_not_executed_as_native() {
        let job = job(
            "jackin-project/jackin-role-action",
            Some("889e01e1fec152cc68271385f8976319244d9251"),
            serde_json::json!({
                "path": ".",
                "skip-build": "false",
                "registry-cache-image": "ghcr.io/jackin-project/the-architect"
            }),
        );
        assert!(violations(&job).is_empty());
        assert!(
            crate::action::native_action_adapter("jackin-project/jackin-role-action").is_none()
        );
    }

    #[test]
    fn validate_job_rejects_unknown_repository() {
        let errors = violations(&job("owner/unknown", Some("abc"), serde_json::json!({})));
        assert_eq!(errors[0].field, "uses");
    }

    #[test]
    fn validate_job_accepts_exact_attestation_surface() {
        let errors = violations(&job(
            "actions/attest-build-provenance",
            Some("0f67c3f4856b2e3261c31976d6725780e5e4c373"),
            serde_json::json!({"subject-path": "dist/*.tar.gz"}),
        ));
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    fn validate_job_rejects_unapproved_attestation_surface() {
        let errors = violations(&job(
            "actions/attest-build-provenance",
            Some("0f67c3f4856b2e3261c31976d6725780e5e4c373"),
            serde_json::json!({"subject-path": "release.tar.gz"}),
        ));
        assert_eq!(errors[0].field, "with.subject-path");
        let missing = violations(&job(
            "actions/attest-build-provenance",
            Some("0f67c3f4856b2e3261c31976d6725780e5e4c373"),
            serde_json::json!({}),
        ));
        assert_eq!(missing[0].received, "absent");
    }

    #[test]
    fn validate_job_rejects_missing_attestation_permissions() {
        let mut target = job(
            "actions/attest-build-provenance",
            Some("0f67c3f4856b2e3261c31976d6725780e5e4c373"),
            serde_json::json!({"subject-path": "dist/*.tar.gz"}),
        );
        target.variables.clear();
        let errors = violations(&target);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "permissions");
        assert_eq!(errors[0].received, "absent or malformed");
    }

    #[test]
    fn validate_job_rejects_missing_attestation_id_token_endpoint() {
        let mut target = job(
            "actions/attest-build-provenance",
            Some("0f67c3f4856b2e3261c31976d6725780e5e4c373"),
            serde_json::json!({"subject-path": "dist/*.tar.gz"}),
        );
        target.resources.endpoints.clear();
        let errors = violations(&target);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "permissions.id-token");
        assert_eq!(errors[0].received, "absent");
    }

    #[test]
    fn validate_job_rejects_unapproved_ref() {
        let errors = violations(&job(
            "actions/cache",
            Some("bad-ref"),
            serde_json::json!({}),
        ));
        assert_eq!(errors[0].field, "ref");
    }

    #[test]
    fn validate_job_rejects_forbidden_input() {
        let errors = violations(&job(
            "mozilla-actions/sccache-action",
            Some("9e7fa8a12102821edf02ca5dbea1acd0f89a2696"),
            serde_json::json!({"token": "secret"}),
        ));
        assert_eq!(errors[0].field, "with.token");
    }

    #[test]
    fn capability_violation_display_never_exposes_received_value() {
        let secret = "ghs_runtime_secret";
        let violation = violation(
            "build",
            "docker/build-push-action",
            "sha",
            "with.secrets",
            secret,
            vec!["context".to_string()],
        );

        let rendered = violation.to_string();
        assert!(!rendered.contains(secret));
        assert!(rendered.contains("received [redacted]"));
        assert_eq!(violation.received, secret);
    }

    #[test]
    fn validate_job_accepts_estate_shaped_job() {
        validate_job_with_context(
            &job(
                "jdx/mise-action",
                Some("dad1bfd3df957f44999b559dd69dc1671cb4e9ea"),
                serde_json::json!({
                    "version": "2026.7.7",
                    "install_args": "rust zig",
                    "github_token": "masked",
                    "cache_key_prefix": "mise-v2",
                    "cache_save": "false"
                }),
            ),
            &[],
        )
        .unwrap();
    }

    #[test]
    fn validate_job_accepts_current_renovate_action() {
        validate_job_with_context(
            &job(
                "renovatebot/github-action",
                Some("3064367f740a1a91cca218698a63902689cce200"),
                serde_json::json!({
                    "token": "masked",
                    "renovate-version": "43",
                    "renovate-image": "ghcr.io/renovatebot/renovate"
                }),
            ),
            &[],
        )
        .unwrap();
    }

    #[test]
    fn validate_job_rejects_unapproved_mise_cache_surface() {
        let errors = violations(&job(
            "jdx/mise-action",
            Some("dad1bfd3df957f44999b559dd69dc1671cb4e9ea"),
            serde_json::json!({"cache_key_prefix": "unapproved-generation"}),
        ));
        assert_eq!(errors[0].field, "with.cache_key_prefix");

        // Post-008 the version must be an exact YYYY.M.D date-version; a
        // selector like `latest` (a live lookup) is rejected before install.
        let errors = violations(&job(
            "jdx/mise-action",
            Some("dad1bfd3df957f44999b559dd69dc1671cb4e9ea"),
            serde_json::json!({"version": "latest"}),
        ));
        assert_eq!(errors[0].field, "with.version");

        // A leading `v` and a flag-shaped install arg are likewise rejected.
        let errors = violations(&job(
            "jdx/mise-action",
            Some("dad1bfd3df957f44999b559dd69dc1671cb4e9ea"),
            serde_json::json!({"version": "v2026.7.7"}),
        ));
        assert_eq!(errors[0].field, "with.version");

        let errors = violations(&job(
            "jdx/mise-action",
            Some("dad1bfd3df957f44999b559dd69dc1671cb4e9ea"),
            serde_json::json!({"install_args": "--yes"}),
        ));
        assert_eq!(errors[0].field, "with.install_args");
    }

    #[test]
    fn validate_upload_artifact_accepts_only_estate_compression_level() {
        let approved = job(
            "actions/upload-artifact",
            Some("043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"),
            serde_json::json!({"name": "seed", "path": "target.tar.zst", "compression-level": "0", "retention-days": "7"}),
        );
        validate_job_with_context(&approved, &[]).unwrap();

        let errors = violations(&job(
            "actions/upload-artifact",
            Some("043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"),
            serde_json::json!({"compression-level": "6"}),
        ));
        assert_eq!(errors[0].field, "with.compression-level");
        assert_eq!(errors[0].accepted, ["0"]);

        let errors = violations(&job(
            "actions/upload-artifact",
            Some("043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"),
            serde_json::json!({"retention-days": "2"}),
        ));
        assert_eq!(errors[0].field, "with.retention-days");
        assert_eq!(errors[0].accepted, ["1", "7", "14", "30", "90"]);
    }

    #[test]
    fn validate_job_accepts_only_reviewed_local_paths_filter_token() {
        validate_job_with_context(
            &job(
                "dorny/paths-filter",
                Some("7b450fff21473bca461d4b92ce414b9d0420d706"),
                serde_json::json!({"filters": "docs: docs/**", "token": ""}),
            ),
            &[],
        )
        .unwrap();

        let errors = violations(&job(
            "dorny/paths-filter",
            Some("7b450fff21473bca461d4b92ce414b9d0420d706"),
            serde_json::json!({"filters": "docs: docs/**", "token": "secret"}),
        ));
        assert_eq!(errors[0].field, "with.token");
        assert_eq!(errors[0].accepted, [""]);
    }

    #[test]
    fn validate_job_accepts_only_reviewed_buildkit_mirror_config() {
        let approved = "[registry.\"docker.io\"]\n  mirrors = [\"mirror.gcr.io\"]";
        validate_job_with_context(
            &job(
                "docker/setup-buildx-action",
                Some("bb05f3f5519dd87d3ba754cc423b652a5edd6d2c"),
                serde_json::json!({"buildkitd-config-inline": approved}),
            ),
            &[],
        )
        .unwrap();

        let errors = violations(&job(
            "docker/setup-buildx-action",
            Some("bb05f3f5519dd87d3ba754cc423b652a5edd6d2c"),
            serde_json::json!({"buildkitd-config-inline": "[registry.\"docker.io\"]\n  insecure = true\n"}),
        ));
        assert_eq!(errors[0].field, "with.buildkitd-config-inline");
        assert_eq!(errors[0].accepted, [approved]);
    }

    #[test]
    fn validate_job_accepts_buildx_cleanup_controls() {
        validate_job_with_context(
            &job(
                "docker/setup-buildx-action",
                Some("bb05f3f5519dd87d3ba754cc423b652a5edd6d2c"),
                serde_json::json!({"cleanup": false, "keep-state": true}),
            ),
            &[],
        )
        .unwrap();
    }

    #[test]
    fn validate_github_script_accepts_only_jackin_patterns() {
        for script in [
            "core.setOutput('docs-xtask', process.env.CONTRACT)",
            "return await import(process.env.JACKIN_ACTION_RUNTIME).then(({ main }) => main())",
        ] {
            validate_job_with_context(
                &job(
                    "actions/github-script",
                    Some("373c709c69115d41ff229c7e5df9f8788daa9553"),
                    serde_json::json!({"github-token": "masked", "script": script}),
                ),
                &[],
            )
            .unwrap();
        }
        let errors = violations(&job(
            "actions/github-script",
            Some("373c709c69115d41ff229c7e5df9f8788daa9553"),
            serde_json::json!({"script": "console.log('adjacent')"}),
        ));
        assert_eq!(errors[0].field, "with.script");
    }

    #[test]
    fn validate_job_expands_matrix_literals_before_capability_checks() {
        let job = job(
            "actions/checkout",
            Some("3d3c42e5aac5ba805825da76410c181273ba90b1"),
            serde_json::json!({"lfs": "${{ matrix.package == 'heimdall' }}"}),
        );
        let context = vec![(
            "matrix".to_string(),
            serde_json::json!({"package": "arbitrum"}),
        )];
        validate_job_with_context(&job, &context).unwrap();
    }

    #[test]
    fn validate_job_rejects_invalid_literal() {
        let errors = violations(&job(
            "actions/cache",
            Some("55cc8345863c7cc4c66a329aec7e433d2d1c52a9"),
            serde_json::json!({"lookup-only": "perhaps"}),
        ));
        assert_eq!(errors[0].field, "with.lookup-only");
    }

    #[test]
    fn validate_job_requires_non_checkout_ref() {
        let errors = violations(&job("actions/cache", None, serde_json::json!({})));
        assert_eq!(errors[0].received, "<missing>");
    }

    #[test]
    fn capabilities_check_accepts_sanitized_job_dump() {
        let path = std::env::temp_dir().join(format!(
            "velnor-capabilities-check-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let job = job(
            "actions/cache",
            Some("55cc8345863c7cc4c66a329aec7e433d2d1c52a9"),
            serde_json::json!({"path": "target", "key": "linux-target"}),
        );
        std::fs::write(&path, serde_json::to_vec(&job).unwrap()).unwrap();
        let result = run(CapabilitiesArgs {
            command: CapabilitiesCommand::Check {
                job_dump: path.clone(),
            },
        });
        let _ = std::fs::remove_file(path);
        result.unwrap();
    }

    #[test]
    fn dual_cache_wrappers_rejected() {
        let mut target = job(
            "mozilla-actions/sccache-action",
            Some("9e7fa8a12102821edf02ca5dbea1acd0f89a2696"),
            serde_json::json!({}),
        );
        let kache_step = job(
            "kunobi-ninja/kache-action",
            Some("49398d37113c616fdb61be434cb497e3c2c8f3e6"),
            serde_json::json!({
                "version": "v0.10.0",
                "github-cache": "false",
                "cache-executables": "false",
                "pr-comment": "false",
                "max-size": "20GiB"
            }),
        )
        .steps
        .remove(0);
        target.steps.push(kache_step);
        let errors = violations(&target);
        assert!(errors
            .iter()
            .any(|error| error.field == "compiler-cache.backend"));
        assert_eq!(compiler_cache_backend(&target), CompilerCacheBackend::Off);
    }

    #[test]
    fn remote_cache_env_rejected_but_legacy_gha_flag_tolerated() {
        let mut target = job(
            "mozilla-actions/sccache-action",
            Some("9e7fa8a12102821edf02ca5dbea1acd0f89a2696"),
            serde_json::json!({}),
        );
        target.environment_variables = vec![serde_json::json!({
            "SCCACHE_GHA_ENABLED": "true",
            "SCCACHE_BUCKET": "remote"
        })];
        let errors = violations(&target);
        assert!(errors
            .iter()
            .any(|error| error.field == "env.SCCACHE_BUCKET"));
        assert!(!errors
            .iter()
            .any(|error| error.field == "env.SCCACHE_GHA_ENABLED"));
    }

    #[test]
    fn backend_selection_matches_single_wrapper_or_off() {
        let sccache = job(
            "mozilla-actions/sccache-action",
            Some("9e7fa8a12102821edf02ca5dbea1acd0f89a2696"),
            serde_json::json!({}),
        );
        let kache = job(
            "kunobi-ninja/kache-action",
            Some("49398d37113c616fdb61be434cb497e3c2c8f3e6"),
            serde_json::json!({}),
        );
        let mut off = sccache.clone();
        off.steps.clear();
        assert_eq!(
            compiler_cache_backend(&sccache),
            CompilerCacheBackend::Sccache
        );
        assert_eq!(compiler_cache_backend(&kache), CompilerCacheBackend::Kache);
        assert_eq!(compiler_cache_backend(&off), CompilerCacheBackend::Off);
    }
}
