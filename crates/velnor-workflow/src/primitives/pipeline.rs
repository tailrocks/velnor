//! The per-unit verification pipelines.
//!
//! One primitive per unit kind and one declared row per unit. Units of a kind
//! share one reusable workflow file so a monorepo stays under GitHub's unique
//! reusable-workflow limit. Each row contributes a CI graph node; the kind
//! workflow body is rendered once by the generator.

use super::{
    Args, CacheBackend, GraphNode, Primitive, RenderCtx, Rendered, UnitContract, BUN_PACKAGE,
    DEFAULT_UNIT_TIMEOUT_MINUTES, DOCKER_IMAGE, DOCS_LINT, GRADLE_PROJECT, HOMEBREW_TAP,
    NODE_PACKAGE, OPENTOFU, RUST_CRATE, SWIFT_PACKAGE,
};
use crate::{GeneratorError, UnitKind};

/// The shared render of every per-unit pipeline.
fn render_unit(ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
    let unit = ctx.unit.ok_or_else(|| {
        GeneratorError::usage(format!(
            "`{}` renders one unit per row and needs `units` to name it",
            ctx.family
        ))
    })?;
    super::validate_cache_transports_for_unit(ctx.lanes.ir(), unit)?;
    super::validate_mutable_mount_seed(unit)?;
    let contract = UnitContract {
        lanes: declared_lanes(ctx, args)?,
        timeout_minutes: declared_timeout(ctx, args)?,
        cache: declared_cache(ctx, args)?,
        // The hosted lane's save step is still guarded by the generated
        // trusted-event expression; the contract only records that the
        // pipeline permits the save lifecycle.
        cache_save: true,
        mutable_mount_seed: unit
            .cache
            .as_ref()
            .is_some_and(|cache| cache.mutable_mount_seed),
    };
    let mut contracts = std::collections::BTreeMap::new();
    contracts.insert(unit.id.clone(), contract);
    if let Some(file) = ctx.file.filter(|file| !file.is_empty()) {
        let canonical = crate::nested_unit_workflow_file(unit);
        if file != canonical {
            return Err(GeneratorError::usage(format!(
                "`{}` must declare unit `{}` as `{canonical}`, not `{file}`; the aggregate callers invoke the canonical file name",
                ctx.family,
                unit.id
            )));
        }
    }
    let file = crate::nested_unit_workflow_file(unit);
    let nodes = vec![GraphNode::Unit {
        unit_id: unit.id.clone(),
        job_id: crate::stack_group_job_id_for_file(&file),
        name: crate::sidebar_group_name(unit),
        file,
    }];
    Ok(Rendered {
        nodes,
        contracts,
        ..Rendered::default()
    })
}

/// The lane jobs the declared unit emits, over the resolved lane matrix.
fn declared_lanes(
    ctx: &RenderCtx<'_>,
    args: &Args<'_>,
) -> Result<Vec<super::LaneJob>, GeneratorError> {
    let Some(names) = args.strings("jobs")? else {
        return Ok(ctx.lanes.jobs().to_vec());
    };
    let supported = ctx.lanes.jobs();
    let mut lanes = Vec::new();
    for name in &names {
        let job = supported
            .iter()
            .find(|job| job.lane.as_str() == name.as_str())
            .copied()
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "`{}` lane `{name}` is not supported by the `{}` runner mode; supported lanes: {}",
                    ctx.family,
                    ctx.config.runners.as_str(),
                    supported
                        .iter()
                        .map(|job| job.lane.as_str().to_owned())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        lanes.push(job);
    }
    if lanes.is_empty() {
        return Err(GeneratorError::usage(format!(
            "`{}` must emit at least one lane job",
            ctx.family
        )));
    }
    Ok(lanes)
}

fn declared_timeout(ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<u32, GeneratorError> {
    match args.integer("timeout_minutes")? {
        None => Ok(DEFAULT_UNIT_TIMEOUT_MINUTES),
        Some(minutes) => u32::try_from(minutes).map_err(|_| {
            GeneratorError::usage(format!(
                "`{}` `timeout_minutes` must be a positive number of minutes, found {minutes}",
                ctx.family
            ))
        }),
    }
}

fn declared_cache(ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<CacheBackend, GeneratorError> {
    match args.string("cache")? {
        None => Ok(ctx.cache.backend()),
        Some(name) => match name.as_str() {
            "detected" => Ok(CacheBackend::Detected),
            "actions" => Ok(CacheBackend::Actions),
            "objects" => Ok(CacheBackend::ObjectCache),
            other => Err(GeneratorError::usage(format!(
                "`{}` `cache` backend must be `detected`, `actions`, or `objects`, found `{other}`",
                ctx.family
            ))),
        },
    }
}

macro_rules! pipeline {
    ($name:ident, $id:ident, $kind:expr, $doc:literal) => {
        #[doc = $doc]
        pub(crate) struct $name;

        impl Primitive for $name {
            fn id(&self) -> &'static str {
                $id
            }

            fn schema(&self) -> &'static [&'static str] {
                &["cache", "coverage", "jobs", "timeout_minutes"]
            }

            fn render(
                &self,
                ctx: &RenderCtx<'_>,
                args: &Args<'_>,
            ) -> Result<Rendered, GeneratorError> {
                if let Some(unit) = ctx.unit
                    && unit.kind != $kind
                {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` is a {} unit, which `{}` does not render",
                        unit.id,
                        unit.kind.label(),
                        $id
                    )));
                }
                render_unit(ctx, args)
            }
        }
    };
}

pipeline!(
    RustCrate,
    RUST_CRATE,
    UnitKind::Rust,
    "Render one Rust crate's verification surface: the crate's build, test,\nclippy, and fmt commands run as one unit job on every declared lane, under\nthe crate's own reusable workflow file."
);
pipeline!(
    BunPackage,
    BUN_PACKAGE,
    UnitKind::Bun,
    "Render one Bun package's verification surface."
);
pipeline!(
    NodePackage,
    NODE_PACKAGE,
    UnitKind::Node,
    "Render one Node.js package's verification surface."
);
pipeline!(
    GradleProject,
    GRADLE_PROJECT,
    UnitKind::Gradle,
    "Render one Gradle project's verification surface."
);
pipeline!(
    SwiftPackage,
    SWIFT_PACKAGE,
    UnitKind::Swift,
    "Render one Swift package's verification surface."
);
pipeline!(
    OpenTofu,
    OPENTOFU,
    UnitKind::OpenTofu,
    "Render one `OpenTofu` module's verification surface."
);
pipeline!(
    DockerImage,
    DOCKER_IMAGE,
    UnitKind::Docker,
    "Render one Docker image's verification surface."
);
pipeline!(
    HomebrewTap,
    HOMEBREW_TAP,
    UnitKind::Homebrew,
    "Render one Homebrew formula's verification surface."
);
pipeline!(
    DocsLint,
    DOCS_LINT,
    UnitKind::Docs,
    "Render one documentation unit's verification surface."
);
