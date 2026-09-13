//! Docker detector: one verification unit per directory holding Dockerfiles.

use std::collections::BTreeMap;

use super::file_walk::is_test_support_path;
use super::{unit, RepositoryShape, ScanContext};
use crate::{identifier_suffix, parent_path, shell_quote, UnitKind};

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    let mut dockerfiles_by_root: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for dockerfile in context.files.iter().filter(|file| {
        !is_test_support_path(file)
            && file
                .rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with("Dockerfile"))
    }) {
        let docker_root = parent_path(dockerfile);
        dockerfiles_by_root
            .entry(docker_root)
            .or_default()
            .push(dockerfile);
    }
    for (docker_root, dockerfiles) in dockerfiles_by_root {
        let build_context = if docker_root == "." {
            "."
        } else {
            &docker_root
        };
        shape.units.push(unit(
            UnitKind::Docker,
            &docker_root,
            if build_context == "." {
                vec!["**".to_owned()]
            } else {
                vec![format!("{build_context}/**")]
            },
            docker_build_commands(build_context, &dockerfiles),
            None,
        ));
    }
}

fn docker_build_command(dockerfile: &str, build_context: &str) -> String {
    format!(
        "docker build --file {} --tag local-ci:{} {}",
        shell_quote(dockerfile),
        identifier_suffix(dockerfile),
        shell_quote(build_context)
    )
}

fn dockerfile_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Pair architecture-specific Dockerfiles so a single-architecture runner
/// builds only the image matching its native architecture. Unknown host
/// architectures fail closed instead of silently selecting amd64.
fn docker_build_commands(build_context: &str, dockerfiles: &[&String]) -> Vec<String> {
    let mut amd64 = None;
    let mut arm64 = None;
    let mut other = Vec::new();
    for dockerfile in dockerfiles {
        match dockerfile_basename(dockerfile) {
            "Dockerfile.amd64" | "Dockerfile.x86_64" => amd64 = Some(*dockerfile),
            "Dockerfile.arm64" | "Dockerfile.aarch64" => arm64 = Some(*dockerfile),
            _ => other.push(*dockerfile),
        }
    }
    let mut commands: Vec<String> = other
        .into_iter()
        .map(|dockerfile| docker_build_command(dockerfile, build_context))
        .collect();
    match (amd64, arm64) {
        (Some(amd64), Some(arm64)) => {
            commands.push(format!(
                r#"case "$(uname -m)" in aarch64|arm64) {} ;; x86_64|amd64) {} ;; *) echo "unsupported Docker build architecture: $(uname -m)" >&2; exit 1 ;; esac"#,
                docker_build_command(arm64, build_context),
                docker_build_command(amd64, build_context)
            ));
        }
        (Some(dockerfile), None) | (None, Some(dockerfile)) => {
            commands.push(docker_build_command(dockerfile, build_context));
        }
        (None, None) => {}
    }
    commands
}

#[cfg(test)]
mod tests {
    #[test]
    fn paired_arch_dockerfiles_select_native_uname_and_fail_closed() {
        let amd = "img/Dockerfile.amd64".to_owned();
        let arm = "img/Dockerfile.arm64".to_owned();
        let commands = super::docker_build_commands("img", &[&amd, &arm]);
        assert_eq!(commands.len(), 1);
        let command = &commands[0];
        assert!(command.contains(r#"case "$(uname -m)" in aarch64|arm64)"#));
        assert!(command.contains("Dockerfile.arm64"));
        assert!(command.contains("Dockerfile.amd64"));
        assert!(command.contains("x86_64|amd64)"));
        assert!(command.contains("unsupported Docker build architecture"));
        assert!(command.contains("exit 1"));
    }

    #[test]
    fn unpaired_dockerfile_stays_a_plain_build() {
        let file = "img/Dockerfile".to_owned();
        let commands = super::docker_build_commands("img", &[&file]);
        assert_eq!(
            commands,
            ["docker build --file 'img/Dockerfile' --tag local-ci:img-dockerfile 'img'"]
        );
    }
}
