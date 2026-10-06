//! Verified cross-job transport for named build products.
//!
//! GitHub-hosted jobs run on ephemeral runners: a producer's outputs vanish
//! with its job, so a consumer in another job cannot read them from the
//! workspace. Transport closes that gap with a declared artifact edge. The
//! `stage-product` runtime command copies the producer's declared outputs
//! into a staging directory with a digest manifest; the caller uploads that
//! directory as one artifact. The consumer's caller waits on the producer's
//! caller, downloads the artifact, and `verify-product` checks the manifest
//! against the plan's expected identity before installing anything.
//!
//! Verification is strict and local: the manifest must name the expected
//! schema, producer, product, and inputs digest; every staged file's SHA-256
//! must match; every declared structural file must be present; every
//! installed path must sit under a declared output root. A mismatch fails
//! the consumer — a corrupt or tampered artifact is never a hit.
//! Provenance is the same run and commit on both ends, so an empty inputs
//! digest (an incomplete closure) still transports safely within the run;
//! cross-run reuse stays disabled until the exact-product cache can bind the
//! stronger identity.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::s2::platform::{valid_env_name, valid_product_output, NamedProduct};
use crate::s2::{shell_quote, GeneratorError};

/// The manifest schema the producer writes and the consumer requires.
pub(crate) const MANIFEST_SCHEMA: &str = "velnor-product-manifest/1";
/// The manifest filename inside the staged artifact directory.
pub(crate) const MANIFEST_FILE: &str = "velnor-product-manifest.json";
/// The artifact-internal directory holding the copied output trees.
pub(crate) const STAGED_OUTPUTS_DIR: &str = "outputs";
/// Newline-separated declared output roots for both subcommands.
pub(crate) const OUTPUTS_ENV: &str = "VELNOR_TRANSPORT_OUTPUTS";
/// Newline-separated declared structural files for `verify-product`.
pub(crate) const OUTPUT_FILES_ENV: &str = "VELNOR_TRANSPORT_OUTPUT_FILES";
/// Same-run artifacts live only for the consuming jobs.
const ARTIFACT_RETENTION_DAYS: u32 = 1;

/// Whether a product can ride the transport: it declares at least one
/// output root. Inputs identity strengthens verification but is not
/// required — same-run same-commit provenance already binds the bytes.
#[must_use]
pub(crate) fn transport_eligible(product: &NamedProduct) -> bool {
    !product.outputs.is_empty()
}

/// The workflow input record identifying one transported edge.
#[must_use]
pub(crate) fn transport_record(producer: &str, product: &str) -> String {
    format!("{producer}/{product}")
}

/// The Actions artifact name for one edge. Sanitized to the artifact-name
/// alphabet; each side is truncated so the name stays well under the
/// service limit while remaining human-readable.
#[must_use]
pub(crate) fn artifact_name(producer: &str, product: &str) -> String {
    fn side(value: &str) -> String {
        value
            .chars()
            .take(80)
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                    character
                } else {
                    '-'
                }
            })
            .collect()
    }
    format!("velnor-product-{}--{}", side(producer), side(product))
}

/// The caller-side readiness verdicts, one `{record}:{verdict}` pair per
/// transportable prerequisite edge. The reusable callee cannot read the
/// caller's `needs` context, so the caller evaluates each producer job's
/// result inline and passes the verdicts as one input; the callee gates
/// each record's download on its own `{record}:true` membership. Edges
/// with no transport on this provider are absent: their consumer always
/// rebuilds. `None` when no edge can ride the transport.
#[must_use]
pub(crate) fn ready_records(edges: &[(String, String)]) -> Option<String> {
    if edges.is_empty() {
        return None;
    }
    let mut sorted = edges.to_vec();
    sorted.sort();
    let records = sorted
        .into_iter()
        .map(|(record, job)| format!("{record}:${{{{ needs.{job}.result == 'success' }}}}"))
        .collect::<Vec<_>>()
        .join(",");
    Some(records)
}

/// One staged symlink: its target plus whether the target was a directory,
/// so Windows recreation can pick the file or directory variant.
#[derive(Debug, Deserialize, Serialize)]
struct LinkEntry {
    target: String,
    dir: bool,
}

/// The digest manifest binding one staged artifact to its product.
#[derive(Debug, Deserialize, Serialize)]
struct ProductManifest {
    schema: String,
    producer: String,
    product: String,
    inputs_digest: String,
    files: BTreeMap<String, String>,
    links: BTreeMap<String, LinkEntry>,
    dirs: Vec<String>,
}

/// The `stage-product` inputs: the plan facts plus the staging directory.
pub(crate) struct StageRequest {
    pub(crate) producer: String,
    pub(crate) product: String,
    pub(crate) inputs_digest: String,
    pub(crate) outputs: Vec<String>,
    pub(crate) stage: PathBuf,
}

