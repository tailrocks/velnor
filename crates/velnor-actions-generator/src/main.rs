//! `velnor-actions-generator` CLI.
//!
//! Subcommands:
//! - `check --root PATH` — skeleton layout self-check (unchanged from plan 004).
//! - `generate --root PATH` — render the five class templates (and, once the
//!   block SHA is bound, the five callable workflows).
//! - `render-consumer --root PATH --repository OWNER/REPO` plus three
//!   owner-local release SHA flags, `--calver VER --output DIR`.
//! - `audit --root PATH` — full fleet audit; prints the exact fleet-valid line.
//!
//! Malformed arguments, a missing layout root, or any audit violation exit
//! nonzero.

use std::path::PathBuf;
use std::process::ExitCode;

use velnor_actions_generator::model::{CensusMetadata, RepositoryKind, RepositoryTier, Visibility};
use velnor_actions_generator::{
    ALL_CLASSES, REQUIRED_LAYOUT, RepositoryClass, audit, generate, render_consumer_to_dir,
    render_package_consumer_to_dir, tools, validate_layout,
};

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    match args.next().as_deref() {
        Some("check") => run_check(args),
        Some("generate") => run_generate(args),
        Some("render-consumer") => run_render_consumer(args),
        Some("render-package-consumer") => run_render_package_consumer(args),
        Some("audit") => run_audit(args),
        Some("census") => run_census(args),
        Some("scaffold") => run_scaffold(args),
        Some("fleet-audit") => run_fleet_audit(args),
        Some("verify-remote") => run_verify_remote(args),
        Some("release-check") => run_release_check(args),
        Some("tool-registry") => run_tool_registry(args),
        Some(other) => Err(format!("unknown subcommand: {other}")),
        None => Err(
            "missing subcommand (expected: check, generate, render-consumer, scaffold, census, audit, or fleet-audit)".to_string(),
        ),
    }
}

fn run_census(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("census requires --root PATH")?;
    let manifest = velnor_actions_generator::model::FleetManifest::load(&root)?;
    Ok(velnor_actions_generator::policy::census_output(&manifest))
}

fn run_scaffold(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root = PathBuf::from(".");
    let mut output = None;
    let mut repository = None;
    let mut class = None;
    let mut tier = None;
    let mut kind = None;
    let mut visibility = None;
    let mut first_seen = None;
    let mut baseline_sha = None;
    let mut research = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(require_value(&mut args, "--root")?),
            "--output" => output = Some(PathBuf::from(require_value(&mut args, "--output")?)),
            "--repository" => repository = Some(require_value(&mut args, "--repository")?),
            "--class" => class = Some(require_value(&mut args, "--class")?),
            "--tier" => tier = Some(require_value(&mut args, "--tier")?),
            "--kind" => kind = Some(require_value(&mut args, "--kind")?),
            "--visibility" => visibility = Some(require_value(&mut args, "--visibility")?),
            "--first-seen" => first_seen = Some(require_value(&mut args, "--first-seen")?),
            "--baseline-sha" => baseline_sha = Some(require_value(&mut args, "--baseline-sha")?),
            "--research" => research = true,
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let class = parse_class(class.as_deref().ok_or("scaffold requires --class")?)?;
    let metadata = parse_metadata(
        tier.as_deref().ok_or("scaffold requires --tier")?,
        kind.as_deref().ok_or("scaffold requires --kind")?,
        visibility
            .as_deref()
            .ok_or("scaffold requires --visibility")?,
        research,
        first_seen
            .as_deref()
            .ok_or("scaffold requires --first-seen YYYY-MM-DD")?,
    )?;
    let repository = repository.ok_or("scaffold requires --repository OWNER/REPO")?;
    let output = output.ok_or("scaffold requires --output DIRECTORY")?;
    let baseline_sha = baseline_sha.ok_or("scaffold requires --baseline-sha SHA")?;
    let written = velnor_actions_generator::scaffold_consumer(
        &root,
        &output,
        &repository,
        class,
        &metadata,
        &baseline_sha,
    )?;
    Ok(format!(
        "scaffolded {} files and registered {repository}",
        written.len()
    ))
}

