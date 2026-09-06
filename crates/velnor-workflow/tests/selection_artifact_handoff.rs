#![cfg(unix)]

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const PLAN_BASE_SHA: &str = "plan-base";
const PLAN_HEAD_SHA: &str = "plan-head";

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = env::temp_dir().join(format!(
            "velnor-selection-artifact-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".github/ci"))?;
        fs::create_dir_all(root.join(".velnor-ci-selection"))?;
        fs::create_dir_all(root.join("fake-bin"))?;
        fs::write(root.join(".github/ci/project.toml"), CONFIG)?;
        fs::write(
            root.join(".velnor-ci-selection/velnor-ci-selection"),
            format!(
                "version=1\nbase_sha={PLAN_BASE_SHA}\nhead_sha={PLAN_HEAD_SHA}\nscope=affected\nunits=selected\nfull_units=selected\n"
            ),
        )?;
        let fake_git = root.join("fake-bin/git");
        fs::write(
            &fake_git,
            "#!/bin/sh\nprintf invoked > git-was-resolved\nexit 0\n",
        )?;
        let mut permissions = fs::metadata(&fake_git)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(fake_git, permissions)?;
        Ok(Self { root })
    }

    fn run(&self, job_base_sha: &str, job_head_sha: &str) -> Command {
        let mut path = OsString::from(self.root.join("fake-bin"));
        path.push(":");
        path.push(env::var_os("PATH").unwrap_or_default());

        let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"));
        command
            .current_dir(&self.root)
            .env("BASE_SHA", job_base_sha)
            .env("HEAD_SHA", job_head_sha)
            .env("EVENT_NAME", "pull_request")
            .env(
                "VELNOR_SELECTION_FILE",
                self.root.join(".velnor-ci-selection/velnor-ci-selection"),
            )
            .env("PATH", path)
            .args([
                "run",
                "--config",
                ".github/ci/project.toml",
                "--scope",
                "affected",
                "--unit",
                "selected",
            ]);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn valid_selection_artifact_runs_subset_without_resolving_git_again() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let output = fixture.run(PLAN_BASE_SHA, PLAN_HEAD_SHA).output()?;
    assert_success(&output);
    assert_eq!(
        fs::read_to_string(fixture.root.join("selected.marker"))?,
        "selected"
    );
    assert!(!fixture.root.join("unselected.marker").exists());
    assert!(!fixture.root.join("git-was-resolved").exists());
    Ok(())
}

#[test]
fn selection_artifact_sha_mismatch_fails_closed_with_both_sha_pairs() -> Result<(), Box<dyn Error>>
{
    let fixture = Fixture::new()?;
    let output = fixture.run("job-base", "job-head").output()?;
    assert!(!output.status.success());
    let output = output_text(&output);
    assert!(output.contains(
        "::warning::CI selection artifact SHA mismatch: plan base SHA `plan-base` vs job base SHA `job-base`; plan head SHA `plan-head` vs job head SHA `job-head`"
    ));
    assert!(output.contains("CI selection artifact does not match this job checkout"));
    assert!(!fixture.root.join("selected.marker").exists());
    Ok(())
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "{}", output_text(output));
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const CONFIG: &str = r#"
schema = 2

[[unit]]
id = "selected"
label = "selected"
kind = "rust"
root = "."
watch = ["selected"]
github_pr_commands = ["printf selected > selected.marker"]
github_full_commands = ["printf selected > selected.marker"]
velnor_pr_commands = ["printf selected > selected.marker"]
velnor_full_commands = ["printf selected > selected.marker"]

[[unit]]
id = "unselected"
label = "unselected"
kind = "rust"
root = "."
watch = ["unselected"]
github_pr_commands = ["printf unselected > unselected.marker"]
github_full_commands = ["printf unselected > unselected.marker"]
velnor_pr_commands = ["printf unselected > unselected.marker"]
velnor_full_commands = ["printf unselected > unselected.marker"]
"#;