/// The `verify-product` inputs: the expected plan identity, the install
/// containment roots, the staging directory, the marker to export, and the
/// `GITHUB_ENV` file receiving it.
pub(crate) struct VerifyRequest {
    pub(crate) producer: String,
    pub(crate) product: String,
    pub(crate) inputs_digest: String,
    pub(crate) outputs: Vec<String>,
    pub(crate) output_files: Vec<String>,
    pub(crate) stage: PathBuf,
    pub(crate) marker: String,
    pub(crate) env_file: PathBuf,
}

fn sha256_file(path: &Path) -> Result<String, GeneratorError> {
    let mut file = fs::File::open(path)
        .map_err(|error| GeneratorError::io("digest product file", path, &error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 16].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| GeneratorError::io("digest product file", path, &error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut output = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

/// Copy a regular file preserving its permission bits.
fn copy_file(source: &Path, dest: &Path) -> Result<(), GeneratorError> {
    let permissions = fs::symlink_metadata(source)
        .map_err(|error| GeneratorError::io("inspect product file", source, &error))?
        .permissions();
    fs::copy(source, dest)
        .map_err(|error| GeneratorError::io("copy product file", dest, &error))?;
    fs::set_permissions(dest, permissions)
        .map_err(|error| GeneratorError::io("copy product file", dest, &error))?;
    Ok(())
}

fn create_link(target: &Path, link: &Path, dir: bool) -> Result<(), GeneratorError> {
    #[cfg(unix)]
    {
        let _ = dir;
        std::os::unix::fs::symlink(target, link)
            .map_err(|error| GeneratorError::io("create product link", link, &error))
    }
    #[cfg(windows)]
    {
        if dir {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
        .map_err(|error| GeneratorError::io("create product link", link, &error))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link, dir);
        Err(GeneratorError::usage(
            "product links need a unix or windows target",
        ))
    }
}

/// Copy one output root into the stage without following symlinks.
fn copy_tree(source: &Path, dest: &Path) -> Result<(), GeneratorError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| GeneratorError::io("inspect product output", source, &error))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)
            .map_err(|error| GeneratorError::io("inspect product output", source, &error))?;
        create_link(&target, dest, false)?;
        return Ok(());
    }
    if !metadata.is_dir() {
        copy_file(source, dest)?;
        return Ok(());
    }
    fs::create_dir_all(dest)
        .map_err(|error| GeneratorError::io("stage product output", dest, &error))?;
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(source)
        .map_err(|error| GeneratorError::io("stage product output", source, &error))?
        .collect::<Result<_, _>>()
        .map_err(|error| GeneratorError::io("stage product output", source, &error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        copy_tree(&entry.path(), &dest.join(entry.file_name()))?;
    }
    fs::set_permissions(dest, metadata.permissions())
        .map_err(|error| GeneratorError::io("stage product output", dest, &error))?;
    Ok(())
}

/// Join a repo-relative manifest path, refusing escapes even from a
/// corrupt artifact: every segment must be normal-form.
fn manifest_rel(value: &str) -> Result<String, GeneratorError> {
    if !valid_product_output(value) {
        return Err(GeneratorError::usage(format!(
            "product manifest lists `{value}`, which is not a repo-relative path in normal form"
        )));
    }
    Ok(value.to_owned())
}

/// Walk the staged outputs without following symlinks, recording regular
/// files with digests, links with targets, and directories for recreation.
/// Parent directories above the declared roots are skipped: the installer
/// recreates them implicitly, and recording them would fail the
/// under-roots containment check on verify.
fn collect_staged(
    dest: &Path,
    dir: &Path,
    outputs: &[String],
    files: &mut BTreeMap<String, String>,
    links: &mut BTreeMap<String, LinkEntry>,
    dirs: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)
        .map_err(|error| GeneratorError::io("digest staged product", dir, &error))?
        .collect::<Result<_, _>>()
        .map_err(|error| GeneratorError::io("digest staged product", dir, &error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(dest)
            .map_err(|_| {
                GeneratorError::usage(format!(
                    "staged product path escapes its directory: {}",
                    path.display()
                ))
            })?
            .components()
            .map(|component| component.as_os_str().to_str().ok_or(()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|()| {
                GeneratorError::usage(format!(
                    "staged product path is not UTF-8: {}",
                    path.display()
                ))
            })?
            .join("/");
        let rel = manifest_rel(&rel)?;
        let file_type = entry
            .file_type()
            .map_err(|error| GeneratorError::io("digest staged product", &path, &error))?;
        if file_type.is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|error| GeneratorError::io("digest staged product", &path, &error))?;
            let dir = path.is_dir();
            let target = target.to_str().ok_or_else(|| {
                GeneratorError::usage(format!(
                    "staged product link target is not UTF-8: {}",
                    path.display()
                ))
            })?;
            links.insert(
                rel,
                LinkEntry {
                    target: target.to_owned(),
                    dir,
                },
            );
        } else if file_type.is_dir() {
            if under_outputs(&rel, outputs) {
                dirs.push(rel);
            }
            collect_staged(dest, &path, outputs, files, links, dirs)?;
        } else {
            files.insert(rel, sha256_file(&path)?);
        }
    }
    Ok(())
}

