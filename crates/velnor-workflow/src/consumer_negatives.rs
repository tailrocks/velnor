#![expect(
    clippy::panic,
    reason = "tests need setup failures to name their root cause"
)]

//! Executable consumer verification suite: bastion A2 gaps G6 (consumer
//! negatives) and G7 (cold consumer).
//!
//! The static tests in `lib.rs` and `runtime_products.rs` pin the consumer
//! scripts' text. These tests execute the scripts: each case runs the real
//! `setup-velnor-workflow` step bodies (extracted from the shipped
//! `action.yml`), the real rendered Velnor provisioner, or the real
//! candidate-acquire verification tail under `bash`, with a stub `gh`, a
//! logging `jq` shim, and fixture git history. Rejections exit nonzero with
//! the scripts' precise errors, and the stub logs prove which trust gates
//! ran.
//!
//! Trust boundaries of the harness:
//! * `gh` is a stub: it serves fixture release/attestation/run answers and
//!   logs every invocation. It refuses `attestation verify` calls that lack
//!   the pinned `--owner`/`--signer-workflow` flags, so a passing run proves
//!   the gate pinned the producer.
//! * `jq` is the real binary behind a logging shim: accept filters evaluate
//!   for real, and the shim log proves they ran (a stubbed `jq` would weaken
//!   every manifest proof).
//! * `git` is real, over fixture history only. Closure expectations come
//!   from `crate::closure`, so script/Rust agreement is asserted, never
//!   assumed.
//! * `install` is a shim implementing exactly the `install -Dm0755 <src>
//!   <dst>` form the Velnor provisioner uses (macOS `install` lacks `-D`);
//!   its log proves whether the slow-path install ran.
//! * The network is unreachable by construction: the pin resolves from
//!   fixture history (or a local-path fetch remote for the lookup-confusion
//!   cases), and the default `GITHUB_SERVER_URL` points at a nonexistent
//!   local path, so an unexpected fetch fails fast instead of dialing out.
//!
//! The candidate-acquire case executes the verification tail of
//! `policy_candidate_step` (artifact download through manifest binding) with
//! the locally computed pin candidate supplied as `$pin_candidate`. The head
//! of that step (PR-run polling plus `velnor-workflow closure` calls) is
//! liveness, not trust: the tail is the whole trust decision.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

use crate::closure::{
    candidate_closure_of_tree, closure_of_tree, is_full_closure, CI_FEATURES, PROFILE_RELEASE,
};
use crate::primitives::runtime_products::RUNTIME_PRODUCTS_FILE;

/// `RUNNER_OS`-`RUNNER_ARCH` platform the fixtures serve.
const FIXTURE_PLATFORM: &str = "Linux-X64";

/// Stub `gh` serving fixture answers from `$GH_STUB_DIR` and logging every
/// invocation to `$GH_STUB_LOG`. Attestation calls must carry the pinned
/// producer flags (`$GH_STUB_EXPECT_OWNER`, `$GH_STUB_EXPECT_SIGNER`);
/// anything else fails loudly, so a passing run proves the pin held.
/// Deliberately array-free so macOS `/bin/bash` (3.2) runs it too.
const STUB_GH: &str = r#"#!/bin/bash
set -u
log() {
  printf '%s\n' "$*" >> "$GH_STUB_LOG"
}
log "gh $*"
command="${1:-}"
subcommand="${2:-}"
if [[ "$command" == "release" && "$subcommand" == "download" ]]; then
  tag="${3:-}"
  shift 3
  repo=""
  dir=""
  patterns=""
  while [[ $# -gt 0 ]]; do
    case "${1:-}" in
      --repo)
        repo="${2:-}"
        shift 2
        ;;
      --pattern)
        patterns="$patterns ${2:-}"
        shift 2
        ;;
      --dir)
        dir="${2:-}"
        shift 2
        ;;
      *)
        shift
        ;;
    esac
  done
  log "release-download tag=$tag repo=$repo patterns=$patterns"
  if [[ "${GH_STUB_RELEASE_FAIL:-0}" == "1" ]]; then
    echo "release $tag not found in $repo" >&2
    exit 1
  fi
  for pattern in $patterns; do
    if [[ ! -f "$GH_STUB_DIR/$pattern" ]]; then
      echo "stub serves no file for pattern $pattern" >&2
      exit 1
    fi
    cp "$GH_STUB_DIR/$pattern" "$dir/$pattern"
  done
  exit 0
fi
if [[ "$command" == "attestation" && "$subcommand" == "verify" ]]; then
  subject="${3:-}"
  shift 3
  log "attestation-verify subject=$subject flags=$*"
  case "$*" in
    *"--owner $GH_STUB_EXPECT_OWNER"*) ;;
    *)
      echo "stub refuses attestation without the pinned --owner" >&2
      exit 1
      ;;
  esac
  case "$*" in
    *"--signer-workflow $GH_STUB_EXPECT_SIGNER"*) ;;
    *)
      echo "stub refuses attestation without the pinned --signer-workflow" >&2
      exit 1
      ;;
  esac
  fail="${GH_STUB_ATTEST_FAIL:-0}"
  if [[ "$fail" == "1" || "$fail" == "$(basename "$subject")" ]]; then
    echo "attestation verification failed for $subject" >&2
    exit 1
  fi
  exit 0
fi
if [[ "$command" == "run" && "$subcommand" == "download" ]]; then
  run_id="${3:-}"
  shift 3
  name=""
  dir=""
  repo=""
  while [[ $# -gt 0 ]]; do
    case "${1:-}" in
      --name)
        name="${2:-}"
        shift 2
        ;;
      --dir)
        dir="${2:-}"
        shift 2
        ;;
      --repo)
        repo="${2:-}"
        shift 2
        ;;
      *)
        shift
        ;;
    esac
  done
  log "run-download id=$run_id name=$name repo=$repo"
  for file in velnor-workflow candidate-manifest.json; do
    cp "$GH_STUB_DIR/$file" "$dir/$file"
  done
  exit 0
fi
echo "unstubbed gh invocation: gh $*" >&2
exit 1
"#;

/// Logging shim over the real `jq` (`$REAL_JQ`): filters evaluate for real
/// and every call lands in `$JQ_STUB_LOG`.
const STUB_JQ: &str = r#"#!/bin/bash
printf 'jq %s\n' "$*" >> "$JQ_STUB_LOG"
exec "$REAL_JQ" "$@"
"#;

