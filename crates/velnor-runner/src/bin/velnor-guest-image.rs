//! Build or stage the reproducible Firecracker guest kernel and rootfs.

use std::path::PathBuf;

use velnor_runner::execution::{
    build_guest_image_cli, expected_checksums_for_arch, stage_release_dir, GuestArch, RealHostFs,
    KERNEL_TARBALL,
};

const USAGE: &str = "usage: velnor-guest-image stage --root DIR --arch ARCH [--rootfs-sha256 HEX] [--guest-agent-sha256 HEX] | build --arch ARCH --out DIR --tarball PATH --guest-agent PATH";

fn main() {
    if let Err(error) = run() {
        eprintln!("velnor-guest-image: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("stage") => stage(&mut args),
        Some("build") => build(&mut args),
        _ => Err(USAGE.into()),
    }
}

/// Fail closed on anything but exactly 64 lowercase hex characters.
fn parse_sha256_pin(flag: &str, value: String) -> Result<String, String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value)
    } else {
        Err(format!("{flag} must be 64 lowercase hex characters"))
    }
}

fn stage(args: &mut impl Iterator<Item = String>) -> Result<(), String> {
    let mut root = None;
    let mut arch = None;
    let mut rootfs_sha256 = None;
    let mut guest_agent_sha256 = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--root" => {
                root = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "missing --root value".to_string())?,
                ));
            }
            "--arch" => {
                arch = Some(
                    args.next()
                        .ok_or_else(|| "missing --arch value".to_string())?,
                );
            }
            "--rootfs-sha256" => {
                rootfs_sha256 = Some(parse_sha256_pin(
                    "--rootfs-sha256",
                    args.next()
                        .ok_or_else(|| "missing --rootfs-sha256 value".to_string())?,
                )?);
            }
            "--guest-agent-sha256" => {
                guest_agent_sha256 = Some(parse_sha256_pin(
                    "--guest-agent-sha256",
                    args.next()
                        .ok_or_else(|| "missing --guest-agent-sha256 value".to_string())?,
                )?);
            }
            other => return Err(format!("unknown stage flag {other}")),
        }
    }
    let root = root.ok_or("missing --root")?;
    let arch = GuestArch::parse(arch.as_deref().ok_or("missing --arch")?)
        .map_err(|error| error.to_string())?;
    let mut expected =
        expected_checksums_for_arch(arch.as_str()).map_err(|error| error.to_string())?;
    if let Some(digest) = rootfs_sha256 {
        expected.rootfs = digest;
    }
    if let Some(digest) = guest_agent_sha256 {
        expected.guest_agent = digest;
    }
    let mut fs = RealHostFs;
    stage_release_dir(&root, &mut fs, Some(&expected)).map_err(|error| error.to_string())?;
    println!("staged coherent microVM identity in {}", root.display());
    Ok(())
}

