//! Producer-owned native application build lane.
//!
//! This workflow only builds and uploads source-bound target outputs.  The
//! existing stable release publisher owns the provider release, resolves its
//! numeric id, assembles the one product manifest, writes archive identity,
//! and publishes after all bytes are verified.  Keeping those responsibilities
//! separate prevents a second publisher from racing the runtime release lane.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::{Args, Primitive, RenderCtx, Rendered, NATIVE_PRODUCT};
use crate::s2::provider::ProviderId;
use crate::s2::{
    selector_runs_on_yaml, shell_quote, yaml_scalar, GeneratorError, GENERATED_HEADER,
};

/// Product build images are pinned here because Apple compilation cannot use a
/// Linux selector.  This is the exact current arm64 preview label.  Intel is
/// retained in the typed target census but explicitly blocked until GitHub
/// offers a macOS 27 Intel image; no fallback image is valid.
pub(crate) const MACOS_ARM64_RUNNER: &str = "xcode-27";
const PRODUCT_MANIFEST_FILE: &str = "product-manifest.json";
pub(crate) const PRODUCT_COMPONENT_CONTRACT_SCHEMA: &str = "velnor.native-product-contract/v1";

pub(crate) fn product_component_contract_file(channel: &str) -> &'static str {
    if channel == "preview" {
        ".github/ci/native-product-preview-contract.json"
    } else {
        ".github/ci/native-product-contract.json"
    }
}

pub(crate) fn is_native_product_side(primitive: &str) -> bool {
    primitive == NATIVE_PRODUCT
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Component {
    name: String,
    package: String,
    binary: String,
    feature: Option<String>,
    identity: String,
}

/// Generic typed native product builder. Product identity comes only from the
/// declaration; this module has no Velnor-specific package lookup.
pub(crate) struct NativeProduct;

impl Primitive for NativeProduct {
    fn id(&self) -> &'static str {
        NATIVE_PRODUCT
    }

    fn schema(&self) -> &'static [&'static str] {
        &[
            "archive_component",
            "archive_identity_schema",
            "archive_manifest_schema",
            "blocked_targets",
            "channel",
            "components",
            "manifest_schema",
            "product_id",
            "source_repository",
            "targets",
        ]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let file = ctx.file.filter(|file| !file.is_empty()).ok_or_else(|| {
            GeneratorError::usage("`native-product` needs `file`; declare the workflow it owns")
        })?;
        let archive_component = required_string(args, "archive_component")?;
        let archive_identity_schema = required_string(args, "archive_identity_schema")?;
        let archive_manifest_schema = required_string(args, "archive_manifest_schema")?;
        let product_id = required_string(args, "product_id")?;
        let channel = required_string(args, "channel")?;
        let manifest_schema = required_string(args, "manifest_schema")?;
        let source_repository = required_string(args, "source_repository")?;
        let targets = args.strings("targets")?.unwrap_or_default();
        let blocked_targets = args.strings("blocked_targets")?.unwrap_or_default();
        let components = parse_components(args.string_tables("components")?)?;
        validate_product_contract(
            &product_id,
            &channel,
            &source_repository,
            &archive_component,
            &archive_identity_schema,
            &archive_manifest_schema,
            &manifest_schema,
            &targets,
            &blocked_targets,
            &components,
        )?;
        let content = render_workflow(
            ctx,
            &product_id,
            &channel,
            &source_repository,
            &archive_component,
            &archive_identity_schema,
            &archive_manifest_schema,
            &manifest_schema,
            &targets,
            &blocked_targets,
            &components,
        );
        let contract = render_component_contract(
            &product_id,
            &channel,
            &archive_component,
            &archive_identity_schema,
            &archive_manifest_schema,
            &manifest_schema,
            &targets,
            &blocked_targets,
            &components,
        )?;
        Ok(Rendered {
            files: [
                (
                    std::path::PathBuf::from(".github/workflows").join(file),
                    content,
                ),
                (
                    std::path::PathBuf::from(product_component_contract_file(&channel)),
                    contract,
                ),
            ]
            .into_iter()
            .collect(),
            ..Rendered::default()
        })
    }
}