/// Stage one product: copy the declared outputs, digest every staged file,
/// and write the manifest. Returns the staged regular-file count.
pub(crate) fn stage_product(root: &Path, request: &StageRequest) -> Result<usize, GeneratorError> {
    if request.outputs.is_empty() {
        return Err(GeneratorError::usage(
            "stage-product needs at least one output; refusing to stage an empty product",
        ));
    }
    for output in &request.outputs {
        manifest_rel(output)?;
    }
    if request.producer.is_empty() || request.product.is_empty() {
        return Err(GeneratorError::usage(
            "stage-product needs a non-empty --producer and --product",
        ));
    }
    let stage = root.join(&request.stage);
    if fs::symlink_metadata(&stage).is_ok() {
        fs::remove_dir_all(&stage)
            .map_err(|error| GeneratorError::io("clear product stage", &stage, &error))?;
    }
    let dest = stage.join(STAGED_OUTPUTS_DIR);
    fs::create_dir_all(&dest)
        .map_err(|error| GeneratorError::io("create product stage", &dest, &error))?;
    for output in &request.outputs {
        let source = root.join(output);
        if fs::symlink_metadata(&source).is_err() {
            return Err(GeneratorError::usage(format!(
                "product output missing: {output}"
            )));
        }
        let target = dest.join(output);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| GeneratorError::io("stage product output", &target, &error))?;
        }
        copy_tree(&source, &target)?;
    }
    let mut files = BTreeMap::new();
    let mut links = BTreeMap::new();
    let mut dirs = Vec::new();
    collect_staged(
        &dest,
        &dest,
        &request.outputs,
        &mut files,
        &mut links,
        &mut dirs,
    )?;
    dirs.sort();
    let manifest = ProductManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        producer: request.producer.clone(),
        product: request.product.clone(),
        inputs_digest: request.inputs_digest.clone(),
        files,
        links,
        dirs,
    };
    let count = manifest.files.len();
    let rendered = serde_json::to_string_pretty(&manifest)
        .map_err(|error| GeneratorError::usage(format!("render product manifest: {error}")))?;
    fs::write(stage.join(MANIFEST_FILE), format!("{rendered}\n"))
        .map_err(|error| GeneratorError::io("write product manifest", &stage, &error))?;
    Ok(count)
}

/// Remove one install target regardless of kind; missing targets are fine.
fn clear_target(target: &Path) -> Result<(), GeneratorError> {
    let Ok(metadata) = fs::symlink_metadata(target) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(target)
            .map_err(|error| GeneratorError::io("clear product install target", target, &error))?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(target)
            .map_err(|error| GeneratorError::io("clear product install target", target, &error))?;
    } else {
        return Err(GeneratorError::usage(format!(
            "product install target is neither a file, link, nor directory: {}",
            target.display()
        )));
    }
    Ok(())
}

/// Whether a manifest path sits under one of the declared output roots.
fn under_outputs(rel: &str, outputs: &[String]) -> bool {
    outputs
        .iter()
        .any(|root| rel == root || rel.starts_with(&format!("{root}/")))
}

fn link_target_safe(target: &str) -> bool {
    if target.is_empty() {
        return false;
    }
    let path = Path::new(target);
    if path.is_absolute() {
        return false;
    }
    !target.split('/').any(|segment| segment == "..")
}
/// Verify one downloaded artifact and install it: check the manifest
/// identity, digest every staged file, require the structural files, and
/// install only under the declared output roots. Returns the installed
/// regular-file count; nothing installs unless every check passes. The
/// ready marker lands in the env file last.
pub(crate) fn verify_product(
    root: &Path,
    request: &VerifyRequest,
) -> Result<usize, GeneratorError> {
    if !valid_env_name(&request.marker) {
        return Err(GeneratorError::usage(format!(
            "verify-product needs a valid --marker env name, got `{}`",
            request.marker
        )));
    }
    for output in &request.outputs {
        manifest_rel(output)?;
    }
    let stage = root.join(&request.stage);
    let manifest = load_manifest(&stage)?;
    check_manifest_identity(&manifest, request)?;
    check_manifest_paths(&manifest, request)?;
    let dest = stage.join(STAGED_OUTPUTS_DIR);
    verify_staged_contents(&manifest, &dest, request)?;
    install_verified_product(&manifest, root, &dest)?;
    export_marker(request)?;
    Ok(manifest.files.len())
}

/// Read and parse the manifest from a downloaded artifact directory.
fn load_manifest(stage: &Path) -> Result<ProductManifest, GeneratorError> {
    let manifest_path = stage.join(MANIFEST_FILE);
    let raw = fs::read_to_string(&manifest_path).map_err(|_| {
        GeneratorError::usage("product manifest missing from downloaded artifact".to_owned())
    })?;
    serde_json::from_str(&raw).map_err(|error| {
        GeneratorError::usage(format!("product manifest is not valid JSON: {error}"))
    })
}

