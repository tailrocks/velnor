use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use rustix::fs::{flock, FlockOperation};
use url::Url;

use crate::{
    checkout::git_auth_env,
    container::sanitize_store_key,
    executor::{CommandResult, CommandRunner, StepLogicFailure},
};

/// Namespace the mirror keeps the refs a job asked for under. Refs here are
/// never deleted, so every object a workspace hard-links out of the mirror
/// stays reachable and survives any future `git gc` in the mirror.
const WANTED_REF_PREFIX: &str = "refs/velnor";

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

/// Exactly what one job needs out of the mirror.
///
/// The mirror used to fetch `+refs/*:refs/*` at full depth on every job, which
/// on GitHub drags in every `refs/pull/*` ref of the repository. A job needs
/// one commit (plus branches and tags when it asked for full history), so the
/// want is stated explicitly and the fetch is built from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorWant {
    /// The commit SHA, ref name, or `HEAD` the checkout resolves to.
    pub git_ref: String,
    /// `fetch-depth: 0` — the job wants branches and full history, not just
    /// the one commit.
    pub full_history: bool,
    /// `fetch-tags: true`.
    pub tags: bool,
}

impl MirrorWant {
    fn wanted_ref(&self) -> String {
        if is_object_id(&self.git_ref) {
            format!("{WANTED_REF_PREFIX}/commits/{}", self.git_ref)
        } else {
            format!(
                "{WANTED_REF_PREFIX}/wanted/{}",
                sanitize_store_key(&self.git_ref)
            )
        }
    }

    fn refspecs(&self) -> Vec<String> {
        let mut refspecs = vec![format!("+{}:{}", self.git_ref, self.wanted_ref())];
        if self.full_history {
            refspecs.push("+refs/heads/*:refs/heads/*".to_string());
        }
        if self.full_history || self.tags {
            refspecs.push("+refs/tags/*:refs/tags/*".to_string());
        }
        refspecs
    }

    /// Refspecs for servers that refuse a bare object id in a fetch request
    /// (`uploadpack.allowAnySHA1InWant` off). Branches and tags carry the
    /// commit instead.
    fn fallback_refspecs(&self) -> Vec<String> {
        vec![
            "+refs/heads/*:refs/heads/*".to_string(),
            "+refs/tags/*:refs/tags/*".to_string(),
        ]
    }
}

/// A mirror that is known-good and known to contain the wanted commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorCheckout {
    pub path: PathBuf,
    /// The resolved commit id the workspace must check out.
    pub sha: String,
    /// False when the wanted commit was already present and no network fetch
    /// ran — the short-circuit an N-way matrix on one commit depends on.
    pub fetched: bool,
    /// True when the mirror was rebuilt from scratch because the on-disk copy
    /// failed its health probe.
    pub repaired: bool,
}

/// Bring the shared mirror up to date for `want` and return the commit.
///
/// Locking: the exclusive lock is taken only to create or rebuild the mirror.
/// The network fetch runs under a shared lock, so concurrent jobs fetch in
/// parallel instead of serializing behind one another, and a job whose commit
/// is already mirrored returns without any lock upgrade or network traffic.
///
/// # Errors
/// Store or lock cannot be created, the mirror cannot be repaired, or the
/// fetch fails. A broken mirror is rebuilt rather than silently bypassed.
pub fn ensure_mirror<R: CommandRunner>(
    runner: &mut R,
    store_root: &Path,
    clone_url: &str,
    token: Option<&str>,
    want: &MirrorWant,
) -> Result<MirrorCheckout> {
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

    flock(&lock, FlockOperation::LockShared)
        .with_context(|| format!("share-lock git mirror {}", mirror.display()))?;

    let mut repaired = false;
    if !mirror_is_healthy(runner, &mirror) {
        flock(&lock, FlockOperation::LockExclusive)
            .with_context(|| format!("lock git mirror {}", mirror.display()))?;
        // Another job may have repaired it while we waited for the upgrade.
        if !mirror_is_healthy(runner, &mirror) {
            if mirror.exists() {
                quarantine_mirror(&mirror)?;
                repaired = true;
            }
            initialize_mirror(runner, &mirror)?;
        }
        flock(&lock, FlockOperation::LockShared)
            .with_context(|| format!("share-lock git mirror {}", mirror.display()))?;
    }

    if let Some(sha) = short_circuit_sha(runner, &mirror, want) {
        return Ok(MirrorCheckout {
            path: mirror,
            sha,
            fetched: false,
            repaired,
        });
    }

    fetch_want(runner, &mirror, clone_url, token, want)?;

    // The fetch reported success, so the mirror holds what was asked for. When
    // rev-parse cannot name it (an object id the mirror stores under our own
    // ref, or a git that does not resolve it), fall through to the requested
    // revision: the workspace checkout resolves it against the linked objects
    // and fails loudly there if it really is absent.
    let sha = resolved_wanted_sha(runner, &mirror, want).unwrap_or_else(|| want.git_ref.clone());
    Ok(MirrorCheckout {
        path: mirror,
        sha,
        fetched: true,
        repaired,
    })
}