/// Shim implementing exactly the `install -Dm0755 <src> <dst>` form the
/// Velnor provisioner uses. Anything else fails loudly, so script drift
/// surfaces instead of silently passing.
const STUB_INSTALL: &str = r#"#!/bin/bash
printf 'install %s\n' "$*" >> "$INSTALL_STUB_LOG"
if [[ "${1:-}" == "-Dm0755" && $# -eq 3 ]]; then
  mkdir -p "$(dirname "$3")"
  cp "$2" "$3"
  chmod 0755 "$3"
  exit 0
fi
echo "unstubbed install invocation: install $*" >&2
exit 1
"#;

fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

fn must_some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{context}"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn find_on_path(tool: &str) -> PathBuf {
    let path = must_some(std::env::var_os("PATH"), "PATH is set for tests");
    must_some(
        std::env::split_paths(&path)
            .map(|dir| dir.join(tool))
            .find(|candidate| candidate.is_file()),
        &format!("{tool} is a test prerequisite on PATH"),
    )
}

fn write_executable(path: &Path, content: &str) {
    must(fs::write(path, content), "write test stub");
    let mut permissions = must(fs::metadata(path), "stat test stub").permissions();
    permissions.set_mode(0o755);
    must(fs::set_permissions(path, permissions), "chmod test stub");
}

fn git_in(dir: &Path, arguments: &[&str]) {
    let status = must(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(arguments)
            .status(),
        "run git",
    );
    assert!(
        status.success(),
        "git {arguments:?} failed in {}",
        dir.display()
    );
}

/// Fixture product history: one commit carrying every closure path, so the
/// shell `ls-tree` pathspec and `crate::closure` resolve the same bytes.
fn init_closure_checkout(root: &Path) -> (PathBuf, String) {
    let checkout = root.join("checkout");
    must(
        fs::create_dir_all(checkout.join("crates/velnor-workflow/src")),
        "create fixture dirs",
    );
    must(
        fs::create_dir_all(checkout.join(".cargo")),
        "create fixture cargo dir",
    );
    for (name, content) in [
        (
            "crates/velnor-workflow/src/lib.rs",
            "pub fn generator() {}\n",
        ),
        (
            "crates/velnor-workflow/Cargo.toml",
            "[package]\nname = \"velnor-workflow\"\n",
        ),
        ("Cargo.toml", "[workspace]\n"),
        ("Cargo.lock", "# lock\n"),
        ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.98.1\"\n"),
        ("rust-toolchain", "1.98.1\n"),
        (".cargo/config.toml", "[build]\n"),
    ] {
        must(
            fs::write(checkout.join(name), content),
            "write fixture file",
        );
    }
    git_in(&checkout, &["init", "--quiet"]);
    git_in(&checkout, &["add", "-A"]);
    git_in(
        &checkout,
        &[
            "-c",
            "user.email=consumer@test",
            "-c",
            "user.name=consumer",
            "commit",
            "--quiet",
            "--message",
            "fixture",
        ],
    );
    let output = must(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["rev-parse", "HEAD"])
            .output(),
        "rev-parse fixture",
    );
    assert!(output.status.success(), "rev-parse fixture HEAD");
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert_eq!(revision.len(), 40, "fixture revision is a full SHA");
    (checkout, revision)
}

/// A repository whose history never contains the pin: the unrelated checkout
/// and fetch-remote side of the lookup-confusion cases.
fn init_unrelated_checkout(root: &Path, name: &str) -> PathBuf {
    let checkout = root.join(name);
    must(fs::create_dir_all(&checkout), "create unrelated checkout");
    must(
        fs::write(checkout.join("README.md"), "unrelated consumer tree\n"),
        "write unrelated file",
    );
    git_in(&checkout, &["init", "--quiet"]);
    git_in(&checkout, &["add", "-A"]);
    git_in(
        &checkout,
        &[
            "-c",
            "user.email=consumer@test",
            "-c",
            "user.name=consumer",
            "commit",
            "--quiet",
            "--message",
            "unrelated",
        ],
    );
    checkout
}

fn setup_action_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github-gen/sources/actions/setup-velnor-workflow/action.yml");
    must(fs::read_to_string(&path), "read setup action")
}

fn dedent(body: &str, indent: usize) -> String {
    let prefix = " ".repeat(indent);
    let mut out = String::new();
    for line in body.lines() {
        out.push_str(line.strip_prefix(prefix.as_str()).unwrap_or(""));
        out.push('\n');
    }
    out
}

/// The `run: |` body of one composite step, dedented to a runnable script.
fn composite_step_body(action: &str, name: &str, indent: usize) -> String {
    let header = format!("    - name: {name}\n");
    let found = must_some(action.find(header.as_str()), "locate step header");
    let after = &action[found + header.len()..];
    let marker = "run: |\n";
    let body_at = must_some(after.find(marker), "locate step run body");
    let body = &after[body_at + marker.len()..];
    let end = body
        .find("\n    - name: ")
        .map_or(body.len(), |index| index + 1);
    dedent(&body[..end], indent)
}

/// The raw YAML block of one composite step (header through the next step),
/// for assertions about step keys like `if:`.
fn composite_step_block<'a>(action: &'a str, name: &str) -> &'a str {
    let header = format!("    - name: {name}\n");
    let start = must_some(action.find(header.as_str()), "locate step block");
    let rest = &action[start + header.len()..];
    let end = rest
        .find("\n    - name: ")
        .map_or(rest.len(), |index| index + 1);
    &action[start..start + header.len() + end]
}

/// The rendered Velnor provisioner body as a runnable script. The revision
/// and checkout path render into the script as literals, exactly as the
/// generated job carries them.
fn velnor_provisioner_script(revision: &str, checkout: &str) -> String {
    let step = crate::workflow_pinned_policy_runtime_velnor(revision, checkout);
    assert!(
        step.contains(&format!("PINNED_REVISION: {revision}")),
        "the env block carries the pin"
    );
    assert!(
        step.contains(&format!("CHECKOUT_PATH: {checkout}")),
        "the env block carries the checkout"
    );
    let marker = "run: |\n";
    let at = must_some(step.find(marker), "locate provisioner body");
    dedent(&step[at + marker.len()..], 10)
}

