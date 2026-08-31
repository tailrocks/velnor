//! Canonical Velnor Actions fleet — generator skeleton library.
//!
//! This crate is the headless seam for the canonical fleet repository. It exposes
//! the stable repository-class taxonomy and the required repository layout so the
//! CLI can prove the skeleton is well formed. Plans 005 and 006 EXTEND this API and
//! these roots; they must not replace them.

use std::path::{Path, PathBuf};

pub mod audit;
pub mod cache;
pub mod composite;
pub mod forks;
pub mod model;
pub mod package;
pub mod policy;
pub mod releases;
pub mod render;
pub mod tools;

/// One of the five normalized repository classes the fleet generator maps every
/// canonical repository onto exactly once.
///
/// The variants are declared in canonical order: code, native, tap, apt, fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepositoryClass {
    /// Rust library and binary code repositories.
    Code,
    /// Code repositories with a required native macOS validation lane.
    Native,
    /// Homebrew tap repositories.
    Tap,
    /// Debian/apt package repositories.
    Apt,
    /// Test fixture repository.
    Fixture,
}

impl RepositoryClass {
    /// Stable lowercase identifier used in declared data and CLI output.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            RepositoryClass::Code => "code",
            RepositoryClass::Native => "native",
            RepositoryClass::Tap => "tap",
            RepositoryClass::Apt => "apt",
            RepositoryClass::Fixture => "fixture",
        }
    }
}

/// The five repository classes in canonical order: code, native, tap, apt, fixture.
pub const ALL_CLASSES: [RepositoryClass; 5] = [
    RepositoryClass::Code,
    RepositoryClass::Native,
    RepositoryClass::Tap,
    RepositoryClass::Apt,
    RepositoryClass::Fixture,
];

/// The two required top-level roots every canonical checkout must expose:
/// reusable building blocks (`actions`) and normalized class templates
/// (`templates`).
pub const REQUIRED_LAYOUT: [&str; 2] = ["actions", "templates"];

/// Validate that every required layout root exists as a directory under `root`.
///
/// Returns `Ok(())` when each entry in [`REQUIRED_LAYOUT`] resolves to an existing
/// directory; otherwise returns an error naming the first missing root.
///
/// # Errors
///
/// Returns `Err` with a human-readable message when a required root is missing or
/// is not a directory.
pub fn validate_layout(root: &Path) -> Result<(), String> {
    for entry in REQUIRED_LAYOUT {
        let candidate = root.join(entry);
        if !candidate.is_dir() {
            return Err(format!(
                "missing required layout root: {}",
                candidate.display()
            ));
        }
    }
    Ok(())
}

