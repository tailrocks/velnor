//! Generic native product publisher.
//!
//! This lane is intentionally separate from the runtime-product primitive.
//! Runtime images and Firecracker guest payloads remain owned by the existing
//! Linux release lane; this primitive packages an application from an
//! explicit typed component/target table and emits the one application
//! manifest consumed by package projections.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::{Args, Primitive, RenderCtx, Rendered, NATIVE_PRODUCT};
use crate::s2::provider::ProviderId;
use crate::s2::{
    selector_runs_on_yaml, shell_quote, yaml_scalar, GeneratorError, GENERATED_HEADER,
    MACOS_HOSTED_RUNS_ON,
};

const PRODUCT_MANIFEST_FILE: &str = "product-manifest.json";

/// Whether a primitive id is a native-product workflow family.
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

/// A generic native product workflow.  It never names a Velnor package or
/// binary: all product and sibling identities come from the declaration.
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
            &components,
        );
        Ok(Rendered {
            files: std::iter::once((
                std::path::PathBuf::from(".github/workflows").join(file),
                content,
            ))
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
fn validate_product_contract(
    product_id: &str,
    channel: &str,
    source_repository: &str,
    archive_component: &str,
    archive_identity_schema: &str,
    archive_manifest_schema: &str,
    manifest_schema: &str,
    targets: &[String],
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
    if !safe_schema(archive_identity_schema) || !safe_schema(archive_manifest_schema) {
        return Err(GeneratorError::usage(
            "`native-product` archive schemas must be portable schema identifiers",
        ));
    }
    if !safe_schema(manifest_schema) {
        return Err(GeneratorError::usage(
            "`native-product` `manifest_schema` must be a portable schema identifier",
        ));
    }
    let mut target_set: BTreeSet<&str> = BTreeSet::new();
    for target in targets {
        if !supported_target(target) || !target_set.insert(target.as_str()) {
            return Err(GeneratorError::usage(format!(
                "`native-product` target `{target}` is unsupported or duplicated"
            )));
        }
    }
    for required in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
    ] {
        if !target_set.contains(required) {
            return Err(GeneratorError::usage(format!(
                "`native-product` targets must explicitly include `{required}`"
            )));
        }
    }
    let mut component_set = BTreeSet::new();
    let mut archive_binary = None;
    for component in components {
        if !component_set.insert(&component.name) {
            return Err(GeneratorError::usage(format!(
                "`native-product` has duplicate component `{}`",
                component.name
            )));
        }
        if component.name == archive_component {
            archive_binary = Some(component.binary.as_str());
        }
    }
    if archive_binary.is_none() {
        return Err(GeneratorError::usage(format!(
            "`native-product` archive_component `{archive_component}` is not in components"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
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
    components: &[Component],
) -> String {
    let archive_prefix = components
        .iter()
        .find(|component| component.name == archive_component)
        .map_or(archive_component, |component| component.binary.as_str());
    let mut matrix = String::new();
    for target in targets {
        let _ = writeln!(
            matrix,
            "          - target: {}\n            runner: {}",
            yaml_scalar(target),
            runner_for_target(ctx, target),
        );
    }

    let mut builds = String::from(
        "          SOURCE_COMMIT=\"$GITHUB_SHA\"\n          SOURCE_REF=\"$GITHUB_REF\"\n          if [ \"$CHANNEL\" = stable ]; then\n            RELEASE_TAG=\"$GITHUB_REF_NAME\"\n          else\n            RELEASE_TAG=\"preview-$GITHUB_SHA\"\n          fi\n          RELEASE_ID=\"$GITHUB_REPOSITORY/$RELEASE_TAG\"\n          export SOURCE_COMMIT SOURCE_REF RELEASE_TAG RELEASE_ID\n          if [ \"$CHANNEL\" = preview ]; then export VELNOR_PREVIEW_SOURCE_SHA=\"$GITHUB_SHA\"; fi\n          if ! command -v sha256sum >/dev/null 2>&1; then\n            sha256sum() { shasum -a 256 \"$@\"; }\n          fi\n",
    );
    for component in components {
        let package = shell_quote(&component.package);
        let binary = shell_quote(&component.binary);
        let feature = component
            .feature
            .as_deref()
            .map(|feature| format!(" --features {}", shell_quote(feature)))
            .unwrap_or_default();
        let identity_arg = match component.identity.as_str() {
            "revision" => "--revision",
            _ => "--version",
        };
        let identity_gate = if component.identity == "revision" {
            "          [ \"$identity\" = \"$SOURCE_COMMIT\" ] || { echo \"::error::component source revision differs from the checked-out source\" >&2; exit 1; }\n"
        } else {
            ""
        };
        let _ = write!(
            builds,
            "          package={package}\n          binary={binary}\n          cargo_features={feature:?}\n          identity_mode={identity:?}\n          component_version=\"$(jq -er --arg package \"$package\" '.packages[] | select(.name == $package) | .version' cargo-metadata.json)\"\n          test -n \"$component_version\" || {{ echo \"::error::typed component package is absent\" >&2; exit 1; }}\n          CARGO_INCREMENTAL=0 VELNOR_RELEASE_BUILD=1 cargo build --locked --release --no-default-features --package \"$package\" --bin \"$binary\"{feature} --target \"$TARGET\"\n          compiled=\"target/$TARGET/release/$binary\"\n          test -x \"$compiled\" || {{ echo \"::error::typed sibling binary is missing\" >&2; exit 1; }}\n          identity=\"$(\"$compiled\" {identity_arg} 2>&1 || true)\"\n          case \"$identity\" in *development*|*unknown*) echo \"::error::publishable component has development/unknown identity\" >&2; exit 1 ;; esac\n{identity_gate}          asset=\"$binary-$TARGET\"\n          cp \"$compiled\" \"dist/$TARGET/$asset\"\n          digest=\"$(sha256sum \"dist/$TARGET/$asset\" | awk '{{print $1}}')\"\n          size=\"$(stat -c%s \"dist/$TARGET/$asset\" 2>/dev/null || stat -f%z \"dist/$TARGET/$asset\")\"\n          jq -cn --arg name \"$asset\" --arg target \"$TARGET\" --arg sha256 \"$digest\" --argjson size \"$size\" '{{name:$name,target:$target,kind:\"binary\",sha256:$sha256,size:$size}}' >> \"dist/$TARGET/artifacts-$TARGET.jsonl\"\n          jq -cn --arg name {name} --arg crate \"$package\" --arg version \"$component_version\" --arg binary {binary} --arg target \"$TARGET\" '{{name:$name,crate:$crate,version:$version,binary:$binary,target:$target}}' >> \"dist/$TARGET/components-$TARGET.jsonl\"\n          install -Dm0755 \"$compiled\" \"dist/$TARGET/archive/bin/$binary\"\n",
            name = shell_quote(&component.name),
            identity = component.identity,
            identity_arg = identity_arg,
            identity_gate = identity_gate,
        );
    }
    // The source repository is an input to the product identity, never a
    // package lookup key.  Keep it visible in the workflow for auditability.
    let component_names = components
        .iter()
        .map(|component| shell_quote(&component.name).clone())
        .collect::<Vec<_>>()
        .join(" ");
    let expected_targets = targets
        .iter()
        .map(|target| shell_quote(target))
        .collect::<Vec<_>>()
        .join(" ");
    let expected_components = format!(
        "[{}]",
        components
            .iter()
            .map(|component| format!("\"{}\"", component.name))
            .collect::<Vec<_>>()
            .join(",")
    );
    let archive_members = components
        .iter()
        .map(|component| shell_quote(&component.binary))
        .collect::<Vec<_>>()
        .join(" ");
    let checkout = ctx.pins.checkout;
    let upload = ctx.pins.upload_artifact;
    let download = ctx.pins.download_artifact;
    let trigger = if channel == "stable" {
        "  push:\n    tags: [v*]".to_owned()
    } else {
        format!(
            "  push:\n    branches: [{}]",
            yaml_scalar(&ctx.config.default_branch)
        )
    };
    let product_id_q = shell_quote(product_id);
    let archive_prefix_q = shell_quote(archive_prefix);
    let archive_identity_schema_q = shell_quote(archive_identity_schema);
    let archive_manifest_schema_q = shell_quote(archive_manifest_schema);
    let source_repository_q = shell_quote(source_repository);
    let default_branch_q = shell_quote(&ctx.config.default_branch);
    let manifest_schema = shell_quote(manifest_schema);
    let channel_q = shell_quote(channel);
    let mut output = format!(
        "{GENERATED_HEADER}name: Native product\nrun-name: Native product · ${{{{ github.ref_name }}}}\n\non:\n{trigger}\n\nconcurrency:\n  group: native-product-${{{{ github.ref }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n  build:\n    name: Build product / ${{{{ matrix.target }}}}\n    runs-on: ${{{{ matrix.runner }}}}\n    timeout-minutes: 90\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    permissions:\n      contents: read\n    env:\n      TARGET: ${{{{ matrix.target }}}}\n      PRODUCT_ID: {product_id_q}\n      CHANNEL: {channel_q}\n      SOURCE_REPOSITORY: {source_repository_q}\n    steps:\n      - name: Checkout exact source\n        uses: {checkout}\n        with:\n          ref: ${{{{ github.sha }}}}\n          fetch-depth: 0\n          persist-credentials: false\n      - name: Prove exact source identity\n        run: |\n          set -euo pipefail\n          actual=\"$(git rev-parse HEAD)\"\n          case \"$actual\" in ''|*[!0-9a-f]*) echo '::error::checkout did not resolve to a lowercase 40-hex commit' >&2; exit 1 ;; esac\n          [ \"${{{{#actual}}}}\" -eq 40 ] && [ \"$actual\" = \"$GITHUB_SHA\" ] || {{ echo '::error::checkout source differs from the event source' >&2; exit 1; }}\n          test -z \"$(git status --porcelain)\" || {{ echo '::error::native product source tree is dirty' >&2; exit 1; }}\n      - name: Add native Rust target\n        run: rustup target add \"$TARGET\"\n      - name: Read Cargo component metadata\n        run: cargo metadata --locked --no-deps --format-version 1 > cargo-metadata.json\n      - name: Build and smoke-test typed sibling inventory\n        run: |\n          set -euo pipefail\n          if [ \"$CHANNEL\" = stable ]; then\n            VERSION=\"$(printf '%s' \"$GITHUB_REF_NAME\" | sed 's/^v//')\"\n          else\n            VERSION=\"0.0.0-preview.$GITHUB_RUN_NUMBER+$(printf '%s' \"$GITHUB_SHA\" | cut -c1-7)\"\n          fi\n          export VERSION\n          mkdir -p \"dist/$TARGET/archive/bin\"\n          : > \"dist/$TARGET/artifacts-$TARGET.jsonl\"\n          : > \"dist/$TARGET/components-$TARGET.jsonl\"\n          test \"$SOURCE_REPOSITORY\" = {source_repository_q}\n{builds}          tar -czf \"dist/$TARGET/$PRODUCT_ID-$VERSION-$TARGET.tar.gz\" -C \"dist/$TARGET/archive\" .\n          digest=\"$(sha256sum \"dist/$TARGET/$PRODUCT_ID-$VERSION-$TARGET.tar.gz\" | awk '{{print $1}}')\"\n          size=\"$(stat -c%s \"dist/$TARGET/$PRODUCT_ID-$VERSION-$TARGET.tar.gz\" 2>/dev/null || stat -f%z \"dist/$TARGET/$PRODUCT_ID-$VERSION-$TARGET.tar.gz\")\"\n          jq -cn --arg name \"$PRODUCT_ID-$VERSION-$TARGET.tar.gz\" --arg target \"$TARGET\" --arg sha256 \"$digest\" --argjson size \"$size\" '{{name:$name,target:$target,kind:\"archive\",sha256:$sha256,size:$size}}' >> \"dist/$TARGET/artifacts-$TARGET.jsonl\"\n      - name: Upload immutable product build\n        uses: {upload}\n        with:\n          name: native-product-${{{{ matrix.target }}}}\n          path: dist/${{{{ matrix.target }}}}\n          if-no-files-found: error\n          retention-days: 2\n\n  publish:\n    name: Assemble one application manifest\n    needs: build\n    runs-on: ubuntu-24.04\n    timeout-minutes: 30\n    permissions:\n      contents: write\n    env:\n      PRODUCT_ID: {product_id_q}\n      CHANNEL: {channel_q}\n      SOURCE_REPOSITORY: {source_repository_q}\n      MANIFEST_SCHEMA: {manifest_schema}\n    steps:\n      - name: Download immutable product builds\n        uses: {download}\n        with:\n          pattern: native-product-*\n          path: artifacts\n          merge-multiple: true\n      - name: Checkout exact source for publication identity\n        uses: {checkout}\n        with:\n          ref: ${{{{ github.sha }}}}\n          fetch-depth: 0\n          persist-credentials: false\n      - name: Assemble and verify application manifest\n        id: manifest\n        run: |\n          set -euo pipefail\n          actual=\"$(git rev-parse HEAD)\"\n          [ \"$actual\" = \"$GITHUB_SHA\" ] || {{ echo '::error::publication source differs from build source' >&2; exit 1; }}\n          SOURCE_COMMIT=\"$actual\"\n          SOURCE_REF=\"$GITHUB_REF\"\n          if [ \"$CHANNEL\" = stable ]; then\n            RELEASE_TAG=\"$GITHUB_REF_NAME\"\n            case \"$RELEASE_TAG\" in v[0-9]*) ;; *) echo '::error::stable product release requires a v<version> tag' >&2; exit 1 ;; esac\n            VERSION=\"${{{{RELEASE_TAG#v}}}}\"\n            RELEASE_ID=\"$GITHUB_REPOSITORY/$RELEASE_TAG\"\n          else\n            RELEASE_TAG=preview\n            VERSION=\"0.0.0-preview.${{{{GITHUB_RUN_NUMBER}}}}+${{{{SOURCE_COMMIT:0:7}}}}\"\n            RELEASE_ID=\"$GITHUB_REPOSITORY/preview/$SOURCE_COMMIT\"\n          fi\n          export SOURCE_COMMIT SOURCE_REF RELEASE_TAG VERSION RELEASE_ID\n          : > component-rows.jsonl\n          : > artifact-rows.jsonl\n          find artifacts -type f -name 'components-*.jsonl' -print0 | sort -z | xargs -0 -r cat > component-rows.jsonl\n          find artifacts -type f -name 'artifacts-*.jsonl' -print0 | sort -z | xargs -0 -r cat > artifact-rows.jsonl\n          test -s component-rows.jsonl && test -s artifact-rows.jsonl\n          expected_components={expected_components}\n          actual_components=\"$(jq -s 'map(.name) | unique | sort' component-rows.jsonl)\"\n          expected_components=\"$(jq -c 'sort' <<<\"$expected_components\")\"\n          [ \"$actual_components\" = \"$expected_components\" ] || {{ echo '::error::component inventory does not equal typed config' >&2; exit 1; }}\n          expected_targets=({expected_targets})\n          for target in \"${{{{expected_targets[@]}}}}\"; do\n            for component in {component_names}; do\n              count=\"$(jq -s --arg target \"$target\" --arg component \"$component\" '[.[] | select(.target == $target and .name == $component)] | length' component-rows.jsonl)\"\n              [ \"$count\" -eq 1 ] || {{ echo \"::error::missing typed component $component for $target\" >&2; exit 1; }}\n            done\n          done\n          jq -s 'group_by(.name) | map({{name: .[0].name, crate: .[0].crate, version: .[0].version, binary: .[0].binary, targets: (map(.target) | sort)}}) | sort_by(.name)' component-rows.jsonl > components.json\n          jq -s '.' artifact-rows.jsonl > artifacts.json\n          jq -S -n --arg schema \"$MANIFEST_SCHEMA\" --arg product_id \"$PRODUCT_ID\" --arg channel \"$CHANNEL\" --arg version \"$VERSION\" --arg source_repository \"$SOURCE_REPOSITORY\" --arg source_ref \"$SOURCE_REF\" --arg source_commit \"$SOURCE_COMMIT\" --arg release_tag \"$RELEASE_TAG\" --arg release_id \"$RELEASE_ID\" --slurpfile artifacts artifacts.json --slurpfile components components.json '{{schema:$schema,product_id:$product_id,channel:$channel,version:$version,source_repository:$source_repository,source_ref:$source_ref,source_commit:$source_commit,release_tag:$release_tag,release_id:$release_id,artifacts:$artifacts[0],components:$components[0]}}' > application-manifest.json\n          jq -e --arg schema \"$MANIFEST_SCHEMA\" --arg commit \"$SOURCE_COMMIT\" --arg version \"$VERSION\" '.schema == $schema and .source_commit == $commit and .version == $version and (.artifacts | length > 0) and (.components | length == {component_count}) and ([.artifacts[].name] | index(\"application-manifest.json\") | not)' application-manifest.json >/dev/null\n          sha256sum application-manifest.json > application-manifest.json.sha256\n          test \"$(awk '{{print $1}}' application-manifest.json.sha256)\" = \"$(sha256sum application-manifest.json | awk '{{print $1}}')\"\n      - name: Verify extracted native package contents\n        run: |\n          set -euo pipefail\n          tmp=\"$(mktemp -d)\"\n          trap 'rm -rf -- \"$tmp\"' EXIT\n          jq -r '.[] | select(.kind == \"archive\") | [.name,.target] | @tsv' artifacts.json | while IFS=$'\\t' read -r archive target; do\n            path=\"$(find artifacts -type f -name \"$archive\" -print -quit)\"\n            test -s \"$path\" || {{ echo \"::error::archive missing: $archive\" >&2; exit 1; }}\n            mkdir -p \"$tmp/$target\"\n            tar -xzf \"$path\" -C \"$tmp/$target\"\n            jq -r --arg target \"$target\" '.[] | select(.target == $target) | .binary' component-rows.jsonl | while IFS= read -r binary; do\n              test -x \"$tmp/$target/bin/$binary\" || {{ echo \"::error::required sibling $binary missing from $archive\" >&2; exit 1; }}\n            done\n          done\n      - name: Publish immutable application assets\n        if: ${{{{ env.CHANNEL == 'stable' }}}}\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          if gh release view \"$RELEASE_TAG\" --repo \"$GITHUB_REPOSITORY\" >/dev/null 2>&1; then\n            echo '::error::application release already exists; immutable producer refuses replacement' >&2\n            exit 1\n          fi\n          assets=()\n          while IFS= read -r name; do\n            path=\"$(find artifacts -type f -name \"$name\" -print -quit)\"\n            test -s \"$path\" || {{ echo \"::error::manifest asset missing: $name\" >&2; exit 1; }}\n            assets+=(\"$path\")\n          done < <(jq -r '.[] | select(.kind == \"archive\" or .kind == \"binary\") | .name' artifacts.json)\n          assets+=(application-manifest.json application-manifest.json.sha256)\n          gh release create \"$RELEASE_TAG\" \"${{{{assets[@]}}}}\" --verify-tag --target \"$SOURCE_COMMIT\" --title \"$RELEASE_TAG\" --generate-notes\n      - name: Upload immutable preview evidence\n        if: ${{{{ env.CHANNEL == 'preview' }}}}\n        uses: {upload}\n        with:\n          name: native-product-preview-${{{{ github.sha }}}}\n          path: |\n            application-manifest.json\n            application-manifest.json.sha256\n          if-no-files-found: error\n          retention-days: 7\n",
        matrix = matrix,
        builds = builds,
        component_count = components.len(),
    );
    // `format!` needs doubled braces for GitHub expressions.  These are
    // shell parameter expansions instead; normalize the generated bytes back
    // to one shell brace pair before the workflow is emitted.
    for (escaped, shell) in [
        ("${{#actual}}", "${#actual}"),
        ("${{RELEASE_TAG#v}}", "${RELEASE_TAG#v}"),
        ("${{GITHUB_RUN_NUMBER}}", "${GITHUB_RUN_NUMBER}"),
        ("${{SOURCE_COMMIT:0:7}}", "${SOURCE_COMMIT:0:7}"),
        ("${{expected_targets[@]}}", "${expected_targets[@]}"),
        ("${{assets[@]}}", "${assets[@]}"),
    ] {
        output = output.replace(escaped, shell);
    }
    output = output.replace(
        "            VERSION=\"${RELEASE_TAG#v}\"\n            RELEASE_ID=",
        "            VERSION=\"${RELEASE_TAG#v}\"\n            [[ \"$VERSION\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+$ ]] || { echo '::error::stable product version must be SemVer X.Y.Z' >&2; exit 1; }\n            RELEASE_ID=",
    );
    output = output.replace(
        "install -Dm0755 \"$compiled\" \"dist/$TARGET/archive/bin/$binary\"",
        "cp \"$compiled\" \"dist/$TARGET/archive/$binary\"\n          chmod 0755 \"dist/$TARGET/archive/$binary\"",
    );
    // Keep the shell producer's bytes identical to ApplicationManifest's
    // canonical struct order and normalized row ordering.
    output = output.replace("jq -S -n --arg schema", "jq -n --arg schema");
    output = output.replace(
        "jq -s '.' artifact-rows.jsonl > artifacts.json",
        "jq -s 'sort_by([.target,.kind,.name])' artifact-rows.jsonl > artifacts.json",
    );
    output = output.replace(
        &format!("      PRODUCT_ID: {product_id_q}\n      CHANNEL: {channel_q}"),
        &format!(
            "      PRODUCT_ID: {product_id_q}\n      ARCHIVE_PREFIX: {archive_prefix_q}\n      ARCHIVE_MANIFEST_SCHEMA: {archive_manifest_schema_q}\n      ARCHIVE_IDENTITY_SCHEMA: {archive_identity_schema_q}\n      DEFAULT_BRANCH: {default_branch_q}\n      CHANNEL: {channel_q}"
        ),
    );
    output = output.replace(
        "$PRODUCT_ID-$VERSION-$TARGET.tar.gz",
        "$ARCHIVE_PREFIX-$VERSION-$TARGET.tar.gz",
    );
    output = output.replace(
        "-C \"dist/$TARGET/archive\" .\n",
        &format!("-C \"dist/$TARGET/archive\" identity.json manifest.json {archive_members}\n"),
    );
    output = output.replace(
        "jq -cn --arg name \"$ARCHIVE_PREFIX-$VERSION-$TARGET.tar.gz\" --arg target \"$TARGET\" --arg sha256 \"$digest\" --argjson size \"$size\" '{{name:$name,target:$target,kind:\"archive\",sha256:$sha256,size:$size}}'",
        "jq -cn --arg name \"$ARCHIVE_PREFIX-$VERSION-$TARGET.tar.gz\" --arg target \"$TARGET\" --arg kind \"$archive_kind\" --arg sha256 \"$digest\" --argjson size \"$size\" '{{name:$name,target:$target,kind:$kind,sha256:$sha256,size:$size}}'",
    );
    output = output.replace("application-manifest.json", PRODUCT_MANIFEST_FILE);
    output = output.replace(
        "            RELEASE_TAG=preview\n",
        "            RELEASE_TAG=\"preview-$SOURCE_COMMIT\"\n",
    );
    output = output.replace(
        "            RELEASE_ID=\"$GITHUB_REPOSITORY/preview/$SOURCE_COMMIT\"",
        "            RELEASE_ID=\"$GITHUB_REPOSITORY/preview-$SOURCE_COMMIT\"",
    );
    output = output.replace(
        "\"$GITHUB_REPOSITORY/$RELEASE_TAG\"",
        "\"$PRODUCT_ID:$RELEASE_TAG:$SOURCE_COMMIT\"",
    );
    output = output.replace(
        "\"$GITHUB_REPOSITORY/preview-$SOURCE_COMMIT\"",
        "\"$PRODUCT_ID:$RELEASE_TAG:$SOURCE_COMMIT\"",
    );
    output = output.replace(
        "            product-manifest.json\n            product-manifest.json.sha256\n          if-no-files-found",
        "            product-manifest.json\n            product-manifest.json.sha256\n            artifacts\n          if-no-files-found",
    );
    let archive_metadata = r#"          archive_kind="archive"
          case "$TARGET" in *-apple-darwin) archive_kind="homebrew-archive" ;; esac
          archive_components="$(jq -s --arg target "$TARGET" --arg version "$VERSION" --arg commit "$SOURCE_COMMIT" --argjson artifacts "$(jq -s '.' "dist/$TARGET/artifacts-$TARGET.jsonl")" '
            [$artifacts[] as $artifact |
             .[] | select(.target == $target) |
             select($artifact.target == $target and $artifact.kind == "binary" and ($artifact.name == .binary or $artifact.name == (.binary + "-" + $target))) |
             {name: .name, crate: .crate, crate_version: .version, release_version: $version, source_commit: $commit, binary_sha256: $artifact.sha256}]
            | sort_by(.name)' "dist/$TARGET/components-$TARGET.jsonl")"
          jq -S -n --arg schema "$ARCHIVE_MANIFEST_SCHEMA" --arg product_id "$PRODUCT_ID" --arg channel "$CHANNEL" --arg version "$VERSION" --arg source_repository "$SOURCE_REPOSITORY" --arg source_ref "$SOURCE_REF" --arg source_commit "$SOURCE_COMMIT" --arg release_tag "$RELEASE_TAG" --arg parent_manifest_id "$RELEASE_ID" --argjson components "$archive_components" '{schema:$schema,product_id:$product_id,channel:$channel,version:$version,source_repository:$source_repository,source_ref:$source_ref,source_commit:$source_commit,release_tag:$release_tag,parent_manifest_id:$parent_manifest_id,components:$components}' > "dist/$TARGET/archive/manifest.json"
          jq -S -n --arg schema "$ARCHIVE_IDENTITY_SCHEMA" --arg product_id "$PRODUCT_ID" --arg channel "$CHANNEL" --arg version "$VERSION" --arg source_repository "$SOURCE_REPOSITORY" --arg source_ref "$SOURCE_REF" --arg source_commit "$SOURCE_COMMIT" --arg release_tag "$RELEASE_TAG" --arg parent_manifest_id "$RELEASE_ID" '{schema:$schema,product_id:$product_id,channel:$channel,version:$version,source_repository:$source_repository,source_ref:$source_ref,source_commit:$source_commit,release_tag:$release_tag,parent_manifest_id:$parent_manifest_id}' > "dist/$TARGET/archive/identity.json"
"#;
    output = output.replace(
        "          tar -czf \"dist/$TARGET/$ARCHIVE_PREFIX-$VERSION-$TARGET.tar.gz\"",
        &format!(
            "{archive_metadata}          tar -czf \"dist/$TARGET/$ARCHIVE_PREFIX-$VERSION-$TARGET.tar.gz\""
        ),
    );
    output = output.replace(
        "jq -r '.[] | select(.kind == \"archive\") | [.name,.target] | @tsv' artifacts.json",
        "jq -r '.[] | select(.kind == \"archive\" or .kind == \"homebrew-archive\") | [.name,.target] | @tsv' artifacts.json",
    );
    output = output.replace("\"$tmp/$target/bin/$binary\"", "\"$tmp/$target/$binary\"");
    output = output.replace(
        "mkdir -p \"dist/$TARGET/archive/bin\"",
        "mkdir -p \"dist/$TARGET/archive\"",
    );
    let verification = r#"      - name: Verify manifest artifact bytes and component identity
        run: |
          set -euo pipefail
          hash_file() {
            if command -v sha256sum >/dev/null 2>&1; then
              sha256sum "$1" | awk '{print $1}'
            else
              shasum -a 256 "$1" | awk '{print $1}'
            fi
          }
          manifest_version="$(jq -er '.version' application-manifest.json)"
          manifest_source_ref="$(jq -er '.source_ref' application-manifest.json)"
          manifest_commit="$(jq -er '.source_commit' application-manifest.json)"
          manifest_tag="$(jq -er '.release_tag' application-manifest.json)"
          manifest_release_id="$(jq -er '.release_id' application-manifest.json)"
          if [ "$CHANNEL" = preview ]; then
            [ "$manifest_source_ref" = "refs/heads/$DEFAULT_BRANCH" ] || { echo '::error::preview product is not bound to the configured default branch' >&2; exit 1; }
            [ "$manifest_tag" = "preview-$manifest_commit" ] || { echo '::error::preview product tag is not bound to its source commit' >&2; exit 1; }
          else
            [ "$manifest_source_ref" = "refs/tags/$manifest_tag" ] || { echo '::error::stable product ref/tag mismatch' >&2; exit 1; }
          fi
          jq -e --arg schema "$MANIFEST_SCHEMA" --arg product "$PRODUCT_ID" --arg channel "$CHANNEL" --arg version "$manifest_version" --arg repository "$SOURCE_REPOSITORY" --arg source_ref "$manifest_source_ref" --arg commit "$manifest_commit" --arg tag "$manifest_tag" --arg release_id "$manifest_release_id" '.schema == $schema and .product_id == $product and .channel == $channel and .version == $version and .source_repository == $repository and .source_ref == $source_ref and .source_commit == $commit and .release_tag == $tag and .release_id == $release_id' application-manifest.json >/dev/null
          while IFS= read -r row; do
            name="$(jq -er '.name' <<<"$row")"
            expected_sha="$(jq -er '.sha256' <<<"$row")"
            expected_size="$(jq -er '.size' <<<"$row")"
            path="$(find artifacts -type f -name "$name" -print -quit)"
            test -f "$path" || { echo "::error::manifest artifact is missing: $name" >&2; exit 1; }
            [ "$(hash_file "$path")" = "$expected_sha" ] || { echo "::error::manifest artifact digest mismatch: $name" >&2; exit 1; }
            actual_size="$(wc -c <"$path" | tr -d '[:space:]')"
            [ "$actual_size" = "$expected_size" ] || { echo "::error::manifest artifact size mismatch: $name" >&2; exit 1; }
          done < <(jq -c '.[]' artifacts.json)
          while IFS= read -r row; do
            component="$(jq -er '.name' <<<"$row")"
            target="$(jq -er '.target' <<<"$row")"
            binary="$(jq -er '.binary' <<<"$row")"
            identity_count="$(jq -s --arg component "$component" '[.[] | select(.name == $component) | {crate,version,binary}] | unique | length' component-rows.jsonl)"
            [ "$identity_count" -eq 1 ] || { echo "::error::component identity differs across targets: $component" >&2; exit 1; }
            binary_count="$(jq -s --arg target "$target" --arg binary "$binary" '[.[] | select(.kind == "binary" and .target == $target and (.name == $binary or .name == ($binary + "-" + $target)))] | length' artifacts.json)
            [ "$binary_count" -eq 1 ] || { echo "::error::component binary artifact is missing: $component/$target" >&2; exit 1; }
          done < component-rows.jsonl
"#;
    output = output.replace(
        "      - name: Verify extracted native package contents",
        &format!("{verification}      - name: Verify extracted native package contents"),
    );
    output = output.replace("application-manifest.json", PRODUCT_MANIFEST_FILE);
    // Keep the generator-owned source URL and schema visible for policy
    // reviews without turning them into a hard-coded product identity.
    let _ = writeln!(output, "\n# Native product source: {source_repository}");
    output
}

fn runner_for_target(ctx: &RenderCtx<'_>, target: &str) -> String {
    if target == "aarch64-apple-darwin" {
        yaml_scalar(MACOS_HOSTED_RUNS_ON)
    } else if target == "x86_64-apple-darwin" {
        // Preserve the source formula's Intel product lane. This label is
        // GitHub's hosted Intel macOS runner; the build still proves the
        // target by compiling and extracting the exact sibling archive.
        yaml_scalar("macos-15-intel")
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

    #[test]
    fn contract_requires_linux_pair_and_both_native_mac_targets() {
        let error = validate_product_contract(
            "example",
            "stable",
            "owner/example",
            "runner",
            "velnor.homebrew-install-identity/v1",
            "velnor.homebrew-install/v1",
            "velnor.product-manifest/v1",
            &["aarch64-apple-darwin".to_owned()],
            &[Component {
                name: "runner".to_owned(),
                package: "runner".to_owned(),
                binary: "runner".to_owned(),
                feature: None,
                identity: "version".to_owned(),
            }],
        )
        .expect_err("incomplete product target set must fail");
        assert!(error.to_string().contains("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn components_are_typed_and_feature_limited() {
        let parsed = parse_components(Some(BTreeMap::from([(
            "runner".to_owned(),
            vec![
                "runner".to_owned(),
                "runner".to_owned(),
                "release-build".to_owned(),
            ],
        )])))
        .expect("typed component");
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
        assert!(parse_components(Some(BTreeMap::from([(
            "runner".to_owned(),
            vec![
                "runner".to_owned(),
                "runner".to_owned(),
                "".to_owned(),
                "unknown".to_owned(),
            ],
        )])))
        .is_err());
    }
}