/// The manifest must name the expected schema, producer, product, and
/// inputs digest, and it must list at least one file.
fn check_manifest_identity(
    manifest: &ProductManifest,
    request: &VerifyRequest,
) -> Result<(), GeneratorError> {
    for (field, got, want) in [
        ("schema", manifest.schema.as_str(), MANIFEST_SCHEMA),
        (
            "producer",
            manifest.producer.as_str(),
            request.producer.as_str(),
        ),
        (
            "product",
            manifest.product.as_str(),
            request.product.as_str(),
        ),
        (
            "inputs_digest",
            manifest.inputs_digest.as_str(),
            request.inputs_digest.as_str(),
        ),
    ] {
        if got != want {
            return Err(GeneratorError::usage(format!(
                "product manifest {field} mismatch: {got:?} != {want:?}"
            )));
        }
    }
    if manifest.files.is_empty() {
        return Err(GeneratorError::usage(
            "product manifest lists no files".to_owned(),
        ));
    }
    Ok(())
}

/// Every manifest path must be normal-form and sit under a declared
/// output root: nothing installs outside the plan's containment.
fn check_manifest_paths(
    manifest: &ProductManifest,
    request: &VerifyRequest,
) -> Result<(), GeneratorError> {
    for rel in manifest.files.keys().chain(manifest.links.keys()) {
        manifest_rel(rel)?;
        if !under_outputs(rel, &request.outputs) {
            return Err(GeneratorError::usage(format!(
                "product manifest path `{rel}` escapes the declared output roots"
            )));
        }
    }
    for dir in &manifest.dirs {
        manifest_rel(dir)?;
        if !under_outputs(dir, &request.outputs) {
            return Err(GeneratorError::usage(format!(
                "product manifest path `{dir}` escapes the declared output roots"
            )));
        }
    }
    Ok(())
}

/// Digest every staged file against the manifest, confirm every staged
/// link, and require every declared structural file.
fn verify_staged_contents(
    manifest: &ProductManifest,
    dest: &Path,
    request: &VerifyRequest,
) -> Result<(), GeneratorError> {
    for (rel, digest) in &manifest.files {
        let path = dest.join(rel);
        let metadata = fs::symlink_metadata(&path).map_err(|_| {
            GeneratorError::usage(format!("product file missing from artifact: {rel}"))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(GeneratorError::usage(format!(
                "product file missing from artifact: {rel}"
            )));
        }
        if sha256_file(&path)? != *digest {
            return Err(GeneratorError::usage(format!(
                "product file digest mismatch: {rel}"
            )));
        }
    }
    for (rel, link) in &manifest.links {
        let path = dest.join(rel);
        let target = fs::read_link(&path).map_err(|_| {
            GeneratorError::usage(format!("product link missing from artifact: {rel}"))
        })?;
        if target.to_str() != Some(link.target.as_str()) {
            return Err(GeneratorError::usage(format!(
                "product link mismatch in artifact: {rel}"
            )));
        }
        if !link_target_safe(&link.target) {
            return Err(GeneratorError::usage(format!(
                "product link target escapes the product tree: {rel}"
            )));
        }
    }
    for want in &request.output_files {
        manifest_rel(want)?;
        if !manifest.files.contains_key(want) && !manifest.links.contains_key(want) {
            return Err(GeneratorError::usage(format!(
                "product missing declared structural file: {want}"
            )));
        }
    }
    Ok(())
}

/// Install a fully verified manifest: directories first, then links,
/// then regular files. Every target was containment-checked already.
fn install_verified_product(
    manifest: &ProductManifest,
    root: &Path,
    dest: &Path,
) -> Result<(), GeneratorError> {
    let mut dirs = manifest.dirs.clone();
    dirs.sort();
    for rel in &dirs {
        let target = root.join(rel);
        clear_target(&target)?;
        fs::create_dir_all(&target)
            .map_err(|error| GeneratorError::io("install product directory", &target, &error))?;
    }
    for (rel, link) in &manifest.links {
        let target = root.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| GeneratorError::io("install product link", &target, &error))?;
        }
        clear_target(&target)?;
        create_link(Path::new(&link.target), &target, link.dir)?;
    }
    for rel in manifest.files.keys() {
        let target = root.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| GeneratorError::io("install product file", &target, &error))?;
        }
        clear_target(&target)?;
        copy_file(&dest.join(rel), &target)?;
    }
    Ok(())
}

/// Export the ready marker last: only a fully installed product sets it.
/// The env file is shared with every other step's exports: append.
fn export_marker(request: &VerifyRequest) -> Result<(), GeneratorError> {
    let mut env = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&request.env_file)
        .map_err(|error| GeneratorError::io("export product marker", &request.env_file, &error))?;
    env.write_all(format!("{}=1\n", request.marker).as_bytes())
        .map_err(|error| GeneratorError::io("export product marker", &request.env_file, &error))?;
    Ok(())
}

