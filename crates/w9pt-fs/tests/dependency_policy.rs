//! Dependency and source-boundary enforcement for the runtime-neutral engine.

use std::{fs, path::Path};

const ALLOWED_DEPENDENCIES: [&str; 3] = ["w9pt", "w9pt-fs-state", "w9pt-fs-storage"];

#[test]
fn normal_dependencies_are_exactly_the_runtime_neutral_core_crates() {
    let manifest = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read engine manifest");
    let dependencies = table_lines(&manifest, "[dependencies]");
    assert_eq!(dependencies, ALLOWED_DEPENDENCIES);

    for forbidden_table in ["[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "engine manifest contains forbidden dependency table {forbidden_table}"
        );
    }
}

#[test]
fn engine_source_has_no_hidden_runtime_or_platform_global() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&source_root, &mut files);
    files.sort();

    let forbidden = [
        "aws_sdk_",
        "getrandom",
        "rand::",
        "sea_orm::",
        "sqlx::",
        "std::env",
        "std::net",
        "std::sync::OnceLock",
        "std::thread",
        "std::time",
        "thread_local!",
        "tokio::",
    ];

    for path in files {
        let source = fs::read_to_string(&path).expect("read Rust source");
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "{} contains forbidden engine-core dependency `{needle}`",
                path.display()
            );
        }
    }
}

fn table_lines<'a>(manifest: &'a str, header: &str) -> Vec<&'a str> {
    let mut in_table = false;
    let mut dependencies = Vec::new();

    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_table = trimmed == header;
            continue;
        }
        if !in_table || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (name, _) = trimmed
            .split_once('=')
            .expect("dependency line contains an equals sign");
        dependencies.push(name.trim());
    }

    dependencies.sort_unstable();
    dependencies
}

fn collect_rust_files(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).expect("read source directory") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}
