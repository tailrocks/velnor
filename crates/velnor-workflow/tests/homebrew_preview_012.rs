//! Candidate Homebrew formula checks stay tied to the tap PR head, run on
//! every declared native platform, and remain required without credentials.

#![expect(
    clippy::expect_used,
    reason = "fixture setup failures should identify the operation"
)]
#![expect(clippy::panic, reason = "fixture setup failures should panic loudly")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value as JsonValue};
use serde_yaml::Value as YamlValue;

const REPOSITORY: &str = "example/homebrew-preview-tap";
const TAP: &str = "example/preview-tap";
const FORMULA: &str = "preview";
const PR_HEAD_SHA: &str = concat!("$", "{{ github.event.pull_request.head.sha }}");
const PR_HEAD_REPOSITORY: &str =
    concat!("$", "{{ github.event.pull_request.head.repo.full_name }}");
const PR_HEAD_SHA_WITH_EMPTY_DEFAULT: &str =
    concat!("$", "{{ github.event.pull_request.head.sha || '' }}");
const PR_HEAD_REPOSITORY_WITH_EMPTY_DEFAULT: &str = concat!(
    "$",
    "{{ github.event.pull_request.head.repo.full_name || '' }}"
);
const PR_HEAD_SHA_WITH_MERGE_FALLBACK: &str = concat!(
    "$",
    "{{ github.event.pull_request.head.sha || needs.plan.outputs.head_sha }}"
);
const PR_HEAD_REPOSITORY_WITH_BASE_FALLBACK: &str = concat!(
    "$",
    "{{ github.event.pull_request.head.repo.full_name || github.repository }}"
);
const PLAN_HEAD_SHA: &str = concat!("$", "{{ needs.plan.outputs.head_sha }}");
const INPUT_HEAD_SHA: &str = concat!("$", "{{ inputs.homebrew_preview_head_sha }}");
const INPUT_HEAD_REPOSITORY: &str = concat!("$", "{{ inputs.homebrew_preview_head_repository }}");
const EXPECTED_PLATFORMS: [(&str, &str); 4] = [
    ("macos-arm64", "macos-26"),
    ("macos-x64", "macos-26-intel"),
    ("linux-x64", "ubuntu-24.04"),
    ("linux-arm64", "ubuntu-24.04-arm"),
];
const EXPECTED_RUNNER_FACTS: [(&str, &str, &str, &str, &str); 4] = [
    ("macos-arm64", "macOS", "ARM64", "arm64", "macos-26"),
    ("macos-x64", "macOS", "X64", "x86_64", "macos-26-intel"),
    ("linux-x64", "Linux", "X64", "x86_64", "ubuntu-24.04"),
    (
        "linux-arm64",
        "Linux",
        "ARM64",
        "aarch64",
        "ubuntu-24.04-arm",
    ),
];

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    output: PathBuf,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RunnerFacts {
    platform: String,
    os: String,
    arch: String,
    machine: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn fixture(homebrew_preview: Option<&str>) -> Fixture {
    fixture_with_service_requirement(homebrew_preview, true)
}

fn fixture_with_service_requirement(
    homebrew_preview: Option<&str>,
    service_required: bool,
) -> Fixture {
    fixture_with_env(homebrew_preview, service_required, None)
}

fn fixture_with_env(
    homebrew_preview: Option<&str>,
    service_required: bool,
    configured_env: Option<&str>,
) -> Fixture {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "velnor-homebrew-preview-012-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let root = base.join("tap");
    let output = base.join("generated");
    fs::create_dir_all(root.join("Formula")).expect("create tap formula directory");
    fs::create_dir_all(root.join(".github-gen")).expect("create generation config directory");

    let mut formula = String::from(
        r#"class Preview < Formula
  desc "Fixture formula for candidate install checks"
  homepage "https://example.invalid/preview"
  url "https://example.invalid/preview-0.1.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  version "0.1.0"

  def install
    bin.install "preview"
  end
"#,
    );
    if service_required {
        formula.push_str(
            r#"
  service do
    run [opt_bin/"preview"]
    keep_alive true
  end
"#,
        );
    }
    formula.push_str(
        r##"
  test do
    assert_match "preview", shell_output("#{bin}/preview --version")
  end
end
"##,
    );
    fs::write(root.join("Formula/preview.rb"), formula).expect("write candidate formula fixture");

    let declaration = homebrew_preview.map_or_else(String::new, |value| {
        format!(
            "\n[[units]]\nid = \"homebrew\"\nkind = \"homebrew\"\nhomebrew_preview = {{ formula = \"{FORMULA}\", service_required = {service_required}, platforms = [{value}] }}\n"
        )
    });
    let unit_env =
        configured_env.map_or_else(String::new, |values| format!("\n[units.env]\n{values}\n"));
    let config = format!(
        "schema = 2\n\n[generator]\nrepository = \"{REPOSITORY}\"\n\n\
         [workflow]\nproviders = [\"github-hosted\"]\nautomatic_providers = [\"github-hosted\"]\n\
         default_branch = \"main\"\n\n\
         [workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n\
         [policy]\nci_required = true\n{declaration}{unit_env}\n\
         [[declare]]\nprimitive = \"watch-graph\"\n\
         [declare.args.reads]\nhomebrew = [{{ paths = [\"Formula/{FORMULA}.rb\"], reason = \"candidate formula is the Homebrew install input\", complete = true }}]\n"
    );
    fs::write(root.join(".github-gen/velnor-workflow.toml"), config)
        .expect("write typed generator config");
    fs::write(
        root.join(".github-gen/visibility.toml"),
        format!("repository = \"{REPOSITORY}\"\nvisibility = \"public\"\n"),
    )
    .expect("write visibility evidence");

    Fixture { base, root, output }
}

fn run_generate(fixture: &Fixture) -> Output {
    let _ = fs::remove_dir_all(&fixture.output);
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--output",
            fixture.output.to_str().expect("output path is UTF-8"),
            fixture.root.to_str().expect("fixture path is UTF-8"),
        ])
        .output()
        .expect("run velnor-workflow")
}

fn generate_ok(fixture: &Fixture) {
    let output = run_generate(fixture);
    assert!(
        output.status.success(),
        "generation failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn workflow(fixture: &Fixture, name: &str) -> YamlValue {
    let path = fixture.output.join(".github/workflows").join(name);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read generated workflow {}: {error}", path.display()));
    serde_yaml::from_str(&source)
        .unwrap_or_else(|error| panic!("generated workflow {} is YAML: {error}", path.display()))
}

fn workflow_text(fixture: &Fixture, name: &str) -> String {
    fs::read_to_string(fixture.output.join(".github/workflows").join(name))
        .expect("read generated workflow text")
}

fn all_workflows(fixture: &Fixture) -> Vec<(String, YamlValue)> {
    let directory = fixture.output.join(".github/workflows");
    fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read generated workflow directory: {error}"))
        .filter_map(|entry| {
            let path = entry.expect("read workflow directory entry").path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("yml"))
                .then_some(path)
        })
        .map(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("workflow filename is UTF-8")
                .to_owned();
            let source = fs::read_to_string(&path).expect("read generated workflow");
            let document: YamlValue = serde_yaml::from_str(&source).expect("workflow is YAML");
            (name, document)
        })
        .collect()
}

fn preview_input(caller: &YamlValue) -> JsonValue {
    match &caller["with"]["homebrew_preview"] {
        YamlValue::String(value) => {
            serde_json::from_str(value).expect("Homebrew preview caller input is JSON")
        }
        value if value.is_mapping() => {
            serde_yaml::from_value(value).expect("decode typed preview caller input")
        }
        value => panic!("Homebrew caller has a typed preview input: {value:?}"),
    }
}

fn yaml_text(value: &YamlValue) -> String {
    serde_yaml::to_string(value).expect("serialize generated YAML value")
}

fn yaml_string_field<'value>(value: &'value YamlValue, field: &str) -> Option<&'value str> {
    value.get(field).and_then(YamlValue::as_str)
}

fn yaml_contains(value: &YamlValue, fragment: &str) -> bool {
    match value {
        YamlValue::String(text) => text.contains(fragment),
        YamlValue::Sequence(values) => values.iter().any(|value| yaml_contains(value, fragment)),
        YamlValue::Mapping(values) => values
            .iter()
            .any(|(key, value)| key.contains(fragment) || yaml_contains(value, fragment)),
        _ => false,
    }
}

fn compact_lowercase(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_sensitive_env_name(value: &str) -> bool {
    let name = value.to_ascii_lowercase();
    name.contains("token")
        || name.contains("secret")
        || name.contains("credential")
        || name.contains("password")
        || name.contains("passwd")
        || name.contains("auth")
        || name.ends_with("_key")
}

fn has_sensitive_shell_variable(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' || index + 1 >= bytes.len() {
            index += 1;
            continue;
        }
        let mut start = index + 1;
        if bytes.get(start..start + 2) == Some(b"{{") {
            index = start + 2;
            continue;
        }
        let braced = bytes[start] == b'{';
        if braced {
            start += 1;
            if bytes.get(start) == Some(&b'!') {
                start += 1;
            }
        }
        let mut end = start;
        while bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            end += 1;
        }
        if end > start && is_sensitive_env_name(&value[start..end]) {
            return true;
        }
        if braced {
            index = bytes[end..]
                .iter()
                .position(|byte| *byte == b'}')
                .map_or(end, |close| end + close + 1);
        } else {
            index = end.max(index + 1);
        }
    }
    false
}

fn has_credential_expression(value: &str) -> bool {
    let compact = compact_lowercase(value);
    let context_reference = [
        "secrets.",
        "secrets[",
        "github.token",
        "github['token']",
        "github[\"token\"]",
        "github['access_token']",
        "github[\"access_token\"]",
    ]
    .iter()
    .any(|marker| compact.contains(marker));
    let template_reference = value
        .split("${{")
        .skip(1)
        .filter_map(|part| part.split_once("}}").map(|(expression, _)| expression))
        .any(is_sensitive_env_name);
    let shell_reference = has_sensitive_shell_variable(value);
    context_reference || template_reference || shell_reference
}

fn contains_token_bearing_material(value: &YamlValue, in_env: bool) -> bool {
    match value {
        YamlValue::String(text) => has_credential_expression(text),
        YamlValue::Sequence(values) => values
            .iter()
            .any(|value| contains_token_bearing_material(value, in_env)),
        YamlValue::Mapping(mapping) => mapping.iter().any(|(key, value)| {
            let key_name = key.as_str();
            if in_env && is_sensitive_env_name(key_name) && key_name != "persist-credentials" {
                return true;
            }
            let child_is_env = key_name == "env";
            contains_token_bearing_material(value, child_is_env)
                || (!child_is_env && contains_token_bearing_material(value, in_env))
        }),
        _ => false,
    }
}

fn command_words(value: &str) -> Vec<String> {
    value
        .to_ascii_lowercase()
        .split(|character: char| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '-'
        })
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn command_has_action(words: &[String], command: &str, actions: &[&str]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        *word == command
            && words
                .iter()
                .skip(index + 1)
                .take(8)
                .any(|argument| actions.contains(&argument.as_str()))
    })
}

fn daemon_action_findings(words: &[String]) -> Vec<String> {
    let mut findings = Vec::new();
    if words.iter().enumerate().any(|(index, word)| {
        *word == "brew"
            && words.get(index + 1).is_some_and(|next| next == "services")
            && words
                .iter()
                .skip(index + 2)
                .take(8)
                .any(|argument| ["start", "restart", "run", "enable"].contains(&argument.as_str()))
    }) {
        findings.push("brew services launch".to_owned());
    }
    if command_has_action(
        words,
        "launchctl",
        &[
            "load",
            "start",
            "bootstrap",
            "kickstart",
            "submit",
            "enable",
        ],
    ) {
        findings.push("launchctl launch".to_owned());
    }
    if command_has_action(
        words,
        "systemctl",
        &[
            "start",
            "restart",
            "enable",
            "reenable",
            "try-restart",
            "reload-or-restart",
        ],
    ) {
        findings.push("systemctl launch".to_owned());
    }
    if command_has_action(words, "service", &["start", "restart", "enable"]) {
        findings.push("service launch".to_owned());
    }
    if command_has_action(words, "rc-service", &["start", "restart", "boot"]) {
        findings.push("rc-service launch".to_owned());
    }
    for (command, actions) in [
        ("initctl", &["start", "restart"][..]),
        ("supervisorctl", &["start", "restart"][..]),
        ("pm2", &["start", "restart", "resurrect"][..]),
        ("forever", &["start", "restart"][..]),
        ("start-stop-daemon", &["--start"][..]),
    ] {
        if command_has_action(words, command, actions) {
            findings.push(format!("{command} launch"));
        }
    }
    if words.iter().any(|word| {
        [
            "start_service",
            "daemonize",
            "systemd-run",
            "nohup",
            "setsid",
            "daemon",
            "runsv",
            "runsvdir",
        ]
        .contains(&word.as_str())
    }) {
        findings.push("explicit daemon launch helper".to_owned());
    }
    if command_has_action(words, "s6-svc", &["-u"]) {
        findings.push("s6-svc launch".to_owned());
    }
    findings
}

fn daemon_launch_commands(value: &str) -> Vec<String> {
    let lower = value.to_ascii_lowercase();
    let words = command_words(value);
    let mut findings = daemon_action_findings(&words);
    if [
        "service-start",
        "start-service",
        "daemon-launch",
        "launch-daemon",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        findings.push("service-launch action reference".to_owned());
    }
    findings
}

fn is_command_field(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "run"
            | "uses"
            | "script"
            | "command"
            | "commands"
            | "cmd"
            | "entrypoint"
            | "args"
            | "shell"
            | "exec"
            | "executable"
            | "program"
    ) || name.ends_with("_command")
        || name.ends_with("_script")
        || name.ends_with("_entrypoint")
}

fn collect_command_strings(value: &YamlValue, path: &str, commands: &mut Vec<(String, String)>) {
    match value {
        YamlValue::String(command) => commands.push((path.to_owned(), command.to_owned())),
        YamlValue::Sequence(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_command_strings(value, &format!("{path}[{index}]"), commands);
            }
        }
        YamlValue::Mapping(values) => {
            for (key, value) in values {
                let key = key.as_str();
                collect_command_strings(value, &format!("{path}.{key}"), commands);
            }
        }
        _ => {}
    }
}

fn collect_job_command_fields(value: &YamlValue, path: &str, commands: &mut Vec<(String, String)>) {
    match value {
        YamlValue::Mapping(values) => {
            for (key, value) in values {
                let key = key.as_str();
                let field_path = format!("{path}.{key}");
                if is_command_field(key) {
                    collect_command_strings(value, &field_path, commands);
                } else {
                    collect_job_command_fields(value, &field_path, commands);
                }
            }
        }
        YamlValue::Sequence(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_job_command_fields(value, &format!("{path}[{index}]"), commands);
            }
        }
        _ => {}
    }
}

fn parsed_job_command_vectors(job: &YamlValue) -> Vec<(String, String)> {
    let mut commands = Vec::new();
    collect_job_command_fields(job, "job", &mut commands);
    commands
}

fn is_direct_pr_head_binding(value: Option<&str>, expected: &str) -> bool {
    let expected = compact_lowercase(expected);
    let empty_default = expected
        .strip_suffix("}}")
        .map(|stem| format!("{stem}||''}}}}"));
    value.is_some_and(|value| {
        let actual = compact_lowercase(value);
        (actual == expected || empty_default.as_deref() == Some(actual.as_str()))
            && !actual.contains("needs.plan.outputs.head_sha")
            && !actual.contains("github.repository")
    })
}

