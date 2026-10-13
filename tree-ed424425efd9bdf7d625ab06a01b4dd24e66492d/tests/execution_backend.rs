//! Shipped config parse and load (twice). Dispatch coverage lives in-crate.

use velnor_model::{ExecutionBackendKind, ExecutionFile};
use velnor_runner::execution::load_execution_file;

#[test]
fn only_docker_and_microvm_are_accepted() {
    let docker = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    assert_eq!(docker.backend(), ExecutionBackendKind::Docker);
    let microvm = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    assert_eq!(microvm.backend(), ExecutionBackendKind::MicroVm);
    let err = ExecutionFile::parse_toml("[execution]\nbackend = \"kata\"\n").unwrap_err();
    assert_eq!(err.field, "[execution] backend");
    assert!(err.to_string().contains("kata"));
}

#[test]
fn load_execution_file_twice_agrees() {
    let dir = std::env::temp_dir().join(format!(
        "velnor-exec-toml-int-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("execution.toml"),
        "[execution]\nbackend = \"microvm\"\n",
    )
    .unwrap();
    let first = load_execution_file(&dir, None).unwrap();
    let second = load_execution_file(&dir, None).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.backend(), ExecutionBackendKind::MicroVm);
    std::fs::remove_dir_all(dir).ok();
}
