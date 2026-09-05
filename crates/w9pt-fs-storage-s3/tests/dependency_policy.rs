#![allow(missing_docs)]

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    process::Command,
};

use serde_json::Value;

const PROTECTED: [&str; 4] = ["w9pt", "w9pt-fs", "w9pt-fs-state", "w9pt-fs-storage"];

fn is_forbidden(name: &str) -> bool {
    name == "w9pt-fs-storage-s3"
        || name.starts_with("aws-")
        || name.starts_with("tokio")
        || matches!(
            name,
            "h2" | "http"
                | "http-body"
                | "http-body-util"
                | "hyper"
                | "hyper-rustls"
                | "native-tls"
                | "openssl"
                | "rustls"
                | "rustls-native-certs"
                | "rustls-pki-types"
                | "rustls-webpki"
                | "tower"
                | "tower-http"
                | "tower-service"
        )
}

#[test]
fn core_crates_do_not_reach_s3_runtime_dependencies() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("adapter crate is under workspace/crates")
        .to_path_buf();
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--all-features",
        ])
        .current_dir(workspace)
        .output()
        .expect("cargo metadata must run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("valid cargo metadata");

    let packages = metadata["packages"]
        .as_array()
        .expect("packages is an array");
    let names = packages
        .iter()
        .map(|package| {
            (
                package["id"].as_str().expect("package id").to_owned(),
                package["name"].as_str().expect("package name").to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let roots = PROTECTED
        .iter()
        .map(|protected| {
            names
                .iter()
                .find_map(|(id, name)| (name == protected).then(|| id.clone()))
                .unwrap_or_else(|| panic!("missing protected package {protected}"))
        })
        .collect::<Vec<_>>();

    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolved nodes is an array");
    let mut edges = BTreeMap::<String, Vec<String>>::new();
    for node in nodes {
        let id = node["id"].as_str().expect("node id").to_owned();
        let mut dependencies = Vec::new();
        for dependency in node["deps"].as_array().expect("node deps is an array") {
            let included = dependency["dep_kinds"]
                .as_array()
                .expect("dependency kinds is an array")
                .iter()
                .any(|kind| kind["kind"].is_null() || kind["kind"] == "build");
            if included {
                dependencies.push(
                    dependency["pkg"]
                        .as_str()
                        .expect("dependency package id")
                        .to_owned(),
                );
            }
        }
        edges.insert(id, dependencies);
    }

    for root in roots {
        let root_name = names.get(&root).expect("protected package name");
        let mut queue = VecDeque::from([root]);
        let mut visited = BTreeSet::new();
        while let Some(id) = queue.pop_front() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let name = names.get(&id).expect("resolved package has metadata");
            assert!(
                !is_forbidden(name),
                "protected crate {root_name} reaches forbidden package {name}"
            );
            queue.extend(edges.get(&id).into_iter().flatten().cloned());
        }
    }
}