fn all_jobs(fixture: &Fixture) -> Vec<(String, String, YamlValue)> {
    let directory = fixture.output.join(".github/workflows");
    fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read generated workflow directory: {error}"))
        .filter_map(|entry| {
            let path = entry.expect("read workflow directory entry").path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("yml"))
                .then_some(path)
        })
        .flat_map(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("workflow filename is UTF-8")
                .to_owned();
            let source = fs::read_to_string(&path).expect("read generated workflow");
            let document: YamlValue = serde_yaml::from_str(&source).expect("workflow is YAML");
            let jobs = document["jobs"]
                .as_mapping()
                .expect("workflow declares jobs mapping");
            jobs.iter()
                .map(move |(job_id, job)| (name.clone(), job_id.to_owned(), job.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn candidate_install_jobs(fixture: &Fixture) -> Vec<(String, String, YamlValue)> {
    all_jobs(fixture)
        .into_iter()
        .filter(|(_, _, job)| yaml_contains(job, "brew install --build-from-source"))
        .collect()
}

fn required_check_step(job: &YamlValue) -> &YamlValue {
    job["steps"]
        .as_sequence()
        .expect("required check has steps")
        .iter()
        .find(|step| step["name"].as_str() == Some("Validate generated stack results"))
        .expect("required check validates generated stack results")
}

fn selected_units(expected_callers: &[JsonValue]) -> JsonValue {
    let mut providers_by_unit = BTreeMap::<String, BTreeSet<String>>::new();
    for caller in expected_callers {
        let unit = caller["unit_id"]
            .as_str()
            .expect("expected caller has a unit ID");
        let provider = caller["provider"]
            .as_str()
            .expect("expected caller has a provider");
        providers_by_unit
            .entry(unit.to_owned())
            .or_default()
            .insert(provider.to_owned());
    }
    JsonValue::Array(
        providers_by_unit
            .into_iter()
            .map(|(unit_id, providers)| {
                json!({ "unit_id": unit_id, "providers": providers.into_iter().collect::<Vec<_>>() })
            })
            .collect(),
    )
}

fn run_required_verdict(
    required: &YamlValue,
    homebrew_job: &str,
    homebrew_result: Option<&str>,
) -> Output {
    let step = required_check_step(required);
    let script = step["run"].as_str().expect("required verdict has a script");
    let environment = step["env"].as_mapping().expect("required verdict has env");
    let expected_text = environment["EXPECTED_CALLERS"]
        .as_str()
        .expect("generated expected-caller contract is JSON text");
    let expected_callers: Vec<JsonValue> =
        serde_json::from_str(expected_text).expect("parse required-caller contract");
    let mut needs = serde_json::Map::new();
    needs.insert("plan".to_owned(), json!({ "result": "success" }));
    needs.insert("policy".to_owned(), json!({ "result": "success" }));
    for caller in &expected_callers {
        let job_id = caller["job_id"].as_str().expect("caller has job ID");
        let result = if job_id == homebrew_job {
            homebrew_result.unwrap_or("success")
        } else {
            "success"
        };
        if !(job_id == homebrew_job && homebrew_result.is_none()) {
            needs.insert(job_id.to_owned(), json!({ "result": result }));
        }
    }

    let mut command = Command::new("bash");
    command
        .args(["-euo", "pipefail", "-c", script])
        .env("NEEDS_JSON", JsonValue::Object(needs).to_string())
        .env(
            "SELECTED_UNITS",
            selected_units(&expected_callers).to_string(),
        )
        .env("PLAN_DIGEST", "fixture-plan-digest")
        .env("EXCLUDED", "[]")
        .env("EXPECTED_CALLERS", expected_text);
    for (key, _) in environment {
        let name = key.as_str();
        if name.starts_with("PROVIDER_ADMITTED_") {
            command.env(name, "true");
        }
    }
    command
        .output()
        .expect("run generated required verdict script")
}

fn run_candidate_result_gate(step: &YamlValue, result: Option<&str>) -> Output {
    let script = step["run"]
        .as_str()
        .expect("candidate result gate runs its status check");
    let mut command = Command::new("bash");
    command.args(["-euo", "pipefail", "-c", script]);
    if let Some(result) = result {
        command.env("CANDIDATE_RESULT", result);
    } else {
        command.env_remove("CANDIDATE_RESULT");
    }
    command
        .output()
        .expect("run candidate matrix result gate with fixture status")
}

fn run_candidate_sha_check(step: &YamlValue, expected_sha: &str, actual_sha: &str) -> Output {
    let script = step["run"]
        .as_str()
        .expect("candidate SHA verification runs a shell check");
    let script = format!(
        "git() {{ [[ \"$#\" -eq 2 && \"$1\" == rev-parse && \"$2\" == HEAD ]] || return 64; printf '%s\\n' \"$FAKE_GIT_HEAD\"; }}\n{script}"
    );
    Command::new("bash")
        .args(["-euo", "pipefail", "-c", &script])
        .env("EXPECTED_HEAD_SHA", expected_sha)
        .env("FAKE_GIT_HEAD", actual_sha)
        .output()
        .expect("execute generated candidate checkout SHA check")
}

fn run_candidate_pr_identity_check(
    step: &YamlValue,
    candidate_sha: &str,
    candidate_repository: &str,
    event_sha: &str,
    event_repository: &str,
) -> Output {
    let script = step["run"]
        .as_str()
        .expect("candidate PR identity validation runs a shell check");
    Command::new("bash")
        .args(["-euo", "pipefail", "-c", script])
        .env("CANDIDATE_HEAD_SHA", candidate_sha)
        .env("CANDIDATE_HEAD_REPOSITORY", candidate_repository)
        .env("EVENT_HEAD_SHA", event_sha)
        .env("EVENT_HEAD_REPOSITORY", event_repository)
        .output()
        .expect("execute generated candidate PR identity check")
}

fn run_fixture_git(root: &std::path::Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run Git against isolated planner fixture");
    assert!(
        output.status.success(),
        "fixture git command failed ({arguments:?}): {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[cfg(unix)]
fn run_generated_pr_planner(
    fixture: &Fixture,
    plan_step: &YamlValue,
    base_sha: &str,
    head_sha: &str,
    output_name: &str,
) -> (Output, Option<String>) {
    use std::os::unix::fs::symlink;

    let generated_config = fixture.output.join(".github/ci/project.toml");
    let fixture_config = fixture.root.join(".github/ci/project.toml");
    fs::create_dir_all(
        fixture_config
            .parent()
            .expect("runtime config has a parent directory"),
    )
    .expect("create isolated runtime config directory");
    fs::copy(&generated_config, &fixture_config)
        .expect("copy generated runtime config into isolated checkout");

    let bin = fixture.base.join("generated-plan-bin");
    fs::create_dir_all(&bin).expect("create generated planner command directory");
    let planner_link = bin.join("velnor-workflow");
    if !planner_link.exists() {
        symlink(
            PathBuf::from(env!("CARGO_BIN_EXE_velnor-workflow")),
            &planner_link,
        )
        .expect("expose compiled planner as the generated command");
    }

    let output_path = fixture
        .base
        .join(format!("{output_name}-github-output.txt"));
    let _ = fs::remove_file(&output_path);
    let _ = fs::remove_dir_all(fixture.root.join(".velnor-ci-expected-work"));
    let script = plan_step["run"]
        .as_str()
        .expect("generated PR planning step has a shell command");
    let mut command = Command::new("bash");
    command
        .args(["-euo", "pipefail", "-c", script])
        .current_dir(&fixture.root)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin:/usr/local/bin", bin.display()),
        )
        .env("HOME", &fixture.base)
        .env("GITHUB_OUTPUT", &output_path)
        .env("GITHUB_WORKSPACE", &fixture.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let step_env = plan_step["env"]
        .as_mapping()
        .expect("generated PR planner step declares its environment");
    for (name, value) in step_env {
        let name = name.as_str();
        let rendered = value
            .as_str()
            .unwrap_or_else(|| panic!("generated planner env {name} is a string"));
        let resolved = match name {
            "EVENT_NAME" => "pull_request",
            "CI_SCOPE_OVERRIDE" => "",
            "BASE_SHA" => base_sha,
            "HEAD_SHA" => head_sha,
            "VELNOR_PROVIDERS" => "github-hosted",
            "VELNOR_EVENT_TRUSTED" => "false",
            _ if rendered.contains("${{") => {
                panic!("planner fixture must bind generated env {name}: {rendered}")
            }
            _ => rendered,
        };
        command.env(name, resolved);
    }
    let output = command
        .output()
        .expect("execute generated PR planning command against local Git fixture");
    let units = fs::read_to_string(output_path).ok().and_then(|contents| {
        contents
            .lines()
            .find_map(|line| line.strip_prefix("units="))
            .map(str::to_owned)
    });
    (output, units)
}

#[cfg(unix)]
fn run_candidate_brew_step(
    script: &str,
    formula: &str,
    fixture: &Fixture,
    call_log: &std::path::Path,
    fail_subcommand: Option<&str>,
) -> Output {
    run_candidate_brew_step_with_runner_environment(
        script,
        formula,
        fixture,
        call_log,
        fail_subcommand,
        None,
    )
}

#[cfg(unix)]
fn run_candidate_brew_step_with_runner_environment(
    script: &str,
    formula: &str,
    fixture: &Fixture,
    call_log: &std::path::Path,
    fail_subcommand: Option<&str>,
    runner_environment: Option<&str>,
) -> Output {
    use std::os::unix::fs::PermissionsExt;

    ensure_candidate_formula_git_baseline(&fixture.root);
    let bin = fixture.base.join("candidate-fake-brew-bin");
    fs::create_dir_all(&bin).expect("create fake brew command directory");
    let fake_brew = bin.join("brew");
    fs::write(
        &fake_brew,
        "#!/usr/bin/env bash\nset -euo pipefail\ncase \"${1-}\" in\n  --repository) printf '%s\\n' \"$GITHUB_WORKSPACE\" ;;\n  ruby) if [[ \"$3\" == *Formulary.factory* ]]; then :; else exec ruby -e \"$3\" \"${@:4}\"; fi ;;\n  install|test) printf '%s\\n' \"$*\" >> \"$FAKE_BREW_LOG\"; if [[ \"${FAKE_BREW_FAIL_SUBCOMMAND:-}\" == \"${1-}\" ]]; then exit 23; fi ;;\n  *) exit 64 ;;\nesac\n",
    )
    .expect("write fake brew executable for install/test commands");
    let mut permissions = fs::metadata(&fake_brew)
        .expect("read fake brew permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_brew, permissions).expect("make fake brew executable");
    let _ = fs::remove_file(call_log);
    let mut command = Command::new("bash");
    command
        .args(["-euo", "pipefail", "-c", script])
        .current_dir(&fixture.root)
        .env("PATH", fake_brew_path(&bin))
        .env("GITHUB_WORKSPACE", &fixture.root)
        .env("TAP", TAP)
        .env("FORMULA", formula)
        .env("FAKE_BREW_LOG", call_log)
        .env("FAKE_BREW_FAIL_SUBCOMMAND", fail_subcommand.unwrap_or(""))
        .env("SERVICE_REQUIRED", "true");
    if let Some(runner_environment) = runner_environment {
        command.env("RUNNER_ENVIRONMENT", runner_environment);
    }
    command
        .output()
        .expect("execute generated candidate install/test script with fake brew")
}

#[cfg(unix)]
fn fake_brew_path(bin: &std::path::Path) -> String {
    format!(
        "{}:/usr/bin:/bin:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
fn ensure_candidate_formula_git_baseline(root: &std::path::Path) {
    if root.join(".git").exists() {
        return;
    }
    run_fixture_git(root, &["init", "--quiet"]);
    run_fixture_git(root, &["config", "user.name", "TASK-012 fixture"]);
    run_fixture_git(root, &["config", "user.email", "task012@example.invalid"]);
    run_fixture_git(root, &["add", "Formula/preview.rb"]);
    run_fixture_git(
        root,
        &["commit", "--quiet", "-m", "candidate formula baseline"],
    );
}

#[cfg(unix)]
struct FormulaMutationControl {
    root: PathBuf,
    formula: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    sentinel: PathBuf,
    stub: PathBuf,
    expected_blob: String,
}

#[cfg(unix)]
fn prepare_formula_mutation_control(fixture: &Fixture, stage: &str) -> FormulaMutationControl {
    let root = fixture.base.join(format!("formula-mutation-{stage}"));
    let formula = root.join("Formula/preview.rb");
    fs::create_dir_all(formula.parent().expect("formula has parent"))
        .expect("create formula mutation checkout");
    fs::write(
        &formula,
        format!(
            r##"if ENV.fetch("FORMULA_MUTATION_STAGE", "") == ENV.fetch("FORMULA_BREW_PHASE", "")
  File.write(ENV.fetch("FORMULA_MUTATION_SENTINEL"), "executed")
  File.write(__FILE__, "# rewritten at {stage}\n")
end
"##
        ),
    )
    .expect("write self-rewriting formula fixture");
    ensure_candidate_formula_git_baseline(&root);
    let expected_blob = run_fixture_git(&root, &["rev-parse", "HEAD:Formula/preview.rb"]);
    let bin = fixture.base.join(format!("formula-mutation-bin-{stage}"));
    fs::create_dir_all(&bin).expect("create formula mutation fake brew directory");
    let stub = fixture
        .base
        .join(format!("formula-mutation-stub-{stage}.rb"));
    fs::write(
        &stub,
        r#"module Formulary
  FormulaFixture = Struct.new(:path) do
    def service?; true; end
  end
  def self.factory(_name)
    formula_path = File.realpath(ENV.fetch("FORMULA_FIXTURE"))
    load formula_path
    FormulaFixture.new(formula_path)
  end
end
"#,
    )
    .expect("write isolated Formulary mutation fixture");
    FormulaMutationControl {
        root,
        formula,
        bin,
        log: fixture.base.join(format!("formula-mutation-{stage}.log")),
        sentinel: fixture
            .base
            .join(format!("formula-mutation-{stage}.sentinel")),
        stub,
        expected_blob,
    }
}

#[cfg(unix)]
fn run_formula_mutation_control(
    script: &str,
    control: &FormulaMutationControl,
    stage: &str,
) -> Output {
    let fake_brew = control.bin.join("brew");
    let contents = format!(
        r##"#!/usr/bin/env bash
set -euo pipefail
case "${{1-}}" in
  --repository) printf '%s\n' "$GITHUB_WORKSPACE" ;;
  ruby)
    ruby_program="$3"
    if [[ "$ruby_program" == *Formulary.factory* ]]; then
      if [[ "$ruby_program" == *formula_file* ]]; then export FORMULA_BREW_PHASE=validation; else export FORMULA_BREW_PHASE=service; fi
      printf '%s\n' service >> "$FAKE_BREW_LOG"
      actual_formula_blob="$(git hash-object --no-filters "$FORMULA_FIXTURE")"
      [[ "$actual_formula_blob" == "$EXPECTED_FORMULA_BLOB" ]] || exit 81
      exec ruby -r "$FORMULARY_STUB" -e "$ruby_program" "${{@:4}}"
    fi
    export FORMULA_BREW_PHASE=validation
    exec ruby -e "$ruby_program" "${{@:4}}"
    ;;
  install|test)
    printf '%s\n' "${{1-}}" >> "$FAKE_BREW_LOG"
    actual_formula_blob="$(git hash-object --no-filters "$FORMULA_FIXTURE")"
    [[ "$actual_formula_blob" == "$EXPECTED_FORMULA_BLOB" ]] || exit 81
    export FORMULA_BREW_PHASE="${{1-}}"
    ruby -e 'load ENV.fetch("FORMULA_FIXTURE")'
    ;;
  *) exit 64 ;;
esac
"##
    );
    let_executable(&fake_brew, &contents);
    let _ = fs::remove_file(&control.log);
    let _ = fs::remove_file(&control.sentinel);
    Command::new("bash")
        .args(["-euo", "pipefail", "-c", script])
        .current_dir(&control.root)
        .env("PATH", fake_brew_path(&control.bin))
        .env("GITHUB_WORKSPACE", &control.root)
        .env("TAP", TAP)
        .env("FORMULA", FORMULA)
        .env(
            "SERVICE_REQUIRED",
            if stage == "validation" {
                "false"
            } else {
                "true"
            },
        )
        .env("FORMULA_FIXTURE", &control.formula)
        .env("FORMULARY_STUB", &control.stub)
        .env("FORMULA_MUTATION_STAGE", stage)
        .env("FORMULA_MUTATION_SENTINEL", &control.sentinel)
        .env("EXPECTED_FORMULA_BLOB", &control.expected_blob)
        .env("FAKE_BREW_LOG", &control.log)
        .output()
        .expect("execute generated candidate step with self-rewriting formula")
}

#[cfg(unix)]
fn assert_candidate_formula_mutation_controls(fixture: &Fixture, script: &str) {
    for (stage, expected_calls) in [
        ("validation", vec!["install", "test"]),
        ("install", vec!["install"]),
        ("test", vec!["install", "test"]),
        ("service", vec!["install", "test", "service"]),
    ] {
        let control = prepare_formula_mutation_control(fixture, stage);
        let result = run_formula_mutation_control(script, &control, stage);
        let calls = fs::read_to_string(&control.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if stage == "validation" {
            assert!(
                result.status.success(),
                "path validation does not load a self-rewriting formula: {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(
                run_fixture_git(
                    &control.root,
                    &["hash-object", "--no-filters", "Formula/preview.rb"],
                ),
                control.expected_blob.as_str(),
                "validation preserves the committed formula blob"
            );
            assert!(
                !control.sentinel.exists(),
                "validation leaves the formula body unevaluated"
            );
        } else {
            assert!(
                !result.status.success(),
                "persistent formula rewrite at {stage} fails the exact blob guard"
            );
            assert!(
                String::from_utf8_lossy(&result.stderr)
                    .contains("candidate formula bytes differ from checked-out Git blob"),
                "rewrite at {stage} reports the blob mismatch: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(
                control.sentinel.exists(),
                "self-rewriting formula actually executes at {stage}"
            );
        }
        assert_eq!(
            calls,
            expected_calls
                .iter()
                .map(|call| (*call).to_owned())
                .collect::<Vec<_>>(),
            "later brew boundaries are not reached after a rewrite at {stage}"
        );
    }
}

fn exact_candidate_brew_call(actual: &str, subcommand: &str) -> bool {
    let expected = match subcommand {
        "install" => format!("install --build-from-source --verbose {TAP}/{FORMULA}"),
        "test" => format!("test --verbose {TAP}/{FORMULA}"),
        _ => return false,
    };
    actual == expected
}

#[cfg(unix)]
struct CandidatePathPoisoningFixture {
    attack_workspace: PathBuf,
    attack_formula: PathBuf,
    github_path: PathBuf,
    rogue_bin: PathBuf,
    formulary_stub: PathBuf,
    trusted_log: PathBuf,
    rogue_log: PathBuf,
    trusted_path: String,
}

#[cfg(unix)]
fn prepare_candidate_path_poisoning_fixture(fixture: &Fixture) -> CandidatePathPoisoningFixture {
    let attack_workspace = fixture.base.join("candidate-path-attack-workspace");
    let attack_formula = attack_workspace.join("Formula").join("preview.rb");
    let github_path = fixture.base.join("candidate-github-path");
    let trusted_bin = fixture.base.join("candidate-trusted-brew-bin");
    let rogue_bin = fixture.base.join("candidate-rogue-brew-bin");
    let formulary_stub = fixture.base.join("candidate-formulary-stub.rb");
    let trusted_log = fixture.base.join("candidate-trusted-brew.log");
    let rogue_log = fixture.base.join("candidate-rogue-brew.log");
    fs::create_dir_all(attack_formula.parent().expect("formula has parent"))
        .expect("create adversarial formula directory");
    fs::create_dir_all(&trusted_bin).expect("create trusted fake brew directory");
    fs::create_dir_all(&rogue_bin).expect("create rogue fake brew directory");
    fs::write(
        &attack_formula,
        r#"File.open(ENV.fetch("GITHUB_PATH"), "a") { |path| path.puts ENV.fetch("FAKE_ROGUE_BREW_BIN") }"#,
    )
    .expect("write formula that poisons the runner path command file");
    fs::write(
        &formulary_stub,
        r#"module Formulary
  FormulaFixture = Struct.new(:path) do
    def service?; true; end
  end
  def self.factory(_name)
    formula_path = File.realpath(ENV.fetch("FORMULA_FIXTURE"))
    load formula_path
    FormulaFixture.new(formula_path)
  end
end
"#,
    )
    .expect("write isolated Formulary stub");
    ensure_candidate_formula_git_baseline(&attack_workspace);

    let_executable(
        &trusted_bin.join("brew"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "${1-}" in
  --repository) printf '%s\n' "$GITHUB_WORKSPACE" ;;
  ruby) exec ruby -r "$FORMULARY_STUB" -e "$3" "${@:4}" ;;
  install)
    ruby -e 'load ENV.fetch("FORMULA_FIXTURE")'
    printf '%s\n' "$*" >> "$FAKE_BREW_LOG"
    ;;
  test) printf '%s\n' "$*" >> "$FAKE_BREW_LOG" ;;
  *) exit 64 ;;
esac
"#,
    );
    let_executable(
        &rogue_bin.join("brew"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_ROGUE_BREW_LOG"
"#,
    );

    let _ = fs::remove_file(&github_path);
    let _ = fs::remove_file(&trusted_log);
    let _ = fs::remove_file(&rogue_log);
    let trusted_path = fake_brew_path(&trusted_bin);

    CandidatePathPoisoningFixture {
        attack_workspace,
        attack_formula,
        github_path,
        rogue_bin,
        formulary_stub,
        trusted_log,
        rogue_log,
        trusted_path,
    }
}

#[cfg(unix)]
fn assert_combined_candidate_step_keeps_trusted_brew(
    script: &str,
    fixture: &CandidatePathPoisoningFixture,
) {
    let combined = Command::new("bash")
        .args(["-euo", "pipefail", "-c", script])
        .current_dir(&fixture.attack_workspace)
        .env("PATH", &fixture.trusted_path)
        .env("GITHUB_WORKSPACE", &fixture.attack_workspace)
        .env("GITHUB_PATH", &fixture.github_path)
        .env("TAP", TAP)
        .env("FORMULA", FORMULA)
        .env("FORMULA_FIXTURE", &fixture.attack_formula)
        .env("FORMULARY_STUB", &fixture.formulary_stub)
        .env("FAKE_ROGUE_BREW_BIN", &fixture.rogue_bin)
        .env("FAKE_BREW_LOG", &fixture.trusted_log)
        .env("FAKE_ROGUE_BREW_LOG", &fixture.rogue_log)
        .env("SERVICE_REQUIRED", "true")
        .output()
        .expect("run the combined candidate formula step with adversarial formula");
    assert!(
        combined.status.success(),
        "combined candidate step completes with PATH update queued: {}{}",
        String::from_utf8_lossy(&combined.stdout),
        String::from_utf8_lossy(&combined.stderr)
    );
    assert_eq!(
        fs::read_to_string(&fixture.trusted_log)
            .expect("trusted brew records same-step install and test")
            .lines()
            .collect::<Vec<_>>(),
        vec![
            format!("install --build-from-source --verbose {TAP}/{FORMULA}"),
            format!("test --verbose {TAP}/{FORMULA}"),
        ],
        "formula-written GITHUB_PATH does not replace PATH before the same-step test"
    );
    assert!(
        !fixture.rogue_log.exists(),
        "rogue brew cannot intercept commands before the runner applies GITHUB_PATH"
    );
    let additions = fs::read_to_string(&fixture.github_path)
        .expect("formula appends rogue directory to GITHUB_PATH");
    assert!(
        additions
            .lines()
            .any(|path| path == fixture.rogue_bin.display().to_string())
            && additions
                .lines()
                .all(|path| path == fixture.rogue_bin.display().to_string()),
        "adversarial formula actually writes the rogue brew directory to the command file"
    );
}

#[cfg(unix)]
fn assert_split_candidate_steps_are_hijackable(fixture: &CandidatePathPoisoningFixture) {
    // Negative control: the former formula-load step writes GITHUB_PATH before
    // the runner starts later Homebrew steps with the added directory on PATH.
    let _ = fs::remove_file(&fixture.github_path);
    let _ = fs::remove_file(&fixture.rogue_log);
    let old_formula_load_step = "set -euo pipefail\nruby -e 'load ENV.fetch(\"FORMULA_FIXTURE\")'";
    let formula_load = Command::new("bash")
        .args(["-euo", "pipefail", "-c", old_formula_load_step])
        .current_dir(&fixture.attack_workspace)
        .env("PATH", &fixture.trusted_path)
        .env("GITHUB_WORKSPACE", &fixture.attack_workspace)
        .env("GITHUB_PATH", &fixture.github_path)
        .env("FORMULA_FIXTURE", &fixture.attack_formula)
        .env("FORMULARY_STUB", &fixture.formulary_stub)
        .env("FAKE_ROGUE_BREW_BIN", &fixture.rogue_bin)
        .output()
        .expect("run former standalone formula-load step");
    assert!(
        formula_load.status.success(),
        "negative-control formula load succeeds: {}{}",
        String::from_utf8_lossy(&formula_load.stdout),
        String::from_utf8_lossy(&formula_load.stderr)
    );
    let additions = fs::read_to_string(&fixture.github_path)
        .expect("negative-control formula load writes GITHUB_PATH");
    assert!(additions
        .lines()
        .any(|path| path == fixture.rogue_bin.display().to_string()));

    // GitHub applies each command-file path before the next job step.
    let applied_path = additions
        .lines()
        .rev()
        .fold(fixture.trusted_path.clone(), |path, addition| {
            format!("{addition}:{path}")
        });
    let later_step_script = format!("set -euo pipefail\nbrew test --verbose {TAP}/{FORMULA}");
    let later_step = Command::new("bash")
        .args(["-euo", "pipefail", "-c", &later_step_script])
        .env("PATH", &applied_path)
        .env("FAKE_ROGUE_BREW_LOG", &fixture.rogue_log)
        .output()
        .expect("run old separate brew test after applying GITHUB_PATH");
    assert!(later_step.status.success());
    assert_eq!(
        fs::read_to_string(&fixture.rogue_log)
            .expect("old split test is hijacked by rogue brew")
            .lines()
            .collect::<Vec<_>>(),
        vec![format!("test --verbose {TAP}/{FORMULA}")],
        "negative control proves GITHUB_PATH takes effect after a separate formula-load step"
    );
}

#[cfg(unix)]
fn assert_candidate_path_poisoning_controls(fixture: &Fixture, script: &str) {
    let attack_fixture = prepare_candidate_path_poisoning_fixture(fixture);
    assert_combined_candidate_step_keeps_trusted_brew(script, &attack_fixture);
    assert_split_candidate_steps_are_hijackable(&attack_fixture);
}

#[cfg(unix)]
fn let_executable(path: &std::path::Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, contents).expect("write executable fixture");
    let mut permissions = fs::metadata(path)
        .expect("read executable fixture permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("mark executable fixture executable");
}

fn run_runner_probe(
    step: &YamlValue,
    expected: &RunnerFacts,
    actual_os: &str,
    actual_arch: &str,
    actual_machine: &str,
) -> Output {
    let script = step["run"]
        .as_str()
        .expect("candidate runner preflight is a shell script");
    let script = format!("uname() {{ printf '%s\\n' \"$FAKE_UNAME\"; }}\n{script}");
    Command::new("bash")
        .args(["-euo", "pipefail", "-c", &script])
        .env("EXPECTED_PLATFORM", &expected.platform)
        .env("EXPECTED_RUNNER_OS", &expected.os)
        .env("EXPECTED_RUNNER_ARCH", &expected.arch)
        .env("EXPECTED_MACHINE", &expected.machine)
        .env("RUNNER_OS", actual_os)
        .env("RUNNER_ARCH", actual_arch)
        .env("FAKE_UNAME", actual_machine)
        .output()
        .expect("execute generated candidate runner preflight")
}

fn ruby_path_check_from_shell(script: &str) -> Option<&str> {
    let prefix = "brew ruby -e '";
    let start = script.find(prefix)? + prefix.len();
    let rest = &script[start..];
    let end = rest.find("' \"$TAP/$FORMULA\"")?;
    Some(&rest[..end])
}

fn ruby_service_check_from_shell(script: &str) -> Option<&str> {
    let prefix = "brew ruby -e '";
    let start = script.rfind(prefix)? + prefix.len();
    let rest = &script[start..];
    let end = rest.find("' \"$TAP/$FORMULA\"")?;
    Some(&rest[..end])
}

#[cfg(unix)]
fn run_formula_path_check(
    ruby_script: &str,
    stub_path: &std::path::Path,
    formula_argument: &str,
    formula_path: &std::path::Path,
    workspace: &std::path::Path,
    outside_formula: &std::path::Path,
    sentinel: &std::path::Path,
) -> Output {
    Command::new("ruby")
        .arg("-r")
        .arg(stub_path)
        .arg("-e")
        .arg(ruby_script)
        .arg(formula_argument)
        .arg(formula_path)
        .arg(workspace)
        .arg(formula_path)
        .env("FORMULA_FIXTURE", outside_formula)
        .env("FORMULA_SENTINEL", sentinel)
        .output()
        .expect("execute generated candidate formula path check in Ruby")
}

#[cfg(unix)]
fn run_formula_service_check(
    ruby_script: &str,
    stub_path: &std::path::Path,
    formula_argument: &str,
    formula_fixture: &std::path::Path,
    sentinel: &std::path::Path,
) -> Output {
    Command::new("ruby")
        .arg("-r")
        .arg(stub_path)
        .arg("-e")
        .arg(ruby_script)
        .arg(formula_argument)
        .env("FORMULA_FIXTURE", formula_fixture)
        .env("FORMULA_SENTINEL", sentinel)
        .output()
        .expect("execute generated candidate service inspection in Ruby")
}

#[cfg(unix)]
struct FormulaEscapePaths<'a> {
    stub_path: &'a std::path::Path,
    formula_argument: &'a str,
    candidate_formula: &'a std::path::Path,
    workspace: &'a std::path::Path,
    sentinel: &'a std::path::Path,
}

#[cfg(unix)]
fn run_path_then_service_check(
    path_script: &str,
    service_script: &str,
    paths: &FormulaEscapePaths<'_>,
    formula_fixture: &std::path::Path,
) -> Output {
    let script = "set -euo pipefail\nruby -r \"$FORMULARY_STUB\" -e \"$PATH_CHECK_RUBY\" \"$FORMULA_ARGUMENT\" \"$CANDIDATE_FORMULA\" \"$WORKSPACE_PATH\" \"$CANDIDATE_FORMULA\"\nruby -r \"$FORMULARY_STUB\" -e \"$SERVICE_CHECK_RUBY\" \"$FORMULA_ARGUMENT\"";
    Command::new("bash")
        .args(["-euo", "pipefail", "-c", script])
        .env("FORMULARY_STUB", paths.stub_path)
        .env("PATH_CHECK_RUBY", path_script)
        .env("SERVICE_CHECK_RUBY", service_script)
        .env("FORMULA_ARGUMENT", paths.formula_argument)
        .env("CANDIDATE_FORMULA", paths.candidate_formula)
        .env("WORKSPACE_PATH", paths.workspace)
        .env("FORMULA_FIXTURE", formula_fixture)
        .env("FORMULA_SENTINEL", paths.sentinel)
        .output()
        .expect("execute generated path guard before service formula loader")
}

#[cfg(unix)]
struct LinuxBrewSetupContext<'a> {
    linux_brew_path: &'a std::path::Path,
    github_path: &'a std::path::Path,
    fake_brew_dir: &'a std::path::Path,
    homebrew_prefix: &'a std::path::Path,
    tap_path: &'a std::path::Path,
    brew_log: &'a std::path::Path,
}

