use std::{fs, path::Path};

use anyhow::{Context, Result};

pub fn clone_or_copy(source: &Path, destination: &Path) -> Result<u64> {
    clone_or_copy_with_method(source, destination).map(|(bytes, _)| bytes)
}

fn clone_or_copy_with_method(source: &Path, destination: &Path) -> Result<(u64, bool)> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    match try_clone(source, destination) {
        Ok(()) => {
            let metadata = fs::metadata(source)
                .with_context(|| format!("stat cloned file {}", source.display()))?;
            fs::set_permissions(destination, metadata.permissions()).with_context(|| {
                format!("set cloned file permissions {}", destination.display())
            })?;
            Ok((metadata.len(), true))
        }
        Err(_) => {
            let _ = fs::remove_file(destination);
            fs::copy(source, destination)
                .map(|bytes| (bytes, false))
                .with_context(|| format!("copy {} to {}", source.display(), destination.display()))
        }
    }
}

#[cfg(target_os = "linux")]
fn try_clone(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = fs::File::open(source)?;
    let destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    rustix::fs::ioctl_ficlone(&destination, &source).map_err(std::io::Error::from)
}

#[cfg(target_os = "macos")]
fn try_clone(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = fs::File::open(source)?;
    let parent = fs::File::open(destination.parent().unwrap_or_else(|| Path::new(".")))?;
    let file_name = destination.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "destination has no file name",
        )
    })?;
    rustix::fs::fclonefileat(&source, &parent, file_name, rustix::fs::CloneFlags::empty())
        .map_err(std::io::Error::from)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn try_clone(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "filesystem cloning is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_or_copy_preserves_content_and_length() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination = root.join("nested/destination");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, b"reflink-or-copy").unwrap();

        let probe = root.join("probe");
        let reflink_capable = try_clone(&source, &probe).is_ok();
        let _ = fs::remove_file(probe);
        let (bytes, used_reflink) = clone_or_copy_with_method(&source, &destination).unwrap();
        assert_eq!(bytes, 15);
        if reflink_capable {
            assert!(
                used_reflink,
                "capable filesystem did not use its clone path"
            );
        }
        assert_eq!(fs::read(&destination).unwrap(), b"reflink-or-copy");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clone_or_copy_replaces_existing_destination() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"stale-long-value").unwrap();

        clone_or_copy(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        fs::remove_dir_all(root).unwrap();
    }
}
