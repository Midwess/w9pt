//! Dependency and source guardrails for the SeaORM-only adapter boundary.

use std::{fs, path::Path, process::Command};

fn collect_rust_files(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).expect("adapter source directory is readable") {
        let path = entry.expect("directory entry is readable").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn seaorm_is_direct_and_sqlx_is_not_a_direct_or_source_dependency() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        fs::read_to_string(crate_root.join("Cargo.toml")).expect("adapter Cargo.toml is readable");
    assert!(manifest.contains("sea-orm = { version = \"=1.1.20\""));
    assert!(manifest.contains("default-features = false"));
    assert!(manifest.contains("\"sqlx-postgres\""));
    assert!(manifest.contains("\"runtime-tokio\""));
    let driver_name = concat!("sql", "x");
    assert!(
        !manifest.lines().any(|line| {
            let line = line.trim();
            line.starts_with(&format!("{driver_name} ="))
                || line.contains(&format!("package = \"{driver_name}\""))
        }),
        "SQLx must not be declared directly under any dependency kind or target"
    );

    let metadata = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--locked",
            "--manifest-path",
        ])
        .arg(crate_root.join("Cargo.toml"))
        .output()
        .expect("cargo metadata executes");
    assert!(metadata.status.success(), "cargo metadata failed");
    let metadata: serde_json::Value =
        serde_json::from_slice(&metadata.stdout).expect("cargo metadata is valid JSON");
    let package = metadata["packages"]
        .as_array()
        .expect("metadata packages is an array")
        .iter()
        .find(|package| package["name"] == "w9pt-fs-state-postgres")
        .expect("adapter package is present");
    let dependencies = package["dependencies"]
        .as_array()
        .expect("package dependencies is an array");
    assert!(
        dependencies
            .iter()
            .all(|dependency| dependency["name"] != driver_name),
        "SQLx must not occur anywhere in the adapter's depth-one Cargo graph"
    );
    let seaorm = dependencies
        .iter()
        .find(|dependency| dependency["name"] == "sea-orm")
        .expect("SeaORM is a direct dependency");
    assert_eq!(seaorm["req"], "=1.1.20");
    let seaorm_migration = dependencies
        .iter()
        .find(|dependency| dependency["name"] == "sea-orm-migration")
        .expect("SeaORM Migration is a direct dependency");
    assert_eq!(seaorm_migration["req"], "=1.1.20");

    let mut rust_files = Vec::new();
    collect_rust_files(crate_root, &mut rust_files);
    for path in rust_files {
        let source = fs::read_to_string(&path).expect("Rust source is readable");
        let direct_path = concat!("sql", "x::");
        let direct_import = concat!("use ", "sql", "x");
        let private_schema = concat!("w9pt_fs_state_", "v1");
        let stock_ledger = concat!("seaql_", "migrations");
        assert!(
            !source.contains(direct_path),
            "direct SQLx path in {path:?}"
        );
        assert!(
            !source.contains(direct_import),
            "direct SQLx import in {path:?}"
        );
        assert!(
            !source.contains(private_schema),
            "removed private schema reference in {path:?}"
        );
        assert!(
            !source.contains(stock_ledger),
            "stock SeaORM migration ledger reference in {path:?}"
        );
    }
}