#[cfg(unix)]
fn run_linuxbrew_setup(
    script: &str,
    fixture: &Fixture,
    context: &LinuxBrewSetupContext<'_>,
) -> Output {
    let replacement = format!("linux_brew='{}'", context.linux_brew_path.display());
    let rewritten = script.replace(
        "linux_brew=/home/linuxbrew/.linuxbrew/bin/brew",
        &replacement,
    );
    assert_ne!(
        rewritten, script,
        "fixture rewrites the fixed Linuxbrew path"
    );
    Command::new("bash")
        .args(["-euo", "pipefail", "-c", &rewritten])
        .env("PATH", "/usr/bin:/bin")
        .env("RUNNER_OS", "Linux")
        .env("GITHUB_WORKSPACE", &fixture.root)
        .env("GITHUB_PATH", context.github_path)
        .env("TAP", TAP)
        .env("FORMULA", FORMULA)
        .env("FAKE_BREW_DIR", context.fake_brew_dir)
        .env("FAKE_HOMEBREW_PREFIX", context.homebrew_prefix)
        .env("FAKE_TAP_PATH", context.tap_path)
        .env("FAKE_BREW_LOG", context.brew_log)
        .output()
        .expect("execute generated Linux Homebrew setup step")
}

#[cfg(unix)]
fn assert_valid_formula_loaders(
    path_check_script: &str,
    service_ruby: &str,
    service_required: bool,
    paths: &FormulaEscapePaths<'_>,
) {
    let formula_source = "File.write(ENV.fetch(\"FORMULA_SENTINEL\"), \"executed\")\nFile.write(__FILE__, \"# rewritten by formula\\n\")\n";
    fs::write(paths.candidate_formula, formula_source)
        .expect("write in-checkout self-rewriting formula for sentinel control");
    let in_checkout = run_formula_path_check(
        path_check_script,
        paths.stub_path,
        paths.formula_argument,
        paths.candidate_formula,
        paths.workspace,
        paths.candidate_formula,
        paths.sentinel,
    );
    assert!(
        in_checkout.status.success(),
        "valid candidate path check succeeds without loading formula code: {}{}",
        String::from_utf8_lossy(&in_checkout.stdout),
        String::from_utf8_lossy(&in_checkout.stderr)
    );
    assert!(
        !paths.sentinel.exists(),
        "valid path check leaves candidate formula code unevaluated"
    );
    assert_eq!(
        fs::read_to_string(paths.candidate_formula).expect("read formula after path check"),
        formula_source,
        "valid path check preserves exact formula source bytes"
    );
    assert!(
        service_required,
        "service metadata is required for this fixture"
    );
    let valid_service = run_formula_service_check(
        service_ruby,
        paths.stub_path,
        paths.formula_argument,
        paths.candidate_formula,
        paths.sentinel,
    );
    assert!(
        valid_service.status.success(),
        "service loader succeeds after candidate path validation: {}{}",
        String::from_utf8_lossy(&valid_service.stdout),
        String::from_utf8_lossy(&valid_service.stderr)
    );
    assert_eq!(
        fs::read_to_string(paths.sentinel).expect("valid service inspection loads its fixture"),
        "executed",
        "service inspection evaluates formula code after path validation"
    );
    assert_ne!(
        fs::read_to_string(paths.candidate_formula).expect("read formula after service load"),
        formula_source,
        "self-rewriting service fixture proves service loader can mutate candidate bytes"
    );
    fs::remove_file(paths.sentinel).expect("reset formula sentinel before escape cases");
}