/// The candidate-acquire verification tail: artifact download through
/// manifest binding. The extraction point is pinned — the tail must open
/// with the download block and still contain every trust gate — so script
/// drift fails here instead of silently testing less.
fn candidate_verify_tail(revision: &str) -> String {
    let step = crate::policy_candidate_step(revision);
    let marker = "candidate=\"$RUNNER_TEMP/velnor-workflow-candidate\"";
    let at = must_some(step.find(marker), "locate candidate download block");
    let line_start = step[..at].rfind('\n').map_or(0, |index| index + 1);
    let tail = dedent(&step[line_start..], 10);
    assert!(
        tail.starts_with("candidate=\"$RUNNER_TEMP/velnor-workflow-candidate\"\n"),
        "the tail opens with the download block"
    );
    for gate in [
        "gh run download",
        "candidate digest mismatch",
        "is not the pin's candidate",
        "VELNOR_WORKFLOW_CANDIDATE_MANIFEST=",
    ] {
        assert!(
            tail.contains(gate),
            "the candidate tail still contains the {gate} gate"
        );
    }
    tail
}

fn github_output_value(path: &Path, key: &str) -> String {
    let needle = format!("{key}=");
    let content = must(fs::read_to_string(path), "read step output file");
    must_some(
        content
            .lines()
            .find_map(|line| line.strip_prefix(needle.as_str()))
            .map(str::to_owned),
        &format!("step output {key} is present"),
    )
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Fixture "binary": a script answering `--closure` with the canned report
/// and refusing anything else, so unexpected execution fails loudly.
fn stub_binary_script(reported_closure: &str) -> String {
    format!(
        "#!/bin/sh\nif [ \"$1\" = \"--closure\" ]; then\n  echo \"{reported_closure}\"\n  exit 0\nfi\necho \"stub binary: unexpected arguments $*\" >&2\nexit 1\n"
    )
}

/// Flip one hex digit, guaranteeing a digest that differs from the input
/// while staying well-formed.
fn flipped_hex(value: &str, index: usize) -> String {
    let mut changed = value.to_owned();
    let replacement = if changed.as_bytes()[index] == b'0' {
        "1"
    } else {
        "0"
    };
    changed.replace_range(index..=index, replacement);
    changed
}

/// One wrong-manifest mutation over an otherwise-good manifest.
type ManifestMutation = fn(&ConsumerFixture, &mut serde_json::Value);

fn mutate_closure_full(fixture: &ConsumerFixture, manifest: &mut serde_json::Value) {
    manifest["closure"] = serde_json::Value::String(flipped_hex(&fixture.closure, 0));
}

/// Same 16-hex tag locator, different full closure: proves the tag is a
/// locator only and acceptance compares all 64 digits.
fn mutate_closure_same_prefix(fixture: &ConsumerFixture, manifest: &mut serde_json::Value) {
    manifest["closure"] = serde_json::Value::String(flipped_hex(&fixture.closure, 17));
}

fn mutate_profile(_fixture: &ConsumerFixture, manifest: &mut serde_json::Value) {
    manifest["profile"] = serde_json::Value::String("debug".to_owned());
}

fn mutate_features(_fixture: &ConsumerFixture, manifest: &mut serde_json::Value) {
    manifest["features"] = serde_json::Value::String("tui".to_owned());
}

fn mutate_malformed_digest(_fixture: &ConsumerFixture, manifest: &mut serde_json::Value) {
    manifest["products"][FIXTURE_PLATFORM]["binary"] =
        serde_json::Value::String("not-a-digest".to_owned());
}

fn mutate_wrong_asset(_fixture: &ConsumerFixture, manifest: &mut serde_json::Value) {
    manifest["products"][FIXTURE_PLATFORM]["asset"] =
        serde_json::Value::String("velnor-workflow-Linux-ARM64".to_owned());
}

fn mutate_missing_platform(_fixture: &ConsumerFixture, manifest: &mut serde_json::Value) {
    manifest["products"] = serde_json::json!({});
}

const MANIFEST_CASES: &[(&str, ManifestMutation)] = &[
    ("wrong closure", mutate_closure_full),
    (
        "wrong closure behind the same tag prefix",
        mutate_closure_same_prefix,
    ),
    ("wrong profile", mutate_profile),
    ("wrong features", mutate_features),
    ("malformed digest", mutate_malformed_digest),
    ("wrong asset", mutate_wrong_asset),
    ("missing platform", mutate_missing_platform),
];

/// One hermetic consumer run: fixture history, stub tool dir, fake home,
/// and the Rust-computed closure expectations the scripts must agree with.
struct ConsumerFixture {
    root: PathBuf,
    home: PathBuf,
    serve: PathBuf,
    checkout: PathBuf,
    revision: String,
    closure: String,
    candidate_closure: String,
    gh_log: PathBuf,
    jq_log: PathBuf,
    install_log: PathBuf,
    real_jq: PathBuf,
    path: std::ffi::OsString,
}

impl ConsumerFixture {
    fn open(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "velnor-consumer-neg-{name}-{}",
            crate::unique_suffix()
        ));
        must(fs::create_dir_all(&root), "create fixture root");
        let home = root.join("home");
        let bin = root.join("bin");
        let serve = root.join("serve");
        for dir in [&home, &bin, &serve] {
            must(fs::create_dir_all(dir), "create fixture dir");
        }
        let (checkout, revision) = init_closure_checkout(&root);
        let closure = must(
            closure_of_tree(&checkout, &revision, CI_FEATURES, PROFILE_RELEASE),
            "closure of fixture",
        );
        assert!(is_full_closure(&closure), "fixture closure is full hex");
        let candidate_closure = must(
            candidate_closure_of_tree(&checkout, &revision),
            "candidate closure of fixture",
        );
        assert!(
            is_full_closure(&candidate_closure),
            "fixture candidate closure is full hex"
        );
        let real_jq = find_on_path("jq");
        write_executable(&bin.join("gh"), STUB_GH);
        write_executable(&bin.join("jq"), STUB_JQ);
        write_executable(&bin.join("install"), STUB_INSTALL);
        let mut path = bin.as_os_str().to_owned();
        path.push(":");
        path.push(must_some(std::env::var_os("PATH"), "PATH is set for tests"));
        let (gh_log, jq_log, install_log) = Self::logs(&root);
        Self {
            root,
            home,
            serve,
            checkout,
            revision,
            closure,
            candidate_closure,
            gh_log,
            jq_log,
            install_log,
            real_jq,
            path,
        }
    }

    fn asset() -> String {
        format!("velnor-workflow-{FIXTURE_PLATFORM}")
    }

    fn logs(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        (
            root.join("gh-stub.log"),
            root.join("jq-stub.log"),
            root.join("install-stub.log"),
        )
    }

    fn gh_log_text(&self) -> String {
        fs::read_to_string(&self.gh_log).unwrap_or_default()
    }

    fn jq_log_text(&self) -> String {
        fs::read_to_string(&self.jq_log).unwrap_or_default()
    }

    fn install_log_text(&self) -> String {
        fs::read_to_string(&self.install_log).unwrap_or_default()
    }

    fn cargo_home(&self) -> PathBuf {
        self.root.join("cargo-home")
    }

    fn slot_binary(&self) -> PathBuf {
        self.cargo_home().join("bin/velnor-workflow-policy")
    }

    fn installed_binary(&self) -> PathBuf {
        self.home
            .join(".cache/velnor/workflow-runtime")
            .join(&self.closure)
            .join("bin/velnor-workflow")
    }

    fn installed_manifest(&self) -> PathBuf {
        self.home
            .join(".cache/velnor/workflow-runtime")
            .join(&self.closure)
            .join("manifest.json")
    }

    fn release_manifest(
        closure: &str,
        profile: &str,
        features: &str,
        binary_digest: &str,
        asset: &str,
    ) -> serde_json::Value {
        let mut products = serde_json::Map::new();
        products.insert(
            FIXTURE_PLATFORM.to_owned(),
            serde_json::json!({"binary": binary_digest, "asset": asset}),
        );
        serde_json::json!({
            "closure": closure,
            "profile": profile,
            "features": features,
            "products": products,
        })
    }

    fn serve_manifest(&self, manifest: &serde_json::Value) {
        let rendered = must(serde_json::to_string_pretty(manifest), "render manifest");
        must(
            fs::write(self.serve.join("manifest.json"), rendered),
            "serve manifest",
        );
    }

    /// Serve a release asset whose bytes report `reported_closure`, and
    /// return the bytes' digest for the manifest.
    fn serve_asset(&self, reported_closure: &str) -> String {
        let bytes = stub_binary_script(reported_closure);
        must(
            fs::write(self.serve.join(Self::asset()), &bytes),
            "serve asset",
        );
        sha256_hex(bytes.as_bytes())
    }

    fn run_script(
        &self,
        name: &str,
        script: &str,
        extra: &[(&str, &str)],
        workdir: Option<&Path>,
    ) -> Output {
        let path = self
            .root
            .join(format!("velnor-test-{name}-{}.sh", crate::unique_suffix()));
        must(fs::write(&path, script), "write test script");
        let repository = crate::workflow_setup_action_repository();
        let owner = must_some(
            repository.split_once('/').map(|(owner, _)| owner),
            "action coordinate has an owner",
        );
        let mut command = Command::new("bash");
        command.arg(&path);
        if let Some(workdir) = workdir {
            command.current_dir(workdir);
        }
        command.env("PATH", &self.path);
        command.env("HOME", &self.home);
        command.env("REAL_JQ", &self.real_jq);
        command.env("GH_STUB_DIR", &self.serve);
        command.env("GH_STUB_LOG", &self.gh_log);
        command.env("JQ_STUB_LOG", &self.jq_log);
        command.env("INSTALL_STUB_LOG", &self.install_log);
        command.env("GH_STUB_EXPECT_OWNER", owner);
        command.env(
            "GH_STUB_EXPECT_SIGNER",
            format!("{repository}/.github/workflows/{RUNTIME_PRODUCTS_FILE}"),
        );
        command.env("RUNNER_OS", "Linux");
        command.env("RUNNER_ARCH", "X64");
        command.env("GH_TOKEN", "");
        command.env("GITHUB_TOKEN", "");
        for (key, value) in extra {
            command.env(key, value);
        }
        must(command.output(), "execute consumer script")
    }

    fn run_setup_closure(&self, checkout: &Path) -> (Output, PathBuf) {
        let action = setup_action_source();
        let script = composite_step_body(&action, "Resolve source closure", 8);
        let output_file = self
            .root
            .join(format!("closure-output-{}", crate::unique_suffix()));
        let checkout_str = must_some(checkout.to_str(), "checkout path is UTF-8");
        let output_str = must_some(output_file.to_str(), "output path is UTF-8");
        let output = self.run_script(
            "closure",
            &script,
            &[
                ("INSTALL_REV", self.revision.as_str()),
                ("CHECKOUT_PATH", checkout_str),
                (
                    "PRODUCT_REPOSITORY",
                    crate::workflow_setup_action_repository(),
                ),
                ("GITHUB_OUTPUT", output_str),
            ],
            None,
        );
        (output, output_file)
    }

    fn run_setup_download(&self, closure: &str, extra: &[(&str, &str)]) -> Output {
        let action = setup_action_source();
        let script = composite_step_body(&action, "Download runtime product", 8);
        let mut env: Vec<(&str, &str)> = vec![
            ("INSTALL_REV", self.revision.as_str()),
            ("CLOSURE", closure),
            (
                "PRODUCT_REPOSITORY",
                crate::workflow_setup_action_repository(),
            ),
        ];
        env.extend_from_slice(extra);
        self.run_script("download", &script, &env, None)
    }

    fn run_setup_verify(&self, closure: &str) -> Output {
        let action = setup_action_source();
        let script = composite_step_body(&action, "Verify runtime product", 8);
        self.run_script("verify", &script, &[("CLOSURE", closure)], None)
    }

    fn run_setup_path(&self, closure: &str) -> (Output, PathBuf) {
        let action = setup_action_source();
        let script = composite_step_body(&action, "Add runtime to PATH", 8);
        let path_file = self
            .root
            .join(format!("github-path-{}", crate::unique_suffix()));
        let path_str = must_some(path_file.to_str(), "path file is UTF-8");
        let output = self.run_script(
            "add-path",
            &script,
            &[("CLOSURE", closure), ("GITHUB_PATH", path_str)],
            None,
        );
        (output, path_file)
    }

    fn run_velnor_provisioner(&self, checkout: &Path, extra: &[(&str, &str)]) -> (Output, PathBuf) {
        let checkout_str = must_some(checkout.to_str(), "checkout path is UTF-8");
        let script = velnor_provisioner_script(&self.revision, checkout_str);
        let cargo_home = self.cargo_home();
        let cargo_home_str = must_some(cargo_home.to_str(), "cargo home is UTF-8");
        let env_file = self
            .root
            .join(format!("github-env-{}", crate::unique_suffix()));
        let env_str = must_some(env_file.to_str(), "env file is UTF-8");
        let dead_server = self.root.join("dead-server");
        let dead_server_str = must_some(dead_server.to_str(), "dead server is UTF-8");
        let mut env: Vec<(&str, &str)> = vec![
            ("PINNED_REVISION", self.revision.as_str()),
            ("CHECKOUT_PATH", checkout_str),
            ("GITHUB_SERVER_URL", dead_server_str),
            ("GITHUB_REPOSITORY", "example/consumer"),
            ("CARGO_HOME", cargo_home_str),
            ("GITHUB_ENV", env_str),
        ];
        env.extend_from_slice(extra);
        // The provisioner's `git fetch` carries no `-C`: in CI the step runs
        // at the workspace root, so the run mirrors that working directory.
        (
            self.run_script("provision", &script, &env, Some(checkout)),
            env_file,
        )
    }

    fn run_candidate_tail(
        &self,
        name: &str,
        run_id: &str,
        pin_candidate: &str,
        extra: &[(&str, &str)],
    ) -> (Output, PathBuf) {
        let tail = candidate_verify_tail(&self.revision);
        let runner_temp = self.root.join("runner-temp");
        must(fs::create_dir_all(&runner_temp), "create runner temp");
        let runner_temp_str = must_some(runner_temp.to_str(), "runner temp is UTF-8");
        let env_file = self
            .root
            .join(format!("github-env-{}", crate::unique_suffix()));
        let env_str = must_some(env_file.to_str(), "env file is UTF-8");
        let mut env: Vec<(&str, &str)> = vec![
            ("RUNNER_TEMP", runner_temp_str),
            ("GITHUB_ENV", env_str),
            (
                "GITHUB_REPOSITORY",
                crate::workflow_setup_action_repository(),
            ),
            ("name", name),
            ("run_id", run_id),
            ("pin_candidate", pin_candidate),
        ];
        env.extend_from_slice(extra);
        (self.run_script("candidate", &tail, &env, None), env_file)
    }
}

