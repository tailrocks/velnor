#![allow(dead_code)]

use crate::{
    command_files::{parse_command_file, FileCommand},
    container::Shell,
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct ScriptStep {
    pub id: String,
    pub script: String,
    pub shell: Shell,
    pub working_directory_container: String,
}

#[derive(Debug, Clone)]
pub struct ScriptStepPlan {
    pub script_host_path: PathBuf,
    pub script_container_path: String,
    pub shell: Shell,
    pub working_directory_container: String,
    pub env: Vec<(String, String)>,
    command_files: CommandFileSet,
}

impl ScriptStepPlan {
    pub fn prepare(step: &ScriptStep, temp_host: &Path) -> Result<Self> {
        fs::create_dir_all(temp_host).with_context(|| format!("create {}", temp_host.display()))?;
        let script_name = format!("{}.sh", step.id);
        let script_host_path = temp_host.join(&script_name);
        fs::write(&script_host_path, fix_script(&step.script))
            .with_context(|| format!("write {}", script_host_path.display()))?;

        let command_files = CommandFileSet::new(&step.id, temp_host);
        command_files.create_empty_files()?;

        Ok(Self {
            script_host_path,
            script_container_path: format!("/__t/{script_name}"),
            shell: step.shell,
            working_directory_container: step.working_directory_container.clone(),
            env: command_files.env(),
            command_files,
        })
    }

    pub fn collect_state(&self) -> Result<StepCommandState> {
        self.command_files.collect_state()
    }
}

#[derive(Debug, Clone)]
struct CommandFileSet {
    output: PathMapping,
    env: PathMapping,
    path: PathMapping,
    state: PathMapping,
    summary: PathMapping,
}

impl CommandFileSet {
    fn new(step_id: &str, temp_host: &Path) -> Self {
        Self {
            output: PathMapping::new(temp_host, step_id, "output"),
            env: PathMapping::new(temp_host, step_id, "env"),
            path: PathMapping::new(temp_host, step_id, "path"),
            state: PathMapping::new(temp_host, step_id, "state"),
            summary: PathMapping::new(temp_host, step_id, "summary"),
        }
    }

    fn create_empty_files(&self) -> Result<()> {
        for path in [
            &self.output.host,
            &self.env.host,
            &self.path.host,
            &self.state.host,
            &self.summary.host,
        ] {
            fs::write(path, "").with_context(|| format!("create {}", path.display()))?;
        }
        Ok(())
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("GITHUB_OUTPUT".into(), self.output.container.clone()),
            ("GITHUB_ENV".into(), self.env.container.clone()),
            ("GITHUB_PATH".into(), self.path.container.clone()),
            ("GITHUB_STATE".into(), self.state.container.clone()),
            ("GITHUB_STEP_SUMMARY".into(), self.summary.container.clone()),
        ]
    }

    fn collect_state(&self) -> Result<StepCommandState> {
        Ok(StepCommandState {
            outputs: commands_to_map(parse_command_file(&self.output.host)?),
            env: commands_to_map(parse_command_file(&self.env.host)?),
            path: fs::read_to_string(&self.path.host)?
                .lines()
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            state: commands_to_map(parse_command_file(&self.state.host)?),
            summary: fs::read_to_string(&self.summary.host)?,
        })
    }
}

#[derive(Debug, Clone)]
struct PathMapping {
    host: PathBuf,
    container: String,
}

impl PathMapping {
    fn new(temp_host: &Path, step_id: &str, name: &str) -> Self {
        let file_name = format!("{step_id}_{name}");
        Self {
            host: temp_host.join(&file_name),
            container: format!("/__t/{file_name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StepCommandState {
    pub outputs: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub path: Vec<String>,
    pub state: BTreeMap<String, String>,
    pub summary: String,
}

fn commands_to_map(commands: Vec<FileCommand>) -> BTreeMap<String, String> {
    commands
        .into_iter()
        .map(|command| (command.name, command.value))
        .collect()
}

fn fix_script(script: &str) -> String {
    let mut fixed = script.replace("\r\n", "\n");
    if !fixed.ends_with('\n') {
        fixed.push('\n');
    }
    fixed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_step_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("velnor-step-test-{nonce}"))
    }

    #[test]
    fn prepares_script_and_command_file_env() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            script: "echo hello".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w/repo".into(),
        };

        let plan = ScriptStepPlan::prepare(&step, &temp).unwrap();

        assert_eq!(
            fs::read_to_string(&plan.script_host_path).unwrap(),
            "echo hello\n"
        );
        assert!(plan
            .env
            .contains(&("GITHUB_OUTPUT".into(), "/__t/step1_output".into())));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn collects_command_file_state() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            script: "echo test".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
        };
        let plan = ScriptStepPlan::prepare(&step, &temp).unwrap();

        fs::write(
            temp.join("step1_output"),
            "answer=42\nmulti<<EOF\none\ntwo\nEOF\n",
        )
        .unwrap();
        fs::write(temp.join("step1_env"), "NAME=value\n").unwrap();
        fs::write(temp.join("step1_path"), "/opt/tool\n\n").unwrap();
        fs::write(temp.join("step1_state"), "cleanup=yes\n").unwrap();
        fs::write(temp.join("step1_summary"), "summary text").unwrap();

        let state = plan.collect_state().unwrap();

        assert_eq!(state.outputs["answer"], "42");
        assert_eq!(state.outputs["multi"], "one\ntwo");
        assert_eq!(state.env["NAME"], "value");
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.state["cleanup"], "yes");
        assert_eq!(state.summary, "summary text");
        fs::remove_dir_all(temp).unwrap();
    }
}