#[cfg(unix)]
fn assert_direct_formula_symlink_rejected(
    path_check_script: &str,
    service_ruby: &str,
    paths: &FormulaEscapePaths<'_>,
    outside_formula: &std::path::Path,
) {
    use std::os::unix::fs::symlink;

    fs::remove_file(paths.candidate_formula)
        .expect("remove in-checkout formula before symlink case");
    symlink(outside_formula, paths.candidate_formula).expect("create outside formula symlink");
    let direct_symlink = run_formula_path_check(
        path_check_script,
        paths.stub_path,
        paths.formula_argument,
        paths.candidate_formula,
        paths.workspace,
        outside_formula,
        paths.sentinel,
    );
    assert!(
        !direct_symlink.status.success(),
        "candidate path check rejects a formula file symlink to an outside file"
    );
    assert!(
        !paths.sentinel.exists(),
        "outside formula body is not loaded before direct symlink rejection"
    );
    let direct_service =
        run_path_then_service_check(path_check_script, service_ruby, paths, outside_formula);
    assert!(
        !direct_service.status.success(),
        "combined path and service steps stop at direct symlink path rejection"
    );
    assert!(
        !paths.sentinel.exists(),
        "direct symlink sentinel proves neither validator nor required service loader evaluates outside formula code"
    );

    fs::remove_file(paths.candidate_formula).expect("remove direct formula symlink");
}

#[cfg(unix)]
fn assert_formula_directory_symlink_rejected(
    path_check_script: &str,
    service_ruby: &str,
    paths: &FormulaEscapePaths<'_>,
    outside_root: &std::path::Path,
    outside_formula: &std::path::Path,
) {
    use std::os::unix::fs::symlink;

    let workspace_formula_dir = paths
        .candidate_formula
        .parent()
        .expect("candidate formula has a Formula directory");
    fs::remove_dir_all(workspace_formula_dir).expect("remove original Formula directory");
    let outside_formula_directory = outside_root.join("Formula");
    fs::create_dir_all(&outside_formula_directory)
        .expect("create outside formula directory symlink target");
    let outside_nested_formula = outside_formula_directory.join("preview.rb");
    fs::copy(outside_formula, &outside_nested_formula)
        .expect("copy sentinel formula under outside Formula directory");
    symlink(&outside_formula_directory, workspace_formula_dir)
        .expect("create outside Formula directory symlink");
    let escaped_directory = run_formula_path_check(
        path_check_script,
        paths.stub_path,
        paths.formula_argument,
        paths.candidate_formula,
        paths.workspace,
        &outside_nested_formula,
        paths.sentinel,
    );
    assert!(
        !escaped_directory.status.success(),
        "candidate path check rejects a Formula directory resolving outside the checkout"
    );
    assert!(
        !paths.sentinel.exists(),
        "outside formula body is not loaded before directory-escape rejection"
    );
    let escaped_service = run_path_then_service_check(
        path_check_script,
        service_ruby,
        paths,
        &outside_nested_formula,
    );
    assert!(
        !escaped_service.status.success(),
        "combined path and service steps stop at Formula directory escape rejection"
    );
    assert!(
        !paths.sentinel.exists(),
        "Formula directory escape sentinel proves required service loader never evaluates outside formula code"
    );
    fs::remove_file(workspace_formula_dir).expect("remove outside Formula directory symlink");
}

#[cfg(unix)]
fn assert_outside_formula_is_rejected_before_loading(
    path_check_script: &str,
    service_ruby: &str,
    service_required: bool,
    fixture: &Fixture,
) {
    let outside_root = fixture.base.join("outside");
    fs::create_dir_all(&outside_root).expect("create outside formula directory");
    let outside_formula = outside_root.join("preview.rb");
    fs::write(
        &outside_formula,
        "File.write(ENV.fetch(\"FORMULA_SENTINEL\"), \"executed\")\n",
    )
    .expect("write side-effecting outside formula fixture");
    let stub_path = fixture.base.join("formulary-stub.rb");
    fs::write(
        &stub_path,
        "module Formulary\n  FormulaFixture = Struct.new(:path) do\n    def service?\n      true\n    end\n  end\n  def self.factory(_name)\n    formula_path = ENV.fetch(\"FORMULA_FIXTURE\")\n    load formula_path\n    FormulaFixture.new(formula_path)\n  end\nend\n",
    )
    .expect("write Formulary fixture stub");
    let workspace_path = fixture.base.join("escape-workspace");
    let workspace_formula_dir = workspace_path.join("Formula");
    fs::create_dir_all(&workspace_formula_dir).expect("create escape-checkout formula directory");
    let candidate_formula = workspace_formula_dir.join("preview.rb");
    let workspace = workspace_path.as_path();
    let sentinel = fixture.base.join("formula-loaded-sentinel");
    let formula_argument = format!("{TAP}/{FORMULA}");
    let paths = FormulaEscapePaths {
        stub_path: &stub_path,
        formula_argument: &formula_argument,
        candidate_formula: &candidate_formula,
        workspace,
        sentinel: &sentinel,
    };

    assert_valid_formula_loaders(path_check_script, service_ruby, service_required, &paths);
    assert_direct_formula_symlink_rejected(
        path_check_script,
        service_ruby,
        &paths,
        &outside_formula,
    );
    assert_formula_directory_symlink_rejected(
        path_check_script,
        service_ruby,
        &paths,
        &outside_root,
        &outside_formula,
    );
}

fn platform_matrix(job: &YamlValue) -> &[YamlValue] {
    job["strategy"]["matrix"]["platform"]
        .as_sequence()
        .expect("candidate job has a native platform matrix")
}

fn platform_runner_cells(value: &YamlValue, cells: &mut Vec<(String, String)>) {
    match value {
        YamlValue::Mapping(mapping) => {
            let platform = ["id", "platform", "platform_id"]
                .iter()
                .find_map(|key| mapping_scalar_value(mapping, key));
            let runner = ["runner", "runs-on", "runs_on", "label"]
                .iter()
                .find_map(|key| mapping_scalar_value(mapping, key));
            if let (Some(platform), Some(runner)) = (platform, runner) {
                cells.push((platform, runner));
            }
            for value in mapping.values() {
                platform_runner_cells(value, cells);
            }
        }
        YamlValue::Sequence(values) => {
            for value in values {
                platform_runner_cells(value, cells);
            }
        }
        _ => {}
    }
}

fn mapping_scalar_value(mapping: &serde_yaml::Mapping, key: &str) -> Option<String> {
    let value = mapping.get(key)?;
    match value {
        YamlValue::String(value) => Some(value.to_owned()),
        _ => None,
    }
}

fn no_continue_on_error(value: &YamlValue) -> bool {
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    match mapping.get("continue-on-error") {
        None => true,
        Some(value) if value.as_bool() == Some(false) => true,
        Some(_) => false,
    }
}

fn no_continue_on_error_rejects_weakening(value: &YamlValue) -> bool {
    [
        YamlValue::Bool(true),
        YamlValue::String("${{ true }}".to_owned()),
    ]
    .into_iter()
    .all(|weakening| {
        let mut candidate = value.clone();
        let Some(mapping) = candidate.as_mapping_mut() else {
            return false;
        };
        mapping.insert("continue-on-error", weakening);
        !no_continue_on_error(&candidate)
    })
}

fn uses_matrix_runner(job: &YamlValue) -> bool {
    job["runs-on"].as_str() == Some("${{ matrix.platform.runner }}")
}

fn job_needs(job: &YamlValue, dependency: &str) -> bool {
    match job.get("needs") {
        Some(YamlValue::String(need)) => need == dependency,
        Some(YamlValue::Sequence(needs)) => {
            needs.iter().any(|need| need.as_str() == Some(dependency))
        }
        _ => false,
    }
}

fn readonly_contents_permission(value: &YamlValue) -> bool {
    let Some(mapping) = value.get("permissions").and_then(YamlValue::as_mapping) else {
        return false;
    };
    mapping.get("contents").and_then(YamlValue::as_str) == Some("read")
        && mapping.values().all(|permission| {
            permission
                .as_str()
                .is_some_and(|value| value == "read" || value == "none")
        })
}

fn is_unconditional_guard(value: &str) -> bool {
    let compact = value
        .replace("${{", "")
        .replace("}}", "")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    [
        "!cancelled()",
        "always()&&!cancelled()",
        "!cancelled()&&always()",
    ]
    .contains(&compact.as_str())
}

fn is_success_guard(value: &str) -> bool {
    value
        .replace("${{", "")
        .replace("}}", "")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        == "success()"
}

fn is_service_required_guard(value: &str) -> bool {
    value
        .replace("${{", "")
        .replace("}}", "")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        == "fromJSON(inputs.homebrew_preview).service_required"
}

fn configured_hosted_admission(fixture: &Fixture, workflow_name: &str) -> String {
    let workflow = workflow(fixture, workflow_name);
    let required = &workflow["jobs"]["ci-required"];
    let step = required_check_step(required);
    let raw = step["env"]["PROVIDER_ADMITTED_GITHUB_HOSTED"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "{workflow_name} required verdict carries the hosted-provider admission: {step:?}"
            )
        })
        .trim();
    raw.strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map(str::trim)
        .filter(|expression| !expression.is_empty())
        .unwrap_or_else(|| {
            panic!("{workflow_name} hosted-provider admission is a generated expression: {raw}")
        })
        .to_owned()
}

fn normalized_guard_clauses(value: &str) -> Vec<String> {
    let compact = value
        .replace("${{", "")
        .replace("}}", "")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
        .replace('"', "'");
    compact
        .chars()
        .filter(|character| *character != '(' && *character != ')')
        .collect::<String>()
        .split("&&")
        .map(str::to_owned)
        .collect()
}

fn is_configured_pr_homebrew_guard(
    value: &str,
    must_run_after_failure: bool,
    source_admission: &str,
) -> bool {
    let mut clauses = normalized_guard_clauses(value);
    let mut expected = vec![
        "github.event_name=='pull_request'".to_owned(),
        "inputs.unit=='homebrew'".to_owned(),
        "inputs.provider=='github-hosted'".to_owned(),
    ];
    expected.extend(normalized_guard_clauses(source_admission));
    if must_run_after_failure {
        expected.push("always".to_owned());
    }
    clauses.sort();
    expected.sort();
    clauses == expected
}

fn caller_guard_terms(value: &str) -> BTreeSet<String> {
    let compact = value
        .replace("${{", "")
        .replace("}}", "")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    compact
        .chars()
        .filter(|character| *character != '(' && *character != ')')
        .collect::<String>()
        .split("&&")
        .map(str::to_owned)
        .collect()
}