fn candidate_manifest(fixture: &ConsumerFixture, closure: &str, digest: &str) -> serde_json::Value {
    serde_json::json!({
        "profile": "debug",
        "platform": FIXTURE_PLATFORM,
        "repository": crate::workflow_setup_action_repository(),
        "run_id": "12345678",
        "revision": fixture.revision.as_str(),
        "closure": closure,
        "binary_sha256": digest,
    })
}

fn serve_candidate(fixture: &ConsumerFixture, reported_closure: &str, closure: &str) {
    let bytes = stub_binary_script(reported_closure);
    must(
        fs::write(fixture.serve.join("velnor-workflow"), &bytes),
        "serve candidate binary",
    );
    let digest = sha256_hex(bytes.as_bytes());
    let manifest = candidate_manifest(fixture, closure, &digest);
    let rendered = must(
        serde_json::to_string(&manifest),
        "render candidate manifest",
    );
    must(
        fs::write(fixture.serve.join("candidate-manifest.json"), rendered),
        "serve candidate manifest",
    );
}

fn pinned_signer_flags() -> (String, String) {
    let repository = crate::workflow_setup_action_repository();
    let owner = must_some(
        repository.split_once('/').map(|(owner, _)| owner),
        "action coordinate has an owner",
    );
    (
        format!("--owner {owner}"),
        format!("--signer-workflow {repository}/.github/workflows/{RUNTIME_PRODUCTS_FILE}"),
    )
}

