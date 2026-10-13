use std::{
    fmt,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
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

    /// The wanted revision is requested by name, exactly as the workspace
    /// checkout would have requested it from the origin; the mirror pins what
    /// that resolved to under `refs/velnor` immediately afterwards. A
    /// `<sha>:<ref>` refspec cannot express this: git reads an all-zero source
    /// as a deletion, and some servers reject a bare object id on the left.
    fn refspecs(&self) -> Vec<String> {
        let mut refspecs = vec![self.git_ref.clone()];
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
    /// Shared reader lease held through workspace hydration. A mirror cannot
    /// be quarantined or rebuilt while this checkout is alive.
    _reader_lease: Arc<File>,
}

impl MirrorCheckout {
    fn new(
        path: PathBuf,
        sha: String,
        fetched: bool,
        repaired: bool,
        lock: File,
        downgrade_to_reader: bool,
    ) -> Result<Self> {
        if downgrade_to_reader {
            flock(&lock, FlockOperation::LockShared)
                .with_context(|| format!("share-lock git mirror {}", path.display()))?;
        }
        Ok(Self {
            path,
            sha,
            fetched,
            repaired,
            _reader_lease: Arc::new(lock),
        })
    }
}

impl Clone for MirrorCheckout {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            sha: self.sha.clone(),
            fetched: self.fetched,
            repaired: self.repaired,
            _reader_lease: Arc::clone(&self._reader_lease),
        }
    }
}

impl fmt::Debug for MirrorCheckout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MirrorCheckout")
            .field("path", &self.path)
            .field("sha", &self.sha)
            .field("fetched", &self.fetched)
            .field("repaired", &self.repaired)
            .finish()
    }
}

impl PartialEq for MirrorCheckout {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.sha == other.sha
            && self.fetched == other.fetched
            && self.repaired == other.repaired
    }
}

impl Eq for MirrorCheckout {}

fn warm_mirror_sha<R: CommandRunner>(
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
    let pinned = resolved_ref_sha(runner, mirror, &want.wanted_ref())?;
    (pinned == want.git_ref).then(|| want.git_ref.clone())
}

/// Bring the shared mirror up to date for `want` and return the commit.
///
/// Locking: health probing runs under a shared lock. All mirror mutations —
/// including ref publication, fetch, pinning, and requested-SHA resolution —
/// run under one exclusive lock. Workspace hydration happens after this
/// function returns and remains independent of the mirror writer lease.
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

    let healthy = mirror_is_healthy(runner, &mirror);
    if healthy && let Some(sha) = warm_mirror_sha(runner, &mirror, want) {
        return MirrorCheckout::new(mirror, sha, false, false, lock, false);
    }
    flock(&lock, FlockOperation::Unlock)
        .with_context(|| format!("unlock git mirror {}", mirror.display()))?;
    flock(&lock, FlockOperation::LockExclusive)
        .with_context(|| format!("lock git mirror {}", mirror.display()))?;

    // The shared read phase is only a fast preflight. A writer may have
    // repaired or changed the mirror after it released the shared lock, so the
    // exclusive owner must always recheck before mutating it.
    let mut repaired = false;
    if !mirror_is_healthy(runner, &mirror) {
        if mirror.exists() {
            quarantine_mirror(&mirror)?;
            repaired = true;
        }
        initialize_mirror(runner, &mirror)?;
    }

    if let Some(sha) = short_circuit_sha(runner, &mirror, want) {
        return MirrorCheckout::new(mirror, sha, false, repaired, lock, true);
    }

    fetch_want(runner, &mirror, clone_url, token, want)?;

    // The fetch reported success, so the mirror holds what was asked for. When
    // rev-parse cannot name it (an object id the mirror stores under our own
    // ref, or a git that does not resolve it), fall through to the requested
    // revision: the workspace checkout resolves it against the linked objects
    // and fails loudly there if it really is absent.
    let sha = resolved_wanted_sha(runner, &mirror, want).unwrap_or_else(|| want.git_ref.clone());
    MirrorCheckout::new(mirror, sha, true, repaired, lock, true)
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
            "--format=%(objectname)".to_string(),
        ],
    ) else {
        return false;
    };
    if refs.code != 0 {
        return false;
    }
    // A freshly initialized mirror has no refs yet and no objects to lose.
    refs.stdout
        .split_whitespace()
        .all(|object| object_exists(runner, mirror, object))
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
        if let Some(sha) = resolved_ref_sha(runner, mirror, &reference) {
            return Some(sha);
        }
    }
    None
}

fn resolved_ref_sha<R: CommandRunner>(
    runner: &mut R,
    mirror: &Path,
    reference: &str,
) -> Option<String> {
    let result = git(
        runner,
        &[
            "-C".to_string(),
            path_arg(mirror),
            "rev-parse".to_string(),
            "--verify".to_string(),
            "--quiet".to_string(),
            format!("{reference}^{{commit}}"),
        ],
    )
    .ok()?;
    (result.code == 0)
        .then(|| result.stdout.trim().to_string())
        .filter(|sha| !sha.is_empty())
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
        return pin_wanted_ref(runner, mirror, want, "FETCH_HEAD");
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
    pin_wanted_ref(runner, mirror, want, &want.git_ref)
}