fn required_string(args: &Args<'_>, key: &str) -> Result<String, GeneratorError> {
    args.string(key)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| GeneratorError::usage(format!("`native-product` needs non-empty `{key}`")))
}

fn parse_components(
    values: Option<BTreeMap<String, Vec<String>>>,
) -> Result<Vec<Component>, GeneratorError> {
    let values = values.unwrap_or_default();
    if values.is_empty() {
        return Err(GeneratorError::usage(
            "`native-product` needs a non-empty `components` table; each value is [crate, binary, optional-feature, optional-identity]",
        ));
    }
    values
        .into_iter()
        .map(|(name, values)| {
            if !(2..=4).contains(&values.len()) {
                return Err(GeneratorError::usage(format!(
                    "`native-product` component `{name}` must be [crate, binary, optional-feature, optional-identity]"
                )));
            }
            let package = values.first().cloned().unwrap_or_default();
            let binary = values.get(1).cloned().unwrap_or_default();
            let feature = values.get(2).cloned().filter(|value| !value.is_empty());
            let identity = values
                .get(3)
                .cloned()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "version".to_owned());
            if !safe_name(&name) || !safe_name(&package) || !safe_name(&binary) {
                return Err(GeneratorError::usage(format!(
                    "`native-product` component `{name}` has an unsafe name"
                )));
            }
            if feature
                .as_deref()
                .is_some_and(|value| value != "release-build")
            {
                return Err(GeneratorError::usage(format!(
                    "`native-product` component `{name}` feature must be `release-build` or empty"
                )));
            }
            if identity != "version" && identity != "revision" {
                return Err(GeneratorError::usage(format!(
                    "`native-product` component `{name}` identity must be `version` or `revision`"
                )));
            }
            Ok(Component {
                name,
                package,
                binary,
                feature,
                identity,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_component_contract(
    product_id: &str,
    channel: &str,
    archive_component: &str,
    archive_identity_schema: &str,
    archive_manifest_schema: &str,
    manifest_schema: &str,
    targets: &[String],
    blocked_targets: &[String],
    components: &[Component],
) -> Result<String, GeneratorError> {
    let value = serde_json::json!({
        "schema": PRODUCT_COMPONENT_CONTRACT_SCHEMA,
        "product_id": product_id,
        "channel": channel,
        "manifest_schema": manifest_schema,
        "archive_component": archive_component,
        "archive_identity_schema": archive_identity_schema,
        "archive_manifest_schema": archive_manifest_schema,
        "targets": targets,
        "blocked_targets": blocked_targets,
        "components": components.iter().map(|component| serde_json::json!({
            "name": component.name,
            "crate": component.package,
            "binary": component.binary,
            "feature": component.feature,
            "identity": component.identity,
            "targets": targets,
        })).collect::<Vec<_>>(),
    });
    let mut output = serde_json::to_string_pretty(&value).map_err(|error| {
        GeneratorError::usage(format!("render native product component contract: {error}"))
    })?;
    output.push('\n');
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn validate_product_contract(
    product_id: &str,
    channel: &str,
    source_repository: &str,
    archive_component: &str,
    archive_identity_schema: &str,
    archive_manifest_schema: &str,
    manifest_schema: &str,
    targets: &[String],
    blocked_targets: &[String],
    components: &[Component],
) -> Result<(), GeneratorError> {
    if !safe_name(product_id) {
        return Err(GeneratorError::usage(
            "`native-product` `product_id` must be a portable lower-case id",
        ));
    }
    if channel != "stable" && channel != "preview" {
        return Err(GeneratorError::usage(
            "`native-product` `channel` must be `stable` or `preview`",
        ));
    }
    if !valid_repository(source_repository) {
        return Err(GeneratorError::usage(
            "`native-product` `source_repository` must be an owner/repository slug",
        ));
    }
    if !safe_name(archive_component) {
        return Err(GeneratorError::usage(
            "`native-product` `archive_component` must be a component name",
        ));
    }
    if !safe_schema(archive_identity_schema)
        || !safe_schema(archive_manifest_schema)
        || !safe_schema(manifest_schema)
    {
        return Err(GeneratorError::usage(
            "`native-product` schemas must be portable schema identifiers",
        ));
    }
    let mut target_set = BTreeSet::new();
    for target in targets {
        if !supported_target(target) || !target_set.insert(target.as_str()) {
            return Err(GeneratorError::usage(format!(
                "`native-product` target `{target}` is unsupported or duplicated"
            )));
        }
    }
    let required_targets = [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
    ];
    if target_set.len() != required_targets.len()
        || required_targets
            .iter()
            .any(|target| !target_set.contains(target))
    {
        let missing = required_targets
            .iter()
            .find(|target| !target_set.contains(**target))
            .copied()
            .unwrap_or("exact four-target matrix");
        return Err(GeneratorError::usage(format!(
            "`native-product` targets must be exactly the four supported targets; missing `{missing}`"
        )));
    }
    let mut blocked_set = BTreeSet::new();
    for target in blocked_targets {
        if !target_set.contains(target.as_str()) || !blocked_set.insert(target.as_str()) {
            return Err(GeneratorError::usage(format!(
                "`native-product` blocked target `{target}` is not a unique declared target"
            )));
        }
    }
    if !blocked_set.contains("x86_64-apple-darwin") {
        return Err(GeneratorError::usage(
            "`native-product` must explicitly block x86_64-apple-darwin until a macOS 27 Intel runner is offered",
        ));
    }
    if blocked_set.len() == target_set.len() {
        return Err(GeneratorError::usage(
            "`native-product` needs at least one unblocked target to build",
        ));
    }
    let mut component_set = BTreeSet::new();
    if components.is_empty() {
        return Err(GeneratorError::usage(
            "`native-product` needs at least one typed component",
        ));
    }
    for component in components {
        if !component_set.insert(component.name.clone()) {
            return Err(GeneratorError::usage(format!(
                "`native-product` has duplicate component `{}`",
                component.name
            )));
        }
    }
    if !component_set.contains(archive_component) {
        return Err(GeneratorError::usage(format!(
            "`native-product` archive_component `{archive_component}` is not in components"
        )));
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]
fn render_workflow(
    ctx: &RenderCtx<'_>,
    product_id: &str,
    channel: &str,
    source_repository: &str,
    archive_component: &str,
    archive_identity_schema: &str,
    archive_manifest_schema: &str,
    manifest_schema: &str,
    targets: &[String],
    blocked_targets: &[String],
    components: &[Component],
) -> String {
    let blocked_set = blocked_targets
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut matrix = String::new();
    for target in targets
        .iter()
        .filter(|target| !blocked_set.contains(target.as_str()))
    {
        let _ = writeln!(
            matrix,
            "          - target: {}\n            runner: {}",
            yaml_scalar(target),
            runner_for_target(ctx, target),
        );
    }

    let blocker_runner = runner_for_target(ctx, "x86_64-unknown-linux-gnu");
    let mut blocked_jobs = String::new();
    for target in blocked_targets {
        let job_id = format!("blocked-{}", target.replace('-', "_"));
        let _ = writeln!(
            blocked_jobs,
            "  {job_id}:\n    name: Blocked native product target / {target}\n    runs-on: {blocker_runner}\n    timeout-minutes: 5\n    permissions:\n      contents: read\n    steps:\n      - name: Refuse unsupported target\n        run: |\n          echo \"::error::native product target {target} is blocked: macOS 27 Intel is unavailable; no fallback runner is permitted\" >&2\n          exit 1\n",
            target = yaml_scalar(target),
        );
    }

    let mut build_steps = String::new();
    for component in components {
        let component_name = shell_quote(&component.name);
        let _ = write!(
            build_steps,
            "          component=\"$(jq -er --arg name {component_name} '.[] | select(.name == $name)' <<<\"$COMPONENTS_JSON\")\"\n          package=\"$(jq -er '.crate' <<<\"$component\")\"\n          binary=\"$(jq -er '.binary' <<<\"$component\")\"\n          component_feature=\"$(jq -r '.feature // empty' <<<\"$component\")\"\n          component_identity=\"$(jq -er '.identity' <<<\"$component\")\"\n          component_version=\"$(jq -er '.version' <<<\"$component\")\"\n          component_targets=\"$(jq -c '.targets | sort' <<<\"$component\")\"\n          [ \"$component_targets\" = \"$(jq -c 'sort' <<<\"$TARGETS_JSON\")\" ] || {{ echo '::error::component target contract is malformed' >&2; exit 1; }}\n          case \"$component_version\" in ''|development|unknown|*[^0-9A-Za-z.+~-]*) echo '::error::component Cargo version is not publishable' >&2; exit 1 ;; esac\n          feature_args=()\n          if [ -n \"$component_feature\" ]; then feature_args+=(--features \"$component_feature\"); fi\n          CARGO_INCREMENTAL=0 VELNOR_RELEASE_BUILD=1 cargo build --locked --release --no-default-features --package \"$package\" --bin \"$binary\" ${{feature_args[@]}} --target \"$TARGET\"\n          compiled=\"target/$TARGET/release/$binary\"\n          test -x \"$compiled\" || {{ echo \"::error::typed sibling binary is missing\" >&2; exit 1; }}\n          case \"$TARGET\" in\n            x86_64-unknown-linux-gnu) file \"$compiled\" | grep -Eq 'ELF 64-bit.*x86-64' ;;\n            aarch64-unknown-linux-gnu) file \"$compiled\" | grep -Eq 'ELF 64-bit.*ARM aarch64' ;;\n            aarch64-apple-darwin) file \"$compiled\" | grep -Eq 'Mach-O 64-bit.*arm64' ;;\n            x86_64-apple-darwin) file \"$compiled\" | grep -Eq 'Mach-O 64-bit.*x86_64' ;;\n            *) false ;;\n          esac || {{ echo \"::error::compiled sibling architecture does not match $TARGET\" >&2; exit 1; }}\n          identity_args=(--version)\n          [ \"$component_identity\" = revision ] && identity_args=(--revision)\n          identity=\"$(\"$compiled\" ${{identity_args[@]}} 2>&1 || true)\"\n          case \"$identity\" in *development*|*unknown*|'') echo '::error::publishable component has development/unknown identity' >&2; exit 1 ;; esac\n          if [ \"$component_identity\" = revision ]; then\n            [ \"$identity\" = \"$SOURCE_COMMIT\" ] || {{ echo \"::error::component source revision differs from checkout\" >&2; exit 1; }}\n          else\n            reported_version=\"${{identity%% *}}\"\n            [ \"$reported_version\" = \"$component_version\" ] || {{ echo \"::error::component version differs from Cargo metadata\" >&2; exit 1; }}\n          fi\n          asset=\"$binary-$TARGET\"\n          cp \"$compiled\" \"dist/$TARGET/$asset\"\n          cp \"$compiled\" \"dist/$TARGET/package/$binary\"\n          digest=\"$(sha256sum \"dist/$TARGET/$asset\" | awk '{{print $1}}')\"\n          size=\"$(stat -c%s \"dist/$TARGET/$asset\" 2>/dev/null || stat -f%z \"dist/$TARGET/$asset\")\"\n          jq -cn --arg name \"$asset\" --arg target \"$TARGET\" --arg sha256 \"$digest\" --argjson size \"$size\" '{{name:$name,target:$target,kind:\"binary\",sha256:$sha256,size:$size}}' >> \"dist/$TARGET/artifacts-$TARGET.jsonl\"\n          jq -cn --arg name {component_name} --arg crate \"$package\" --arg version \"$component_version\" --arg binary \"$binary\" --arg target \"$TARGET\" --arg feature \"$component_feature\" --arg identity \"$component_identity\" '{{name:$name,crate:$crate,version:$version,binary:$binary,target:$target,feature:(if $feature == \"\" then null else $feature end),identity:$identity}}' >> \"dist/$TARGET/components-$TARGET.jsonl\"\n",
            component_name = component_name,
        );
    }
    build_steps = build_steps
        .replace(
            " ${feature_args[@]} --target",
            " \"${feature_args[@]}\" --target",
        )
        .replace(
            "\"$compiled\" ${identity_args[@]} 2>&1",
            "\"$compiled\" \"${identity_args[@]}\" 2>&1",
        );
    build_steps = build_steps.replace(
        " 2>&1 || true)\"\n",
        " 2>&1)\" || { echo '::error::component identity command failed' >&2; exit 1; }\n",
    );
    build_steps = build_steps.replace(
        "          cp \"$compiled\" \"dist/$TARGET/$asset\"\n          cp \"$compiled\" \"dist/$TARGET/package/$binary\"\n",
        "          cp \"$compiled\" \"dist/$TARGET/$asset\"\n          cp \"$compiled\" \"dist/$TARGET/package/$binary\"\n          chmod 0755 \"dist/$TARGET/$asset\" \"dist/$TARGET/package/$binary\"\n          test -x \"dist/$TARGET/$asset\" && test -x \"dist/$TARGET/package/$binary\" || { echo '::error::typed sibling mode was lost before upload' >&2; exit 1; }\n",
    );

    let components_json = format!(
        "[{}]",
        components
            .iter()
            .map(|component| {
                format!(
                    "{{\"name\":\"{}\",\"crate\":\"{}\",\"binary\":\"{}\",\"identity\":\"{}\",\"feature\":{}}}",
                    component.name,
                    component.package,
                    component.binary,
                    component.identity,
                    component
                        .feature
                        .as_ref()
                        .map_or_else(|| "null".to_owned(), |feature| format!("\"{feature}\""))
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    );
    let targets_json = format!(
        "[{}]",
        targets
            .iter()
            .map(|target| format!("\"{target}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    let checkout = ctx.pins.checkout;
    let upload = ctx.pins.upload_artifact;
    let product_id_q = shell_quote(product_id);
    let source_repository_q = shell_quote(source_repository);
    let channel_q = shell_quote(channel);
    let manifest_schema_q = shell_quote(manifest_schema);
    let archive_component_q = shell_quote(archive_component);
    let archive_identity_schema_q = shell_quote(archive_identity_schema);
    let archive_manifest_schema_q = shell_quote(archive_manifest_schema);
    let targets_json_q = shell_quote(&targets_json);
    let blocked_targets_json = format!(
        "[{}]",
        blocked_targets
            .iter()
            .map(|target| format!("\"{target}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    let blocked_targets_json_q = shell_quote(&blocked_targets_json);
    let components_json_q = shell_quote(&components_json);
    let default_branch_q = shell_quote(&ctx.config.default_branch);

    let mut output = format!(
        "{GENERATED_HEADER}# Build-only producer. release.yml is the sole provider publisher.\nname: Native product build\nrun-name: Native product build · ${{{{ github.ref_name }}}}\n\non:\n  workflow_call:\n\nconcurrency:\n  group: native-product-build-${{{{ github.workflow }}}}-${{{{ github.run_id }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n{blocked_jobs}  build:\n    name: Build product / ${{{{ matrix.target }}}}\n    runs-on: ${{{{ matrix.runner }}}}\n    timeout-minutes: 90\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    permissions:\n      contents: read\n    env:\n      TARGET: ${{{{ matrix.target }}}}\n      PRODUCT_ID: {product_id_q}\n      CHANNEL: {channel_q}\n      SOURCE_REPOSITORY: {source_repository_q}\n      MANIFEST_SCHEMA: {manifest_schema_q}\n      ARCHIVE_COMPONENT: {archive_component_q}\n      ARCHIVE_IDENTITY_SCHEMA: {archive_identity_schema_q}\n      ARCHIVE_MANIFEST_SCHEMA: {archive_manifest_schema_q}\n      DEFAULT_BRANCH: {default_branch_q}\n      TARGETS_JSON: {targets_json_q}\n      BLOCKED_TARGETS_JSON: {blocked_targets_json_q}\n      COMPONENTS_JSON: {components_json_q}\n    steps:\n      - name: Checkout exact source\n        uses: {checkout}\n        with:\n          ref: ${{{{ github.sha }}}}\n          fetch-depth: 0\n          persist-credentials: false\n      - name: Prove exact source identity\n        run: |\n          set -euo pipefail\n          actual=\"$(git rev-parse HEAD)\"\n          case \"$actual\" in ''|*[!0-9a-f]*) echo '::error::checkout is not a lowercase 40-hex commit' >&2; exit 1 ;; esac\n          [ \"${{{{#actual}}}}\" -eq 40 ] && [ \"$actual\" = \"$GITHUB_SHA\" ] || {{ echo '::error::checkout source differs from event source' >&2; exit 1; }}\n          test -z \"$(git status --porcelain)\" || {{ echo '::error::native product checkout is dirty' >&2; exit 1; }}\n      - name: Add Rust target\n        run: rustup target add \"$TARGET\"\n      - name: Read Cargo component metadata\n        run: cargo metadata --locked --no-deps --format-version 1 > cargo-metadata.json\n      - name: Build and verify typed sibling inventory\n        run: |\n          set -euo pipefail\n          if [ \"$CHANNEL\" = stable ]; then\n            PRODUCT_VERSION=\"${{{{ github.ref_name }}}}\"\n            PRODUCT_VERSION=\"${{PRODUCT_VERSION#v}}\"\n            [[ \"$PRODUCT_VERSION\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+$ ]] || {{ echo '::error::stable product source tag is not SemVer' >&2; exit 1; }}\n          else\n            PRODUCT_VERSION=\"0.0.0-preview.${{{{ github.run_number }}}}+$(printf '%s' \"$GITHUB_SHA\" | cut -c1-7)\"\n          fi\n          SOURCE_COMMIT=\"$GITHUB_SHA\"\n          SOURCE_REF=\"$GITHUB_REF\"\n          RELEASE_TAG=\"$GITHUB_REF_NAME\"\n          export PRODUCT_VERSION SOURCE_COMMIT SOURCE_REF RELEASE_TAG\n          mkdir -p \"dist/$TARGET/package\"\n          : > \"dist/$TARGET/artifacts-$TARGET.jsonl\"\n          : > \"dist/$TARGET/components-$TARGET.jsonl\"\n{build_steps}          jq -S -n --arg schema \"$MANIFEST_SCHEMA\" --arg product_id \"$PRODUCT_ID\" --arg channel \"$CHANNEL\" --arg source_repository \"$SOURCE_REPOSITORY\" --arg source_ref \"$SOURCE_REF\" --arg source_commit \"$SOURCE_COMMIT\" --arg release_tag \"$RELEASE_TAG\" --arg version \"$PRODUCT_VERSION\" --arg archive_component \"$ARCHIVE_COMPONENT\" --arg archive_identity_schema \"$ARCHIVE_IDENTITY_SCHEMA\" --arg archive_manifest_schema \"$ARCHIVE_MANIFEST_SCHEMA\" --argjson targets \"$TARGETS_JSON\" --argjson blocked_targets \"$BLOCKED_TARGETS_JSON\" --argjson components \"$COMPONENTS_JSON\" '{{schema:$schema,product_id:$product_id,channel:$channel,source_repository:$source_repository,source_ref:$source_ref,source_commit:$source_commit,release_tag:$release_tag,version:$version,archive_component:$archive_component,archive_identity_schema:$archive_identity_schema,archive_manifest_schema:$archive_manifest_schema,targets:$targets,blocked_targets:$blocked_targets,components:$components}}' > \"dist/$TARGET/product-contract-$TARGET.json\"\n          test \"$(jq -s 'map(.name) | unique | sort | length' \"dist/$TARGET/components-$TARGET.jsonl\")\" -eq {component_count}\n          test \"$(jq -s 'map(.target) | unique | sort | length' \"dist/$TARGET/components-$TARGET.jsonl\")\" -eq 1\n      - name: Upload source-bound product build\n        uses: {upload}\n        with:\n          name: native-product-${{{{ matrix.target }}}}\n          path: dist/${{{{ matrix.target }}}}\n          if-no-files-found: error\n          retention-days: 2\n\n# The stable release workflow downloads these build artifacts, asks the provider\n# for its numeric release id, then writes {PRODUCT_MANIFEST_FILE}; this file\n# never calls gh release create/upload and cannot publish a competing product.\n",
        matrix = matrix,
        blocked_jobs = blocked_jobs,
        build_steps = build_steps,
        component_count = components.len(),
    )
    .replace("${{#actual}}", "${#actual}")
    .replace(
        "never calls gh release create/upload and cannot publish a competing product.",
        "never mutates provider releases or publishes a competing product.",
    );
    output = output.replace(
        "      - name: Build and verify typed sibling inventory\n",
        "      - name: Materialize independent component contract\n        run: |\n          set -euo pipefail\n          COMPONENTS_JSON=\"$(jq -cS --slurpfile metadata cargo-metadata.json --argjson targets \"$TARGETS_JSON\" '[.[] as $component | ($metadata[0].packages | map(select(.name == $component.crate))) as $packages | if ($packages | length) != 1 then error(\"component crate is not unique\") else $component + {version:$packages[0].version,targets:$targets} end]' <<<\"$COMPONENTS_JSON\")\"\n          jq -e 'all(.[]; (keys | sort) == [\"binary\",\"crate\",\"feature\",\"identity\",\"name\",\"targets\",\"version\"] and (.version | type == \"string\") and (.targets | type == \"array\"))' <<<\"$COMPONENTS_JSON\" >/dev/null\n          printf 'COMPONENTS_JSON=%s\\n' \"$COMPONENTS_JSON\" >> \"$GITHUB_ENV\"\n          printf '%s\\n' \"$COMPONENTS_JSON\" > components.json\n      - name: Build and verify typed sibling inventory\n",
    );
    output = output.replace(
        "          test -z \"$(git status --porcelain)\" || { echo '::error::native product checkout is dirty' >&2; exit 1; }\n",
        r#"          test -z "$(git status --porcelain)" || { echo '::error::native product checkout is dirty' >&2; exit 1; }
          [ "$SOURCE_REPOSITORY" = "$GITHUB_REPOSITORY" ] || { echo '::error::configured source repository differs from event repository' >&2; exit 1; }
          remote_repository="$(git config --get remote.origin.url || true)"
          case "$remote_repository" in
            https://github.com/*) remote_repository="${remote_repository#https://github.com/}" ;;
            git@github.com:*) remote_repository="${remote_repository#git@github.com:}" ;;
            ssh://git@github.com/*) remote_repository="${remote_repository#ssh://git@github.com/}" ;;
            *) echo '::error::checkout origin is not a GitHub repository URL' >&2; exit 1 ;;
          esac
          remote_repository="${remote_repository%.git}"
          [ "$remote_repository" = "$SOURCE_REPOSITORY" ] || { echo '::error::checkout origin differs from configured source repository' >&2; exit 1; }
"#,
    );
    output = output.replace(
        "__disabled_legacy_native_binary_row__",
        "          jq -cn --arg name \"$asset\" --arg target \"$TARGET\" --arg sha256 \"$digest\" --argjson size \"$size\" '{name:$name,target:$target,kind:\"binary\",sha256:$sha256,size:$size}' >> \"dist/$TARGET/artifacts-$TARGET.jsonl\"\n\
          component_feature=\"$(jq -r --arg crate \"$package\" --arg binary \"$binary\" '.[] | select(.crate == $crate and .binary == $binary) | .feature // empty' <<<\"$COMPONENTS_JSON\")\"\n\
          component_identity=\"$(jq -er --arg crate \"$package\" --arg binary \"$binary\" '.[] | select(.crate == $crate and .binary == $binary) | .identity' <<<\"$COMPONENTS_JSON\")\"\n",
    );
    output = output.replace(
        "__disabled_legacy_native_component_row__",
        " --arg target \"$TARGET\" --arg feature \"$component_feature\" --arg identity \"$component_identity\" '{name:$name,crate:$crate,version:$version,binary:$binary,target:$target,feature:(if $feature == \"\" then null else $feature end),identity:$identity}' >> \"dist/$TARGET/components-$TARGET.jsonl\"\n",
    );
    output
}

fn runner_for_target(ctx: &RenderCtx<'_>, target: &str) -> String {
    if target == "aarch64-apple-darwin" {
        yaml_scalar(MACOS_ARM64_RUNNER)
    } else if target.starts_with("aarch64-") {
        yaml_scalar("ubuntu-24.04-arm")
    } else {
        ctx.config
            .selectors
            .get(&ProviderId::GithubHosted)
            .map_or_else(|| yaml_scalar("ubuntu-24.04"), selector_runs_on_yaml)
    }
}

fn supported_target(target: &str) -> bool {
    matches!(
        target,
        "x86_64-unknown-linux-gnu"
            | "aarch64-unknown-linux-gnu"
            | "aarch64-apple-darwin"
            | "x86_64-apple-darwin"
    )
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
        && !value.starts_with(['.', '-', '_'])
        && !value.ends_with(['.', '-', '_'])
}

fn safe_schema(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'/' | b'_')
        })
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(owner), Some(repo), None) if safe_name(owner) && safe_name(repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(name: &str) -> Component {
        Component {
            name: name.to_owned(),
            package: name.to_owned(),
            binary: name.to_owned(),
            feature: None,
            identity: "version".to_owned(),
        }
    }

    fn four_targets() -> Vec<String> {
        [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn valid_contract(
        targets: &[String],
        blocked_targets: &[String],
    ) -> Result<(), GeneratorError> {
        validate_product_contract(
            "example",
            "stable",
            "owner/example",
            "runner",
            "velnor.homebrew-install-identity/v1",
            "velnor.homebrew-install/v1",
            "velnor.product-manifest/v1",
            targets,
            blocked_targets,
            &[component("runner")],
        )
    }

    #[test]
    #[expect(clippy::panic, reason = "malformed contract fixtures must fail loudly")]
    fn contract_requires_exact_four_target_census() {
        let mut targets = four_targets();
        targets.pop();
        let blocked = vec!["x86_64-apple-darwin".to_owned()];
        let error = match valid_contract(&targets, &blocked) {
            Ok(()) => panic!("partial target census must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("x86_64-apple-darwin"));
        targets.push("aarch64-apple-darwin".to_owned());
        targets.push("aarch64-apple-darwin".to_owned());
        assert!(valid_contract(&targets, &blocked).is_err());
    }

    #[test]
    #[expect(clippy::panic, reason = "missing Intel blocker must fail loudly")]
    fn intel_target_requires_explicit_blocker_and_stays_in_census() {
        let targets = four_targets();
        let empty = Vec::new();
        let error = match valid_contract(&targets, &empty) {
            Ok(()) => panic!("Intel blocker is required"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("x86_64-apple-darwin"));
        let blocked = vec!["x86_64-apple-darwin".to_owned()];
        assert!(valid_contract(&targets, &blocked).is_ok());
        assert!(error.to_string().contains("macOS 27 Intel"));
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "malformed typed component fixture must fail loudly"
    )]
    fn components_are_typed_and_feature_limited() {
        let parsed = parse_components(Some(BTreeMap::from([(
            "runner".to_owned(),
            vec![
                "runner".to_owned(),
                "runner".to_owned(),
                "release-build".to_owned(),
            ],
        )])));
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(error) => panic!("typed component parses: {error}"),
        };
        assert_eq!(parsed[0].feature.as_deref(), Some("release-build"));
        assert!(parse_components(Some(BTreeMap::from([(
            "runner".to_owned(),
            vec![
                "runner".to_owned(),
                "runner".to_owned(),
                "unknown".to_owned()
            ],
        )])))
        .is_err());
    }

    #[test]
    fn product_build_lane_has_no_second_publisher_or_old_intel_fallback() {
        let source = include_str!("native_product.rs");
        assert!(source.contains("workflow_call:"));
        assert!(source.contains("xcode-27"));
        let invented_arm_runner = ["macos", "27"].join("-");
        let forbidden_intel_runner = ["macos", "26-intel"].join("-");
        assert!(!source.contains(&invented_arm_runner));
        assert!(!source.contains(&forbidden_intel_runner));
        assert!(source.contains("blocked target"));
        assert!(source.contains("architecture does not match"));
        assert!(source.contains("component version differs"));
        assert!(source.contains("typed sibling mode was lost before upload"));
    }
}