/// Every ref the mirror holds outside the internal `refs/velnor` namespace,
/// as `(refname, object id)`. A workspace maps these onto its own remote
/// tracking refs after hard-linking the objects.
///
/// # Errors
/// `git for-each-ref` cannot run.
pub fn mirror_refs<R: CommandRunner>(
    runner: &mut R,
    mirror: &Path,
) -> Result<Vec<(String, String)>> {
    let result = git(
        runner,
        &[
            "-C".to_string(),
            path_arg(mirror),
            "for-each-ref".to_string(),
            "--format=%(objectname) %(refname)".to_string(),
        ],
    )?;
    if result.code != 0 {
        bail!("git for-each-ref failed with code {}", result.code);
    }
    Ok(result
        .stdout
        .lines()
        .filter_map(|line| {
            let (object, refname) = line.trim().split_once(' ')?;
            if refname.starts_with(WANTED_REF_PREFIX) {
                return None;
            }
            Some((refname.to_string(), object.to_string()))
        })
        .collect())
}

fn initialize_mirror<R: CommandRunner>(runner: &mut R, mirror: &Path) -> Result<()> {
    let result = git(
        runner,
        &["init".to_string(), "--bare".to_string(), path_arg(mirror)],
    )?;
    ensure_success(result.code, "git init --bare", &result.stderr)?;
    // Auto gc must never run here: workspaces hard-link this object store, and
    // a background repack would churn packs under them for no benefit. Objects
    // stay reachable through `refs/velnor/*` regardless.
    for (key, value) in [("gc.auto", "0"), ("gc.autoDetach", "false")] {
        let result = git(
            runner,
            &[
                "-C".to_string(),
                path_arg(mirror),
                "config".to_string(),
                key.to_string(),
                value.to_string(),
            ],
        )?;
        ensure_success(result.code, "git config", &result.stderr)?;
    }
    Ok(())
}