fn resolve_closure(fixture: &ConsumerFixture) -> String {
    let (closure_out, output_file) = fixture.run_setup_closure(&fixture.checkout);
    assert!(
        closure_out.status.success(),
        "closure resolution succeeds: {}",
        stderr_of(&closure_out)
    );
    let resolved = github_output_value(&output_file, "value");
    assert_eq!(
        resolved, fixture.closure,
        "the script resolves the fixture's true closure"
    );
    resolved
}

#[test]
fn setup_action_missing_product_fails_closed_naming_the_producer() {
    let fixture = ConsumerFixture::open("sa-missing");
    let resolved = resolve_closure(&fixture);
    let output = fixture.run_setup_download(&resolved, &[("GH_STUB_RELEASE_FAIL", "1")]);
    assert!(!output.status.success(), "a missing product fails");
    let stderr = stderr_of(&output);
    let revision = &fixture.revision;
    let prefix = &fixture.closure[..16];
    assert!(
        stderr.contains(&format!("no runtime product for revision {revision}")),
        "the error names the revision: {stderr}"
    );
    assert!(
        stderr.contains(&format!("(closure {prefix})")),
        "the error carries the resolved closure: {stderr}"
    );
    assert!(
        stderr.contains("mainline runtime-product publisher"),
        "the error names the producer: {stderr}"
    );
    let log = fixture.gh_log_text();
    let repository = crate::workflow_setup_action_repository();
    assert!(
        log.contains(&format!(
            "release-download tag=velnor-workflow-runtime-v1-{prefix} repo={repository}"
        )),
        "the lookup addresses the product release: {log}"
    );
    assert!(
        !log.contains("attestation-verify"),
        "nothing is trusted before the download: {log}"
    );
    assert!(
        !fixture.installed_binary().exists(),
        "no product means no install"
    );
}

#[test]
fn setup_action_wrong_digest_rejects_before_install() {
    let fixture = ConsumerFixture::open("sa-digest");
    let digest = fixture.serve_asset(&fixture.closure);
    let wrong = flipped_hex(&digest, 5);
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &wrong, &asset);
    fixture.serve_manifest(&manifest);
    let resolved = resolve_closure(&fixture);
    let output = fixture.run_setup_download(&resolved, &[]);
    assert!(!output.status.success(), "a wrong digest rejects");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("runtime digest mismatch"),
        "the error names the mismatch: {stderr}"
    );
    assert!(
        !fixture.installed_binary().exists(),
        "unproven bytes never install"
    );
}