fn run_fleet_audit(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    let mut write_deferred = false;
    let mut offline = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            "--write-deferred" => write_deferred = true,
            "--offline" => offline = true,
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("fleet-audit requires --root PATH")?;
    audit::fleet_audit(&root, write_deferred, offline)
}

fn parse_class(value: &str) -> Result<RepositoryClass, String> {
    ALL_CLASSES
        .iter()
        .copied()
        .find(|class| class.code() == value)
        .ok_or_else(|| format!("unknown class {value:?}"))
}

fn parse_metadata(
    tier: &str,
    kind: &str,
    visibility: &str,
    research: bool,
    first_seen: &str,
) -> Result<CensusMetadata, String> {
    let tier = match tier {
        "leaf" | "Leaf" => RepositoryTier::Leaf,
        "workspace" | "Workspace" => RepositoryTier::Workspace,
        "polyglot" | "Polyglot" => RepositoryTier::Polyglot,
        _ => return Err(format!("unknown tier {tier:?}")),
    };
    let kind = match kind {
        "app" | "App" => RepositoryKind::App,
        "iac" | "IaC" => RepositoryKind::Iac,
        "dist" => RepositoryKind::Dist,
        "ci-producer" | "CI-producer" => RepositoryKind::CiProducer,
        "out-of-scope" => RepositoryKind::OutOfScope,
        _ => return Err(format!("unknown kind {kind:?}")),
    };
    let visibility = match visibility {
        "public" => Visibility::Public,
        "private" => Visibility::Private,
        "internal" => Visibility::Internal,
        _ => return Err(format!("unknown visibility {visibility:?}")),
    };
    if first_seen.len() != 10
        || first_seen.as_bytes().get(4) != Some(&b'-')
        || first_seen.as_bytes().get(7) != Some(&b'-')
        || !first_seen
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return Err(format!("first-seen {first_seen:?} is not YYYY-MM-DD"));
    }
    Ok(CensusMetadata {
        tier,
        kind,
        visibility,
        research,
        first_seen: first_seen.to_owned(),
    })
}

