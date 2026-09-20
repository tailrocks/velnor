//! Shared source-closure identity primitives.
//!
//! This module is included by both the runtime closure code and `build.rs`.
//! Keep it independent of the generator crate: the build script cannot import
//! the crate it is compiling.  A clean worktree deliberately returns Git's
//! exact v1 `ls-tree` lines.  A dirty worktree is rebuilt from the bytes and
//! modes that Cargo will actually compile, so a HEAD stamp cannot bless
//! changed source.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// A fail-closed identity error.  The runtime maps this to `GeneratorError`;
/// the build script turns it into an `unknown` stamp.
#[derive(Debug)]
pub(crate) struct IdentityError(String);

impl IdentityError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Return the canonical digest for a set of v1 closure lines.
pub(crate) fn canonical_digest(
    lines: &[String],
    version: u8,
    features: &str,
    profile: &str,
) -> String {
    use sha2::{Digest as _, Sha256};

    let mut sorted: Vec<&str> = lines.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut bytes = Vec::new();
    for line in sorted {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(
        format!("closure-version:{version}\nfeatures:{features}\nprofile:{profile}\n").as_bytes(),
    );
    let digest = Sha256::digest(&bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

/// Compute a closure digest from the files currently visible in `repo`.
///
/// `rev` supplies the tracked baseline.  If every relevant path still has
/// the exact Git object and mode, the raw v1 `git ls-tree` lines are returned;
/// that preserves the established clean-tree digest byte for byte.  Once a
/// relevant path changes, each entry is rehashed from worktree bytes and
/// current stage-0 index plus nonignored untracked additions are included.
/// Deleted paths disappear.
pub(crate) fn worktree_digest(
    repo: &Path,
    rev: &str,
    paths: &[&str],
    version: u8,
    features: &str,
    profile: &str,
) -> Result<String, IdentityError> {
    let lines = worktree_lines(repo, rev, paths)?;
    Ok(canonical_digest(&lines, version, features, profile))
}

/// Return canonical v1 closure lines for the current worktree.
pub(crate) fn worktree_lines(
    repo: &Path,
    rev: &str,
    paths: &[&str],
) -> Result<Vec<String>, IdentityError> {
    let baseline = git_ls_tree_z(repo, rev, paths)?;
    if baseline.is_empty() {
        return Err(IdentityError::new(format!(
            "revision {rev} has no closure inputs in {}",
            repo.display()
        )));
    }
    let baseline_paths: BTreeSet<PathBuf> = baseline
        .iter()
        .map(|entry| checked_path(&entry.path))
        .collect::<Result<_, _>>()?;
    let additions = worktree_additions(repo, paths, &baseline_paths)?;
    let mut candidate_paths = baseline_paths.clone();
    candidate_paths.extend(additions.iter().cloned());
    let regular_paths = regular_worktree_paths(repo, &candidate_paths)?;
    let regular_hashes = batch_git_blobs(repo, &regular_paths)?;

    let mut changed = !additions.is_empty();
    let mut current = Vec::with_capacity(baseline.len() + additions.len());
    for entry in baseline {
        let path = checked_path(&entry.path)?;
        let value = worktree_entry(repo, &path, paths, &regular_hashes)?;
        let Some(value) = value else {
            changed = true;
            continue;
        };
        if value.mode != entry.mode || value.kind != entry.kind || value.object != entry.object {
            changed = true;
        }
        current.push(value.line(&path));
    }
    for path in additions {
        if current.iter().any(|line| line_path(line) == path) {
            return Err(IdentityError::new(format!(
                "additional closure input duplicates a tracked path: {}",
                path.display()
            )));
        }
        let value = worktree_entry(repo, &path, paths, &regular_hashes)?.ok_or_else(|| {
            IdentityError::new(format!(
                "additional closure input disappeared while inspecting: {}",
                path.display()
            ))
        })?;
        current.push(value.line(&path));
    }
    if !changed {
        let raw = git_ls_tree_raw(repo, rev, paths)?;
        let lines: Vec<String> = raw.lines().map(str::to_owned).collect();
        if lines.is_empty() {
            return Err(IdentityError::new(format!(
                "revision {rev} has no closure inputs in {}",
                repo.display()
            )));
        }
        return Ok(lines);
    }
    if current.is_empty() {
        return Err(IdentityError::new(format!(
            "worktree has no closure inputs in {}",
            repo.display()
        )));
    }
    Ok(current)
}

fn worktree_additions(
    repo: &Path,
    paths: &[&str],
    baseline_paths: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, IdentityError> {
    let index_paths = git_index_paths(repo, paths)?;
    let index_set: BTreeSet<PathBuf> = index_paths.iter().cloned().collect();
    let untracked = git_paths(repo, &["ls-files", "--others", "--exclude-standard"], paths)?;
    let ignored = git_paths(
        repo,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
        paths,
    )?;
    for path in ignored {
        if !is_proven_cargo_output(&path) {
            return Err(IdentityError::new(format!(
                "ignored closure input is not a proven Cargo output: {}",
                path.display()
            )));
        }
    }

    let mut additions = BTreeSet::new();
    for path in index_paths {
        if !baseline_paths.contains(&path) {
            additions.insert(path);
        }
    }
    for path in untracked {
        if index_set.contains(&path) {
            return Err(IdentityError::new(format!(
                "closure input is both indexed and untracked: {}",
                path.display()
            )));
        }
        if !baseline_paths.contains(&path) {
            additions.insert(path);
        }
    }
    Ok(additions)
}

#[derive(Clone, Debug)]
struct TreeEntry {
    mode: String,
    kind: String,
    object: String,
    path: Vec<u8>,
}

#[derive(Clone, Debug)]
struct WorktreeEntry {
    mode: String,
    kind: String,
    object: String,
}

impl WorktreeEntry {
    fn line(&self, path: &Path) -> String {
        let path = path.to_string_lossy();
        format!("{} {} {}\t{path}", self.mode, self.kind, self.object)
    }
}

fn git_ls_tree_z(repo: &Path, rev: &str, paths: &[&str]) -> Result<Vec<TreeEntry>, IdentityError> {
    let mut arguments = vec!["ls-tree", "-r", "-z", rev, "--"];
    arguments.extend_from_slice(paths);
    let output = run_git(repo, &arguments)?;
    let mut entries = Vec::new();
    for record in output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| {
                IdentityError::new("git ls-tree returned a closure entry without a path")
            })?;
        let metadata = std::str::from_utf8(&record[..tab])
            .map_err(|_| IdentityError::new("git ls-tree returned non-UTF-8 closure metadata"))?;
        let mut fields = metadata.splitn(3, ' ');
        let mode = fields
            .next()
            .ok_or_else(|| IdentityError::new("git ls-tree returned no mode"))?;
        let kind = fields
            .next()
            .ok_or_else(|| IdentityError::new("git ls-tree returned no object type"))?;
        let object = fields
            .next()
            .ok_or_else(|| IdentityError::new("git ls-tree returned no object id"))?;
        if !matches!(mode, "100644" | "100755" | "120000") || kind != "blob" {
            return Err(IdentityError::new(format!(
                "unsupported closure entry mode/type {mode} {kind}"
            )));
        }
        if object.len() != 40 || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(IdentityError::new(
                "git ls-tree returned an invalid object id",
            ));
        }
        entries.push(TreeEntry {
            mode: mode.to_owned(),
            kind: kind.to_owned(),
            object: object.to_owned(),
            path: record[tab + 1..].to_vec(),
        });
    }
    Ok(entries)
}

fn git_ls_tree_raw(repo: &Path, rev: &str, paths: &[&str]) -> Result<String, IdentityError> {
    let mut arguments = vec!["ls-tree", "-r", rev, "--"];
    arguments.extend_from_slice(paths);
    let output = run_git(repo, &arguments)?;
    String::from_utf8(output)
        .map_err(|_| IdentityError::new("git ls-tree returned non-UTF-8 closure lines"))
}

/// Return paths currently present in the index. The index is part of the
/// worktree view for identity purposes: a staged addition is already a file
/// Cargo can compile even though `git ls-files --others` does not report it.
/// Unmerged entries are ambiguous and fail closed rather than choosing one
/// stage's path.
fn git_index_paths(repo: &Path, paths: &[&str]) -> Result<Vec<PathBuf>, IdentityError> {
    let mut arguments = vec!["ls-files", "--cached", "--stage", "-z", "--"];
    arguments.extend_from_slice(paths);
    let output = run_git(repo, &arguments)?;
    let mut entries = BTreeSet::new();
    for record in output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| {
                IdentityError::new("git ls-files returned an index entry without a path")
            })?;
        let metadata = std::str::from_utf8(&record[..tab])
            .map_err(|_| IdentityError::new("git ls-files returned non-UTF-8 index metadata"))?;
        let mut fields = metadata.split_whitespace();
        let _mode = fields
            .next()
            .ok_or_else(|| IdentityError::new("git ls-files returned no index mode"))?;
        let _object = fields
            .next()
            .ok_or_else(|| IdentityError::new("git ls-files returned no index object"))?;
        let stage = fields
            .next()
            .ok_or_else(|| IdentityError::new("git ls-files returned no index stage"))?;
        if fields.next().is_some() {
            return Err(IdentityError::new(
                "git ls-files returned malformed index metadata",
            ));
        }
        if stage != "0" {
            return Err(IdentityError::new(format!(
                "closure index contains an unmerged entry at stage {stage}"
            )));
        }
        let path = checked_path(&record[tab + 1..])?;
        if !entries.insert(path.clone()) {
            return Err(IdentityError::new(format!(
                "git ls-files returned a duplicate index path: {}",
                path.display()
            )));
        }
    }
    Ok(entries.into_iter().collect())
}