/// Move a broken mirror out of the way before rebuilding, then delete it.
/// Renaming first keeps the rebuild atomic from the point of view of a job
/// that is about to resolve the mirror path.
fn quarantine_mirror(mirror: &Path) -> Result<()> {
    let quarantine = mirror.with_extension(format!(
        "corrupt-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::rename(mirror, &quarantine)
        .with_context(|| format!("quarantine corrupt git mirror {}", mirror.display()))?;
    eprintln!(
        "Rebuilding corrupt git mirror {} (quarantined at {})",
        mirror.display(),
        quarantine.display()
    );
    fs::remove_dir_all(&quarantine)
        .with_context(|| format!("remove quarantined git mirror {}", quarantine.display()))
}

/// Deterministic health probe. `HEAD` existing (the previous check) says
/// nothing about the object database or the ref store, so a mirror that lost
/// its objects silently degraded every later job. This probes the three things
/// a hydrating checkout depends on: git recognizes the repository, the ref
/// store is readable, and the object named by a ref is actually present.
fn mirror_is_healthy<R: CommandRunner>(runner: &mut R, mirror: &Path) -> bool {
    if !mirror.join("HEAD").exists() || !mirror.join("objects").is_dir() {
        return false;
    }
    let Ok(result) = git(
        runner,
        &[
            "-C".to_string(),
            path_arg(mirror),
            "rev-parse".to_string(),
            "--git-dir".to_string(),
        ],
    ) else {
        return false;
    };
    if result.code != 0 {
        return false;
    }
    let Ok(refs) = git(
        runner,
        &[
            "-C".to_string(),
            path_arg(mirror),
            "for-each-ref".to_string(),
            "--count=1".to_string(),
            "--format=%(objectname)".to_string(),
        ],
    ) else {
        return false;
    };
    if refs.code != 0 {
        return false;
    }
    let Some(object) = refs.stdout.split_whitespace().next() else {
        // A freshly initialized mirror has no refs yet and no objects to lose.
        return true;
    };
    object_exists(runner, mirror, object)
}

fn object_exists<R: CommandRunner>(runner: &mut R, mirror: &Path, object: &str) -> bool {
    git(
        runner,
        &[
            "-C".to_string(),
            path_arg(mirror),
            "cat-file".to_string(),
            "-e".to_string(),
            format!("{object}^{{commit}}"),
        ],
    )
    .map(|result| result.code == 0)
    .unwrap_or(false)
}

/// The wanted commit is already mirrored: no lock upgrade, no network.
///
/// Only an immutable object id can short-circuit — a branch or tag name may
/// have moved upstream, so it always refetches. A job asking for full history
/// or tags refetches too, because presence of the commit says nothing about
/// presence of the rest of the branch graph.
fn short_circuit_sha<R: CommandRunner>(
    runner: &mut R,
    mirror: &Path,
    want: &MirrorWant,
) -> Option<String> {
    if want.full_history || want.tags || !is_object_id(&want.git_ref) {
        return None;
    }
    if !object_exists(runner, mirror, &want.git_ref) {
        return None;
    }
    // Pin the commit under refs/velnor so it stays reachable for every
    // workspace that hard-links it.
    let wanted = want.wanted_ref();
    let updated = git(
        runner,
        &[
            "-C".to_string(),
            path_arg(mirror),
            "update-ref".to_string(),
            wanted,
            want.git_ref.clone(),
        ],
    )
    .ok()?;
    (updated.code == 0).then(|| want.git_ref.clone())
}

fn resolved_wanted_sha<R: CommandRunner>(
    runner: &mut R,
    mirror: &Path,
    want: &MirrorWant,
) -> Option<String> {
    for reference in [want.wanted_ref(), want.git_ref.clone()] {
        let Ok(result) = git(
            runner,
            &[
                "-C".to_string(),
                path_arg(mirror),
                "rev-parse".to_string(),
                "--verify".to_string(),
                "--quiet".to_string(),
                format!("{reference}^{{commit}}"),
            ],
        ) else {
            continue;
        };
        if result.code == 0 {
            let sha = result.stdout.trim().to_string();
            if !sha.is_empty() {
                return Some(sha);
            }
        }
    }
    None
}

fn fetch_want<R: CommandRunner>(
    runner: &mut R,
    mirror: &Path,
    clone_url: &str,
    token: Option<&str>,
    want: &MirrorWant,
) -> Result<()> {
    let env = git_auth_env(clone_url, token);
    let result = runner.run_with_env(
        "git",
        &fetch_args(mirror, clone_url, &want.refspecs()),
        &env,
    )?;
    if result.code == 0 {
        return Ok(());
    }
    if !is_object_id(&want.git_ref) {
        return ensure_success(result.code, "git mirror fetch", &result.stderr);
    }
    // The server refused a bare object id in the want list. Fetch the ref
    // universe the commit is reachable from instead, then pin it locally.
    let fallback = runner.run_with_env(
        "git",
        &fetch_args(mirror, clone_url, &want.fallback_refspecs()),
        &env,
    )?;
    ensure_success(fallback.code, "git mirror fetch", &fallback.stderr)?;
    let pinned = git(
        runner,
        &[
            "-C".to_string(),
            path_arg(mirror),
            "update-ref".to_string(),
            want.wanted_ref(),
            want.git_ref.clone(),
        ],
    )?;
    ensure_success(pinned.code, "git mirror update-ref", &pinned.stderr)
}

fn fetch_args(mirror: &Path, clone_url: &str, refspecs: &[String]) -> Vec<String> {
    let mut args = vec![
        "-C".to_string(),
        path_arg(mirror),
        "-c".to_string(),
        "protocol.version=2".to_string(),
        "-c".to_string(),
        "gc.auto=0".to_string(),
        "fetch".to_string(),
        "--no-tags".to_string(),
        "--force".to_string(),
        clone_url.to_string(),
    ];
    args.extend(refspecs.iter().cloned());
    args
}

fn git<R: CommandRunner>(runner: &mut R, args: &[String]) -> Result<CommandResult> {
    runner.run("git", args)
}

fn is_object_id(value: &str) -> bool {
    let length = value.len();
    (length == 40 || length == 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Mirror identity. The host is part of the key: without it the same
/// `owner/repo` on github.com and on a GHES instance shared one mirror and
/// force-updated each other's refs.
fn repository_store_name(clone_url: &str) -> Result<String> {
    let parsed = Url::parse(clone_url).ok();
    let path = parsed
        .as_ref()
        .map(|url| url.path().to_string())
        .unwrap_or_else(|| clone_url.to_string());
    let host = parsed
        .as_ref()
        .and_then(|url| {
            url.host_str().map(|host| match url.port() {
                Some(port) => format!("{host}_{port}"),
                None => host.to_string(),
            })
        })
        .or_else(|| scp_like_host(clone_url))
        .unwrap_or_else(|| "local".to_string());
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
        "{}__{}__{}",
        sanitize_store_key(&host),
        sanitize_store_key(components[components.len() - 2]),
        sanitize_store_key(components[components.len() - 1])
    ))
}

/// `git@host:owner/repo.git` carries its host before the colon and does not
/// parse as a URL.
fn scp_like_host(clone_url: &str) -> Option<String> {
    let (authority, _) = clone_url.split_once(':')?;
    if authority.contains('/') {
        return None;
    }
    let host = authority.rsplit('@').next()?;
    (!host.is_empty()).then(|| host.to_string())
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A failing git command in the mirror is a failure of the checkout step that
/// asked for it, not of the runner: it carries git's own exit code so the step
/// result matches what a direct checkout would have produced.
fn ensure_success(code: i32, operation: &str, stderr: &str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(StepLogicFailure::new(
            code,
            "",
            format!("{operation} failed with code {code}: {stderr}"),
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::ProcessCommandRunner;

    fn want(git_ref: &str) -> MirrorWant {
        MirrorWant {
            git_ref: git_ref.to_string(),
            full_history: false,
            tags: false,
        }
    }

    struct Fixture {
        root: PathBuf,
        origin: PathBuf,
        work: PathBuf,
        store: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("velnor-mirror-{}", uuid::Uuid::new_v4()));
            let origin = root.join("owner/repo.git");
            let work = root.join("work");
            fs::create_dir_all(origin.parent().unwrap()).unwrap();
            let mut runner = ProcessCommandRunner;
            for args in [
                vec!["init".into(), "--bare".into(), path_arg(&origin)],
                vec![
                    "-C".into(),
                    path_arg(&origin),
                    "config".into(),
                    // A commit id in a fetch request is what GitHub allows and
                    // what the mirror asks for; enable it on the fixture too.
                    "uploadpack.allowAnySHA1InWant".into(),
                    "true".into(),
                ],
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
            let store = root.join("store");
            Self {
                root,
                origin,
                work,
                store,
            }
        }

        fn clone_url(&self) -> String {
            format!("file://{}", self.origin.display())
        }

        fn commit(&self, value: &str) -> String {
            let mut runner = ProcessCommandRunner;
            fs::write(self.work.join("value"), value).unwrap();
            commit_and_push(&mut runner, &self.work, &self.origin, value);
            rev_parse(&mut runner, &self.work, "HEAD")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn mirror_fetches_initial_and_delta_commits_without_persisting_auth() {
        let fixture = Fixture::new();
        let first = fixture.commit("one");
        let mut runner = ProcessCommandRunner;
        let mirror = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            Some("not-persisted-token"),
            &want(&first),
        )
        .unwrap();
        assert_eq!(mirror.sha, first);
        assert!(mirror.fetched);

        let second = fixture.commit("two");
        let refreshed = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            Some("not-persisted-token"),
            &want(&second),
        )
        .unwrap();
        assert_eq!(refreshed.sha, second);
        assert_ne!(first, second);

        let config = fs::read_to_string(mirror.path.join("config")).unwrap();
        assert!(!config.contains("token"));
        assert!(!config.contains("url ="));
    }

    #[test]
    fn mirror_skips_the_network_when_the_commit_is_already_present() {
        let fixture = Fixture::new();
        let sha = fixture.commit("one");
        let mut runner = ProcessCommandRunner;
        let first = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            None,
            &want(&sha),
        )
        .unwrap();
        assert!(first.fetched);

        // Break the origin: a second request for the same commit must not
        // touch it at all.
        fs::remove_dir_all(&fixture.origin).unwrap();
        let second = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            None,
            &want(&sha),
        )
        .unwrap();
        assert!(!second.fetched);
        assert_eq!(second.sha, sha);
    }

    #[test]
    fn mirror_does_not_fetch_pull_request_refs() {
        let fixture = Fixture::new();
        let sha = fixture.commit("one");
        let mut runner = ProcessCommandRunner;
        assert_eq!(
            runner
                .run(
                    "git",
                    &[
                        "-C".into(),
                        path_arg(&fixture.origin),
                        "update-ref".into(),
                        "refs/pull/1/head".into(),
                        sha.clone(),
                    ],
                )
                .unwrap()
                .code,
            0
        );

        let mirror = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            None,
            &want(&sha),
        )
        .unwrap();
        let refs = runner
            .run(
                "git",
                &[
                    "-C".into(),
                    path_arg(&mirror.path),
                    "for-each-ref".into(),
                    "--format=%(refname)".into(),
                ],
            )
            .unwrap()
            .stdout;
        assert!(
            !refs.contains("refs/pull/"),
            "mirror fetched pull request refs: {refs}"
        );
    }

    #[test]
    fn mirror_rebuilds_a_corrupt_object_store() {
        let fixture = Fixture::new();
        let sha = fixture.commit("one");
        let mut runner = ProcessCommandRunner;
        let mirror = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            None,
            &want(&sha),
        )
        .unwrap();

        // A mirror whose objects are gone but whose refs and HEAD survive was
        // previously accepted as healthy for the rest of the host's life.
        fs::remove_dir_all(mirror.path.join("objects")).unwrap();
        fs::create_dir_all(mirror.path.join("objects")).unwrap();
        assert!(!mirror_is_healthy(&mut runner, &mirror.path));

        let repaired = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            None,
            &want(&sha),
        )
        .unwrap();
        assert!(repaired.repaired);
        assert_eq!(repaired.sha, sha);
        assert!(mirror_is_healthy(&mut runner, &repaired.path));
    }

    #[test]
    fn store_name_is_host_owner_and_repository_scoped() {
        assert_eq!(
            repository_store_name("https://github.com/Owner/Repo.git").unwrap(),
            "github.com__Owner__Repo"
        );
        assert_eq!(
            repository_store_name("git@github.com:Owner/Repo.git").unwrap(),
            "github.com__Owner__Repo"
        );
    }

    #[test]
    fn store_name_separates_the_same_repository_on_two_hosts() {
        let github = repository_store_name("https://github.com/acme/repo.git").unwrap();
        let ghes = repository_store_name("https://ghe.acme.test/acme/repo.git").unwrap();
        assert_ne!(github, ghes);
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