fn run_render_package_consumer(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root = PathBuf::from(".");
    let mut repository = None;
    let mut jackin_release_sha = None;
    let mut tailrocks_release_sha = None;
    let mut chainargos_release_sha = None;
    let mut calver = None;
    let mut output = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(require_value(&mut args, "--root")?),
            "--repository" => repository = Some(require_value(&mut args, "--repository")?),
            "--jackin-release-sha" => {
                jackin_release_sha = Some(require_value(&mut args, "--jackin-release-sha")?)
            }
            "--tailrocks-release-sha" => {
                tailrocks_release_sha = Some(require_value(&mut args, "--tailrocks-release-sha")?)
            }
            "--chainargos-release-sha" => {
                chainargos_release_sha = Some(require_value(&mut args, "--chainargos-release-sha")?)
            }
            "--calver" => calver = Some(require_value(&mut args, "--calver")?),
            "--output" => output = Some(PathBuf::from(require_value(&mut args, "--output")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let repository = repository.ok_or("render-package-consumer requires --repository")?;
    let release_shas = [
        jackin_release_sha.ok_or("render-package-consumer requires --jackin-release-sha")?,
        tailrocks_release_sha.ok_or("render-package-consumer requires --tailrocks-release-sha")?,
        chainargos_release_sha
            .ok_or("render-package-consumer requires --chainargos-release-sha")?,
    ];
    let calver = calver.ok_or("render-package-consumer requires --calver")?;
    let output = output.ok_or("render-package-consumer requires --output")?;
    let path = render_package_consumer_to_dir(
        &root,
        &repository,
        [&release_shas[0], &release_shas[1], &release_shas[2]],
        &calver,
        &output,
    )?;
    Ok(format!("rendered {}", path.display()))
}

fn run_release_check(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    let mut release: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            "--release" => release = Some(require_value(&mut args, "--release")?),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("release-check requires --root PATH")?;
    audit::release_check(&root, release.as_deref())
}

fn run_verify_remote(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("verify-remote requires --root PATH")?;
    audit::verify_remote_closure(&root)
}

fn run_check(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("check requires --root PATH")?;
    validate_layout(&root)?;
    Ok(format!(
        "skeleton valid: {} classes, {} roots",
        ALL_CLASSES.len(),
        REQUIRED_LAYOUT.len()
    ))
}

fn run_generate(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("generate requires --root PATH")?;
    let written = generate(&root)?;
    Ok(format!("generated {} files", written.len()))
}

fn run_audit(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("audit requires --root PATH")?;
    audit::audit(&root)
}

fn run_tool_registry(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root = PathBuf::from(".");
    let mut registry_path = None;
    let mut fleet_path = None;
    let mut mise_path = None;
    let mut lock_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(require_value(&mut args, "--root")?),
            "--registry" => {
                registry_path = Some(PathBuf::from(require_value(&mut args, "--registry")?))
            }
            "--fleet" => fleet_path = Some(PathBuf::from(require_value(&mut args, "--fleet")?)),
            "--mise" => mise_path = Some(PathBuf::from(require_value(&mut args, "--mise")?)),
            "--lock" => lock_path = Some(PathBuf::from(require_value(&mut args, "--lock")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let registry =
        tools::ToolRegistry::load(&registry_path.unwrap_or_else(|| tools::registry_path(&root)))?;
    let count = match fleet_path {
        Some(path) => registry.check_fleet(&root, &path)?,
        None => registry.check_files(
            &mise_path.unwrap_or_else(|| root.join("mise.toml")),
            &lock_path.unwrap_or_else(|| root.join("mise.lock")),
        )?,
    };
    Ok(format!("tool registry valid: {count} effective tools"))
}

fn run_render_consumer(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root = PathBuf::from(".");
    let mut repository: Option<String> = None;
    let mut jackin_release_sha: Option<String> = None;
    let mut tailrocks_release_sha: Option<String> = None;
    let mut chainargos_release_sha: Option<String> = None;
    let mut calver: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(require_value(&mut args, "--root")?),
            "--repository" => repository = Some(require_value(&mut args, "--repository")?),
            "--jackin-release-sha" => {
                jackin_release_sha = Some(require_value(&mut args, "--jackin-release-sha")?)
            }
            "--tailrocks-release-sha" => {
                tailrocks_release_sha = Some(require_value(&mut args, "--tailrocks-release-sha")?)
            }
            "--chainargos-release-sha" => {
                chainargos_release_sha = Some(require_value(&mut args, "--chainargos-release-sha")?)
            }
            "--calver" => calver = Some(require_value(&mut args, "--calver")?),
            "--output" => output = Some(PathBuf::from(require_value(&mut args, "--output")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let repository = repository.ok_or("render-consumer requires --repository OWNER/REPO")?;
    let release_shas = [
        jackin_release_sha.ok_or("render-consumer requires --jackin-release-sha")?,
        tailrocks_release_sha.ok_or("render-consumer requires --tailrocks-release-sha")?,
        chainargos_release_sha.ok_or("render-consumer requires --chainargos-release-sha")?,
    ];
    let calver = calver.ok_or("render-consumer requires --calver VERSION")?;
    let output = output.ok_or("render-consumer requires --output DIRECTORY")?;
    let path = render_consumer_to_dir(
        &root,
        &repository,
        [&release_shas[0], &release_shas[1], &release_shas[2]],
        &calver,
        &output,
    )?;
    Ok(format!("rendered {}", path.display()))
}

fn require_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}
