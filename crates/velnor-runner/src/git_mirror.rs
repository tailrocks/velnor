use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use rustix::fs::{flock, FlockOperation};
use url::Url;

use crate::{checkout::git_auth_env, container::sanitize_store_key, executor::CommandRunner};

pub fn store_root(legacy_work_root: &Path, trust_scope: &str) -> PathBuf {
    if let Some(layout) = crate::storage::StorageLayout::resolve() {
        layout.cache_class(trust_scope, "git-mirrors")
    } else {
        legacy_work_root
            .join("_velnor_git")
            .join(sanitize_store_key(trust_scope))
            .join("git-mirrors")
    }
}

pub fn ensure_mirror<R: CommandRunner>(
    runner: &mut R,
    store_root: &Path,
    clone_url: &str,
    token: Option<&str>,
) -> Result<PathBuf> {
    fs::create_dir_all(store_root)
        .with_context(|| format!("create git mirror store {}", store_root.display()))?;
    let name = repository_store_name(clone_url)?;
    let mirror = store_root.join(format!("{name}.git"));
    let lock_path = store_root.join(format!("{name}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open git mirror lock {}", lock_path.display()))?;
    flock(&lock, FlockOperation::LockExclusive)
        .with_context(|| format!("lock git mirror {}", mirror.display()))?;

    if !mirror.join("HEAD").exists() {
        let result = runner.run("git", &["init".into(), "--bare".into(), path_arg(&mirror)])?;
        ensure_success(result.code, "git init --bare", &result.stderr)?;
    }

    let args = vec![
        "-C".into(),
        path_arg(&mirror),
        "-c".into(),
        "protocol.version=2".into(),
        "fetch".into(),
        clone_url.into(),
        "+refs/*:refs/*".into(),
    ];
    let env = git_auth_env(clone_url, token);
    let result = runner.run_with_env("git", &args, &env)?;
    ensure_success(result.code, "git mirror fetch", &result.stderr)?;
    Ok(mirror)
}

fn repository_store_name(clone_url: &str) -> Result<String> {
    let path = Url::parse(clone_url)
        .ok()
        .map(|url| url.path().to_string())
        .unwrap_or_else(|| clone_url.to_string());
    let components = path
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if components.len() < 2 {
        bail!("cannot derive owner/repository identity from clone URL")
    }
    Ok(format!(
        "{}__{}",
        sanitize_store_key(components[components.len() - 2]),
        sanitize_store_key(components[components.len() - 1])
    ))
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn ensure_success(code: i32, operation: &str, stderr: &str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        bail!("{operation} failed with code {code}: {stderr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{CommandResult, ProcessCommandRunner};

    #[test]
    fn mirror_fetches_initial_and_delta_commits_without_persisting_auth() {
        let root = std::env::temp_dir().join(format!("velnor-mirror-{}", uuid::Uuid::new_v4()));
        let origin = root.join("owner/repo.git");
        let work = root.join("work");
        fs::create_dir_all(origin.parent().unwrap()).unwrap();
        let mut runner = ProcessCommandRunner;
        for args in [
            vec!["init".into(), "--bare".into(), path_arg(&origin)],
            vec!["init".into(), path_arg(&work)],
            vec![
                "-C".into(),
                path_arg(&work),
                "config".into(),
                "user.email".into(),
                "test@example.com".into(),
            ],
            vec![
                "-C".into(),
                path_arg(&work),
                "config".into(),
                "user.name".into(),
                "Test".into(),
            ],
        ] {
            assert_eq!(runner.run("git", &args).unwrap().code, 0);
        }
        fs::write(work.join("value"), "one").unwrap();
        commit_and_push(&mut runner, &work, &origin, "one");
        let store = root.join("store");
        let mirror = ensure_mirror(
            &mut runner,
            &store,
            &format!("file://{}", origin.display()),
            Some("not-persisted-token"),
        )
        .unwrap();
        let first = rev_parse(&mut runner, &mirror, "refs/heads/master");

        fs::write(work.join("value"), "two").unwrap();
        commit_and_push(&mut runner, &work, &origin, "two");
        ensure_mirror(
            &mut runner,
            &store,
            &format!("file://{}", origin.display()),
            Some("not-persisted-token"),
        )
        .unwrap();
        let second = rev_parse(&mut runner, &mirror, "refs/heads/master");
        assert_ne!(first, second);
        let config = fs::read_to_string(mirror.join("config")).unwrap();
        assert!(!config.contains("token"));
        assert!(!config.contains("url ="));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn store_name_is_owner_and_repository_scoped() {
        assert_eq!(
            repository_store_name("https://github.com/Owner/Repo.git").unwrap(),
            "Owner__Repo"
        );
    }

    fn commit_and_push(
        runner: &mut ProcessCommandRunner,
        work: &Path,
        origin: &Path,
        message: &str,
    ) {
        for args in [
            vec!["-C".into(), path_arg(work), "add".into(), ".".into()],
            vec![
                "-C".into(),
                path_arg(work),
                "commit".into(),
                "-m".into(),
                message.into(),
            ],
            vec![
                "-C".into(),
                path_arg(work),
                "push".into(),
                path_arg(origin),
                "HEAD:master".into(),
            ],
        ] {
            let result = runner.run("git", &args).unwrap();
            assert_eq!(result.code, 0, "{}", result.stderr);
        }
    }

    fn rev_parse(runner: &mut ProcessCommandRunner, repo: &Path, reference: &str) -> String {
        let CommandResult {
            code,
            stdout,
            stderr,
        } = runner
            .run(
                "git",
                &[
                    "-C".into(),
                    path_arg(repo),
                    "rev-parse".into(),
                    reference.into(),
                ],
            )
            .unwrap();
        assert_eq!(code, 0, "{stderr}");
        stdout.trim().to_string()
    }
}