#[test]
fn setup_action_wrong_manifest_rejects() {
    let fixture = ConsumerFixture::open("sa-manifest");
    let digest = fixture.serve_asset(&fixture.closure);
    let asset = ConsumerFixture::asset();
    for (case, mutate) in MANIFEST_CASES.iter().copied() {
        let mut manifest =
            ConsumerFixture::release_manifest(&fixture.closure, "release", "", &digest, &asset);
        mutate(&fixture, &mut manifest);
        fixture.serve_manifest(&manifest);
        let resolved = resolve_closure(&fixture);
        let output = fixture.run_setup_download(&resolved, &[]);
        assert!(!output.status.success(), "{case}: the manifest rejects");
        assert!(
            !fixture.installed_binary().exists(),
            "{case}: rejection installs nothing"
        );
    }
}

#[test]
fn setup_action_untrusted_signer_rejects() {
    let fixture = ConsumerFixture::open("sa-signer");
    let digest = fixture.serve_asset(&fixture.closure);
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &digest, &asset);
    fixture.serve_manifest(&manifest);
    let resolved = resolve_closure(&fixture);
    for subject in ["1", "manifest.json"] {
        let output = fixture.run_setup_download(&resolved, &[("GH_STUB_ATTEST_FAIL", subject)]);
        assert!(
            !output.status.success(),
            "untrusted attestation ({subject}) rejects"
        );
        assert!(
            !fixture.installed_binary().exists(),
            "untrusted bytes never install"
        );
    }
    let log = fixture.gh_log_text();
    let (owner_flag, signer_flag) = pinned_signer_flags();
    assert!(
        log.contains(&owner_flag),
        "the rejecting gate pinned the owner: {log}"
    );
    assert!(
        log.contains(&signer_flag),
        "the rejecting gate pinned the producer workflow: {log}"
    );
}

#[test]
fn setup_action_cached_bytes_are_reverified() {
    let fixture = ConsumerFixture::open("sa-cache");
    let binary = fixture.installed_binary();
    must(
        fs::create_dir_all(must_some(binary.parent(), "install dir")),
        "plant cache dir",
    );
    must(
        fs::write(&binary, "tampered bytes\n"),
        "plant tampered bytes",
    );
    let digest = fixture.serve_asset(&fixture.closure);
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &digest, &asset);
    let rendered = must(serde_json::to_string(&manifest), "render manifest");
    must(
        fs::write(fixture.installed_manifest(), rendered),
        "plant cache manifest",
    );
    let output = fixture.run_setup_verify(&fixture.closure);
    assert!(!output.status.success(), "tampered cache bytes reject");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("cached runtime digest mismatch"),
        "the error names the cache mismatch: {stderr}"
    );
}

#[test]
fn setup_action_self_report_mismatch_rejects() {
    let fixture = ConsumerFixture::open("sa-self-report");
    let wrong_report = flipped_hex(&fixture.closure, 9);
    let digest = fixture.serve_asset(&wrong_report);
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &digest, &asset);
    fixture.serve_manifest(&manifest);
    let resolved = resolve_closure(&fixture);
    let download = fixture.run_setup_download(&resolved, &[]);
    assert!(
        download.status.success(),
        "digest-good bytes install: {}",
        stderr_of(&download)
    );
    let (path_out, _) = fixture.run_setup_path(&resolved);
    assert!(!path_out.status.success(), "a lying self-report rejects");
    let stderr = stderr_of(&path_out);
    assert!(stderr.contains("reports closure"), "{stderr}");
    assert!(
        stderr.contains(&resolved),
        "the error names the expectation: {stderr}"
    );
}

#[test]
fn setup_action_declares_no_checksum_input() {
    let action = setup_action_source();
    let inputs_at = must_some(action.find("\ninputs:\n"), "inputs block");
    let outputs_at = must_some(action.find("\noutputs:\n"), "outputs block");
    let inputs = &action[inputs_at..outputs_at];
    for smuggled in ["checksum", "digest", "sha256"] {
        assert!(
            !inputs.contains(smuggled),
            "no {smuggled} input exists to smuggle trust through"
        );
    }
    assert!(
        !action.contains("inputs.checksum"),
        "no step reads a checksum input"
    );
}

#[test]
fn velnor_provisioner_missing_product_fails_closed() {
    let fixture = ConsumerFixture::open("velnor-missing");
    let (output, _) =
        fixture.run_velnor_provisioner(&fixture.checkout, &[("GH_STUB_RELEASE_FAIL", "1")]);
    assert!(!output.status.success(), "a missing product fails");
    let stderr = stderr_of(&output);
    let revision = &fixture.revision;
    let prefix = &fixture.closure[..16];
    assert!(
        stderr.contains(&format!(
            "no policy runtime product for revision {revision}"
        )),
        "the error names the revision: {stderr}"
    );
    assert!(
        stderr.contains(&format!("(closure {prefix})")),
        "the error carries the resolved closure: {stderr}"
    );
    assert!(
        stderr.contains("mainline runtime-product publisher"),
        "the error names the producer: {stderr}"
    );
    let log = fixture.gh_log_text();
    assert!(
        log.contains("patterns= manifest.json"),
        "the manifest goes first: {log}"
    );
    assert!(
        !log.contains("attestation-verify"),
        "nothing is trusted before the download: {log}"
    );
    assert!(!fixture.slot_binary().exists(), "no product means no slot");
}

#[test]
fn velnor_provisioner_wrong_digest_rejects() {
    let fixture = ConsumerFixture::open("velnor-digest");
    let digest = fixture.serve_asset(&fixture.closure);
    let wrong = flipped_hex(&digest, 5);
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &wrong, &asset);
    fixture.serve_manifest(&manifest);
    let (output, _) = fixture.run_velnor_provisioner(&fixture.checkout, &[]);
    assert!(!output.status.success(), "a wrong digest rejects");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("policy runtime digest mismatch"),
        "the error names the mismatch: {stderr}"
    );
    assert!(
        !fixture.slot_binary().exists(),
        "unproven bytes never install"
    );
    assert!(
        fixture.install_log_text().is_empty(),
        "the digest gate precedes the install"
    );
}