fn caller_selects_configured_homebrew(
    value: &str,
    provider: &str,
    plan_result: &str,
    selected_units: &str,
    cancelled: bool,
) -> bool {
    let expected = [
        "!cancelled".to_owned(),
        "needs.plan.result=='success'".to_owned(),
        "containsneeds.plan.outputs.units,'\"unit_id\":\"homebrew\"'".to_owned(),
        "true".to_owned(),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let selected_needle = r#""unit_id":"homebrew""#;
    caller_guard_terms(value) == expected
        && provider == "github-hosted"
        && plan_result == "success"
        && selected_units.contains(selected_needle)
        && !cancelled
}

fn candidate_guard_allows(
    value: &str,
    event_name: &str,
    provider: &str,
    unit: &str,
    must_run_after_failure: bool,
    source_admission: &str,
) -> bool {
    is_configured_pr_homebrew_guard(value, must_run_after_failure, source_admission)
        && event_name == "pull_request"
        && provider == "github-hosted"
        && unit == "homebrew"
}

fn exact_platform_cells(cells: &[(String, String)]) -> bool {
    let expected = EXPECTED_PLATFORMS
        .iter()
        .map(|(platform, runner)| (platform.to_string(), runner.to_string()))
        .collect::<BTreeSet<_>>();
    cells.len() == expected.len() && cells.iter().cloned().collect::<BTreeSet<_>>() == expected
}

fn assert_pr_planner_uses_source_head(fixture: &Fixture) {
    let pr = workflow(fixture, "ci-pr.yml");
    let plan = &pr["jobs"]["plan"];
    assert!(plan.is_mapping(), "PR workflow has the ordinary CI planner");
    assert!(
        !yaml_contains(plan, "github.event.pull_request.head.sha"),
        "ordinary CI planner does not repurpose candidate formula SHA"
    );
    assert_eq!(
        plan["outputs"]["head_sha"].as_str(),
        Some("${{ steps.plan.outputs.head_sha }}"),
        "the planner exposes the SHA from its plan step"
    );
    let planning_step = plan["steps"]
        .as_sequence()
        .expect("PR planner has steps")
        .iter()
        .find(|step| yaml_string_field(step, "id") == Some("plan"))
        .expect("PR planner has its plan step");
    let planning_script =
        yaml_string_field(planning_step, "run").expect("PR planner invokes its selection command");
    assert!(
        planning_script.contains("velnor-workflow plan"),
        "the plan step passes its selected head SHA to the planner"
    );
    assert_eq!(
        planning_step["env"]["HEAD_SHA"].as_str(),
        Some("${{ github.sha }}"),
        "ordinary CI planning remains pinned to GitHub's merge-result SHA"
    );
    assert!(
        planning_script.contains("velnor-workflow plan --config .github/ci/project.toml")
            && !planning_script.contains("--head-sha"),
        "the supported planner consumes its explicitly bound HEAD_SHA environment input"
    );
}

fn homebrew_pr_callers(fixture: &Fixture) -> Vec<YamlValue> {
    let pr = workflow(fixture, "ci-pr.yml");
    let pr_jobs = pr["jobs"]
        .as_mapping()
        .expect("PR workflow declares jobs mapping");
    pr_jobs
        .values()
        .filter(|job| {
            yaml_string_field(job, "uses")
                .is_some_and(|path| path.ends_with("ci-unit-homebrew.yml"))
        })
        .cloned()
        .collect()
}

fn assert_pr_caller_identity_bindings(caller: &YamlValue) {
    assert!(
        no_continue_on_error(caller),
        "Homebrew reusable result remains a required PR caller"
    );
    assert!(
        no_continue_on_error_rejects_weakening(caller),
        "caller oracle rejects boolean and expression-valued continue-on-error"
    );
    assert_eq!(
        caller["with"]["provider"].as_str(),
        Some("github-hosted"),
        "candidate installation uses the qualified native hosted provider"
    );
    assert_eq!(
        caller["with"]["head_sha"].as_str(),
        Some(PLAN_HEAD_SHA),
        "ordinary reusable checks keep the merge-result plan SHA"
    );
    assert!(
        is_direct_pr_head_binding(
            caller["with"]["homebrew_preview_head_sha"].as_str(),
            PR_HEAD_SHA
        ) && is_direct_pr_head_binding(
            caller["with"]["homebrew_preview_head_repository"].as_str(),
            PR_HEAD_REPOSITORY
        ),
        "candidate identity inputs bind only PR-head fields, optionally defaulting empty"
    );
    assert!(is_direct_pr_head_binding(
        Some(PR_HEAD_SHA_WITH_EMPTY_DEFAULT),
        PR_HEAD_SHA
    ));
    assert!(is_direct_pr_head_binding(
        Some(PR_HEAD_REPOSITORY_WITH_EMPTY_DEFAULT),
        PR_HEAD_REPOSITORY
    ));
    for bad_binding in [None, Some(""), Some(PR_HEAD_SHA_WITH_MERGE_FALLBACK)] {
        assert!(
            !is_direct_pr_head_binding(bad_binding, PR_HEAD_SHA),
            "candidate SHA binding oracle rejects missing, empty or fallback values"
        );
    }
    assert!(
        !is_direct_pr_head_binding(
            Some(PR_HEAD_REPOSITORY_WITH_BASE_FALLBACK),
            PR_HEAD_REPOSITORY
        ),
        "candidate repository binding oracle rejects base-repository fallback"
    );
    let preview = preview_input(caller);
    assert_eq!(preview["tap"].as_str(), Some(TAP));
    assert_eq!(preview["formula"].as_str(), Some(FORMULA));
    assert_eq!(preview["service_required"].as_bool(), Some(true));
    assert_eq!(
        preview,
        json!({ "tap": TAP, "formula": FORMULA, "service_required": true }),
        "reusable input contains only typed formula/service identity; platform set is compiled into the matrix"
    );
    assert!(
        caller
            .get("needs")
            .and_then(YamlValue::as_sequence)
            .is_some_and(|needs| needs.iter().any(|need| need.as_str() == Some("plan"))),
        "Homebrew caller depends on the source-head planner"
    );
}

fn assert_pr_caller_selection_guard(caller: &YamlValue) {
    let caller_guard = yaml_string_field(caller, "if")
        .expect("Homebrew caller is gated by planned unit selection");
    let selected_homebrew = json!([{ "unit_id": "homebrew" }]).to_string();
    assert!(caller_selects_configured_homebrew(
        caller_guard,
        "github-hosted",
        "success",
        &selected_homebrew,
        false,
    ));
    for rejected_guard in [
        format!("{caller_guard} && false"),
        format!("false && {caller_guard}"),
        caller_guard.replace("'\"unit_id\":\"homebrew\"'", "'\"unit_id\":\"stable\"'"),
        format!("{caller_guard} && needs.plan.result == 'failure'"),
    ] {
        assert!(
            !caller_selects_configured_homebrew(
                &rejected_guard,
                "github-hosted",
                "success",
                &selected_homebrew,
                false,
            ),
            "caller-selection oracle rejects forced skip, wrong unit, or failed-plan clauses"
        );
    }
    for (provider, result, selected, cancelled) in [
        ("velnor", "success", selected_homebrew.as_str(), false),
        (
            "github-hosted",
            "failure",
            selected_homebrew.as_str(),
            false,
        ),
        ("github-hosted", "success", "stable", false),
        ("github-hosted", "success", selected_homebrew.as_str(), true),
    ] {
        assert!(
            !caller_selects_configured_homebrew(
                caller_guard,
                provider,
                result,
                selected,
                cancelled,
            ),
            "caller guard rejects wrong provider, failed plan, wrong selection, or cancellation"
        );
    }
}

fn assert_candidate_checkout_step(steps: &[YamlValue], file: &str, job_id: &str) {
    let checkout = steps
        .iter()
        .find(|step| {
            yaml_string_field(step, "uses")
                .is_some_and(|action| action.contains("actions/checkout"))
        })
        .unwrap_or_else(|| panic!("candidate job {file}:{job_id} checks out the candidate tap"));
    assert_eq!(
        checkout["with"]["persist-credentials"].as_bool(),
        Some(false),
        "candidate checkout does not persist credentials: {file}:{job_id}"
    );
    assert_eq!(
        checkout["with"]["repository"].as_str(),
        Some(INPUT_HEAD_REPOSITORY),
        "checkout repository follows the selected PR head repository"
    );
    let ref_value = yaml_string_field(&checkout["with"], "ref")
        .expect("candidate checkout pins its separate formula revision");
    assert_eq!(
        compact_lowercase(ref_value),
        compact_lowercase(INPUT_HEAD_SHA),
        "candidate checkout pins exactly the candidate formula head SHA"
    );
    assert_eq!(
        checkout["with"]["path"].as_str(),
        Some("."),
        "candidate checkout is rooted at GITHUB_WORKSPACE so formula path proof targets it"
    );
}

fn assert_candidate_checkout_sha(steps: &[YamlValue], file: &str, job_id: &str) {
    let sha_check = steps
        .iter()
        .find(|step| yaml_string_field(step, "name") == Some("Verify candidate checkout SHA"))
        .expect("candidate checkout SHA is verified after checkout");
    assert_eq!(
        sha_check["env"]["EXPECTED_HEAD_SHA"].as_str(),
        Some(INPUT_HEAD_SHA),
        "checkout SHA comparison uses the candidate-specific input"
    );
    let sha_script =
        yaml_string_field(sha_check, "run").expect("candidate SHA verification runs a shell check");
    assert!(
        sha_script.contains("git rev-parse HEAD")
            && sha_script.contains("[[ \"$actual_head\" == \"$EXPECTED_HEAD_SHA\" ]]"),
        "actual checkout commit must equal the candidate formula head: {file}:{job_id}"
    );
    let expected_sha = "a".repeat(40);
    let matching = run_candidate_sha_check(sha_check, &expected_sha, &expected_sha);
    assert!(matching.status.success(), "matching checkout SHA passes");
    let wrong_sha = "b".repeat(40);
    let mismatching = run_candidate_sha_check(sha_check, &expected_sha, &wrong_sha);
    assert!(
        !mismatching.status.success()
            && String::from_utf8_lossy(&mismatching.stderr).contains("candidate checkout mismatch"),
        "generated checkout SHA guard rejects a different commit with a diagnostic"
    );
}

fn assert_candidate_pr_identity(steps: &[YamlValue], file: &str, job_id: &str) {
    let identity_name = "Validate candidate pull request identity";
    let identity_check = steps
        .iter()
        .find(|step| yaml_string_field(step, "name") == Some(identity_name))
        .expect("candidate install job validates its PR-head identity before checkout");
    let position = |expected_name: &str| {
        steps
            .iter()
            .position(|step| yaml_string_field(step, "name") == Some(expected_name))
            .unwrap_or_else(|| {
                panic!("candidate job has ordered step {expected_name}: {file}:{job_id}")
            })
    };
    assert!(
        position(identity_name) < position("Checkout candidate tap head"),
        "malformed or missing PR identity fails before candidate checkout"
    );
    for (name, expected) in [
        ("CANDIDATE_HEAD_SHA", INPUT_HEAD_SHA),
        ("CANDIDATE_HEAD_REPOSITORY", INPUT_HEAD_REPOSITORY),
        ("EVENT_HEAD_SHA", PR_HEAD_SHA),
        ("EVENT_HEAD_REPOSITORY", PR_HEAD_REPOSITORY),
    ] {
        assert_eq!(
            identity_check["env"][name].as_str(),
            Some(expected),
            "PR identity validation binds the exact candidate and event fields"
        );
    }
    let expected_sha = "a".repeat(40);
    let valid_repo = "fork-owner/preview-tap";
    let matching = run_candidate_pr_identity_check(
        identity_check,
        &expected_sha,
        valid_repo,
        &expected_sha,
        valid_repo,
    );
    assert!(
        matching.status.success(),
        "valid matching PR-head identity passes"
    );
    assert_invalid_candidate_identities(identity_check, &expected_sha, valid_repo);
}

fn assert_invalid_candidate_identities(
    identity_check: &YamlValue,
    expected_sha: &str,
    valid_repo: &str,
) {
    let wrong_sha = "b".repeat(40);
    let failures: [(&str, &str, &str, &str, &str); 12] = [
        ("", valid_repo, expected_sha, valid_repo, "missing SHA"),
        (
            "not-a-sha",
            valid_repo,
            expected_sha,
            valid_repo,
            "malformed SHA",
        ),
        (
            "same-malformed-sha",
            valid_repo,
            "same-malformed-sha",
            valid_repo,
            "equal malformed SHAs",
        ),
        (
            expected_sha,
            "",
            expected_sha,
            valid_repo,
            "missing repository",
        ),
        (
            expected_sha,
            valid_repo,
            "",
            valid_repo,
            "missing event SHA",
        ),
        (
            expected_sha,
            valid_repo,
            expected_sha,
            "",
            "missing event repository",
        ),
        (
            expected_sha,
            "malformed-repository",
            expected_sha,
            valid_repo,
            "malformed repository",
        ),
        (
            expected_sha,
            "malformed-repository",
            expected_sha,
            "malformed-repository",
            "equal malformed repositories",
        ),
        (
            expected_sha,
            valid_repo,
            "malformed-event-sha",
            valid_repo,
            "malformed event SHA",
        ),
        (
            expected_sha,
            valid_repo,
            expected_sha,
            "malformed-event-repository",
            "malformed event repository",
        ),
        (
            &wrong_sha,
            valid_repo,
            expected_sha,
            valid_repo,
            "nonmatching event SHA",
        ),
        (
            expected_sha,
            "other-owner/preview-tap",
            expected_sha,
            valid_repo,
            "nonmatching event repository",
        ),
    ];
    for (candidate_sha, candidate_repo, event_sha, event_repo, reason) in failures {
        let rejected = run_candidate_pr_identity_check(
            identity_check,
            candidate_sha,
            candidate_repo,
            event_sha,
            event_repo,
        );
        assert!(
            !rejected.status.success(),
            "generated PR identity guard rejects {reason}"
        );
    }
}

#[test]
fn candidate_checkout_is_the_tested_tap() {
    let fixture = fixture(Some(
        "\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\"",
    ));
    generate_ok(&fixture);
    let candidates = candidate_install_jobs(&fixture);
    assert!(!candidates.is_empty(), "candidate install job is generated");
    assert_pr_planner_uses_source_head(&fixture);
    let callers = homebrew_pr_callers(&fixture);
    assert!(
        !callers.is_empty(),
        "PR workflow calls the generated Homebrew unit"
    );
    for caller in &callers {
        assert_pr_caller_identity_bindings(caller);
        assert_pr_caller_selection_guard(caller);
    }
    for (file, job_id, job) in &candidates {
        let steps = job["steps"].as_sequence().expect("candidate job has steps");
        assert_candidate_checkout_step(steps, file, job_id);
        assert_candidate_checkout_sha(steps, file, job_id);
        assert_candidate_pr_identity(steps, file, job_id);
        let setup = steps
            .iter()
            .find(|step| yaml_string_field(step, "name") == Some("Set up Homebrew candidate tap"))
            .expect("candidate tap setup links the candidate checkout");
        let setup_script =
            yaml_string_field(setup, "run").expect("candidate tap setup is a shell step");
        let formula_check = steps
            .iter()
            .find(|step| {
                yaml_string_field(step, "name")
                    == Some("Verify, install, and test candidate formula")
            })
            .expect("candidate formula is validated, installed, and tested in one step");
        let formula_path_script = yaml_string_field(formula_check, "run")
            .expect("candidate formula check is a shell command");
        assert!(
            setup_script.contains("\"$GITHUB_WORKSPACE/Formula/$FORMULA.rb\"")
                && setup_script.contains("ln -s \"$GITHUB_WORKSPACE\""),
            "candidate formula file and tap link come from this checkout: {file}:{job_id}"
        );
        assert!(
            formula_path_script.contains("\"$GITHUB_WORKSPACE/Formula/$FORMULA.rb\"")
                && formula_path_script.contains("Formulary.factory"),
            "the exact local candidate formula is validated before Homebrew loads it: {file}:{job_id}"
        );
    }
}

fn candidate_source_admission(fixture: &Fixture) -> String {
    let main = configured_hosted_admission(fixture, "ci-main.yml");
    let pr = configured_hosted_admission(fixture, "ci-pr.yml");
    assert_eq!(
        pr, main,
        "PR and default-branch required checks use the same hosted-provider admission expression"
    );
    pr
}

fn assert_candidate_matrix_guard(job: &YamlValue, source_admission: &str) {
    let guard = yaml_string_field(job, "if")
        .expect("candidate matrix is explicitly gated to the selected Homebrew PR unit");
    assert!(
        is_configured_pr_homebrew_guard(guard, false, source_admission),
        "candidate matrix runs only for the selected Homebrew PR unit and carries source admission: {guard}"
    );
    assert!(candidate_guard_allows(
        guard,
        "pull_request",
        "github-hosted",
        "homebrew",
        false,
        source_admission,
    ));
    for (event, provider, unit) in [
        ("push", "github-hosted", "homebrew"),
        ("pull_request", "velnor", "homebrew"),
        ("pull_request", "github-hosted", "stable"),
    ] {
        assert!(
            !candidate_guard_allows(guard, event, provider, unit, false, source_admission),
            "candidate guard rejects non-PR, unsupported-provider or wrong-unit inputs"
        );
    }
    assert!(!is_configured_pr_homebrew_guard(
        &format!("{guard} && false"),
        false,
        source_admission,
    ));
    let wrong_provider = guard.replace(
        "inputs.provider == 'github-hosted'",
        "inputs.provider == 'velnor'",
    );
    assert!(!is_configured_pr_homebrew_guard(
        &wrong_provider,
        false,
        source_admission,
    ));
    let wrong_admission = guard.replacen(source_admission, "false", 1);
    assert!(
        !is_configured_pr_homebrew_guard(&wrong_admission, false, source_admission),
        "candidate guard oracle rejects admission different from the configured source"
    );
    if job["strategy"]["matrix"].is_mapping() {
        assert_eq!(
            job["strategy"]["fail-fast"].as_bool(),
            Some(false),
            "one failing native cell must not cancel other platform checks"
        );
    }
}

fn candidate_runner_probe(job: &YamlValue) -> YamlValue {
    assert!(
        uses_matrix_runner(job),
        "candidate matrix uses its native runner"
    );
    let mut static_runner = job.clone();
    static_runner["runs-on"] = YamlValue::String("ubuntu-24.04".to_owned());
    assert!(
        !uses_matrix_runner(&static_runner),
        "runner oracle rejects a static runner that bypasses native matrix placement"
    );
    let steps = job["steps"]
        .as_sequence()
        .expect("candidate matrix has steps");
    let probe = steps
        .iter()
        .find(|step| {
            yaml_string_field(step, "run").is_some_and(|script| script.contains("RUNNER_OS"))
        })
        .expect("candidate matrix probes its native runner")
        .clone();
    let script = yaml_string_field(&probe, "run").expect("runner probe is a script");
    let env = probe["env"]
        .as_mapping()
        .expect("runner probe receives native facts");
    for (name, expected) in [
        ("EXPECTED_PLATFORM", "${{ matrix.platform.id }}"),
        ("EXPECTED_RUNNER_OS", "${{ matrix.platform.os }}"),
        ("EXPECTED_RUNNER_ARCH", "${{ matrix.platform.arch }}"),
        ("EXPECTED_MACHINE", "${{ matrix.platform.machine }}"),
    ] {
        assert_eq!(env.get(name).and_then(YamlValue::as_str), Some(expected));
    }
    assert!(
        script.contains("[[ \"$RUNNER_OS\" == \"$EXPECTED_RUNNER_OS\" ]]")
            && script.contains("[[ \"$RUNNER_ARCH\" == \"$EXPECTED_RUNNER_ARCH\" ]]")
            && script.contains("[[ \"$actual_machine\" == \"$EXPECTED_MACHINE\" ]]")
            && script.contains("actual_machine=\"$(uname -m)\""),
        "runner preflight compares OS, architecture and uname to its matrix cell"
    );
    probe
}

fn assert_native_platform_facts(
    job: &YamlValue,
    probe: &YamlValue,
) -> BTreeSet<(String, String, String, String, String)> {
    let mut actual = BTreeSet::new();
    for cell in platform_matrix(job) {
        let mapping = cell
            .as_mapping()
            .expect("native platform cell is a mapping");
        let platform = mapping_scalar_value(mapping, "id").expect("platform ID");
        let runner = mapping_scalar_value(mapping, "runner").expect("runner label");
        let os = mapping_scalar_value(mapping, "os").expect("runner OS");
        let arch = mapping_scalar_value(mapping, "arch").expect("runner architecture");
        let machine = mapping_scalar_value(mapping, "machine").expect("machine architecture");
        let facts = RunnerFacts {
            platform: platform.clone(),
            os: os.clone(),
            arch: arch.clone(),
            machine: machine.clone(),
        };
        actual.insert((
            platform.clone(),
            os.clone(),
            arch.clone(),
            machine.clone(),
            runner,
        ));
        let matching = run_runner_probe(probe, &facts, &os, &arch, &machine);
        assert!(
            matching.status.success(),
            "runner preflight accepts native cell {platform}"
        );
        for (actual_os, actual_arch, actual_machine) in [
            ("Windows", arch.as_str(), machine.as_str()),
            (os.as_str(), "wrong-architecture", machine.as_str()),
            (os.as_str(), arch.as_str(), "wrong-machine"),
        ] {
            let mismatch = run_runner_probe(probe, &facts, actual_os, actual_arch, actual_machine);
            assert!(
                !mismatch.status.success(),
                "runner preflight rejects mismatched facts for {platform}"
            );
        }
    }
    actual
}

fn expected_native_platform_facts() -> BTreeSet<(String, String, String, String, String)> {
    EXPECTED_RUNNER_FACTS
        .iter()
        .map(|(platform, os, arch, machine, runner)| {
            (
                platform.to_string(),
                os.to_string(),
                arch.to_string(),
                machine.to_string(),
                runner.to_string(),
            )
        })
        .collect()
}

fn assert_candidate_result_gate(
    all_jobs: &[(String, String, YamlValue)],
    file: &str,
    install_job_id: &str,
    source_admission: &str,
) {
    let (_, _, result_job) = all_jobs
        .iter()
        .find(|(candidate_file, _, job)| candidate_file == file && job_needs(job, install_job_id))
        .unwrap_or_else(|| {
            panic!("candidate result gate waits for matrix {file}:{install_job_id}")
        });
    assert!(no_continue_on_error(result_job));
    assert!(no_continue_on_error_rejects_weakening(result_job));
    let guard = yaml_string_field(result_job, "if")
        .expect("candidate result gate has explicit always-run condition");
    assert!(is_configured_pr_homebrew_guard(
        guard,
        true,
        source_admission
    ));
    assert!(!is_configured_pr_homebrew_guard(
        guard,
        false,
        source_admission
    ));
    assert!(candidate_guard_allows(
        guard,
        "pull_request",
        "github-hosted",
        "homebrew",
        true,
        source_admission
    ));
    assert!(!candidate_guard_allows(
        guard,
        "push",
        "github-hosted",
        "homebrew",
        true,
        source_admission
    ));
    let result_step = result_job["steps"]
        .as_sequence()
        .expect("result gate has steps")
        .iter()
        .find(|step| step["env"]["CANDIDATE_RESULT"].as_str().is_some())
        .expect("result gate passes matrix result into shell verdict");
    assert!(no_continue_on_error(result_step));
    assert!(no_continue_on_error_rejects_weakening(result_step));
    let expected = format!("{}{{{{ needs.{install_job_id}.result }}}}", "$");
    assert_eq!(
        result_step["env"]["CANDIDATE_RESULT"].as_str(),
        Some(expected.as_str())
    );
    let script = yaml_string_field(result_step, "run").expect("result gate runs a shell verdict");
    assert!(
        script.contains("if [[ \"$CANDIDATE_RESULT\" != success ]]") && script.contains("exit 1")
    );
    assert!(run_candidate_result_gate(result_step, Some("success"))
        .status
        .success());
    for result in ["failure", "skipped", "cancelled"] {
        assert!(!run_candidate_result_gate(result_step, Some(result))
            .status
            .success());
    }
    assert!(!run_candidate_result_gate(result_step, None)
        .status
        .success());
}

fn assert_unsupported_provider_is_rejected() {
    let non_hosted = fixture_with_service_requirement(
        Some("\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\""),
        true,
    );
    let path = non_hosted.root.join(".github-gen/velnor-workflow.toml");
    let config = fs::read_to_string(&path)
        .expect("read negative provider fixture")
        .replace(
            "providers = [\"github-hosted\"]",
            "providers = [\"velnor\"]",
        )
        .replace(
            "automatic_providers = [\"github-hosted\"]",
            "automatic_providers = [\"velnor\"]",
        )
        .replace(
            "[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]",
            "[workflow.selectors.velnor]\nruns_on = [\"self-hosted\", \"synthetic-target\"]",
        );
    fs::write(path, config).expect("write unsupported-provider fixture");
    let output = run_generate(&non_hosted);
    assert!(
        !output.status.success(),
        "provider set without GitHub-hosted is rejected"
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    assert!(diagnostic.contains("github-hosted") && diagnostic.contains("provider"));
}

fn assert_required_homebrew_aggregate(fixture: &Fixture) {
    let pr = workflow(fixture, "ci-pr.yml");
    let required = &pr["jobs"]["ci-required"];
    assert!(required.is_mapping(), "the required aggregate is generated");
    let needs = required["needs"]
        .as_sequence()
        .expect("required aggregate declares caller dependencies");
    let step = required_check_step(required);
    assert!(no_continue_on_error(required));
    assert!(no_continue_on_error_rejects_weakening(required));
    assert!(no_continue_on_error(step));
    assert!(no_continue_on_error_rejects_weakening(step));
    let text = yaml_string_field(&step["env"], "EXPECTED_CALLERS")
        .expect("required aggregate freezes caller contract");
    let expected: Vec<JsonValue> = serde_json::from_str(text).expect("expected callers are JSON");
    let homebrew_job = expected
        .iter()
        .find(|caller| caller["unit_id"] == "homebrew")
        .and_then(|caller| caller["job_id"].as_str())
        .expect("Homebrew caller appears in required expected set");
    assert!(needs.iter().any(|need| need.as_str() == Some(homebrew_job)));
    let guard =
        yaml_string_field(required, "if").expect("required aggregate has explicit status guard");
    assert!(
        is_unconditional_guard(guard),
        "ci-required runs after failed or skipped callers"
    );
    assert!(
        run_required_verdict(required, homebrew_job, Some("success"))
            .status
            .success()
    );
    for result in ["failure", "skipped", "cancelled"] {
        assert!(!run_required_verdict(required, homebrew_job, Some(result))
            .status
            .success());
    }
    assert!(!run_required_verdict(required, homebrew_job, None)
        .status
        .success());
}

#[test]
fn required_platforms_are_rendered() {
    let fixture = fixture(Some(
        "\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\"",
    ));
    generate_ok(&fixture);
    let candidates = candidate_install_jobs(&fixture);
    assert!(!candidates.is_empty(), "candidate install job is generated");
    let source_admission = candidate_source_admission(&fixture);
    let mut rendered = Vec::new();
    for (_, _, job) in &candidates {
        assert_candidate_matrix_guard(job, &source_admission);
        let probe = candidate_runner_probe(job);
        let actual = assert_native_platform_facts(job, &probe);
        assert_eq!(
            actual,
            expected_native_platform_facts(),
            "all platform facts match native runner contract"
        );
        platform_runner_cells(job, &mut rendered);
    }
    assert!(
        exact_platform_cells(&rendered),
        "matrix has exactly the four required native runner cells"
    );
    let mut missing = rendered.clone();
    missing.retain(|(platform, _)| platform != "linux-arm64");
    assert!(
        !exact_platform_cells(&missing),
        "required-cell oracle rejects missing Linux ARM64"
    );
    let mut duplicate = rendered.clone();
    duplicate.push(rendered[0].clone());
    assert!(
        !exact_platform_cells(&duplicate),
        "required-cell oracle rejects a duplicate runner"
    );
    let all = all_jobs(&fixture);
    for (file, install_job_id, _) in &candidates {
        assert_candidate_result_gate(&all, file, install_job_id, &source_admission);
    }
    assert_unsupported_provider_is_rejected();
    assert_required_homebrew_aggregate(&fixture);
}

fn assert_candidate_required_steps(steps: &[YamlValue], file: &str, job_id: &str) {
    for name in [
        "Require GitHub-hosted candidate runner",
        "Validate candidate pull request identity",
        "Verify Homebrew candidate runner",
        "Checkout candidate tap head",
        "Verify candidate checkout SHA",
        "Set up Homebrew candidate tap",
        "Verify, install, and test candidate formula",
    ] {
        let step = steps
            .iter()
            .find(|step| yaml_string_field(step, "name") == Some(name))
            .unwrap_or_else(|| panic!("candidate job has required step {name}: {file}:{job_id}"));
        if let Some(guard) = yaml_string_field(step, "if") {
            assert!(
                is_success_guard(guard),
                "candidate safety/install step cannot be skipped: {file}:{job_id}:{name}: {guard}"
            );
        }
    }
    for step in steps.iter().filter(|step| {
        yaml_contains(step, "brew install")
            || yaml_contains(step, "brew test")
            || yaml_contains(step, "service?")
    }) {
        if let Some(guard) = yaml_string_field(step, "if") {
            let service_check = yaml_contains(step, "service?");
            assert!(
                is_success_guard(guard)
                    || is_unconditional_guard(guard)
                    || (service_check && is_service_required_guard(guard)),
                "candidate install/test/metadata steps cannot be skipped: {file}:{job_id}: {guard}"
            );
        }
    }
}

fn candidate_step_position(steps: &[YamlValue], name: &str, file: &str, job_id: &str) -> usize {
    steps
        .iter()
        .position(|step| yaml_string_field(step, "name") == Some(name))
        .unwrap_or_else(|| panic!("candidate job has ordered step {name}: {file}:{job_id}"))
}

fn assert_candidate_step_order(steps: &[YamlValue], file: &str, job_id: &str) -> usize {
    let hosted_runner = candidate_step_position(
        steps,
        "Require GitHub-hosted candidate runner",
        file,
        job_id,
    );
    let identity = candidate_step_position(
        steps,
        "Validate candidate pull request identity",
        file,
        job_id,
    );
    let runner = candidate_step_position(steps, "Verify Homebrew candidate runner", file, job_id);
    let checkout = candidate_step_position(steps, "Checkout candidate tap head", file, job_id);
    let sha = candidate_step_position(steps, "Verify candidate checkout SHA", file, job_id);
    let setup = candidate_step_position(steps, "Set up Homebrew candidate tap", file, job_id);
    let formula = candidate_step_position(
        steps,
        "Verify, install, and test candidate formula",
        file,
        job_id,
    );
    assert!(
        hosted_runner == 0
            && hosted_runner < identity
            && identity < runner
            && runner < checkout
            && checkout < sha
            && sha < setup
            && setup < formula,
        "runner, checkout, and tap identity validate before formula loading/install/testing: {file}:{job_id}"
    );
    let formula_script = yaml_string_field(&steps[formula], "run")
        .expect("candidate formula checks run in one shell step");
    for command in [
        "Formulary.factory(ARGV.fetch(0))",
        "brew install --build-from-source --verbose",
        "brew test --verbose",
        "formula.service?",
    ] {
        assert!(
            formula_script.contains(command),
            "candidate formula validation/install/test/service runs in one terminal step ({command}): {file}:{job_id}"
        );
    }
    assert!(
        steps.iter().skip(formula + 1).all(|step| {
            !yaml_string_field(step, "run").is_some_and(|script| script.contains("brew "))
        }),
        "no later candidate step invokes Homebrew after formula code can change runner command files: {file}:{job_id}"
    );
    formula
}

fn assert_homebrew_setup_script(setup_script: &str, file: &str, job_id: &str) {
    for fragment in [
        "if [[ \"$RUNNER_OS\" == Linux ]]; then",
        "linux_brew=/home/linuxbrew/.linuxbrew/bin/brew",
        "[[ -x \"$linux_brew\" ]]",
        "\"$linux_brew\" shellenv",
        "$HOMEBREW_PREFIX/bin",
        "$HOMEBREW_PREFIX/sbin",
        "$GITHUB_PATH",
        "command -v brew",
        "brew --version",
    ] {
        assert!(
            setup_script.contains(fragment),
            "native runner provisions/verifies Homebrew ({fragment}): {file}:{job_id}"
        );
    }
}

#[cfg(unix)]
fn assert_linux_homebrew_setup(fixture: &Fixture, setup_script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let prefix = fixture.base.join("fake-homebrew");
    let brew_dir = prefix.join("bin");
    fs::create_dir_all(brew_dir.join("../sbin")).expect("create fixture Homebrew prefix");
    let fake_brew = brew_dir.join("brew");
    let source = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_BREW_LOG"
case "${1-}" in
  shellenv) printf 'export HOMEBREW_PREFIX=%q\n' "$FAKE_HOMEBREW_PREFIX"; printf 'export PATH=%q:$PATH\n' "$FAKE_BREW_DIR" ;;
  --version) printf 'Homebrew fixture\n' ;;
  --repository) printf '%s\n' "$FAKE_TAP_PATH" ;;
  *) exit 64 ;;