/// Split a newline-separated step env list, dropping blank lines.
fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Run `stage-product` from the runtime CLI: `root` is the repository
/// checkout, the outputs arrive via [`OUTPUTS_ENV`].
pub(crate) fn stage_product_cli(root: &Path, arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options =
        crate::s2::runtime::parse_options(arguments, &["producer", "product", "digest", "stage"])?;
    let missing = ["producer", "product", "stage"]
        .into_iter()
        .find(|name| !options.contains_key(*name));
    if let Some(name) = missing {
        return Err(GeneratorError::usage(format!(
            "stage-product needs --{name}"
        )));
    }
    let request = StageRequest {
        producer: options["producer"].clone(),
        product: options["product"].clone(),
        inputs_digest: options.get("digest").cloned().unwrap_or_default(),
        outputs: env_list(OUTPUTS_ENV),
        stage: PathBuf::from(&options["stage"]),
    };
    let count = stage_product(root, &request)?;
    println!(
        "velnor: staged {count} product files for {}/{}",
        request.producer, request.product
    );
    Ok(())
}

/// Run `verify-product` from the runtime CLI: `root` is the repository
/// checkout, the containment roots arrive via [`OUTPUTS_ENV`], the
/// structural files via [`OUTPUT_FILES_ENV`], and the marker lands in
/// `GITHUB_ENV`.
pub(crate) fn verify_product_cli(
    root: &Path,
    arguments: &[OsString],
) -> Result<(), GeneratorError> {
    let options = crate::s2::runtime::parse_options(
        arguments,
        &["producer", "product", "digest", "stage", "marker"],
    )?;
    let missing = ["producer", "product", "stage", "marker"]
        .into_iter()
        .find(|name| !options.contains_key(*name));
    if let Some(name) = missing {
        return Err(GeneratorError::usage(format!(
            "verify-product needs --{name}"
        )));
    }
    let env_file = std::env::var("GITHUB_ENV").map_err(|_| {
        GeneratorError::usage("verify-product needs GITHUB_ENV; it runs in a GitHub Actions step")
    })?;
    let request = VerifyRequest {
        producer: options["producer"].clone(),
        product: options["product"].clone(),
        inputs_digest: options.get("digest").cloned().unwrap_or_default(),
        outputs: env_list(OUTPUTS_ENV),
        output_files: env_list(OUTPUT_FILES_ENV),
        stage: PathBuf::from(&options["stage"]),
        marker: options["marker"].clone(),
        env_file: PathBuf::from(env_file),
    };
    let count = verify_product(root, &request)?;
    println!(
        "velnor: verified and installed {count} product files for {}/{}",
        request.producer, request.product
    );
    Ok(())
}

/// The producer block for one product: stage the declared outputs with the
/// runtime, then upload the staging directory as one artifact.
/// Single-directory upload keeps the artifact layout deterministic —
/// multi-path uploads would re-anchor on a least common ancestor.
#[must_use]
pub(crate) fn render_producer_block(
    upload_artifact_pin: &str,
    producer: &str,
    product: &NamedProduct,
) -> String {
    let artifact = artifact_name(producer, &product.name);
    let digest = product.inputs_digest.clone().unwrap_or_default();
    let mut command = format!(
        "velnor-workflow stage-product --producer {} --product {}",
        shell_quote(producer),
        shell_quote(&product.name),
    );
    if !digest.is_empty() {
        let _ = write!(command, " --digest {}", shell_quote(&digest));
    }
    let _ = write!(
        command,
        " --stage \"$RUNNER_TEMP/velnor-products/{artifact}\""
    );
    format!(
        "      - name: Stage product {artifact}\n        env:\n          {OUTPUTS_ENV}: |\n{}\n        run: {command}\n      - name: Upload product {artifact}\n        uses: {upload_artifact_pin}\n        with:\n          name: {artifact}\n          path: ${{{{ runner.temp }}}}/velnor-products/{artifact}\n          if-no-files-found: error\n          retention-days: {ARTIFACT_RETENTION_DAYS}\n",
        indent_block(&product.outputs.join("\n"), "            "),
    )
}