fn build(args: &mut impl Iterator<Item = String>) -> Result<(), String> {
    let mut arch = None;
    let mut out = None;
    let mut tarball = None;
    let mut guest_agent = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--arch" => {
                arch = Some(
                    args.next()
                        .ok_or_else(|| "missing --arch value".to_string())?,
                );
            }
            "--out" => {
                out = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "missing --out value".to_string())?,
                ));
            }
            "--tarball" => {
                tarball = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "missing --tarball value".to_string())?,
                ));
            }
            "--guest-agent" => {
                guest_agent = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "missing --guest-agent value".to_string())?,
                ));
            }
            other => return Err(format!("unknown build flag {other}")),
        }
    }
    let arch =
        GuestArch::parse(arch.as_deref().ok_or("missing --arch")?).map_err(|e| e.to_string())?;
    let out = out.ok_or("missing --out")?;
    let tarball_path = tarball.ok_or("missing --tarball")?;
    let tarball_bytes = std::fs::read(&tarball_path)
        .map_err(|error| format!("read {}: {error}", tarball_path.display()))?;
    let guest_agent_path = guest_agent.ok_or("missing --guest-agent")?;
    let agent_bytes = std::fs::read(&guest_agent_path)
        .map_err(|error| format!("read {}: {error}", guest_agent_path.display()))?;
    let work = out.join("work");
    let (kernel, rootfs) = build_guest_image_cli(&work, &out, arch, &tarball_bytes, &agent_bytes)
        .map_err(|error| error.to_string())?;
    println!("built {} and {}", kernel.display(), rootfs.display());
    println!("kernel source {KERNEL_TARBALL}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use velnor_runner::execution::{HostFs, MemoryFs};

    fn strings(args: &[&str]) -> std::vec::IntoIter<String> {
        args.iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn sha256_pin_accepts_64_lowercase_hex() {
        let digest = "a".repeat(63) + "9";
        assert_eq!(
            parse_sha256_pin("--rootfs-sha256", digest.clone()).unwrap(),
            digest
        );
    }

    #[test]
    fn sha256_pin_rejects_bad_length_and_case() {
        let err = parse_sha256_pin("--rootfs-sha256", "a".repeat(63)).unwrap_err();
        assert!(err.contains("--rootfs-sha256"), "{err}");
        let err = parse_sha256_pin("--rootfs-sha256", "A".repeat(64)).unwrap_err();
        assert!(err.contains("64 lowercase hex"), "{err}");
        let err = parse_sha256_pin("--guest-agent-sha256", "g".repeat(64)).unwrap_err();
        assert!(err.contains("--guest-agent-sha256"), "{err}");
    }

    #[test]
    fn stage_rejects_unknown_flag() {
        let error = stage(&mut strings(&[
            "--root", "/x", "--arch", "x86_64", "--bogus",
        ]))
        .unwrap_err();
        assert_eq!(error, "unknown stage flag --bogus");
    }

    #[test]
    fn stage_rejects_invalid_rootfs_digest_flag() {
        let error = stage(&mut strings(&[
            "--root",
            "/x",
            "--arch",
            "x86_64",
            "--rootfs-sha256",
            "not-hex",
        ]))
        .unwrap_err();
        assert!(error.contains("--rootfs-sha256"), "{error}");
    }

    #[test]
    fn build_requires_guest_agent() {
        let error = build(&mut strings(&[
            "--arch",
            "x86_64",
            "--out",
            "/x",
            "--tarball",
            "/missing",
        ]))
        .unwrap_err();
        assert_eq!(error, "missing --guest-agent");
    }

    #[test]
    fn staged_rootfs_must_match_passed_digest() {
        let mut fs = MemoryFs::default();
        let root = PathBuf::from("/release/microvm");
        fs.write(&root.join("firecracker"), b"fc").unwrap();
        fs.write(&root.join("jailer"), b"jailer").unwrap();
        fs.write(&root.join("vmlinux"), b"kernel").unwrap();
        fs.write(&root.join("rootfs.ext4"), b"rootfs").unwrap();
        fs.write(&root.join("velnor-guest-agent"), b"agent")
            .unwrap();
        let mut expected = stage_release_dir(&root, &mut fs, None).unwrap();
        expected.rootfs = "0".repeat(64);
        let error = stage_release_dir(&root, &mut fs, Some(&expected)).unwrap_err();
        assert_eq!(error.requirement, "artifacts.checksum");
        assert!(error.to_string().contains("rootfs"), "{error}");
        expected.rootfs = velnor_runner::execution::stage_release_dir(&root, &mut fs, None)
            .unwrap()
            .rootfs;
        expected.guest_agent = "1".repeat(64);
        let error = stage_release_dir(&root, &mut fs, Some(&expected)).unwrap_err();
        assert_eq!(error.requirement, "artifacts.checksum");
        assert!(error.to_string().contains("guest_agent"), "{error}");
    }
}