fn git_paths(repo: &Path, command: &[&str], paths: &[&str]) -> Result<Vec<PathBuf>, IdentityError> {
    let mut arguments = command.to_vec();
    arguments.push("-z");
    arguments.push("--");
    arguments.extend_from_slice(paths);
    let output = run_git(repo, &arguments)?;
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(checked_path)
        .collect()
}

/// Find regular files that need a content object. One `hash-object
/// --stdin-paths` process hashes the complete batch. Symlink targets remain
/// on the byte-input path below because Git follows symlinks when given a
/// filesystem path; their link text is intentionally hashed separately.
fn regular_worktree_paths(
    repo: &Path,
    paths: &BTreeSet<PathBuf>,
) -> Result<Vec<PathBuf>, IdentityError> {
    let mut regular = Vec::new();
    for path in paths {
        let full = repo.join(path);
        let metadata = match fs::symlink_metadata(&full) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(IdentityError::new(format!(
                    "inspect closure input {}: {error}",
                    path.display()
                )))
            }
        };
        if metadata.file_type().is_file() {
            regular.push(path.clone());
        }
    }
    Ok(regular)
}

fn batch_git_blobs(
    repo: &Path,
    paths: &[PathBuf],
) -> Result<HashMap<PathBuf, String>, IdentityError> {
    if paths.is_empty() {
        return Ok(HashMap::new());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["hash-object", "--stdin-paths", "--no-filters"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| IdentityError::new(format!("run batched git hash-object: {error}")))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| IdentityError::new("batched git hash-object stdin unavailable"))?;

    // `hash-object` can emit one object id per input path.  Write the input
    // concurrently with `wait_with_output`, which drains stdout/stderr.  A
    // write-all-then-wait sequence deadlocks once either pipe fills: Git is
    // blocked writing output while the parent is blocked writing paths.
    let paths_for_writer = paths.to_vec();
    let writer = thread::spawn(move || -> Result<(), IdentityError> {
        let mut stdin = stdin;
        for path in paths_for_writer {
            let path = path.to_str().ok_or_else(|| {
                IdentityError::new(format!(
                    "closure path is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            stdin
                .write_all(path.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .map_err(|error| IdentityError::new(format!("write batched git paths: {error}")))?;
        }
        Ok(())
    });
    let output = child.wait_with_output().map_err(|error| {
        IdentityError::new(format!("wait for batched git hash-object: {error}"))
    })?;
    let writer_result = writer
        .join()
        .map_err(|_| IdentityError::new("batched git path writer panicked"))?;
    writer_result?;
    if !output.status.success() {
        return Err(IdentityError::new(format!(
            "batched git hash-object failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let objects: Vec<&[u8]> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|object| !object.is_empty())
        .collect();
    if objects.len() != paths.len() {
        return Err(IdentityError::new(format!(
            "batched git hash-object returned {} objects for {} paths",
            objects.len(),
            paths.len()
        )));
    }
    let mut hashes = HashMap::with_capacity(paths.len());
    for (path, object) in paths.iter().zip(objects) {
        let object = String::from_utf8(object.to_vec())
            .map_err(|_| IdentityError::new("batched git hash-object returned non-UTF-8"))?;
        if object.len() != 40 || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(IdentityError::new(
                "batched git hash-object returned an invalid object id",
            ));
        }
        hashes.insert(path.clone(), object);
    }
    Ok(hashes)
}

fn checked_path(bytes: &[u8]) -> Result<PathBuf, IdentityError> {
    let path = std::str::from_utf8(bytes)
        .map_err(|_| IdentityError::new("closure path is not valid UTF-8"))?;
    if path.is_empty()
        || path.contains(['\0', '\n', '\r', '\t', '\\', '"'])
        || path.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(IdentityError::new(format!(
            "closure path cannot be represented by v1 lines: {path:?}"
        )));
    }
    let path = PathBuf::from(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(IdentityError::new(format!(
            "closure path is not repository-relative: {}",
            path.display()
        )));
    }
    Ok(path)
}

fn line_path(line: &str) -> PathBuf {
    line.split_once('\t')
        .map(|(_, path)| PathBuf::from(path))
        .unwrap_or_default()
}

fn is_proven_cargo_output(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(Component::Normal(value)) if value == "target"
    )
}

fn worktree_entry(
    repo: &Path,
    path: &Path,
    closure_paths: &[&str],
    regular_hashes: &HashMap<PathBuf, String>,
) -> Result<Option<WorktreeEntry>, IdentityError> {
    let full = repo.join(path);
    let metadata = match fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(IdentityError::new(format!(
                "inspect closure input {}: {error}",
                path.display()
            )))
        }
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            validate_symlink_target(repo, path, closure_paths)?;
            let target = fs::read_link(&full).map_err(|error| {
                IdentityError::new(format!("read closure symlink {}: {error}", path.display()))
            })?;
            let object = git_blob(repo, target.as_os_str().as_bytes())?;
            return Ok(Some(WorktreeEntry {
                mode: "120000".to_owned(),
                kind: "blob".to_owned(),
                object,
            }));
        }
        #[cfg(not(unix))]
        {
            return Err(IdentityError::new(format!(
                "symlink closure input is unsupported on this platform: {}",
                path.display()
            )));
        }
    }
    if !file_type.is_file() {
        return Err(IdentityError::new(format!(
            "unsupported closure input type: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    let object = regular_hashes.get(path).ok_or_else(|| {
        IdentityError::new(format!(
            "regular closure input was not included in the hash batch: {}",
            path.display()
        ))
    })?;
    Ok(Some(WorktreeEntry {
        mode: if executable { "100755" } else { "100644" }.to_owned(),
        kind: "blob".to_owned(),
        object: object.clone(),
    }))
}

/// A symlink's link text is part of the Git tree, but Cargo follows the link
/// when it reads a source file.  Accept only an existing regular-file target
/// that resolves inside the declared closure.  This keeps the clean v1 line
/// contract while rejecting an unrepresented source dependency; no filename
/// exception is needed for the tracked `CLAUDE.md -> AGENTS.md` link because
/// that target is itself inside `crates/velnor-workflow`.
#[cfg(unix)]
fn validate_symlink_target(
    repo: &Path,
    path: &Path,
    closure_paths: &[&str],
) -> Result<(), IdentityError> {
    let root = fs::canonicalize(repo).map_err(|error| {
        IdentityError::new(format!(
            "canonicalize closure repository {}: {error}",
            repo.display()
        ))
    })?;
    let target = fs::canonicalize(repo.join(path)).map_err(|error| {
        IdentityError::new(format!(
            "resolve closure symlink {}: {error}",
            path.display()
        ))
    })?;
    let relative = target.strip_prefix(&root).map_err(|_| {
        IdentityError::new(format!(
            "symlink closure input target escapes repository: {} -> {}",
            path.display(),
            target.display()
        ))
    })?;
    let in_closure = closure_paths.iter().any(|closure_path| {
        let closure_path = Path::new(closure_path);
        relative == closure_path || relative.starts_with(closure_path)
    });
    if !in_closure {
        return Err(IdentityError::new(format!(
            "symlink closure input target escapes declared closure: {} -> {}",
            path.display(),
            relative.display()
        )));
    }
    if !fs::metadata(repo.join(path))
        .map_err(|error| {
            IdentityError::new(format!(
                "inspect closure symlink target {}: {error}",
                path.display()
            ))
        })?
        .is_file()
    {
        return Err(IdentityError::new(format!(
            "symlink closure input target is not a regular file: {} -> {}",
            path.display(),
            relative.display()
        )));
    }
    Ok(())
}

/// Hash byte content that has no filesystem path, currently a symlink's raw
/// target text. Regular files use the batch path API above.
fn git_blob(repo: &Path, bytes: &[u8]) -> Result<String, IdentityError> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| IdentityError::new(format!("run git hash-object: {error}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| IdentityError::new("git hash-object stdin unavailable"))?;
    stdin
        .write_all(bytes)
        .map_err(|error| IdentityError::new(format!("write git hash-object input: {error}")))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| IdentityError::new(format!("wait for git hash-object: {error}")))?;
    if !output.status.success() {
        return Err(IdentityError::new(format!(
            "git hash-object failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let object = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if object.len() != 40 || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(IdentityError::new(
            "git hash-object returned an invalid object id",
        ));
    }
    Ok(object)
}

fn run_git(repo: &Path, arguments: &[&str]) -> Result<Vec<u8>, IdentityError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(arguments);
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| IdentityError::new(format!("run git {}: {error}", arguments.join(" "))))?;
    let output = child.wait_with_output().map_err(|error| {
        IdentityError::new(format!("wait for git {}: {error}", arguments.join(" ")))
    })?;
    if !output.status.success() {
        return Err(IdentityError::new(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "fixture assertions identify identity failures"
    )]

    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn must<T, E: fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[test]
    fn clean_lines_are_git_v1_lines_and_dirty_inputs_are_reflected() {
        let root = fixture();
        let paths = ["crates/velnor-workflow", "Cargo.toml"];
        let raw = must(git_ls_tree_raw(&root, "HEAD", &paths), "raw closure");
        let clean = must(worktree_lines(&root, "HEAD", &paths), "clean closure");
        assert_eq!(clean, raw.lines().map(str::to_owned).collect::<Vec<_>>());

        must(
            fs::write(root.join("crates/velnor-workflow/src/lib.rs"), b"dirty\n"),
            "dirty tracked input",
        );
        let dirty = must(worktree_lines(&root, "HEAD", &paths), "dirty closure");
        assert!(dirty
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/lib.rs")));
        assert_ne!(dirty, clean);

        must(
            fs::write(root.join("crates/velnor-workflow/src/new.rs"), b"new\n"),
            "untracked closure input",
        );
        let added = must(worktree_lines(&root, "HEAD", &paths), "added closure");
        assert!(added
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/new.rs")));
        must(
            fs::remove_file(root.join("crates/velnor-workflow/src/lib.rs")),
            "delete tracked input",
        );
        let deleted = must(worktree_lines(&root, "HEAD", &paths), "deleted closure");
        assert!(!deleted
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/lib.rs")));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            must(
                fs::set_permissions(root.join("Cargo.toml"), fs::Permissions::from_mode(0o755)),
                "mode change",
            );
            let mode = must(worktree_lines(&root, "HEAD", &paths), "mode closure");
            assert!(mode
                .iter()
                .any(|line| line.starts_with("100755 blob ") && line.ends_with("\tCargo.toml")));

            must(
                std::os::unix::fs::symlink(
                    "new.rs",
                    root.join("crates/velnor-workflow/src/lib.rs"),
                ),
                "symlink replacement",
            );
            let symlink = must(worktree_lines(&root, "HEAD", &paths), "symlink closure");
            assert!(symlink.iter().any(|line| {
                line.starts_with("120000 blob ")
                    && line.ends_with("\tcrates/velnor-workflow/src/lib.rs")
            }));
        }

        must(
            fs::remove_file(root.join("crates/velnor-workflow/src/lib.rs")),
            "remove replacement",
        );
        must(
            fs::create_dir(root.join("crates/velnor-workflow/src/lib.rs")),
            "directory replacement",
        );
        let error = match worktree_lines(&root, "HEAD", &paths) {
            Ok(_) => panic!("directory replacement must fail closed"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("unsupported closure input type"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn staged_add_delete_rename_and_restage_inputs_are_reflected() {
        let root = fixture();
        let paths = ["crates/velnor-workflow", "Cargo.toml"];
        let clean = must(worktree_lines(&root, "HEAD", &paths), "clean closure");

        let staged = root.join("crates/velnor-workflow/src/staged.rs");
        must(
            fs::write(&staged, b"pub const VALUE: &str = \"one\";\n"),
            "staged add",
        );
        assert!(test_git(
            &root,
            &["add", "--", "crates/velnor-workflow/src/staged.rs"]
        ));
        let staged_lines = must(worktree_lines(&root, "HEAD", &paths), "staged addition");
        assert!(staged_lines
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/staged.rs")));
        assert_ne!(staged_lines, clean);

        assert!(test_git(
            &root,
            &[
                "mv",
                "crates/velnor-workflow/src/staged.rs",
                "crates/velnor-workflow/src/renamed.rs"
            ]
        ));
        let renamed_lines = must(worktree_lines(&root, "HEAD", &paths), "staged rename");
        assert!(!renamed_lines
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/staged.rs")));
        assert!(renamed_lines
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/renamed.rs")));

        let renamed = root.join("crates/velnor-workflow/src/renamed.rs");
        must(
            fs::write(&renamed, b"pub const VALUE: &str = \"two\";\n"),
            "restaged rename content",
        );
        assert!(test_git(
            &root,
            &["add", "--", "crates/velnor-workflow/src/renamed.rs"]
        ));
        let restaged_lines = must(worktree_lines(&root, "HEAD", &paths), "restaged rename");
        assert_ne!(restaged_lines, renamed_lines);

        assert!(test_git(
            &root,
            &["rm", "-f", "--", "crates/velnor-workflow/src/renamed.rs"]
        ));
        let deleted_lines = must(worktree_lines(&root, "HEAD", &paths), "staged deletion");
        assert!(!deleted_lines
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/staged.rs")));
        assert!(!deleted_lines
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/renamed.rs")));
        assert!(test_git(
            &root,
            &["rm", "--", "crates/velnor-workflow/src/lib.rs"]
        ));
        let tracked_deleted_lines = must(
            worktree_lines(&root, "HEAD", &paths),
            "staged tracked deletion",
        );
        assert!(!tracked_deleted_lines
            .iter()
            .any(|line| line.ends_with("\tcrates/velnor-workflow/src/lib.rs")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn batched_git_blob_hashes_match_single_blob_hashes() {
        let root = fixture();
        let spaced = root.join("crates/velnor-workflow/src/name with spaces.rs");
        must(fs::write(&spaced, b"spaced\n"), "spaced fixture");
        let paths = vec![
            PathBuf::from("Cargo.toml"),
            PathBuf::from("crates/velnor-workflow/src/lib.rs"),
            PathBuf::from("crates/velnor-workflow/src/name with spaces.rs"),
        ];
        let batched = must(batch_git_blobs(&root, &paths), "batch hashes");
        for path in &paths {
            let bytes = must(fs::read(root.join(path)), "read hash fixture");
            let expected = must(git_blob(&root, &bytes), "single hash");
            assert_eq!(
                batched.get(path),
                Some(&expected),
                "hash for {}",
                path.display()
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn batched_git_blob_hashes_drain_while_writing_large_input() {
        let root = fixture();
        // Repeating a valid path keeps the fixture small while putting more
        // than a pipe buffer through stdin and stdout.  This must complete;
        // the old write-all-before-wait implementation deadlocked here.
        let paths = vec![PathBuf::from("Cargo.toml"); 20_000];
        let batched = must(batch_git_blobs(&root, &paths), "large batch hashes");
        assert_eq!(batched.len(), 1);
        let expected = must(
            git_blob(
                &root,
                &must(fs::read(root.join("Cargo.toml")), "read fixture Cargo"),
            ),
            "fixture Cargo hash",
        );
        assert_eq!(batched.get(Path::new("Cargo.toml")), Some(&expected));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn staged_addition_cargo_fixture_is_included_in_identity() {
        let root = cargo_fixture(CargoFixture::StagedAddition);
        let raw_before = must(
            git_ls_tree_raw(&root, "HEAD", &["src"]),
            "raw staged closure",
        );
        must(
            fs::write(
                root.join("src/generated.rs"),
                b"pub const VALUE: &str = \"staged\";\n",
            ),
            "staged source",
        );
        must(
            fs::write(
                root.join("src/lib.rs"),
                b"#[path = \"generated.rs\"]\nmod generated;\npub fn value() -> &'static str { generated::VALUE }\n",
            ),
            "staged source importer",
        );
        assert!(test_git(&root, &["add", "--", "src/generated.rs"]));
        assert_eq!(
            raw_before,
            must(
                git_ls_tree_raw(&root, "HEAD", &["src"]),
                "raw after staged add"
            )
        );
        cargo_clean(&root);
        let output = cargo_run(&root);
        assert!(
            output.ends_with("|staged"),
            "Cargo compiled staged source: {output}"
        );
        let lines = must(
            worktree_lines(&root, "HEAD", &["src"]),
            "staged Cargo closure",
        );
        assert!(lines
            .iter()
            .any(|line| line.ends_with("\tsrc/generated.rs")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ignored_relevant_inputs_fail_closed() {
        let root = fixture();
        must(
            fs::write(
                root.join("crates/velnor-workflow/ignored.txt"),
                b"ignored\n",
            ),
            "ignored input",
        );
        must(
            fs::create_dir_all(root.join("crates/velnor-workflow/target")),
            "target output directory",
        );
        must(
            fs::write(
                root.join("crates/velnor-workflow/target/generated"),
                b"generated\n",
            ),
            "target output",
        );
        let paths = ["crates/velnor-workflow", "Cargo.toml"];
        let error = match worktree_lines(&root, "HEAD", &paths) {
            Ok(_) => panic!("ignored closure input must fail closed"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("ignored closure input"), "{error}");
        must(
            fs::remove_file(root.join("crates/velnor-workflow/ignored.txt")),
            "remove ignored source",
        );
        let nested_target_error = match worktree_lines(&root, "HEAD", &paths) {
            Ok(_) => panic!("nested target path must not be treated as Cargo output"),
            Err(error) => error.to_string(),
        };
        assert!(
            nested_target_error.contains("ignored closure input"),
            "{nested_target_error}"
        );
        assert!(is_proven_cargo_output(Path::new("target/generated")));
        assert!(!is_proven_cargo_output(Path::new("src/target/generated")));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn external_symlink_cargo_fixture_proves_unrepresented_source_and_fails_closed() {
        let root = cargo_fixture(CargoFixture::ExternalSymlink);
        let raw_before = must(
            git_ls_tree_raw(&root, "HEAD", &["src"]),
            "raw symlink closure",
        );
        let first = cargo_run(&root);
        let (first_tree, first_value) = must(
            first
                .rsplit_once('|')
                .ok_or("symlink fixture output has no value"),
            "split symlink fixture output",
        );
        assert_eq!(first_tree, raw_before.replace('\n', "|"));

        must(
            fs::write(
                root.join("outside.rs"),
                b"pub const VALUE: &str = \"two\";\n",
            ),
            "change external symlink target",
        );
        let raw_after = must(
            git_ls_tree_raw(&root, "HEAD", &["src"]),
            "raw changed closure",
        );
        assert_eq!(
            raw_before, raw_after,
            "Git v1 lines omit the external target"
        );
        cargo_clean(&root);
        let second = cargo_run(&root);
        let (second_tree, second_value) = must(
            second
                .rsplit_once('|')
                .ok_or("changed symlink fixture output has no value"),
            "split changed symlink fixture output",
        );
        assert_eq!(second_tree, first_tree);
        assert_ne!(
            first_value, second_value,
            "Cargo compiled changed target bytes"
        );

        let error = match worktree_lines(&root, "HEAD", &["src"]) {
            Ok(_) => panic!("external source symlink must fail closed"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("escapes declared closure"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn ignored_nested_target_cargo_fixture_proves_source_and_fails_closed() {
        let root = cargo_fixture(CargoFixture::IgnoredNestedTarget);
        let raw_before = must(
            git_ls_tree_raw(&root, "HEAD", &["src"]),
            "raw target closure",
        );
        let first = cargo_run(&root);
        let (first_tree, first_value) = must(
            first
                .rsplit_once('|')
                .ok_or("target fixture output has no value"),
            "split target fixture output",
        );
        assert_eq!(first_tree, raw_before.replace('\n', "|"));

        must(
            fs::write(
                root.join("src/target/generated.rs"),
                b"pub const VALUE: &str = \"two\";\n",
            ),
            "change ignored target source",
        );
        let raw_after = must(
            git_ls_tree_raw(&root, "HEAD", &["src"]),
            "raw changed target closure",
        );
        assert_eq!(
            raw_before, raw_after,
            "Git v1 lines omit the ignored source"
        );
        cargo_clean(&root);
        let second = cargo_run(&root);
        let (second_tree, second_value) = must(
            second
                .rsplit_once('|')
                .ok_or("changed target fixture output has no value"),
            "split changed target fixture output",
        );
        assert_eq!(second_tree, first_tree);
        assert_ne!(
            first_value, second_value,
            "Cargo compiled ignored source bytes"
        );

        let error = match worktree_lines(&root, "HEAD", &["src"]) {
            Ok(_) => panic!("ignored nested target source must fail closed"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("ignored closure input"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum CargoFixture {
        ExternalSymlink,
        IgnoredNestedTarget,
        StagedAddition,
    }

    #[cfg(unix)]
    fn cargo_fixture(kind: CargoFixture) -> PathBuf {
        let root = fixture_root("cargo");
        must(
            fs::write(
                root.join("Cargo.toml"),
                b"[package]\nname = \"identity-probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\nbuild = \"build.rs\"\n",
            ),
            "cargo fixture manifest",
        );
        must(
            fs::write(
                root.join("build.rs"),
                br#"use std::process::Command;

fn main() {
    let output = Command::new("git")
        .args(["ls-tree", "-r", "HEAD", "--", "src"])
        .output()
        .expect("git ls-tree");
    assert!(output.status.success());
    let tree = String::from_utf8(output.stdout).expect("tree utf8").replace('\n', "|");
    println!("cargo:rustc-env=PROBE_TREE={tree}");
}
"#,
            ),
            "cargo fixture build script",
        );
        must(
            fs::write(
                root.join("src/lib.rs"),
                match kind {
                    CargoFixture::ExternalSymlink => {
                        b"#[path = \"generated.rs\"]\nmod generated;\npub fn value() -> &'static str { generated::VALUE }\n" as &[u8]
                    }
                    CargoFixture::IgnoredNestedTarget => {
                        b"#[path = \"target/generated.rs\"]\nmod generated;\npub fn value() -> &'static str { generated::VALUE }\n"
                    }
                    CargoFixture::StagedAddition => {
                        b"pub fn value() -> &'static str { \"base\" }\n"
                    }
                },
            ),
            "cargo fixture library",
        );
        must(
            fs::write(
                root.join("src/main.rs"),
                b"fn main() { println!(\"{}|{}\", env!(\"PROBE_TREE\"), identity_probe::value()); }\n",
            ),
            "cargo fixture binary",
        );
        match kind {
            CargoFixture::ExternalSymlink => {
                must(
                    fs::write(
                        root.join("outside.rs"),
                        b"pub const VALUE: &str = \"one\";\n",
                    ),
                    "external target",
                );
                must(
                    std::os::unix::fs::symlink("../outside.rs", root.join("src/generated.rs")),
                    "external source symlink",
                );
            }
            CargoFixture::IgnoredNestedTarget => {
                must(
                    fs::write(root.join(".gitignore"), b"src/target/\n"),
                    "target ignore",
                );
                must(
                    fs::create_dir_all(root.join("src/target")),
                    "nested target directory",
                );
                must(
                    fs::write(
                        root.join("src/target/generated.rs"),
                        b"pub const VALUE: &str = \"one\";\n",
                    ),
                    "ignored target source",
                );
            }
            CargoFixture::StagedAddition => {}
        }
        assert!(test_git(&root, &["add", "."]));
        assert!(test_git(&root, &["commit", "-qm", "cargo fixture"]));
        root
    }

    #[cfg(unix)]
    fn cargo_run(root: &Path) -> String {
        let output = must(
            Command::new("cargo")
                .current_dir(root)
                .env("CARGO_TARGET_DIR", root.join("cargo-target"))
                .args(["run", "--quiet", "--offline"])
                .output(),
            "run cargo fixture",
        );
        assert!(
            output.status.success(),
            "cargo fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        must(String::from_utf8(output.stdout), "cargo fixture stdout")
            .trim()
            .to_owned()
    }

    #[cfg(unix)]
    fn cargo_clean(root: &Path) {
        let status = must(
            Command::new("cargo")
                .current_dir(root)
                .env("CARGO_TARGET_DIR", root.join("cargo-target"))
                .args(["clean"])
                .status(),
            "clean cargo fixture",
        );
        assert!(status.success(), "cargo clean failed");
    }

    fn fixture_root(kind: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-identity-{kind}-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            must(
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH),
                "fixture timestamp",
            )
            .as_nanos()
        ));
        must(fs::create_dir_all(root.join("src")), "fixture source dirs");
        assert!(test_git(&root, &["init", "-q"]));
        assert!(test_git(
            &root,
            &["config", "user.email", "test@example.invalid"]
        ));
        assert!(test_git(&root, &["config", "user.name", "test"]));
        root
    }

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-identity-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            must(
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH),
                "fixture timestamp",
            )
            .as_nanos()
        ));
        must(
            fs::create_dir_all(root.join("crates/velnor-workflow/src")),
            "fixture dirs",
        );
        must(
            fs::write(root.join("Cargo.toml"), b"[workspace]\n"),
            "fixture manifest",
        );
        must(
            fs::write(
                root.join(".gitignore"),
                b"crates/velnor-workflow/ignored.txt\ncrates/velnor-workflow/target/\n",
            ),
            "fixture ignore",
        );
        must(
            fs::write(root.join("crates/velnor-workflow/src/lib.rs"), b"clean\n"),
            "fixture source",
        );
        assert!(test_git(&root, &["init", "-q"]));
        assert!(test_git(
            &root,
            &["config", "user.email", "test@example.invalid"]
        ));
        assert!(test_git(&root, &["config", "user.name", "test"]));
        assert!(test_git(&root, &["add", "."]));
        assert!(test_git(&root, &["commit", "-qm", "fixture"]));
        root
    }

    fn test_git(root: &Path, arguments: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .status()
            .is_ok_and(|status| status.success())
    }
}