/// Keep the fetched revision alive under `refs/velnor` so a later job finds it
/// without touching the network, and so no gc can ever reach the objects a
/// workspace has hard-linked out of this store.
fn pin_wanted_ref<R: CommandRunner>(
    runner: &mut R,
    mirror: &Path,
    want: &MirrorWant,
    revision: &str,
) -> Result<()> {
    let pinned = git(
        runner,
        &[
            "-C".to_string(),
            path_arg(mirror),
            "update-ref".to_string(),
            want.wanted_ref(),
            revision.to_string(),
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
    let (host, path) = clone_url_host_and_path(clone_url);
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

/// Split a clone URL into its host and its repository path. `git@host:owner/repo.git`
/// is not a URL, so it is split on the scp-style colon instead.
fn clone_url_host_and_path(clone_url: &str) -> (String, String) {
    if let Ok(url) = Url::parse(clone_url)
        && let Some(host) = url.host_str()
    {
        let host = match url.port() {
            Some(port) => format!("{host}_{port}"),
            None => host.to_string(),
        };
        return (host, url.path().to_string());
    }
    if let Some((authority, path)) = clone_url.split_once(':')
        && !authority.contains('/')
        && !path.is_empty()
    {
        let host = authority.rsplit('@').next().unwrap_or(authority);
        if !host.is_empty() {
            return (host.to_string(), path.to_string());
        }
    }
    ("local".to_string(), clone_url.to_string())
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    use super::*;
    use crate::executor::{CommandRunner, ProcessCommandRunner};

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
        let mirror_path = mirror.path.clone();
        drop(mirror);

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

        let config = fs::read_to_string(mirror_path.join("config")).unwrap();
        assert!(!config.contains("token"));
        assert!(!config.contains("url ="));
    }

    #[test]
    fn concurrent_cold_fetches_serialize_mirror_writers() {
        let fixture = Fixture::new();
        let first = fixture.commit("one");
        let second = fixture.commit("two");
        let clone_url = fixture.clone_url();
        let gate = Arc::new(Barrier::new(2));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));

        let first_handle = spawn_mirror_fetch(
            fixture.store.clone(),
            clone_url.clone(),
            first.clone(),
            gate.clone(),
            in_flight.clone(),
            max_in_flight.clone(),
        );
        let second_handle = spawn_mirror_fetch(
            fixture.store.clone(),
            clone_url,
            second.clone(),
            gate,
            in_flight,
            max_in_flight.clone(),
        );

        let first_result = first_handle
            .join()
            .expect("first mirror fetch thread panicked")
            .expect("first mirror fetch failed");
        let second_result = second_handle
            .join()
            .expect("second mirror fetch thread panicked")
            .expect("second mirror fetch failed");

        assert_eq!(first_result, first);
        assert_eq!(second_result, second);
        assert_eq!(max_in_flight.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn checkout_reader_lease_blocks_mirror_repair() {
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
        let lock_path = fixture.store.join(format!(
            "{}.lock",
            repository_store_name(&fixture.clone_url()).unwrap()
        ));
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();

        assert!(matches!(
            flock(&contender, FlockOperation::NonBlockingLockExclusive),
            Err(rustix::io::Errno::WOULDBLOCK)
        ));
        drop(mirror);
        flock(&contender, FlockOperation::NonBlockingLockExclusive).unwrap();
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
    fn mutable_ref_refetches_and_repins_to_current_commit() {
        let fixture = Fixture::new();
        let first = fixture.commit("one");
        let mut runner = ProcessCommandRunner;
        let initial = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            None,
            &want("master"),
        )
        .unwrap();
        assert_eq!(initial.sha, first);
        assert!(initial.fetched);
        drop(initial);

        let second = fixture.commit("two");
        let refreshed = ensure_mirror(
            &mut runner,
            &fixture.store,
            &fixture.clone_url(),
            None,
            &want("master"),
        )
        .unwrap();
        assert_eq!(refreshed.sha, second);
        assert!(refreshed.fetched);
        assert_eq!(
            rev_parse(&mut runner, &refreshed.path, "refs/velnor/wanted/master"),
            second
        );
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
        let mirror_path = mirror.path.clone();
        drop(mirror);

        // A mirror whose objects are gone but whose refs and HEAD survive was
        // previously accepted as healthy for the rest of the host's life.
        fs::remove_dir_all(mirror_path.join("objects")).unwrap();
        fs::create_dir_all(mirror_path.join("objects")).unwrap();
        assert!(!mirror_is_healthy(&mut runner, &mirror_path));

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
    fn mirror_health_checks_every_ref_tip() {
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
        let mirror_path = mirror.path.clone();
        drop(mirror);

        let heads = mirror_path.join("refs/heads");
        fs::create_dir_all(&heads).unwrap();
        fs::write(heads.join("aaa"), format!("{sha}\n")).unwrap();
        fs::write(
            heads.join("zzz"),
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n",
        )
        .unwrap();

        assert!(!mirror_is_healthy(&mut runner, &mirror_path));
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

    fn spawn_mirror_fetch(
        store: PathBuf,
        clone_url: String,
        sha: String,
        gate: Arc<Barrier>,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    ) -> std::thread::JoinHandle<Result<String>> {
        std::thread::spawn(move || {
            gate.wait();
            let mut runner = CoordinatedFetchRunner {
                inner: ProcessCommandRunner,
                in_flight,
                max_in_flight,
            };
            ensure_mirror(&mut runner, &store, &clone_url, None, &want(&sha))
                .map(|checkout| checkout.sha)
        })
    }

    struct CoordinatedFetchRunner {
        inner: ProcessCommandRunner,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    }

    impl CommandRunner for CoordinatedFetchRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.inner.run(program, args)
        }

        fn run_with_env(
            &mut self,
            program: &str,
            args: &[String],
            env: &[(String, String)],
        ) -> Result<CommandResult> {
            let is_fetch = program == "git" && args.iter().any(|arg| arg == "fetch");
            if !is_fetch {
                return self.inner.run_with_env(program, args, env);
            }

            let active = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(active, Ordering::SeqCst);
            let result = self.inner.run_with_env(program, args, env);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            result
        }
    }
}
