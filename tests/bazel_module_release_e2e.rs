//! `scripts/package-bazel-module.sh`: the release's Bazel module names the SHA-256 of every compiler
//! archive the release publishes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = "2.4.21-build.7";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The hosts `extensions.bzl` registers a toolchain for, in its (sorted) order.
fn extension_hosts() -> Vec<String> {
    let extension = fs::read_to_string(root().join("tools/krusty-bazel/extensions.bzl"))
        .expect("read extensions.bzl");
    extension
        .lines()
        .filter_map(|line| line.strip_prefix("    \""))
        .filter_map(|line| line.split_once("\": [\"@platforms"))
        .map(|(triple, _)| triple.to_owned())
        .collect()
}

fn archive_name(triple: &str) -> String {
    let extension = if triple.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("krusty-{VERSION}-{triple}.{extension}")
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("run sha256sum");
    assert!(output.status.success(), "sha256sum failed: {output:?}");
    String::from_utf8(output.stdout).expect("sha256sum output is UTF-8")[..64].to_owned()
}

/// A dist directory as the release job sees it: one compiler archive per host, plus files the
/// module must not pick up.
fn dist(name: &str) -> PathBuf {
    let dist =
        std::env::temp_dir().join(format!("krusty-bazel-module-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dist);
    fs::create_dir_all(&dist).expect("create dist");
    for triple in extension_hosts() {
        fs::write(
            dist.join(archive_name(&triple)),
            format!("compiler for {triple}"),
        )
        .expect("write compiler archive");
        fs::write(
            dist.join(format!("krusty-lsp-{VERSION}-{triple}.tar.gz")),
            "language server",
        )
        .expect("write language server archive");
    }
    fs::write(dist.join(format!("krusty-{VERSION}-SHA256SUMS")), "sums").expect("write sums");
    dist
}

fn package(dist: &Path) -> std::process::Output {
    Command::new(root().join("scripts/package-bazel-module.sh"))
        .arg(VERSION)
        .arg(dist)
        .output()
        .expect("run package-bazel-module.sh")
}

fn tar(args: &[&str], archive: &Path) -> String {
    let output = Command::new("tar")
        .args(args)
        .arg(archive)
        .output()
        .expect("run tar");
    assert!(output.status.success(), "tar {args:?} failed: {output:?}");
    String::from_utf8(output.stdout).expect("tar output is UTF-8")
}

#[test]
fn stamps_the_release_and_every_compiler_checksum_into_the_module() {
    let dist = dist("stamp");
    let output = package(&dist);
    assert!(output.status.success(), "packaging failed: {output:?}");
    let archive = dist.join(format!("krusty-bazel-{VERSION}.tar.gz"));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n", archive.display())
    );

    assert_eq!(
        tar(&["-tzf"], &archive),
        "BUILD.bazel\nLICENSE\nMODULE.bazel\nREADME.md\ndefs.bzl\nextensions.bzl\ntoolchain.bzl\n"
    );
    let module = root().join("tools/krusty-bazel");
    for file in [
        "BUILD.bazel",
        "README.md",
        "defs.bzl",
        "extensions.bzl",
        "toolchain.bzl",
    ] {
        let packaged = Command::new("tar")
            .arg("-xzOf")
            .arg(&archive)
            .arg(file)
            .output()
            .expect("run tar");
        assert_eq!(
            packaged.stdout,
            fs::read(module.join(file)).unwrap(),
            "{file} is packaged unchanged"
        );
    }

    let source = fs::read_to_string(module.join("MODULE.bazel")).unwrap();
    let sums: String = extension_hosts()
        .iter()
        .map(|triple| {
            format!(
                "        \"{triple}\": \"{}\",\n",
                sha256(&dist.join(archive_name(triple)))
            )
        })
        .collect();
    let expected = source
        .replace(
            "    version = \"0.0.1\",\n",
            &format!("    version = \"{VERSION}\",\n"),
        )
        .replace(
            "# @krusty-release@\n",
            &format!(
                "krusty.release(\n    version = \"{VERSION}\",\n    sha256 = {{\n{sums}    }},\n)\n"
            ),
        );
    assert_ne!(expected, source, "the source module has both stamp points");
    let stamped = Command::new("tar")
        .arg("-xzOf")
        .arg(&archive)
        .arg("MODULE.bazel")
        .output()
        .expect("run tar");
    assert_eq!(String::from_utf8(stamped.stdout).unwrap(), expected);
    assert_eq!(
        fs::read_to_string(dist.join(format!("krusty-bazel-{VERSION}.MODULE.bazel"))).unwrap(),
        expected,
        "the registry's copy of MODULE.bazel is the archive's"
    );

    assert_eq!(
        fs::read_to_string(dist.join(format!("krusty-bazel-{VERSION}.tar.gz.sha256"))).unwrap(),
        format!("{}  krusty-bazel-{VERSION}.tar.gz\n", sha256(&archive))
    );

    // The same inputs give the same bytes, so the archive's checksum is reproducible.
    let first = fs::read(&archive).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let again = package(&dist);
    assert!(again.status.success(), "repackaging failed: {again:?}");
    assert_eq!(fs::read(&archive).unwrap(), first);
    let _ = fs::remove_dir_all(&dist);
}

#[test]
fn refuses_a_release_missing_a_host() {
    let dist = dist("missing");
    let missing = dist.join(archive_name("aarch64-pc-windows-msvc"));
    fs::remove_file(&missing).unwrap();
    let output = package(&dist);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!("missing compiler archive {}\n", missing.display())
    );
    assert!(!dist.join(format!("krusty-bazel-{VERSION}.tar.gz")).exists());
    let _ = fs::remove_dir_all(&dist);
}

#[test]
fn refuses_an_archive_for_an_unknown_host() {
    let dist = dist("unknown");
    let unknown = dist.join(format!(
        "krusty-{VERSION}-riscv64gc-unknown-linux-gnu.tar.gz"
    ));
    fs::write(&unknown, "compiler").unwrap();
    let output = package(&dist);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "{} is for a host extensions.bzl does not know\n",
            unknown.display()
        )
    );
    let _ = fs::remove_dir_all(&dist);
}
