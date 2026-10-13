//! Scan pass: turn a repository directory into a typed `RepositoryShape`.
//!
//! The pass is a fixed pipeline of detectors. Every detector reads only the
//! file walk output, appends evidence to the shape, and never executes
//! project code or invents a command for something it could not prove. The
//! renderer consumes the shape; repository-specific estate profiles are
//! applied by the caller, never here.

mod docker;
mod docs;
pub(crate) mod file_walk;
mod gradle;
mod homebrew;
mod node;
mod opentofu;
pub(crate) mod rust;
mod signals;
pub(crate) mod swift;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::s2::provider::{
    Capabilities, Platform, ProviderId, ProviderSelector, ProviderSet, TrustReq,
};
use crate::s2::{
    default_workflow_files, identifier_suffix, AnalysisSummary, CacheSpec, GeneratorError,
    MaintenanceSpec, ProjectConfig, Unit, UnitKind,
};

/// Run the detector pipeline over `root` and return what it proved.
///
/// # Errors
/// Returns filesystem errors with the affected path.
pub(crate) fn scan_shape(
    root: &Path,
    providers: &ProviderSet,
    default_branch: &str,
    exclude: &[String],
) -> Result<RepositoryShape, GeneratorError> {
    let files = file_walk::repository_files(root, exclude)?;
    let file_set: BTreeSet<String> = files.iter().cloned().collect();
    let context = ScanContext {
        root,
        files: &files,
        file_set: &file_set,
    };
    let mut shape = RepositoryShape {
        files: Vec::new(),
        units: Vec::new(),
        boltffi_producers: Vec::new(),
        swift_consumers: Vec::new(),
        detected: Vec::new(),
        // Standing boundaries of static inspection. Every scan reports them,
        // whatever the detectors find.
        limitations: vec![
            "Project code, build scripts, task runners, and commands are never executed during analysis.".to_owned(),
            "Release, signing, registry, deployment, branch-protection, and runner-capability contracts remain explicit manual inputs.".to_owned(),
        ],
        default_branch: default_branch.to_owned(),
        providers: providers.clone(),
    };
    // Detector order is part of the contract: ids are sorted stably below, so
    // the first detector to claim an id keeps the un-suffixed form.
    file_walk::detect(&context, &mut shape);
    rust::detect(&context, &mut shape)?;
    signals::detect(&context, &mut shape);
    gradle::detect(&context, &mut shape)?;
    node::detect(&context, &mut shape)?;
    swift::detect(&context, &mut shape);
    opentofu::detect(&context, &mut shape);
    docker::detect(&context, &mut shape);
    homebrew::detect(&context, &mut shape);
    docs::detect(&context, &mut shape);
    join_native_producers(&mut shape, &file_set);
    shape.finalize();
    shape.files = files;
    Ok(shape)
}

/// Join Swift local-path binary targets against `BoltFFI` Apple producers.
///
/// A consumer joins its producer only when the normalized consumer path
/// equals the producer's normalized `{parent}/{framework}.xcframework`
/// output and, when the stanza names the target, the name equals the
/// producer's `{framework}FFI` module. A unique match becomes a taskless
/// product edge — `NamedProduct` on the Rust unit, `Prerequisite` on the
/// Swift unit — which later compiles into a selection dependency; the
/// execution adapter that materializes the framework lands separately, so
/// the matched limitation keeps naming the materialization obligation.
/// Ambiguous producers, module mismatches, and unresolvable paths stay
/// diagnostics with no edge: the join never guesses.
fn join_native_producers(shape: &mut RepositoryShape, file_set: &BTreeSet<String>) {
    let consumers = std::mem::take(&mut shape.swift_consumers);
    for consumer in &consumers {
        let name = consumer.name.as_deref().unwrap_or("<unnamed>");
        let Some(resolved) = file_walk::resolve_repo_path(&consumer.package_root, &consumer.path)
        else {
            shape.limitations.push(format!(
                "Swift package {} declares binary target `{name}` at `{}`, which is absolute or escapes the repository; no producer can be joined statically.",
                consumer.manifest, consumer.path,
            ));
            continue;
        };
        // Cloned, not borrowed: the wiring below needs `shape` mutably.
        let matches: Vec<rust::BoltffiProducer> = shape
            .boltffi_producers
            .iter()
            .filter(|producer| producer.output == resolved)
            .cloned()
            .collect();
        if matches.is_empty() {
            let tracked = file_set.contains(&resolved)
                || file_set
                    .iter()
                    .any(|file| file.starts_with(&format!("{resolved}/")));
            if !tracked {
                shape.limitations.push(format!(
                    "Swift package {} references binary target `{name}` at `{}`, which no tracked file provides; the producing step must materialize it before `swift build` consumes the package.",
                    consumer.manifest, consumer.path,
                ));
            }
            continue;
        }
        // At most one producer claims an output: the Rust detector drops
        // conflicting claimants with their own diagnostic, so a match here
        // is unique and ambiguity needs no second branch.
        let Some(producer) = matches.first() else {
            continue;
        };
        if consumer
            .name
            .as_deref()
            .is_some_and(|name| name != producer.ffi_module)
        {
            shape.limitations.push(format!(
                "Swift package {} binary target `{name}` at `{}` matches the XCFramework output of {}, which produces FFI module `{}`; the module disagrees, so no product edge was constructed.",
                consumer.manifest, consumer.path, producer.manifest, producer.ffi_module,
            ));
            continue;
        }
        wire_native_edge(shape, consumer, producer, name);
    }
}