/// The consumer block for one edge: download the artifact, then verify the
/// manifest and install the outputs. The verify step exports the ready
/// marker, which the consumer's guarded rebuild reads.
#[must_use]
pub(crate) fn render_consumer_block(
    download_artifact_pin: &str,
    producer: &str,
    product: &NamedProduct,
    marker: &str,
) -> String {
    let artifact = artifact_name(producer, &product.name);
    let digest = product.inputs_digest.clone().unwrap_or_default();
    let mut command = format!(
        "velnor-workflow verify-product --producer {} --product {}",
        shell_quote(producer),
        shell_quote(&product.name),
    );
    if !digest.is_empty() {
        let _ = write!(command, " --digest {}", shell_quote(&digest));
    }
    let _ = write!(
        command,
        " --stage \"$RUNNER_TEMP/velnor-products/{artifact}\" --marker {marker}"
    );
    let outputs_value = format!(
        "          {OUTPUTS_ENV}: |\n{}\n",
        indent_block(&product.outputs.join("\n"), "            ")
    );
    let files_value = if product.output_files.is_empty() {
        format!("          {OUTPUT_FILES_ENV}: \"\"\n")
    } else {
        format!(
            "          {OUTPUT_FILES_ENV}: |\n{}\n",
            indent_block(&product.output_files.join("\n"), "            ")
        )
    };
    format!(
        "      - name: Download product {artifact}\n        uses: {download_artifact_pin}\n        with:\n          name: {artifact}\n          path: ${{{{ runner.temp }}}}/velnor-products/{artifact}\n      - name: Verify product {artifact}\n        env:\n{outputs_value}{files_value}        run: {command}\n",
    )
}