#[test]
fn velnor_provisioner_wrong_manifest_rejects() {
    let fixture = ConsumerFixture::open("velnor-manifest");
    let digest = fixture.serve_asset(&fixture.closure);
    let asset = ConsumerFixture::asset();
    for (case, mutate) in MANIFEST_CASES.iter().copied() {
        let mut manifest =
            ConsumerFixture::release_manifest(&fixture.closure, "release", "", &digest, &asset);
        mutate(&fixture, &mut manifest);
        fixture.serve_manifest(&manifest);
        let (output, _) = fixture.run_velnor_provisioner(&fixture.checkout, &[]);
        assert!(!output.status.success(), "{case}: the manifest rejects");
        assert!(
            !fixture.slot_binary().exists(),
            "{case}: rejection installs no slot"
        );
    }
    assert!(
        fixture.install_log_text().is_empty(),
        "no case reached the install"
    );
}

#[test]
fn velnor_provisioner_untrusted_signer_rejects() {
    let fixture = ConsumerFixture::open("velnor-signer");
    let digest = fixture.serve_asset(&fixture.closure);
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &digest, &asset);
    fixture.serve_manifest(&manifest);
    let (output, _) = fixture.run_velnor_provisioner(
        &fixture.checkout,
        &[("GH_STUB_ATTEST_FAIL", "manifest.json")],
    );
    assert!(!output.status.success(), "an untrusted manifest rejects");
    assert!(
        !fixture.slot_binary().exists(),
        "untrusted bytes never install"
    );
    assert!(
        fixture.install_log_text().is_empty(),
        "the attestation gate precedes the install"
    );
    let log = fixture.gh_log_text();
    assert!(
        log.contains("attestation-verify subject=") && log.contains("manifest.json"),
        "the manifest attestation ran: {log}"
    );
    let (owner_flag, signer_flag) = pinned_signer_flags();
    assert!(
        log.contains(&owner_flag),
        "the rejecting gate pinned the owner: {log}"
    );
    assert!(
        log.contains(&signer_flag),
        "the rejecting gate pinned the producer workflow: {log}"
    );
}

#[test]
fn velnor_provisioner_fails_closed_when_pin_unresolvable() {
    let fixture = ConsumerFixture::open("velnor-unresolvable");
    let checkout = init_unrelated_checkout(&fixture.root, "consumer-checkout");
    init_unrelated_checkout(&fixture.root, "remotes/consumer/repo");
    let server = fixture.root.join("remotes");
    let server_str = must_some(server.to_str(), "server path is UTF-8");
    let (output, _) = fixture.run_velnor_provisioner(
        &checkout,
        &[
            ("GITHUB_SERVER_URL", server_str),
            ("GITHUB_REPOSITORY", "consumer/repo"),
        ],
    );
    assert!(!output.status.success(), "an unresolvable pin fails");
    assert!(
        !fixture.gh_log_text().contains("release-download"),
        "no trust decision without a resolved closure"
    );
    assert!(!fixture.slot_binary().exists(), "failure installs no slot");
}

#[test]
fn velnor_provisioner_resolves_fetched_pin_to_its_true_closure() {
    let fixture = ConsumerFixture::open("velnor-fetch");
    let checkout = init_unrelated_checkout(&fixture.root, "consumer-checkout");
    let remote = fixture.root.join("remotes/consumer/repo");
    let status = must(
        Command::new("git")
            .arg("clone")
            .arg("--quiet")
            .arg(&fixture.checkout)
            .arg(&remote)
            .status(),
        "clone fetch remote",
    );
    assert!(status.success(), "the fetch remote carries the pin");
    let server = fixture.root.join("remotes");
    let server_str = must_some(server.to_str(), "server path is UTF-8");
    let (output, _) = fixture.run_velnor_provisioner(
        &checkout,
        &[
            ("GITHUB_SERVER_URL", server_str),
            ("GITHUB_REPOSITORY", "consumer/repo"),
            ("GH_STUB_RELEASE_FAIL", "1"),
        ],
    );
    assert!(!output.status.success(), "the missing product fails");
    let stderr = stderr_of(&output);
    let prefix = &fixture.closure[..16];
    assert!(
        stderr.contains(&format!("(closure {prefix})")),
        "resolution yields the pin's true closure, not the checkout's tree: {stderr}"
    );
}

#[test]
fn velnor_provisioner_self_report_mismatch_rejects() {
    let fixture = ConsumerFixture::open("velnor-self-report");
    let wrong_report = flipped_hex(&fixture.closure, 9);
    let digest = fixture.serve_asset(&wrong_report);
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &digest, &asset);
    fixture.serve_manifest(&manifest);
    let (output, _) = fixture.run_velnor_provisioner(&fixture.checkout, &[]);
    assert!(!output.status.success(), "a lying self-report rejects");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("reports closure"), "{stderr}");
    assert!(
        !fixture.install_log_text().is_empty(),
        "the self-report gates the installed slot"
    );
    assert!(
        fixture.slot_binary().exists(),
        "digest-good bytes installed before the report failed"
    );
}

#[test]
fn velnor_provisioner_reuses_slot_only_on_digest_match() {
    let fixture = ConsumerFixture::open("velnor-reuse");
    let slot = fixture.slot_binary();
    must(
        fs::create_dir_all(must_some(slot.parent(), "slot dir")),
        "create slot dir",
    );
    let bytes = stub_binary_script(&fixture.closure);
    write_executable(&slot, &bytes);
    let before = sha256_hex(bytes.as_bytes());
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &before, &asset);
    fixture.serve_manifest(&manifest);
    let (output, env_file) = fixture.run_velnor_provisioner(&fixture.checkout, &[]);
    assert!(
        output.status.success(),
        "a digest-matching slot reuses: {}",
        stderr_of(&output)
    );
    let log = fixture.gh_log_text();
    assert!(
        log.contains("patterns= manifest.json"),
        "the manifest still downloads fresh: {log}"
    );
    assert!(
        !log.contains(&asset),
        "no asset fetch on the reuse path: {log}"
    );
    assert!(
        fixture.install_log_text().is_empty(),
        "reuse installs nothing"
    );
    let after = sha256_hex(&must(fs::read(&slot), "read slot"));
    assert_eq!(before, after, "the slot bytes are untouched");
    let env = must(fs::read_to_string(&env_file), "read github env");
    assert!(
        env.contains(&format!("VELNOR_WORKFLOW_PINNED_BINARY={}", slot.display())),
        "the slot exports for the guard: {env}"
    );
}