/// Wire one agreed producer/consumer pair: ensure the product on the Rust
/// unit, add the prerequisite on the Swift unit, and record the
/// materialization obligation. Every failure stays a diagnostic with no
/// partial edge.
fn wire_native_edge(
    shape: &mut RepositoryShape,
    consumer: &swift::SwiftBinaryConsumer,
    producer: &rust::BoltffiProducer,
    name: &str,
) {
    let Some(producer_unit) = producer.unit.clone() else {
        shape.limitations.push(format!(
            "Swift package {} binary target `{name}` matches BoltFFI producer {}, but no Rust unit owns `{}`; no product edge was constructed.",
            consumer.manifest, producer.manifest, producer.root,
        ));
        return;
    };
    let product_name = native_product_name(&producer.framework);
    let producer_index = shape.units.iter().position(|unit| unit.id == producer_unit);
    let consumer_index = shape.units.iter().position(|unit| unit.id == consumer.unit);
    let (Some(producer_index), Some(consumer_index)) = (producer_index, consumer_index) else {
        shape.limitations.push(format!(
            "Swift package {} binary target `{name}` matches BoltFFI producer {}, but the owning unit is missing; no product edge was constructed.",
            consumer.manifest, producer.manifest,
        ));
        return;
    };
    if shape.units[producer_index].products.iter().any(|product| {
        product.name == product_name
            && (product.outputs.len() != 1
                || product
                    .outputs
                    .first()
                    .is_some_and(|output| output != producer.output.as_str()))
    }) {
        shape.limitations.push(format!(
            "BoltFFI manifest {} framework `{}` collides with product `{product_name}` on unit `{producer_unit}`; no product edge was constructed.",
            producer.manifest, producer.framework,
        ));
        return;
    }
    if !shape.units[producer_index]
        .products
        .iter()
        .any(|product| product.name == product_name)
    {
        shape.units[producer_index]
            .products
            .push(crate::s2::platform::NamedProduct {
                name: product_name.clone(),
                task: None,
                env: std::collections::BTreeMap::new(),
                outputs: vec![producer.output.clone()],
                output_files: producer.output_files.clone(),
                bindings_dir: producer.bindings_dir.clone(),
                bindings_file: producer.bindings_file.clone(),
                deployment_target: producer.deployment_target.clone(),
                inputs: producer.inputs.clone(),
                inputs_unknown: producer.inputs_unknown.clone(),
                inputs_digest: producer.inputs_digest.clone(),
            });
        // The pack runs Apple tooling, so the producing unit inherits the
        // macOS requirement and carries the typed recipe commands after
        // its own checks. A second consumer of the same product reuses
        // the materialized output instead of appending a second pack.
        let unit = &mut shape.units[producer_index];
        unit.platform = crate::s2::provider::Platform::MacosArm64;
        unit.capabilities.native_macos_arm64 = true;
        let recipe_commands = producer.recipe.commands(&producer.root, &producer.output);
        unit.pr_commands.extend(recipe_commands.clone());
        unit.full_commands.extend(recipe_commands);
    }
    shape.units[consumer_index]
        .prerequisites
        .push(crate::s2::platform::Prerequisite {
            producer: producer_unit,
            product: product_name,
            task: None,
            env: std::collections::BTreeMap::new(),
        });
    shape.limitations.push(format!(
        "Swift package {} consumes binary target `{name}` from BoltFFI manifest {} (crate `{}`); the producer step must materialize `{}` before `swift build` consumes the package.",
        consumer.manifest, producer.manifest, producer.crate_name, producer.output,
    ));
}

/// The product name for a framework: lowercase, shell-safe, within the
/// product-name length cap. Collisions with a same-named different-output
/// product fail closed at the join.
fn native_product_name(framework: &str) -> String {
    let mut sanitized: String = framework
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
            {
                character
            } else if character.is_ascii_uppercase() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // `platform::valid_product_name` caps names at 100 bytes.
    while sanitized.len() + "xcframework-".len() > 100 {
        sanitized.pop();
    }
    format!("xcframework-{sanitized}")
}