esac
"#;
    fs::write(&fake_brew, source).expect("write fixture brew executable");
    let mut permissions = fs::metadata(&fake_brew)
        .expect("read fixture brew permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_brew, permissions).expect("make fixture brew executable");
    let github_path = fixture.base.join("github-path");
    let tap_path = fixture.base.join("brew-taps/example/preview-tap");
    let brew_log = fixture.base.join("fake-brew.log");
    let context = LinuxBrewSetupContext {
        linux_brew_path: &fake_brew,
        github_path: &github_path,
        fake_brew_dir: &brew_dir,
        homebrew_prefix: &prefix,
        tap_path: &tap_path,
        brew_log: &brew_log,
    };
    let setup = run_linuxbrew_setup(setup_script, fixture, &context);
    assert!(
        setup.status.success(),
        "Linux setup accepts isolated Homebrew fixture: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    assert_eq!(
        fs::canonicalize(&tap_path).expect("resolve candidate tap link"),
        fs::canonicalize(&fixture.root).expect("resolve candidate checkout")
    );
    let paths = fs::read_to_string(&github_path).expect("Linux setup persists Homebrew paths");
    assert!(paths.contains(&prefix.join("bin").display().to_string()));
    assert!(paths.contains(&prefix.join("sbin").display().to_string()));
    let calls = fs::read_to_string(&brew_log).expect("read fake brew calls");
    assert!(calls.lines().any(|line| line == "shellenv"));
    assert!(calls.lines().any(|line| line == "--version"));
    assert!(calls
        .lines()
        .any(|line| line == format!("--repository {TAP}")));
    let missing_context = LinuxBrewSetupContext {
        linux_brew_path: &fixture.base.join("missing-linuxbrew"),
        github_path: &fixture.base.join("missing-github-path"),
        fake_brew_dir: &brew_dir,
        homebrew_prefix: &prefix,
        tap_path: &fixture.base.join("missing-tap"),
        brew_log: &fixture.base.join("missing-brew.log"),
    };
    let missing = run_linuxbrew_setup(setup_script, fixture, &missing_context);
    assert!(
        !missing.status.success(),
        "Linux setup fails if documented Homebrew binary is missing"
    );
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Homebrew is unavailable"));
}

fn candidate_scripts(steps: &[YamlValue]) -> Vec<&str> {
    steps
        .iter()
        .filter_map(|step| yaml_string_field(step, "run"))
        .collect()
}

fn assert_candidate_formula_environment(job: &YamlValue) {
    assert_eq!(
        job["env"]["TAP"].as_str(),
        Some("${{ fromJSON(inputs.homebrew_preview).tap }}")
    );
    assert_eq!(
        job["env"]["FORMULA"].as_str(),
        Some("${{ fromJSON(inputs.homebrew_preview).formula }}")
    );
}

fn assert_candidate_formula_path(
    job: &YamlValue,
    content: &str,
    scripts: &[&str],
    file: &str,
    job_id: &str,
) -> String {
    assert!(
        scripts.iter().any(
            |script| script.contains("tap_path=\"$(brew --repository \"$TAP\")\"")
                && script.contains("ln -s \"$GITHUB_WORKSPACE\"")
        ),
        "candidate checkout is linked into the tap Homebrew resolves: {file}:{job_id}"
    );
    assert!(
        scripts
            .iter()
            .any(|script| script.contains("verify_candidate_formula_bytes")
                && script.contains("git rev-parse \"HEAD:Formula/$FORMULA.rb\"")
                && script.contains("git hash-object --no-filters")
                && script.contains("File.symlink?(formula_file)")
                && script.contains("workspace_prefix")
                && script.contains("expected.start_with?(workspace_prefix)")
                && script.contains("File.realpath(tap_formula_file) == expected")
                && script.contains("\"$TAP/$FORMULA\"")
                && script.contains("\"$GITHUB_WORKSPACE/Formula/$FORMULA.rb\"")
                && script.contains("\"$GITHUB_WORKSPACE\"")),
        "candidate formula path and committed bytes are verified without loading the formula: {file}:{job_id}"
    );
    let path_check = scripts
        .iter()
        .find_map(|script| ruby_path_check_from_shell(script))
        .unwrap_or_else(|| panic!("candidate path check exposes its Ruby guard: {file}:{job_id}"));
    assert!(
        !path_check.contains("Formulary.factory"),
        "path validation does not evaluate candidate formula code: {file}:{job_id}"
    );
    let direct = path_check
        .find("File.symlink?(formula_file)")
        .expect("guard rejects a direct formula symlink");
    let containment = path_check
        .find("expected.start_with?(workspace_prefix)")
        .expect("guard rejects resolved paths outside workspace");
    let tap_match = path_check
        .find("File.realpath(tap_formula_file) == expected")
        .expect("guard resolves the tap formula to the candidate file");
    assert!(
        direct < containment && containment < tap_match,
        "symlink and checkout containment checks precede the tap-path match"
    );
    assert!(
        content.contains("brew install --build-from-source"),
        "candidate formula is installed from source: {file}:{job_id}"
    );
    assert!(job.is_mapping());
    path_check.to_owned()
}

fn candidate_install_script<'script>(
    scripts: &[&'script str],
    file: &str,
    job_id: &str,
) -> &'script str {
    let script = scripts
        .iter()
        .find(|script| script.contains("brew install --build-from-source"))
        .unwrap_or_else(|| panic!("candidate job installs a formula: {file}:{job_id}"));
    assert!(
        script.contains("brew install --build-from-source --verbose \"$TAP/$FORMULA\""),
        "install targets typed candidate formula from local tap: {file}:{job_id}"
    );
    script
}

fn candidate_test_script<'script>(
    scripts: &[&'script str],
    file: &str,
    job_id: &str,
) -> &'script str {
    let script = scripts
        .iter()
        .find(|script| script.contains("brew test --verbose"))
        .unwrap_or_else(|| panic!("candidate job tests a formula: {file}:{job_id}"));
    assert!(
        script.contains("brew test --verbose \"$TAP/$FORMULA\""),
        "test runs the installed candidate formula: {file}:{job_id}"
    );
    script
}