fn indent_block(value: &str, indent: &str) -> String {
    value
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{
        artifact_name, ready_records, stage_product, transport_eligible, verify_product,
        StageRequest, VerifyRequest,
    };
    use crate::s2::platform::NamedProduct;

    fn product(outputs: &[&str]) -> NamedProduct {
        NamedProduct {
            name: "xcframework-bridgecore".to_owned(),
            outputs: outputs.iter().map(ToString::to_string).collect(),
            ..NamedProduct::default()
        }
    }

    #[test]
    fn transport_eligible_requires_declared_outputs() {
        assert!(transport_eligible(&product(&["out/Foo.xcframework"])));
        assert!(!transport_eligible(&product(&[])));
    }

    #[test]
    fn artifact_name_sanitizes_and_separates_sides() {
        assert_eq!(
            artifact_name("rust-ffi", "xcframework-bridgecore"),
            "velnor-product-rust-ffi--xcframework-bridgecore"
        );
        assert_eq!(
            artifact_name("unit/a b", "prod:c"),
            "velnor-product-unit-a-b--prod-c"
        );
        let long = "u".repeat(200);
        let name = artifact_name(&long, "p");
        assert!(name.len() < 200, "{name}");
        assert!(name.starts_with("velnor-product-uuuu"), "{name}");
    }

    #[test]
    fn ready_records_sort_and_render_caller_verdicts() {
        assert_eq!(ready_records(&[]), None);
        let records = ready_records(&[
            ("b/prod".to_owned(), "hosted-b".to_owned()),
            ("a/prod".to_owned(), "hosted-a".to_owned()),
        ])
        .expect("records render");
        assert_eq!(
            records,
            "a/prod:${{ needs.hosted-a.result == 'success' }},\
             b/prod:${{ needs.hosted-b.result == 'success' }}"
        );
    }

    #[test]
    fn rendered_blocks_carry_pins_commands_and_env() {
        let mut full = product(&["target/xcframework/BridgeCore.xcframework"]);
        full.inputs_digest = Some("abc123".to_owned());
        full.output_files = vec!["target/xcframework/BridgeCore.xcframework/Info.plist".to_owned()];
        let produced = super::render_producer_block("upload@pinned", "rust-ffi", &full);
        assert!(produced.contains("uses: upload@pinned"), "{produced}");
        assert!(produced.contains("if-no-files-found: error"), "{produced}");
        assert!(produced.contains("retention-days: 1"), "{produced}");
        assert!(
            produced.contains("velnor-workflow stage-product --producer"),
            "{produced}"
        );
        assert!(produced.contains("--digest"), "{produced}");
        assert!(
            produced.contains("VELNOR_TRANSPORT_OUTPUTS: |"),
            "{produced}"
        );
        assert!(
            produced.contains("velnor-product-rust-ffi--xcframework-bridgecore"),
            "{produced}"
        );
        let consumed = super::render_consumer_block(
            "download@pinned",
            "rust-ffi",
            &full,
            "VELNOR_PRODUCT_RUST_FFI__READY",
        );
        assert!(consumed.contains("uses: download@pinned"), "{consumed}");
        assert!(
            consumed.contains("velnor-workflow verify-product --producer"),
            "{consumed}"
        );
        assert!(
            consumed.contains("--marker VELNOR_PRODUCT_RUST_FFI__READY"),
            "{consumed}"
        );
        assert!(
            consumed.contains("${{ runner.temp }}/velnor-products/"),
            "{consumed}"
        );
        assert!(
            consumed.contains("VELNOR_TRANSPORT_OUTPUT_FILES: |"),
            "{consumed}"
        );

        // An empty digest omits the flag instead of rendering an empty value.
        let mut bare = product(&["out/Foo.xcframework"]);
        bare.output_files = Vec::new();
        let bare_block = super::render_consumer_block("download@pinned", "rust-ffi", &bare, "M");
        assert!(!bare_block.contains("--digest"), "{bare_block}");
        assert!(
            bare_block.contains("VELNOR_TRANSPORT_OUTPUT_FILES: \"\""),
            "{bare_block}"
        );
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-transport-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("create scratch directory");
        root
    }

    fn stage_fixture(root: &std::path::Path) {
        let framework = root.join("out/Foo.xcframework/macos-arm64");
        std::fs::create_dir_all(framework.join("Headers")).expect("fixture dirs");
        std::fs::write(framework.join("libfoo.a"), "archive-bytes").expect("fixture lib");
        std::fs::write(framework.join("Headers/foo.h"), "header-bytes").expect("fixture header");
        std::fs::create_dir_all(root.join("out/Foo.xcframework/empty-dir")).expect("fixture empty");
        std::os::unix::fs::symlink("A", root.join("out/Foo.xcframework/Current"))
            .expect("fixture link");
    }

    fn stage_request(stage: &std::path::Path) -> StageRequest {
        StageRequest {
            producer: "rust-ffi".to_owned(),
            product: "xcframework-foo".to_owned(),
            inputs_digest: "digest-1".to_owned(),
            outputs: vec!["out/Foo.xcframework".to_owned()],
            stage: stage.to_path_buf(),
        }
    }

    fn verify_request(stage: &std::path::Path, env_file: &std::path::Path) -> VerifyRequest {
        VerifyRequest {
            producer: "rust-ffi".to_owned(),
            product: "xcframework-foo".to_owned(),
            inputs_digest: "digest-1".to_owned(),
            outputs: vec!["out/Foo.xcframework".to_owned()],
            output_files: vec!["out/Foo.xcframework/macos-arm64/libfoo.a".to_owned()],
            stage: stage.to_path_buf(),
            marker: "VELNOR_PRODUCT_MARKER".to_owned(),
            env_file: env_file.to_path_buf(),
        }
    }

    fn copy_dir(source: &std::path::Path, dest: &std::path::Path) {
        std::fs::create_dir_all(dest).expect("copy dest");
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(source)
            .expect("copy source")
            .collect::<Result<_, _>>()
            .expect("copy entries");
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let target = dest.join(entry.file_name());
            let file_type = entry.file_type().expect("entry type");
            if file_type.is_symlink() {
                std::os::unix::fs::symlink(
                    std::fs::read_link(entry.path()).expect("link"),
                    &target,
                )
                .expect("copy link");
            } else if file_type.is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy file");
            }
        }
    }

    #[test]
    fn stage_then_verify_round_trips_files_links_and_empty_dirs() {
        let producer_root = scratch("producer");
        let consumer_root = scratch("consumer");
        stage_fixture(&producer_root);
        let count = stage_product(&producer_root, &stage_request(&producer_root.join("stage")))
            .expect("stage");
        assert_eq!(count, 2);
        // The artifact crosses jobs as a plain directory copy here.
        let shipped = consumer_root.join("stage");
        copy_dir(&producer_root.join("stage"), &shipped);
        let env_file = consumer_root.join("github-env");
        std::fs::write(&env_file, "EXISTING=1\n").expect("env file");
        let installed =
            verify_product(&consumer_root, &verify_request(&shipped, &env_file)).expect("verify");
        assert_eq!(installed, 2);
        assert_eq!(
            std::fs::read(consumer_root.join("out/Foo.xcframework/macos-arm64/libfoo.a"))
                .expect("installed lib"),
            b"archive-bytes"
        );
        assert!(
            consumer_root.join("out/Foo.xcframework/empty-dir").is_dir(),
            "empty dirs survive"
        );
        assert_eq!(
            std::fs::read_link(consumer_root.join("out/Foo.xcframework/Current")).expect("link"),
            std::path::PathBuf::from("A"),
            "links survive with their targets"
        );
        let env = std::fs::read_to_string(&env_file).expect("marker file");
        assert!(env.contains("EXISTING=1\n"), "markers append: {env}");
        assert!(env.contains("VELNOR_PRODUCT_MARKER=1\n"), "{env}");
    }

    #[test]
    fn stage_rejects_empty_missing_and_escaping_outputs() {
        let root = scratch("stage-errors");
        let empty = stage_product(
            &root,
            &StageRequest {
                outputs: Vec::new(),
                ..stage_request(&root.join("stage"))
            },
        );
        assert!(format!("{}", empty.expect_err("empty")).contains("empty product"));

        stage_fixture(&root);
        let missing = stage_product(
            &root,
            &StageRequest {
                outputs: vec!["out/does-not-exist".to_owned()],
                ..stage_request(&root.join("stage"))
            },
        );
        assert!(format!("{}", missing.expect_err("missing")).contains("output missing"));

        let escape = stage_product(
            &root,
            &StageRequest {
                outputs: vec!["../escape".to_owned()],
                ..stage_request(&root.join("stage"))
            },
        );
        assert!(format!("{}", escape.expect_err("escape")).contains("normal form"));
    }

    fn staged(root: &std::path::Path) -> std::path::PathBuf {
        stage_fixture(root);
        let stage = root.join("stage");
        stage_product(root, &stage_request(&stage)).expect("stage");
        stage
    }

    #[test]
    fn verify_rejects_tampered_staged_bytes() {
        let stage = staged(&scratch("tamper-producer"));
        std::fs::write(
            stage.join("outputs/out/Foo.xcframework/macos-arm64/libfoo.a"),
            "tampered-bytes",
        )
        .expect("tamper");
        let consumer = scratch("tamper-consumer");
        let env_file = consumer.join("github-env");
        let result = verify_product(&consumer, &verify_request(&stage, &env_file));
        let message = format!("{}", result.expect_err("tampering must fail"));
        assert!(message.contains("digest mismatch"), "{message}");
        assert!(
            !consumer.join("out/Foo.xcframework").exists(),
            "nothing installs on failure"
        );
    }

    #[test]
    fn verify_rejects_identity_and_structural_mismatch() {
        let stage = staged(&scratch("identity-producer"));
        let consumer = scratch("identity-consumer");
        let env_file = consumer.join("github-env");
        let wrong_digest = verify_product(
            &consumer,
            &VerifyRequest {
                inputs_digest: "digest-2".to_owned(),
                ..verify_request(&stage, &env_file)
            },
        );
        let message = format!("{}", wrong_digest.expect_err("digest must fail"));
        assert!(message.contains("inputs_digest"), "{message}");

        let manifest_path = stage.join(super::MANIFEST_FILE);
        let manifest = std::fs::read_to_string(&manifest_path).expect("manifest");
        std::fs::write(
            &manifest_path,
            manifest.replace("xcframework-foo", "xcframework-evil"),
        )
        .expect("rewrite manifest");
        let wrong_product = verify_product(&consumer, &verify_request(&stage, &env_file));
        let message = format!("{}", wrong_product.expect_err("product must fail"));
        assert!(message.contains("product"), "{message}");

        let stage = staged(&scratch("structural-producer"));
        let missing_file = verify_product(
            &consumer,
            &VerifyRequest {
                output_files: vec!["out/Foo.xcframework/macos-arm64/missing.a".to_owned()],
                ..verify_request(&stage, &env_file)
            },
        );
        let message = format!("{}", missing_file.expect_err("structural must fail"));
        assert!(message.contains("structural file"), "{message}");
    }

    #[test]
    fn verify_rejects_manifest_paths_outside_output_roots() {
        let stage = staged(&scratch("escape-producer"));
        let manifest_path = stage.join(super::MANIFEST_FILE);
        let manifest = std::fs::read_to_string(&manifest_path).expect("manifest");
        std::fs::write(
            &manifest_path,
            manifest.replace(
                "out/Foo.xcframework/macos-arm64/libfoo.a",
                "out/other/libfoo.a",
            ),
        )
        .expect("rewrite manifest");
        let consumer = scratch("escape-consumer");
        let env_file = consumer.join("github-env");
        let result = verify_product(&consumer, &verify_request(&stage, &env_file));
        let message = format!("{}", result.expect_err("escape must fail"));
        assert!(
            message.contains("escapes the declared output roots"),
            "{message}"
        );
        assert!(
            !consumer.join("out").exists(),
            "nothing installs on failure"
        );
    }

    #[test]
    fn verify_rejects_bad_marker_and_missing_manifest() {
        let stage = staged(&scratch("marker-producer"));
        let consumer = scratch("marker-consumer");
        let env_file = consumer.join("github-env");
        let bad_marker = verify_product(
            &consumer,
            &VerifyRequest {
                marker: "not a name".to_owned(),
                ..verify_request(&stage, &env_file)
            },
        );
        let message = format!("{}", bad_marker.expect_err("marker must fail"));
        assert!(message.contains("--marker"), "{message}");

        let missing = verify_product(
            &consumer,
            &VerifyRequest {
                stage: consumer.join("no-stage"),
                ..verify_request(&stage, &env_file)
            },
        );
        let message = format!("{}", missing.expect_err("missing must fail"));
        assert!(message.contains("manifest missing"), "{message}");
    }

    #[test]
    fn cli_wrappers_reject_bad_arguments_before_touching_state() {
        use std::ffi::OsString;
        let root = scratch("cli");
        let missing_flag =
            super::stage_product_cli(&root, &[OsString::from("--producer"), OsString::from("p")]);
        let message = format!("{}", missing_flag.expect_err("missing flag"));
        assert!(message.contains("stage-product needs --"), "{message}");
        let unknown = super::verify_product_cli(
            &root,
            &[
                OsString::from("--producer"),
                OsString::from("p"),
                OsString::from("--bogus"),
                OsString::from("x"),
            ],
        );
        let message = format!("{}", unknown.expect_err("unknown flag"));
        assert!(message.contains("--bogus"), "{message}");
    }
}