/// Generate every deterministic output from the declared fleet data under `root`.
///
/// Always writes the two composite building-block actions to
/// `actions/<name>/action.yml` (from their canonical bytes) and renders the five
/// consumer class templates to
/// `templates/<class>/ci.yml`. When `fleet/block-sha` is bound to a 40-hex commit
/// SHA, also renders the five owner-local callable workflows to
/// `.github/workflows/ci-<class>.yml`, pinning their internal composite closure to
/// that SHA. Returns the written paths in a stable order.
///
/// # Errors
///
/// Returns `Err` on any data-contract violation, a malformed block SHA, or an I/O
/// failure while writing.
pub fn generate(root: &Path) -> Result<Vec<PathBuf>, String> {
    let forks = forks::ForkTable::load(root)?;
    let manifest = model::FleetManifest::load(root)?;
    let caches = cache::CacheContract::load(&root.join("fleet").join("caches.toml"))?;
    let packages = package::PackagePolicy::load(root, &forks)?;
    let registry = tools::ToolRegistry::load(&tools::registry_path(root))?;
    let mut written = Vec::new();

    // Composite building blocks: canonical bytes live in `composite`, so they are
    // regenerated and byte-compared exactly like every other generated file.
    for name in composite::COMPOSITE_NAMES {
        let body = composite::canonical(name)
            .ok_or_else(|| format!("no canonical bytes for composite {name:?}"))?;
        let dir = root.join("actions").join(name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let path = dir.join("action.yml");
        write_if_changed(&path, body)?;
        written.push(path);
    }

    for class in ALL_CLASSES {
        let body = render::consumer_template_for(class, &forks);
        let dir = root.join("templates").join(class.code());
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let path = dir.join("ci.yml");
        write_if_changed(&path, &body)?;
        written.push(path);

        let path = dir.join(".alint.yml");
        write_if_changed(&path, &policy::alint_config(class))?;
        written.push(path);
    }

    let tools_template = root.join("templates").join("tools").join("mise.toml");
    let tool_names = registry.entries().keys().map(String::as_str);
    let tools_body = registry.render_tools_block(tool_names)?;
    if let Some(parent) = tools_template.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    write_if_changed(&tools_template, &tools_body)?;
    written.push(tools_template);

    let audit_workflow = root
        .join(".github")
        .join("workflows")
        .join("fleet-audit.yml");
    if let Some(parent) = audit_workflow.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    write_if_changed(&audit_workflow, policy::FLEET_AUDIT_WORKFLOW)?;
    written.push(audit_workflow);

    let block_sha_path = root.join("fleet").join("block-sha");
    if block_sha_path.exists() {
        let raw = std::fs::read_to_string(&block_sha_path)
            .map_err(|e| format!("reading {}: {e}", block_sha_path.display()))?;
        let block_sha = raw.trim();
        if !model::is_sha40(block_sha) {
            return Err(format!(
                "fleet/block-sha {block_sha:?} is not a 40 lowercase hex commit SHA"
            ));
        }
        let dir = root.join(".github").join("workflows");
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        for class in ALL_CLASSES {
            let body =
                render::callable_workflow_for(manifest.class(class), &caches, block_sha, &forks);
            let path = dir.join(render::callable_file_name(class));
            write_if_changed(&path, &body)?;
            written.push(path);
        }
        let updater = packages.render_updater();
        for (name, body) in [
            ("package-signer.yml", package::SIGNER_WORKFLOW),
            ("package-updater.yml", updater.as_str()),
        ] {
            let path = dir.join(name);
            write_if_changed(&path, body)?;
            written.push(path);
        }
        for (class, body) in [
            ("tap", package::TAP_TEMPLATE),
            ("apt", package::APT_TEMPLATE),
        ] {
            let path = root
                .join("templates")
                .join(class)
                .join("package-update.yml");
            write_if_changed(&path, body)?;
            written.push(path);
        }
    }

    Ok(written)
}

/// Materialize one consumer repository's `ci.yml` from its class template,
/// replacing the shared release placeholders with `release_sha` and `calver`, and
/// write only `<output>/.github/workflows/ci.yml`.
///
/// # Errors
///
/// Returns `Err` if the repository is not a fleet member, the release identity is
/// invalid, a placeholder survives, or writing fails.
pub fn render_consumer_to_dir(
    root: &Path,
    repository: &str,
    release_shas: [&str; 3],
    calver: &str,
    output: &Path,
) -> Result<PathBuf, String> {
    let forks = forks::ForkTable::load(root)?;
    let manifest = model::FleetManifest::load(root)?;
    let repo = manifest
        .repositories()
        .iter()
        .find(|r| r.slug == repository)
        .ok_or_else(|| format!("{repository:?} is not a fleet member"))?;
    let template = render::consumer_template_for(repo.class, &forks);
    let body = render::render_consumer_for(&template, &forks, &release_shas, calver)?;
    validate_consumer_envelope(output)?;
    let dir = output.join(".github").join("workflows");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join("ci.yml");
    write_if_changed(&path, &body)?;
    let previous_ignore = std::fs::read_to_string(output.join(".ignore")).ok();
    let ignore_path = output.join(".ignore");
    write_if_changed(&ignore_path, policy::IGNORE_FILE)?;
    let agents_path = output.join("AGENTS.md");
    let agents = std::fs::read_to_string(&agents_path)
        .map_err(|e| format!("reading {}: {e}", agents_path.display()))?;
    let metadata = &repo.census;
    let rendered_agents = policy::render_agents(&agents, metadata);
    write_if_changed(&agents_path, &rendered_agents)?;
    let claude_path = output.join("CLAUDE.md");
    ensure_pointer(&claude_path, "AGENTS.md")?;
    let alint_path = output.join(".alint.yml");
    write_if_changed(&alint_path, &policy::alint_config(repo.class))?;
    let repolint_path = output.join("repolint.toml");
    if repolint_path.is_file() {
        let existing = std::fs::read_to_string(&repolint_path)
            .map_err(|e| format!("reading {}: {e}", repolint_path.display()))?;
        let rendered = policy::render_repolint(&existing, metadata);
        write_if_changed(&repolint_path, &rendered)?;
    }
    let mise_path = output.join("mise.toml");
    let registry = tools::ToolRegistry::load(&tools::registry_path(root))?;
    tools::check_mise_vocabulary(&mise_path)?;
    let existing = std::fs::read_to_string(&mise_path)
        .map_err(|e| format!("reading {}: {e}", mise_path.display()))?;
    let (migrated, legacy_nextest) = tools::ToolRegistry::migrate_legacy_nextest(&existing)?;
    let (migrated, legacy_cargo_audit) =
        tools::ToolRegistry::migrate_legacy_cargo_audit(&migrated)?;
    let mut required_tools = tools::FLEET_TASK_TOOLS.to_vec();
    if legacy_nextest {
        required_tools.push("nextest");
    }
    if legacy_cargo_audit {
        required_tools.push("cargo-audit");
    }
    let normalized =
        registry.normalize_mise_file_with_tools(&migrated, required_tools.iter().copied())?;
    let with_tasks =
        policy::render_mise_tasks(&normalized, &policy::fleet_tasks(metadata, repo.class));
    write_if_changed(&mise_path, &with_tasks)?;
    let lock_path = output.join("mise.lock");
    let existing_lock = std::fs::read_to_string(&lock_path)
        .map_err(|e| format!("reading {}: {e}", lock_path.display()))?;
    let source_lock = std::fs::read_to_string(root.join("mise.lock"))
        .map_err(|e| format!("reading generator mise.lock: {e}"))?;
    let projected_lock =
        registry.project_lock_file(&existing_lock, &source_lock, required_tools.iter().copied())?;
    registry.check_text(&with_tasks, &projected_lock)?;
    write_if_changed(&lock_path, &projected_lock)?;
    let ignore_changed = previous_ignore.as_deref() != Some(policy::IGNORE_FILE);
    if ignore_changed && repolint_path.is_file() {
        run_repolint_map(output)?;
    }
    Ok(path)
}

fn validate_consumer_envelope(output: &Path) -> Result<(), String> {
    for name in ["README.md", "AGENTS.md", "mise.toml", "mise.lock"] {
        let path = output.join(name);
        if !path.is_file() {
            return Err(format!(
                "render requires existing {name}; use scaffold for the initial envelope"
            ));
        }
    }
    Ok(())
}

fn ensure_pointer(path: &Path, target: &str) -> Result<(), String> {
    if path.is_symlink() {
        let existing =
            std::fs::read_link(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        if existing == Path::new(target) {
            return Ok(());
        }
        std::fs::remove_file(path).map_err(|e| format!("removing {}: {e}", path.display()))?;
    } else if path.exists() {
        let existing = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        if existing.trim() == target {
            return Ok(());
        }
        std::fs::remove_file(path).map_err(|e| format!("removing {}: {e}", path.display()))?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path)
        .map_err(|e| format!("creating {}: {e}", path.display()))?;
    #[cfg(not(unix))]
    std::fs::write(path, format!("{target}\n"))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(())
}

fn run_repolint_map(root: &Path) -> Result<(), String> {
    let status = std::process::Command::new("repolint")
        .arg("--root")
        .arg(root)
        .arg("map")
        .arg("--write")
        .status()
        .map_err(|e| format!("running repolint map --write: {e}"))?;
    if !status.success() {
        return Err(format!("repolint map --write exited with {status}"));
    }
    Ok(())
}

/// Materialize one package consumer's owner-routed updater workflow.
pub fn render_package_consumer_to_dir(
    root: &Path,
    repository: &str,
    release_shas: [&str; 3],
    calver: &str,
    output: &Path,
) -> Result<PathBuf, String> {
    let forks = forks::ForkTable::load(root)?;
    let policy = package::PackagePolicy::load(root, &forks)?;
    let body = policy.render_consumer(repository, release_shas, calver)?;
    let dir = output.join(".github").join("workflows");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join("package-update.yml");
    std::fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path)
}

/// Create a new repository envelope and register it in the canonical census.
///
/// This is the only operation allowed to create the initial README, AGENTS,
/// security, repolint, and mise files. It runs the exact M1 map writer before
/// appending the census row, so a successful scaffold is registered and map
/// visible as one operation.
pub fn scaffold_consumer(
    root: &Path,
    output: &Path,
    repository: &str,
    class: RepositoryClass,
    metadata: &model::CensusMetadata,
    baseline_sha: &str,
) -> Result<Vec<PathBuf>, String> {
    let manifest = model::FleetManifest::load(root)?;
    if manifest
        .repositories()
        .iter()
        .any(|repo| repo.slug == repository)
    {
        return Err(format!("{repository:?} is already a fleet member"));
    }
    if !model::is_sha40(baseline_sha) {
        return Err(format!(
            "baseline SHA {baseline_sha:?} is not 40 lowercase hex"
        ));
    }
    if output.exists()
        && output
            .read_dir()
            .map_err(|e| format!("reading {}: {e}", output.display()))?
            .next()
            .is_some()
    {
        return Err(format!("scaffold output {} is not empty", output.display()));
    }
    std::fs::create_dir_all(output)
        .map_err(|e| format!("creating scaffold output {}: {e}", output.display()))?;
    let registry = tools::ToolRegistry::load(&tools::registry_path(root))?;
    let source_lock = std::fs::read_to_string(root.join("mise.lock"))
        .map_err(|e| format!("reading generator mise.lock: {e}"))?;
    let scaffold_tools = ["mise", "repolint", "alint", "zizmor"];
    let mise_body = scaffold_mise_file(&registry, metadata, class)?;
    let lock_body = registry.project_lock_file("", &source_lock, scaffold_tools)?;
    registry.check_text(&mise_body, &lock_body)?;
    let name = repository
        .rsplit_once('/')
        .map_or(repository, |(_, name)| name);
    let files = [
        (
            "README.md",
            format!(
                "# {name}\n\n> {repository} — {} repository.\n\n## Repository map\n\n<!-- MAP:BEGIN -->\n<!-- MAP:END -->\n",
                metadata.kind.token()
            ),
        ),
        (
            "AGENTS.md",
            policy::agents_block(true, metadata.tier) + "\n",
        ),
        (".ignore", policy::IGNORE_FILE.to_owned()),
        (
            "SECURITY.md",
            "# Security\n\nReport security issues privately to the repository owner.\n".to_owned(),
        ),
        (
            "repolint.toml",
            format!(
                "{}\n[map.dirs]\n\n[map.files]\n",
                policy::repolint_repo_table(metadata)
            ),
        ),
        ("mise.toml", mise_body),
        ("mise.lock", lock_body),
    ];
    let mut written = Vec::new();
    for (name, body) in files {
        let path = output.join(name);
        std::fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
        written.push(path);
    }
    ensure_pointer(&output.join("CLAUDE.md"), "AGENTS.md")?;
    written.push(output.join("CLAUDE.md"));
    run_repolint_map(output)?;
    append_census_row(root, repository, class, metadata, baseline_sha)?;
    Ok(written)
}

fn scaffold_mise_file(
    registry: &tools::ToolRegistry,
    metadata: &model::CensusMetadata,
    class: RepositoryClass,
) -> Result<String, String> {
    let tasks = policy::fleet_tasks(metadata, class);
    let tools = registry.render_tools_block(["mise", "repolint", "alint", "zizmor"])?;
    Ok(format!(
        "{tools}\n[tasks.fmt]\ndescription = \"Check formatting\"\nrun = \"echo define fmt\"\n\n[tasks.\"fmt-fix\"]\ndescription = \"Fix formatting\"\nrun = \"echo define fmt-fix\"\n\n[tasks.lint]\ndescription = \"Run lint\"\nrun = \"echo define lint\"\n\n[tasks.test]\ndescription = \"Run tests\"\nrun = \"echo define test\"\n\n[tasks.check]\ndescription = \"Run the fast check\"\nrun = \"mise run fmt && mise run lint && mise run test\"\n\n[tasks.ci]\ndescription = \"Run every repository gate\"\nrun = \"mise run check\"\n\n{tasks}"
    ))
}

fn append_census_row(
    root: &Path,
    repository: &str,
    class: RepositoryClass,
    metadata: &model::CensusMetadata,
    baseline_sha: &str,
) -> Result<(), String> {
    let path = root.join("fleet").join("repositories.toml");
    let mut body =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    body.push_str(&format!(
        "\n[[repository]]\nslug = {repository:?}\nclass = {class:?}\nbaseline_sha = {baseline_sha:?}\ntier = {tier:?}\nkind = {kind:?}\nvisibility = {visibility:?}\nresearch = {}\nfirst_seen = {:?}\n",
        metadata.research,
        metadata.first_seen,
        repository = repository,
        class = class.code(),
        baseline_sha = baseline_sha,
        tier = metadata.tier.token().to_ascii_lowercase(),
        kind = metadata.kind.token(),
        visibility = metadata.visibility.token(),
    ));
    std::fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))
}

fn write_if_changed(path: &Path, body: &str) -> Result<(), String> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == body
    {
        return Ok(());
    }
    std::fs::write(path, body).map_err(|e| format!("writing {}: {e}", path.display()))
}