fn assert_candidate_service_loader(
    steps: &[YamlValue],
    scripts: &[&str],
    formula_position: usize,
    file: &str,
    job_id: &str,
) -> String {
    let script = scripts
        .iter()
        .find(|script| {
            script.contains("brew ruby")
                && script.contains("service?")
                && script.contains("brew install --build-from-source")
                && script.contains("brew test --verbose")
        })
        .unwrap_or_else(|| {
            panic!("candidate service metadata shares formula check step: {file}:{job_id}")
        });
    let step = &steps[formula_position];
    assert_eq!(yaml_string_field(step, "run"), Some(*script));
    assert_eq!(
        yaml_string_field(&step["env"], "SERVICE_REQUIRED"),
        Some("${{ fromJSON(inputs.homebrew_preview).service_required }}")
    );
    assert!(
        script.contains("Formulary.factory(ARGV.fetch(0))")
            && script.contains("formula.service?")
            && script.contains(
                "abort \"candidate formula has no service declaration\" unless formula.service?"
            )
            && script.contains("\"$TAP/$FORMULA\"")
            && script.contains("if [[ \"$SERVICE_REQUIRED\" == true ]]; then"),
        "exact candidate service DSL is inspected without starting it and inside the formula-check terminal step: {file}:{job_id}"
    );
    assert_eq!(
        script.matches("verify_candidate_formula_bytes").count(),
        6,
        "byte guard brackets path validation and every formula-load boundary: {file}:{job_id}"
    );
    for (index, step) in steps.iter().enumerate() {
        if yaml_contains(step, "Formulary.factory") && yaml_contains(step, "service?") {
            assert!(
                index == formula_position,
                "every formula and service loader shares the guarded candidate terminal step: {file}:{job_id}:{index}"
            );
        }
    }
    let service_ruby = ruby_service_check_from_shell(script)
        .unwrap_or_else(|| panic!("candidate service Ruby program is present: {file}:{job_id}"));
    assert!(
        script.find("brew test --verbose") < script.rfind("brew ruby -e"),
        "service declaration check follows candidate test in the same terminal step: {file}:{job_id}"
    );
    let install = script
        .find("brew install --build-from-source")
        .expect("candidate install is present in the guarded step");
    let post_install = script[install..]
        .find("verify_candidate_formula_bytes\nbrew test --verbose")
        .map(|offset| install + offset)
        .expect("candidate bytes are checked after install and before test");
    let test = script
        .find("brew test --verbose")
        .expect("candidate test is present in the guarded step");
    let post_test = script[test..]
        .find("verify_candidate_formula_bytes\nif [[ \"$SERVICE_REQUIRED\" == true ]]")
        .map(|offset| test + offset)
        .expect("candidate bytes are checked after test and before service inspection");
    let service = script
        .rfind("brew ruby -e")
        .expect("candidate service inspection is present");
    let post_service = script[service..]
        .find("verify_candidate_formula_bytes")
        .map(|offset| service + offset)
        .expect("candidate bytes are checked after service inspection");
    assert!(
        install < post_install && post_install < test && test < post_test && post_test < service && service < post_service,
        "committed formula bytes are checked across install, test, and service loads: {file}:{job_id}"
    );
    service_ruby.to_owned()
}

#[cfg(unix)]
fn assert_candidate_install_controls(fixture: &Fixture, job_id: &str, script: &str) {
    let log = fixture.base.join(format!("fake-brew-install-{job_id}.log"));
    let result = run_candidate_brew_step(script, FORMULA, fixture, &log, None);
    assert!(
        result.status.success(),
        "generated install executes against fake brew: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let calls = fs::read_to_string(&log).expect("fake brew records generated install command");
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        vec![
            format!("install --build-from-source --verbose {TAP}/{FORMULA}"),
            format!("test --verbose {TAP}/{FORMULA}"),
        ]
    );
    assert!(
        exact_candidate_brew_call(calls.lines().next().unwrap_or_default(), "install")
            && exact_candidate_brew_call(calls.lines().nth(1).unwrap_or_default(), "test")
    );
    assert_candidate_path_poisoning_controls(fixture, script);
    let failure_log = fixture
        .base
        .join(format!("fake-brew-install-failure-{job_id}.log"));
    let failure = run_candidate_brew_step(script, FORMULA, fixture, &failure_log, Some("install"));
    assert!(
        !failure.status.success(),
        "candidate step propagates fake brew install failure"
    );
    assert_eq!(
        fs::read_to_string(&failure_log)
            .expect("read failing install control")
            .lines()
            .collect::<Vec<_>>(),
        vec![format!(
            "install --build-from-source --verbose {TAP}/{FORMULA}"
        )]
    );
    assert_candidate_formula_mutation_controls(fixture, script);
    let wrong_log = fixture.base.join(format!("fake-brew-wrong-{job_id}.log"));
    let wrong = run_candidate_brew_step(script, "different-formula", fixture, &wrong_log, None);
    assert!(
        !wrong.status.success(),
        "candidate step fails closed when its committed formula blob is missing"
    );
    assert!(
        !wrong_log.exists(),
        "no install or test reaches Homebrew for a formula without a checked-out blob"
    );
}

#[cfg(unix)]
fn assert_candidate_test_controls(fixture: &Fixture, job_id: &str, script: &str) {
    let log = fixture.base.join(format!("fake-brew-test-{job_id}.log"));
    let result = run_candidate_brew_step(script, FORMULA, fixture, &log, None);
    assert!(
        result.status.success(),
        "generated formula test executes against fake brew: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let calls = fs::read_to_string(&log).expect("fake brew records generated test command");
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        vec![
            format!("install --build-from-source --verbose {TAP}/{FORMULA}"),
            format!("test --verbose {TAP}/{FORMULA}"),
        ]
    );
    assert!(
        exact_candidate_brew_call(calls.lines().next().unwrap_or_default(), "install")
            && exact_candidate_brew_call(calls.lines().nth(1).unwrap_or_default(), "test")
    );
    let failure_log = fixture
        .base
        .join(format!("fake-brew-test-failure-{job_id}.log"));
    let failure = run_candidate_brew_step(script, FORMULA, fixture, &failure_log, Some("test"));
    assert!(
        !failure.status.success(),
        "candidate step propagates fake brew test failure"
    );
    assert_eq!(
        fs::read_to_string(&failure_log)
            .expect("read failing test control")
            .lines()
            .collect::<Vec<_>>(),
        vec![
            format!("install --build-from-source --verbose {TAP}/{FORMULA}"),
            format!("test --verbose {TAP}/{FORMULA}"),
        ]
    );
}

fn assert_no_daemon_launch(content: &str, file: &str, job_id: &str) {
    let lower = content.to_ascii_lowercase();
    for forbidden in [
        "brew services",
        "launchctl",
        "systemctl",
        "service start",
        "start_service",
    ] {
        assert!(
            !lower.contains(forbidden),
            "service inspection never starts daemon ({forbidden}): {file}:{job_id}"
        );
    }
}

fn assert_service_formula_fixture(fixture: &Fixture) {
    let formula = fs::read_to_string(fixture.root.join("Formula/preview.rb"))
        .expect("read service-bearing formula fixture");
    assert!(
        formula.contains("service do"),
        "fixture exercises service metadata"
    );
    assert!(formula.contains("test do"), "fixture declares formula test");
}

#[test]
fn install_and_test_steps_are_present() {
    let fixture = fixture(Some(
        "\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\"",
    ));
    generate_ok(&fixture);
    let candidates = candidate_install_jobs(&fixture);
    assert!(!candidates.is_empty(), "candidate install job is generated");
    let pr = workflow(&fixture, "ci-pr.yml");
    let caller = pr["jobs"]
        .as_mapping()
        .expect("PR workflow declares jobs")
        .values()
        .find(|job| {
            yaml_string_field(job, "uses")
                .is_some_and(|path| path.ends_with("ci-unit-homebrew.yml"))
        })
        .expect("PR workflow calls the Homebrew unit");
    let service_required = preview_input(caller)["service_required"]
        .as_bool()
        .expect("service requirement is a typed boolean");
    assert!(
        service_required,
        "service inspection loader is exercised with service_required=true"
    );
    for (file, job_id, job) in candidates {
        let content = yaml_text(&job);
        let steps = job["steps"].as_sequence().expect("candidate job has steps");
        assert_candidate_required_steps(steps, &file, &job_id);
        let path_position = assert_candidate_step_order(steps, &file, &job_id);
        let setup = steps
            .iter()
            .find(|step| yaml_string_field(step, "name") == Some("Set up Homebrew candidate tap"))
            .expect("Homebrew is initialized before resolving formula");
        let setup_script =
            yaml_string_field(setup, "run").expect("Homebrew setup executes in a shell");
        assert_homebrew_setup_script(setup_script, &file, &job_id);
        #[cfg(unix)]
        assert_linux_homebrew_setup(&fixture, setup_script);
        assert_candidate_formula_environment(&job);
        let scripts = candidate_scripts(steps);
        let install_script = candidate_install_script(&scripts, &file, &job_id);
        let test_script = candidate_test_script(&scripts, &file, &job_id);
        assert_eq!(
            install_script, test_script,
            "formula validation, install, test, and service inspection share one step: {file}:{job_id}"
        );
        let path_check = assert_candidate_formula_path(&job, &content, &scripts, &file, &job_id);
        let service_ruby =
            assert_candidate_service_loader(steps, &scripts, path_position, &file, &job_id);
        #[cfg(unix)]
        assert_outside_formula_is_rejected_before_loading(
            &path_check,
            &service_ruby,
            service_required,
            &fixture,
        );
        #[cfg(unix)]
        {
            assert_candidate_install_controls(&fixture, &job_id, install_script);
            assert_candidate_test_controls(&fixture, &job_id, test_script);
        }
        assert_no_daemon_launch(&content, &file, &job_id);
    }
    assert_service_formula_fixture(&fixture);
}

#[test]
fn candidate_install_shadows_inherited_configured_env() {
    let fixture = fixture_with_env(
        Some("\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\""),
        true,
        Some("TAP_TOKEN = \"${{ secrets.TAP_TOKEN }}\"\nCUSTOM_SETTING = \"verify-only\""),
    );
    generate_ok(&fixture);
    let generated = workflow(&fixture, "ci-unit-homebrew.yml");
    let verification = &generated["jobs"]["verify-github-hosted"];
    let inherited_token = verification
        .get("env")
        .and_then(|env| env.get("TAP_TOKEN"))
        .or_else(|| generated["env"].get("TAP_TOKEN"));
    assert_eq!(
        inherited_token.and_then(YamlValue::as_str),
        Some("${{ secrets.TAP_TOKEN }}"),
        "normal verification keeps the configured secret expression"
    );
    let inherited_setting = verification
        .get("env")
        .and_then(|env| env.get("CUSTOM_SETTING"))
        .or_else(|| generated["env"].get("CUSTOM_SETTING"));
    assert_eq!(
        inherited_setting.and_then(YamlValue::as_str),
        Some("verify-only"),
        "normal verification keeps ordinary configured env"
    );

    let candidate = &generated["jobs"]["homebrew-candidate-install"];
    assert_eq!(
        candidate["env"]["TAP_TOKEN"].as_str(),
        Some(""),
        "candidate install shadows inherited secrets"
    );
    assert_eq!(
        candidate["env"]["CUSTOM_SETTING"].as_str(),
        Some(""),
        "candidate install shadows every other inherited configured key"
    );
    let candidate_result = &generated["jobs"]["homebrew-candidate-result"];
    assert_eq!(
        candidate_result["env"]["TAP_TOKEN"].as_str(),
        Some(""),
        "candidate result shadows inherited secrets"
    );
    assert_eq!(
        candidate_result["env"]["CUSTOM_SETTING"].as_str(),
        Some(""),
        "candidate result shadows every other inherited configured key"
    );
    assert_candidate_formula_environment(candidate);
    assert_eq!(
        candidate["env"]["HOMEBREW_NO_AUTO_UPDATE"].as_str(),
        Some("1")
    );
    assert_eq!(
        candidate["env"]["HOMEBREW_NO_INSTALL_CLEANUP"].as_str(),
        Some("1")
    );

    assert_candidate_runner_environment_guard(candidate, &fixture);
}

fn assert_candidate_runner_environment_guard(candidate: &YamlValue, fixture: &Fixture) {
    let steps = candidate["steps"]
        .as_sequence()
        .expect("candidate job has steps");
    let runner_guard = steps.first().expect("runner guard is the first step");
    assert_eq!(
        yaml_string_field(runner_guard, "name"),
        Some("Require GitHub-hosted candidate runner")
    );
    let runner_script = yaml_string_field(runner_guard, "run")
        .expect("GitHub-hosted runner guard executes a shell check");
    assert!(
        runner_script.contains("RUNNER_ENVIRONMENT") && runner_script.contains("github-hosted"),
        "candidate formula checks require a hosted runner"
    );
    let install_script = steps
        .iter()
        .find(|step| {
            yaml_string_field(step, "name") == Some("Verify, install, and test candidate formula")
        })
        .and_then(|step| yaml_string_field(step, "run"))
        .expect("candidate install and test step runs Homebrew");
    let guarded_install_script = format!("{runner_script}\n{install_script}");
    #[cfg(unix)]
    {
        let self_hosted_log = fixture.base.join("self-hosted-candidate-brew.log");
        let self_hosted = run_candidate_brew_step_with_runner_environment(
            &guarded_install_script,
            FORMULA,
            fixture,
            &self_hosted_log,
            None,
            Some("self-hosted"),
        );
        assert!(
            !self_hosted.status.success(),
            "self-hosted runner fails before candidate formula install"
        );
        assert!(
            !self_hosted_log.exists(),
            "self-hosted runner never invokes the formula installer"
        );

        let hosted_log = fixture.base.join("github-hosted-candidate-brew.log");
        let hosted = run_candidate_brew_step_with_runner_environment(
            &guarded_install_script,
            FORMULA,
            fixture,
            &hosted_log,
            None,
            Some("github-hosted"),
        );
        assert!(
            hosted.status.success(),
            "GitHub-hosted native runner reaches candidate install"
        );
        assert_eq!(
            fs::read_to_string(hosted_log)
                .expect("read GitHub-hosted candidate install call")
                .lines()
                .collect::<Vec<_>>(),
            vec![
                format!("install --build-from-source --verbose {TAP}/{FORMULA}"),
                format!("test --verbose {TAP}/{FORMULA}"),
            ]
        );
    }
}

fn assert_candidate_jobs_have_no_credentials(candidates: &[(String, String, YamlValue)]) {
    for (file, job_id, job) in candidates {
        assert!(
            !contains_token_bearing_material(job, false),
            "parsed candidate job has no token/secret/credential input: {file}:{job_id}"
        );
        let text = yaml_text(job);
        assert!(
            !text.contains("contents: write") && !text.contains("write-all"),
            "candidate job has no write permission: {file}:{job_id}"
        );
        assert!(
            no_continue_on_error(job) && no_continue_on_error_rejects_weakening(job),
            "candidate installation remains required: {file}:{job_id}"
        );
        for step in job["steps"].as_sequence().expect("candidate job has steps") {
            assert!(
                no_continue_on_error(step) && no_continue_on_error_rejects_weakening(step),
                "candidate steps remain required: {file}:{job_id}"
            );
            if yaml_string_field(step, "uses")
                .is_some_and(|action| action.contains("actions/checkout"))
            {
                assert_eq!(
                    step["with"]["persist-credentials"].as_bool(),
                    Some(false),
                    "checkout credentials are not persisted: {file}:{job_id}"
                );
            }
        }
    }
}

fn assert_pr_callers_have_no_credentials(fixture: &Fixture) {
    let pr = workflow(fixture, "ci-pr.yml");
    assert!(
        !workflow_text(fixture, "ci-pr.yml").contains("pull_request_target"),
        "formula execution never uses pull_request_target"
    );
    let jobs = pr["jobs"]
        .as_mapping()
        .expect("PR workflow declares jobs mapping");
    for caller in jobs.values().filter(|job| {
        yaml_string_field(job, "uses").is_some_and(|path| path.ends_with("ci-unit-homebrew.yml"))
    }) {
        let passed_secrets = caller
            .get("secrets")
            .map(yaml_text)
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            !caller
                .get("with")
                .is_some_and(|value| contains_token_bearing_material(value, false))
                && !caller
                    .get("secrets")
                    .is_some_and(|value| contains_token_bearing_material(value, false))
                && !passed_secrets.contains("inherit"),
            "PR caller passes no secret/publication credential to formula validation"
        );
    }
}

fn assert_protected_workflows_have_no_credentials(
    fixture: &Fixture,
    candidates: &[(String, String, YamlValue)],
) {
    let mut protected = candidates
        .iter()
        .map(|(file, _, _)| file.clone())
        .collect::<BTreeSet<_>>();
    protected.insert("ci-pr.yml".to_owned());
    for (file, document) in all_workflows(fixture)
        .into_iter()
        .filter(|(file, _)| protected.contains(file))
    {
        assert!(
            !contains_token_bearing_material(&document, false),
            "full parsed candidate workflow has no token-bearing input/expression: {file}"
        );
    }
}

