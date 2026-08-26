//! Build or stage the reproducible Firecracker guest kernel and rootfs.

use std::path::PathBuf;

use velnor_runner::execution::{
    build_guest_image_cli, stage_release_dir, GuestArch, RealHostFs, KERNEL_TARBALL,
};

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
        _ => Err(
            "usage: velnor-guest-image stage --root DIR | build --arch ARCH --out DIR --tarball PATH [--guest-agent PATH]"
                .into(),
        ),
    }
}

fn stage(args: &mut impl Iterator<Item = String>) -> Result<(), String> {
    let root = flag_value(args, "--root")?;
    let mut fs = RealHostFs;
    stage_release_dir(&root, &mut fs, None).map_err(|error| error.to_string())?;
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
    let agent_bytes = match guest_agent {
        Some(path) => Some(
            std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
        ),
        None => None,
    };
    let work = out.join("work");
    let (kernel, rootfs) =
        build_guest_image_cli(&work, &out, arch, &tarball_bytes, agent_bytes.as_deref())
            .map_err(|error| error.to_string())?;
    println!("built {} and {}", kernel.display(), rootfs.display());
    println!("kernel source {KERNEL_TARBALL}");
    Ok(())
}

fn flag_value(args: &mut impl Iterator<Item = String>, name: &str) -> Result<PathBuf, String> {
    match args.next().as_deref() {
        Some(flag) if flag == name => args
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| format!("missing {name} value")),
        other => Err(format!(
            "expected {name}, got {}",
            other.unwrap_or_default()
        )),
    }
}