impl RepositoryShape {
    /// Canonical ordering applied once every detector has run.
    fn finalize(&mut self) {
        self.units.sort_by(|left, right| left.id.cmp(&right.id));
        for unit in &mut self.units {
            refresh_service_capabilities(unit);
        }
        disambiguate_unit_ids(&mut self.units);
        self.detected.sort();
        self.detected.dedup();
        self.limitations.sort();
        self.limitations.dedup();
    }

    /// Every verification unit id the scan produced, in canonical order.
    /// Every repository path the walk observed, relative to the root.
    pub(crate) fn files(&self) -> &[String] {
        &self.files
    }

    pub(crate) fn unit_ids(&self) -> impl Iterator<Item = &str> {
        self.units.iter().map(|unit| unit.id.as_str())
    }

    /// Canonical serialization of the shape.
    ///
    /// The shape stores every sequence in canonical sorted order and no float,
    /// so the derived serialization is stable across runs and machines; the
    /// state file digests exactly this string as the `scan` generation input.
    ///
    /// # Errors
    /// Returns an error if serialization fails, which cannot happen for the
    /// shape's own types but is never unwrapped.
    pub(crate) fn canonical_json(&self) -> Result<String, GeneratorError> {
        serde_json::to_string(self).map_err(|error| {
            GeneratorError::usage(format!("canonicalize repository shape: {error}"))
        })
    }
}

/// Typed output of the scan pass, before any repository-specific policy is
/// applied.
///
/// Every sequence is stored in canonical sorted order (units by id, capability
/// strings and limitations deduplicated and sorted, files sorted by the file
/// walk), so a derived serialization of the shape is stable across runs and
/// machines. The digest phase relies on that canonical ordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct RepositoryShape {
    /// Every repository file observed by the file walk, normalized and sorted.
    files: Vec<String>,
    /// Verification units derived from manifests and directory layout.
    units: Vec<Unit>,
    /// `BoltFFI` Apple producers awaiting the native join. Skipped from
    /// canonical serialization: the join compiles them into product edges,
    /// so they must not perturb the scan digest on their own.
    #[serde(skip_serializing)]
    pub(crate) boltffi_producers: Vec<rust::BoltffiProducer>,
    /// Swift local-path binary targets awaiting the native join. Skipped
    /// from canonical serialization for the same reason; consumed by the
    /// join before `finalize`.
    #[serde(skip_serializing)]
    pub(crate) swift_consumers: Vec<swift::SwiftBinaryConsumer>,
    /// Detected capabilities, sorted and deduplicated.
    detected: Vec<String>,
    /// What static inspection could not prove, sorted and deduplicated.
    limitations: Vec<String>,
    default_branch: String,
    providers: ProviderSet,
}

/// Read-only view of the walked repository that every detector receives.
pub(crate) struct ScanContext<'a> {
    root: &'a Path,
    files: &'a [String],
    file_set: &'a BTreeSet<String>,
}

/// Build a verification unit with the shared id, label, and command contract.
pub(crate) fn unit(
    kind: UnitKind,
    root: &str,
    watch: Vec<String>,
    commands: Vec<String>,
    cache: Option<CacheSpec>,
) -> Unit {
    let suffix = if root == "." {
        String::new()
    } else {
        format!("-{}", identifier_suffix(root))
    };
    let (platform, trust, capabilities) = detection_contract(kind);
    Unit {
        id: format!("{}{}", kind.id_prefix(), suffix),
        label: if root == "." {
            kind.label().to_owned()
        } else {
            format!("{} ({root})", kind.label())
        },
        kind,
        root: root.to_owned(),
        watch,
        pr_commands: commands.clone(),
        full_commands: commands,
        depends_on: Vec::new(),
        cache,
        pinned_lockfile: false,
        tool_version: None,
        mise_tools: Vec::new(),
        toolchain: None,
        services: Vec::new(),
        trust,
        platform,
        capabilities,
        workspace_check: false,
        products: Vec::new(),
        prerequisites: Vec::new(),
        docker_contexts: Vec::new(),
        env: std::collections::BTreeMap::new(),
        mbx: None,
        prepared_tools: Vec::new(),
    }
}