fn assert_credential_scanner_controls() {
    for text in [
        "jobs:\n  candidate:\n    steps:\n      - run: \"echo ${{ secrets['UNRECOGNIZED'] }}\"\n",
        "jobs:\n  candidate:\n    steps:\n      - run: \"echo ${{ github['token'] }}\"\n",
        "jobs:\n  candidate:\n    env:\n      ARBITRARY_AUTH: \"${{ vars.RELEASE_TOKEN }}\"\n",
        "jobs:\n  candidate:\n    env:\n      UNRECOGNIZED_TOKEN_NAME: \"fixture\"\n",
        "jobs:\n  candidate:\n    steps:\n      - run: \"echo ${GITHUB_TOKEN}\"\n",
        "jobs:\n  candidate:\n    steps:\n      - run: \"echo ${AUTH:-missing}\"\n",
        "jobs:\n  candidate:\n    env:\n      UNRELATED_NAME: \"$PUBLISH_CREDENTIAL\"\n",
        "jobs:\n  candidate:\n    env:\n      DYNAMIC_VALUE: \"${{ vars.ROTATING_SECRET }}\"\n",
        "jobs:\n  candidate:\n    env:\n      AUTH_MATERIAL: \"configured\"\n",
    ] {
        let control: YamlValue =
            serde_yaml::from_str(text).expect("credential scanner negative control is YAML");
        assert!(
            contains_token_bearing_material(&control, false),
            "credential scanner catches arbitrary workflow/expression/shell/env control"
        );
    }
    for safe in [
        "echo \"$HOME\"",
        "echo ${RUNNER_TEMP}",
        "${{ github.sha }}",
        "${{ github.event_name }}",
    ] {
        assert!(
            !has_credential_expression(safe),
            "ordinary runtime value remains allowed: {safe}"
        );
    }
}

fn assert_daemon_command_controls() {
    for read_only in [
        "brew ruby -e 'formula = Formulary.factory(ARGV.fetch(0)); formula.service?'",
        "brew services list",
        "brew services info preview",
        "launchctl list gui/501",
        "systemctl --user status preview.service",
        "brew ruby -e 'formula.service?'",
    ] {
        assert!(
            daemon_launch_commands(read_only).is_empty(),
            "read-only service inspection remains allowed: {read_only}"
        );
    }
    for control in [
        "brew services start preview",
        "sudo -E /usr/local/bin/brew services restart preview",
        "launchctl bootstrap gui/501 preview.plist",
        "launchctl -w load preview.plist",
        "sudo -u runner launchctl kickstart -k gui/501/com.example.preview",
        "sudo systemctl enable --now preview.service",
        "systemctl --user start preview.service",
        "/usr/bin/env -S systemctl --user --no-block restart preview.service",
        "systemd-run --unit=preview.service /usr/local/bin/preview",
        "service preview start",
        "rc-service preview restart",
        "initctl start preview",
        "supervisorctl start preview",
        "pm2 start app.js",
        "start-stop-daemon --start --exec /usr/local/bin/preview",
        "nohup /usr/local/bin/preview >/dev/null 2>&1 &",
        "start_service preview",
        "daemonize preview",
        "s6-svc -u /run/service/preview",
    ] {
        assert!(
            !daemon_launch_commands(control).is_empty(),
            "daemon scanner catches launch command control: {control}"
        );
    }
}

fn assert_parsed_daemon_command_controls() {
    let control: YamlValue = serde_yaml::from_str(
        "jobs:\n  candidate:\n    steps:\n      - name: hidden service launch\n        uses: example/service-start@v1\n      - name: runner command vector\n        uses: example/action@v1\n        with:\n          args: sudo systemctl --user start preview.service\n",
    ).expect("daemon negative control parses as a complete job");
    let commands = parsed_job_command_vectors(&control["jobs"]["candidate"]);
    assert!(
        commands.iter().any(|(path, _)| path == "job.steps[0].uses")
            && commands
                .iter()
                .any(|(path, _)| path == "job.steps[1].with.args"),
        "parsed daemon scan inventories action references and conventional args"
    );
    for (path, expected_launch) in [
        ("job.steps[0].uses", true),
        ("job.steps[1].uses", false),
        ("job.steps[1].with.args", true),
    ] {
        let command = commands
            .iter()
            .find_map(|(found, command)| (found == path).then_some(command))
            .unwrap_or_else(|| panic!("parsed daemon scan omits {path}: {commands:?}"));
        assert_eq!(
            !daemon_launch_commands(command).is_empty(),
            expected_launch,
            "parsed daemon scanner classifies action/command control at {path}: {command}"
        );
    }
    let nested: YamlValue = serde_yaml::from_str(
        "jobs:\n  candidate:\n    steps:\n      - name: shell input hidden in action data\n        uses: example/action@v1\n        with:\n          script: launchctl bootstrap gui/501 preview.plist\n          configuration:\n            runtime:\n              command: pm2 start app.js\n    custom:\n      workflow:\n        commands:\n          - nohup /usr/local/bin/preview >/dev/null 2>&1 &\n",
    ).expect("nested daemon negative control parses as complete job");
    let nested_commands = parsed_job_command_vectors(&nested["jobs"]["candidate"]);
    for path in [
        "job.steps[0].with.script",
        "job.steps[0].with.configuration.runtime.command",
        "job.custom.workflow.commands[0]",
    ] {
        let command = nested_commands
            .iter()
            .find_map(|(found, command)| (found == path).then_some(command))
            .unwrap_or_else(|| panic!("parsed command scan omits {path}: {nested_commands:?}"));
        assert!(
            !daemon_launch_commands(command).is_empty(),
            "daemon scan catches launch command at {path}: {command}"
        );
    }
}

fn assert_generated_jobs_do_not_launch_daemons(fixture: &Fixture) {
    for (file, job_id, job) in all_jobs(fixture) {
        for (vector, command) in parsed_job_command_vectors(&job) {
            let findings = daemon_launch_commands(&command);
            assert!(
                findings.is_empty(),
                "generated jobs avoid daemon launch: {file}:{job_id}:{vector}: {findings:?}"
            );
        }
    }
}

fn assert_candidate_permissions_are_read_only(
    fixture: &Fixture,
    candidates: &[(String, String, YamlValue)],
) -> bool {
    let candidate_files = candidates
        .iter()
        .map(|(file, _, _)| file.clone())
        .collect::<BTreeSet<_>>();
    let mut found = false;
    for (file, _, job) in all_jobs(fixture)
        .into_iter()
        .filter(|(file, _, _)| candidate_files.contains(file))
    {
        let document = workflow(fixture, &file);
        assert!(
            !workflow_text(fixture, &file).contains("pull_request_target"),
            "formula validation never uses pull_request_target"
        );
        found |= readonly_contents_permission(&document) || readonly_contents_permission(&job);
        for scope in [&document, &job] {
            if let Some(mapping) = scope.get("permissions").and_then(YamlValue::as_mapping) {
                assert!(
                    mapping.values().all(|value| value
                        .as_str()
                        .is_some_and(|permission| permission == "read" || permission == "none")),
                    "candidate workflow/job permissions are read-only"
                );
            }
        }
    }
    found
}

#[test]
fn untrusted_formula_has_no_write_token() {
    let fixture = fixture(Some(
        "\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\"",
    ));
    generate_ok(&fixture);
    let candidates = candidate_install_jobs(&fixture);
    assert!(!candidates.is_empty(), "candidate install job is generated");
    assert_candidate_jobs_have_no_credentials(&candidates);
    assert_pr_callers_have_no_credentials(&fixture);
    assert_protected_workflows_have_no_credentials(&fixture, &candidates);
    assert_credential_scanner_controls();
    assert_daemon_command_controls();
    assert_parsed_daemon_command_controls();
    assert_generated_jobs_do_not_launch_daemons(&fixture);
    assert!(
        assert_candidate_permissions_are_read_only(&fixture, &candidates),
        "candidate installation workflow declares read-only contents permission"
    );
}

#[test]
fn unsupported_platform_is_visible() {
    let fixture_factory = fixture;
    let fixture = fixture_factory(Some(
        "\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\", \"freebsd-x64\"",
    ));
    let output = run_generate(&fixture);
    assert!(
        !output.status.success(),
        "an unsupported platform must fail generation instead of disappearing"
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        diagnostic.contains("freebsd-x64"),
        "diagnostic names invalid platform: {diagnostic}"
    );
    assert!(
        diagnostic.to_ascii_lowercase().contains("platform"),
        "diagnostic explains the invalid platform: {diagnostic}"
    );

    let incomplete = fixture_factory(Some("\"macos-arm64\", \"macos-x64\", \"linux-x64\""));
    let output = run_generate(&incomplete);
    assert!(
        !output.status.success(),
        "generation rejects a missing required Linux ARM64 platform instead of shrinking the matrix"
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    assert!(
        diagnostic.contains("linux-arm64") && diagnostic.contains("exactly one each"),
        "incomplete matrix diagnosis names the missing native platform: {diagnostic}"
    );
}

fn assert_existing_homebrew_unit(fixture: &Fixture) {
    generate_ok(fixture);
    assert!(
        candidate_install_jobs(fixture).is_empty(),
        "candidate installs are opt-in through typed preview formula configuration"
    );
    let config = fs::read_to_string(fixture.output.join(".github/ci/project.toml"))
        .expect("generated project config exists");
    assert!(
        config.contains("id = \"homebrew\""),
        "Homebrew unit remains present"
    );
    assert!(
        config.contains("brew audit --strict --online"),
        "existing online Homebrew audit remains in typed unit commands"
    );
    assert!(
        !config.contains("homebrew_preview"),
        "generation-only candidate declaration does not leak into runtime config"
    );
    assert!(
        config.contains("reads_closed = true") && config.contains("Formula/preview.rb"),
        "formula input is watched under a complete read contract"
    );
    assert!(
        yaml_contains(
            &workflow(fixture, "ci-unit-homebrew.yml"),
            "velnor-workflow run"
        ),
        "existing Homebrew reusable still runs its typed pipeline"
    );
}

#[cfg(unix)]
fn setup_homebrew_planner_fixture() -> (Fixture, YamlValue, String, String) {
    let fixture = fixture_with_service_requirement(
        Some("\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\""),
        true,
    );
    generate_ok(&fixture);
    assert!(
        !candidate_install_jobs(&fixture).is_empty(),
        "typed formula input renders candidate checks"
    );
    let pr = workflow(&fixture, "ci-pr.yml");
    let caller = pr["jobs"]
        .as_mapping()
        .expect("PR workflow declares jobs")
        .values()
        .find(|job| {
            yaml_string_field(job, "uses")
                .is_some_and(|path| path.ends_with("ci-unit-homebrew.yml"))
        })
        .expect("PR workflow calls the Homebrew reusable unit");
    let guard = yaml_string_field(caller, "if")
        .expect("Homebrew caller is plan-gated")
        .to_owned();
    let planner = pr["jobs"]["plan"]["steps"]
        .as_sequence()
        .expect("PR workflow planner has steps")
        .iter()
        .find(|step| yaml_string_field(step, "id") == Some("plan"))
        .expect("generated PR plan step is present")
        .clone();
    assert!(
        yaml_string_field(&planner, "run").is_some_and(
            |script| script.contains("velnor-workflow plan --config .github/ci/project.toml")
        ),
        "selection uses the generated planner command"
    );
    let generated = fixture.output.join(".github/ci/project.toml");
    let checkout = fixture.root.join(".github/ci/project.toml");
    fs::create_dir_all(
        checkout
            .parent()
            .expect("generated runtime config has parent"),
    )
    .expect("create planner fixture runtime config directory");
    fs::copy(&generated, &checkout).expect("place generated runtime config in Git checkout");
    run_fixture_git(&fixture.root, &["init", "--quiet"]);
    run_fixture_git(&fixture.root, &["config", "user.name", "TASK-012 fixture"]);
    run_fixture_git(
        &fixture.root,
        &["config", "user.email", "task012@example.invalid"],
    );
    run_fixture_git(&fixture.root, &["add", "-A"]);
    run_fixture_git(&fixture.root, &["commit", "--quiet", "-m", "baseline"]);
    let base_sha = run_fixture_git(&fixture.root, &["rev-parse", "HEAD"]);
    (fixture, planner, guard, base_sha)
}

#[cfg(unix)]
fn assert_formula_change_selects_candidate(
    fixture: &Fixture,
    planner: &YamlValue,
    guard: &str,
    base_sha: &str,
) {
    let formula_path = fixture.root.join("Formula/preview.rb");
    let mut formula = fs::read_to_string(&formula_path).expect("read baseline formula");
    formula.push_str("\n# formula input changed\n");
    fs::write(&formula_path, formula).expect("change candidate formula input");
    run_fixture_git(&fixture.root, &["add", "Formula/preview.rb"]);
    run_fixture_git(
        &fixture.root,
        &["commit", "--quiet", "-m", "change formula input"],
    );
    let head = run_fixture_git(&fixture.root, &["rev-parse", "HEAD"]);
    let (plan, units) =
        run_generated_pr_planner(fixture, planner, base_sha, &head, "formula-change");
    assert!(
        plan.status.success(),
        "generated PR planner succeeds for formula change: {}{}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    let units = units.expect("formula plan writes generated units output");
    assert!(
        caller_selects_configured_homebrew(guard, "github-hosted", "success", &units, false),
        "Formula/preview.rb change selects required PR candidate checks"
    );
    assert!(
        !String::from_utf8_lossy(&plan.stdout).contains("planned_no_work=true"),
        "formula change is not classified as no-work"
    );
}

#[cfg(unix)]
fn assert_unrelated_change_skips_candidate(
    fixture: &Fixture,
    planner: &YamlValue,
    guard: &str,
    base_sha: &str,
) {
    run_fixture_git(&fixture.root, &["reset", "--hard", base_sha]);
    fs::write(
        fixture.root.join("unrelated-input.txt"),
        "unrelated non-formula input\n",
    )
    .expect("write unrelated change");
    run_fixture_git(&fixture.root, &["add", "unrelated-input.txt"]);
    run_fixture_git(
        &fixture.root,
        &["commit", "--quiet", "-m", "change unrelated input"],
    );
    let head = run_fixture_git(&fixture.root, &["rev-parse", "HEAD"]);
    let (plan, units) =
        run_generated_pr_planner(fixture, planner, base_sha, &head, "unrelated-change");
    assert!(
        plan.status.success(),
        "planner accepts unrelated change as no-work: {}{}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(
        String::from_utf8_lossy(&plan.stdout).contains("planned_no_work=true"),
        "no-formula changes produce explicit no-work plan"
    );
    let units = units.expect("unrelated plan writes generated units output");
    let document: JsonValue = serde_json::from_str(&units).expect("planned units output is JSON");
    assert!(
        document
            .as_array()
            .is_some_and(|items| items.iter().all(|unit| unit["unit_id"] != "homebrew")),
        "unrelated file does not select Homebrew candidate unit"
    );
    assert!(
        !caller_selects_configured_homebrew(guard, "github-hosted", "success", &units, false),
        "generated caller skips candidate checks when formula did not change"
    );
}

fn assert_no_service_candidate() {
    let fixture = fixture_with_service_requirement(
        Some("\"macos-arm64\", \"macos-x64\", \"linux-x64\", \"linux-arm64\""),
        false,
    );
    generate_ok(&fixture);
    let formula = fs::read_to_string(fixture.root.join("Formula/preview.rb"))
        .expect("read no-service formula");
    assert!(
        !formula.contains("service do"),
        "service_required=false fixture formula has no service declaration"
    );
    let candidates = candidate_install_jobs(&fixture);
    assert!(
        !candidates.is_empty(),
        "optional service inspection keeps candidate checks"
    );
    let pr = workflow(&fixture, "ci-pr.yml");
    let caller = pr["jobs"]
        .as_mapping()
        .expect("PR workflow has jobs")
        .values()
        .find(|job| {
            yaml_string_field(job, "uses")
                .is_some_and(|path| path.ends_with("ci-unit-homebrew.yml"))
        })
        .expect("Homebrew caller exists for no-service fixture");
    assert_eq!(
        preview_input(caller)["service_required"].as_bool(),
        Some(false),
        "typed input disables service verification"
    );
    for (file, job_id, job) in candidates {
        let content = yaml_text(&job);
        assert!(
            content.contains("brew install --build-from-source")
                && content.contains("brew test --verbose"),
            "optional service inspection keeps install/test: {file}:{job_id}"
        );
        let step = job["steps"]
            .as_sequence()
            .expect("candidate job has steps")
            .iter()
            .find(|step| {
                yaml_string_field(step, "name")
                    == Some("Verify, install, and test candidate formula")
            })
            .expect("formula checks share one candidate step");
        let script = yaml_string_field(step, "run").expect("candidate formula step has a script");
        assert!(
            yaml_string_field(&step["env"], "SERVICE_REQUIRED")
                == Some("${{ fromJSON(inputs.homebrew_preview).service_required }}")
                && script.contains("if [[ \"$SERVICE_REQUIRED\" == true ]]; then"),
            "false service flag skips DSL inspection inside the single candidate step: {file}:{job_id}"
        );
    }
}

#[test]
fn existing_homebrew_units_still_render() {
    let fixture = fixture(None);
    assert_existing_homebrew_unit(&fixture);
    #[cfg(unix)]
    {
        let (candidate, planner, guard, base_sha) = setup_homebrew_planner_fixture();
        assert_formula_change_selects_candidate(&candidate, &planner, &guard, &base_sha);
        assert_unrelated_change_skips_candidate(&candidate, &planner, &guard, &base_sha);
    }
    assert_no_service_candidate();
}