#[test]
fn candidate_acquire_recomputes_the_digest() {
    let fixture = ConsumerFixture::open("candidate-digest");
    let bytes = stub_binary_script(&fixture.candidate_closure);
    must(
        fs::write(fixture.serve.join("velnor-workflow"), &bytes),
        "serve candidate binary",
    );
    let digest = sha256_hex(bytes.as_bytes());
    let manifest = candidate_manifest(
        &fixture,
        &fixture.candidate_closure,
        &flipped_hex(&digest, 2),
    );
    let rendered = must(serde_json::to_string(&manifest), "render manifest");
    must(
        fs::write(fixture.serve.join("candidate-manifest.json"), rendered),
        "serve candidate manifest",
    );
    let prefix = &fixture.candidate_closure[..16];
    let name = format!("velnor-workflow-candidate-{prefix}-{FIXTURE_PLATFORM}");
    let candidate = fixture.candidate_closure.clone();
    let (output, _) = fixture.run_candidate_tail(&name, "12345678", &candidate, &[]);
    assert!(!output.status.success(), "a wrong candidate digest rejects");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("candidate digest mismatch"),
        "the error names the mismatch: {stderr}"
    );
}

#[test]
fn candidate_checksum_alone_confers_no_trust() {
    let fixture = ConsumerFixture::open("candidate-checksum");
    let foreign = flipped_hex(&fixture.candidate_closure, 21);
    serve_candidate(&fixture, &foreign, &foreign);
    let prefix = &fixture.candidate_closure[..16];
    let name = format!("velnor-workflow-candidate-{prefix}-{FIXTURE_PLATFORM}");
    let candidate = fixture.candidate_closure.clone();
    let (output, _) = fixture.run_candidate_tail(&name, "12345678", &candidate, &[]);
    assert!(
        !output.status.success(),
        "a foreign closure rejects despite a matching checksum"
    );
    let stderr = stderr_of(&output);
    assert!(
        !stderr.contains("digest mismatch"),
        "the digest gate passed, so the binding must reject: {stderr}"
    );
    assert!(
        stderr.contains("is not the pin's candidate"),
        "the closure binding rejects: {stderr}"
    );
}

#[test]
fn candidate_acquire_exports_bound_product() {
    let fixture = ConsumerFixture::open("candidate-good");
    let candidate = fixture.candidate_closure.clone();
    serve_candidate(&fixture, &candidate, &candidate);
    let prefix = &candidate[..16];
    let name = format!("velnor-workflow-candidate-{prefix}-{FIXTURE_PLATFORM}");
    let (output, env_file) = fixture.run_candidate_tail(&name, "12345678", &candidate, &[]);
    assert!(
        output.status.success(),
        "a bound candidate acquires: {}",
        stderr_of(&output)
    );
    let env = must(fs::read_to_string(&env_file), "read github env");
    assert!(
        env.contains("VELNOR_WORKFLOW_PINNED_BINARY="),
        "the binary exports: {env}"
    );
    assert!(
        env.contains("VELNOR_WORKFLOW_CANDIDATE_MANIFEST="),
        "the manifest exports for the validator binding: {env}"
    );
    assert!(
        fixture.gh_log_text().contains("run-download"),
        "the artifact came from the run download"
    );
}

#[test]
fn cold_consumer_verifies_everything_with_zero_waivers() {
    let fixture = ConsumerFixture::open("cold");
    let action = setup_action_source();
    let download_block = composite_step_block(&action, "Download runtime product");
    assert!(
        download_block.contains("if: steps.cache.outputs.cache-hit != 'true'"),
        "a cold cache runs the download: {download_block}"
    );
    let verify_block = composite_step_block(&action, "Verify runtime product");
    assert!(
        !verify_block.contains("\n      if:"),
        "verification is unconditional: {verify_block}"
    );
    let repository = crate::workflow_setup_action_repository();
    assert!(
        action.contains(&format!("PRODUCT_REPOSITORY: {repository}")),
        "the product comes from the product repository"
    );
    assert!(
        !fixture.installed_binary().exists(),
        "the cache starts empty"
    );
    assert!(
        !fixture.installed_manifest().exists(),
        "the cache starts empty"
    );
    let digest = fixture.serve_asset(&fixture.closure);
    let asset = ConsumerFixture::asset();
    let manifest =
        ConsumerFixture::release_manifest(&fixture.closure, "release", "", &digest, &asset);
    fixture.serve_manifest(&manifest);
    let (closure_out, output_file) = fixture.run_setup_closure(&fixture.checkout);
    assert!(
        closure_out.status.success(),
        "closure resolution succeeds: {}",
        stderr_of(&closure_out)
    );
    let resolved = github_output_value(&output_file, "value");
    assert_eq!(resolved, fixture.closure, "the full closure resolves");
    assert!(
        is_full_closure(&resolved),
        "the full digest is compared, never a prefix"
    );
    let download = fixture.run_setup_download(&resolved, &[]);
    assert!(
        download.status.success(),
        "cold download succeeds: {}",
        stderr_of(&download)
    );
    let verify = fixture.run_setup_verify(&resolved);
    assert!(
        verify.status.success(),
        "the installed bytes re-verify: {}",
        stderr_of(&verify)
    );
    let (path_out, path_file) = fixture.run_setup_path(&resolved);
    assert!(
        path_out.status.success(),
        "the self-report matches: {}",
        stderr_of(&path_out)
    );
    assert!(fixture.installed_binary().exists(), "the binary installs");
    assert!(
        fixture.installed_manifest().exists(),
        "the manifest installs beside it"
    );
    let path_text = must(fs::read_to_string(&path_file), "read github path");
    assert!(
        path_text.contains(&fixture.closure),
        "the closure bin dir joins PATH: {path_text}"
    );
    let tag = crate::closure::product_tag(&fixture.closure);
    let log = fixture.gh_log_text();
    assert!(
        log.contains(&format!("release-download tag={tag} repo={repository}")),
        "the tag locates, the digest proves: {log}"
    );
    assert_eq!(
        log.matches("attestation-verify").count(),
        2,
        "asset and manifest both verify: {log}"
    );
    assert!(
        log.contains(&format!(
            "--signer-workflow {repository}/.github/workflows/{RUNTIME_PRODUCTS_FILE}"
        )),
        "verification pinned the producer workflow: {log}"
    );
    let jq_log = fixture.jq_log_text();
    assert_eq!(
        jq_log.matches(".closure == $closure").count(),
        2,
        "the accept filter ran on both paths: {jq_log}"
    );
}