/// The typed platform/trust/capability contract the scan derives per kind.
/// Detectors refine capabilities afterwards (services imply container
/// readiness); the contract never invents a requirement the kind cannot prove.
fn detection_contract(kind: UnitKind) -> (Platform, TrustReq, Capabilities) {
    let trust = TrustReq::UntrustedOk;
    match kind {
        // A SwiftPM package is portable: it verifies wherever its toolchain
        // provisions. Only Xcode scheme work and XCFramework consumers carry
        // the Apple need, which the Swift detector overlays afterwards.
        UnitKind::Swift => (Platform::LinuxX64, trust, Capabilities::default()),
        UnitKind::Docker => (
            Platform::LinuxX64,
            trust,
            Capabilities {
                docker: true,
                buildx_compose: true,
                ..Capabilities::default()
            },
        ),
        UnitKind::Rust | UnitKind::Gradle | UnitKind::Node | UnitKind::Bun => (
            Platform::LinuxX64,
            trust,
            Capabilities {
                docker: true,
                testcontainers: true,
                ..Capabilities::default()
            },
        ),
        UnitKind::OpenTofu | UnitKind::Homebrew | UnitKind::Docs => {
            (Platform::LinuxX64, trust, Capabilities::default())
        }
    }
}

/// Refresh one unit's capabilities from its detected services: a unit with
/// service containers needs container readiness on top of its kind contract.
pub(crate) fn refresh_service_capabilities(unit: &mut Unit) {
    if unit.services.is_empty() {
        return;
    }
    unit.capabilities.docker = true;
    unit.capabilities.services_with_readiness = true;
}

/// Scan-default `runs-on` routing per provider. A repo-owned config
/// overrides per provider; local defaults are disjoint dedicated selectors.
pub(crate) fn default_selectors() -> crate::s2::provider::SelectorMap {
    [
        (
            ProviderId::GithubHosted,
            ProviderSelector {
                runs_on: vec!["ubuntu-24.04".to_owned()],
            },
        ),
        (
            ProviderId::GithubSelfHosted,
            ProviderSelector {
                runs_on: vec!["bastion-scale-set".to_owned()],
            },
        ),
        (
            ProviderId::Velnor,
            ProviderSelector {
                runs_on: vec!["velnor-native".to_owned()],
            },
        ),
    ]
    .into_iter()
    .collect()
}

fn disambiguate_unit_ids(units: &mut [Unit]) {
    let mut counts = BTreeMap::new();
    for unit in units {
        let base_id = unit.id.clone();
        let count = counts.entry(base_id.clone()).or_insert(0_usize);
        *count += 1;
        if *count > 1 {
            unit.id = format!("{base_id}-{count}");
        }
    }
}

impl From<RepositoryShape> for ProjectConfig {
    fn from(shape: RepositoryShape) -> Self {
        Self {
            repository: String::new(),
            workflow_revision: crate::s2::SOURCE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: AnalysisSummary {
                method: "static-filesystem-and-manifest-inspection".to_owned(),
                detected: shape.detected,
                limitations: shape.limitations,
            },
            verified: true,
            workflow_files: default_workflow_files(),
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: shape.default_branch,
            providers: shape.providers.clone(),
            automatic_providers: shape.providers.clone(),
            default_dispatch_providers: shape.providers,
            selectors: default_selectors(),
            release_enabled: false,
            release_reason: "Release is fail-closed. Enable only after declaring immutable artifact, registry, provenance, and tag-protection policy.".to_owned(),
            release: None,
            renovate_enabled: false,
            renovate_reason: "Renovate is fail-closed. Enable only after declaring a Renovate config, trusted Velnor runners, and a dedicated PAT secret.".to_owned(),
            renovate: None,
            docs_enabled: false,
            docs_reason: "Docs-site publishing is fail-closed. Enable only after declaring the site address, the built output directory, and the build and check commands.".to_owned(),
            docs: None,
            maintenance: MaintenanceSpec::default(),
            units: shape.units,
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            rust_needs: crate::s2::RustNeeds::Parallel,
            concurrency_group: None,
            serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            github_cache: crate::s2::config::CacheGithubSection::default(),
            velnor_host_cache: crate::s2::config::CacheVelnorSection::default(),
            check_profiles: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::native_product_name;

    #[test]
    fn native_product_name_sanitizes_frameworks() {
        assert_eq!(native_product_name("BridgeCore"), "xcframework-bridgecore");
        assert_eq!(
            native_product_name("My Framework!"),
            "xcframework-my-framework-"
        );
        assert_eq!(native_product_name("a.b_c-d"), "xcframework-a.b_c-d");
        assert_eq!(native_product_name("Ünïcode"), "xcframework--n-code");
        for name in [
            native_product_name("BridgeCore"),
            native_product_name("My Framework!"),
            native_product_name("Ünïcode"),
        ] {
            assert!(crate::s2::platform::valid_product_name(&name), "{name}");
        }
    }

    #[test]
    fn native_product_name_truncates_to_the_length_cap() {
        let name = native_product_name(&"F".repeat(200));
        assert!(name.len() <= 100, "{name}");
        assert!(crate::s2::platform::valid_product_name(&name), "{name}");
        assert!(name.starts_with("xcframework-ffff"));
    }
}
