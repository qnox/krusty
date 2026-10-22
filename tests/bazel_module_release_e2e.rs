//! `scripts/package-bazel-module.sh`: the release's Bazel module names the SHA-256 of every compiler
//! archive the release publishes, and the release job packages it before publishing.

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
fn the_extension_registers_exactly_the_hosts_the_release_builds() {
    let workflow =
        fs::read_to_string(root().join(".github/workflows/ci.yml")).expect("read ci.yml");
    let mut built: Vec<String> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- { triple: "))
        .map(|line| line.split(',').next().unwrap().trim().to_owned())
        .collect();
    built.sort();
    assert_eq!(
        extension_hosts(),
        built,
        "extensions.bzl HOSTS must list every build-release triple, sorted"
    );
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

#[test]
fn the_release_job_packages_the_module_before_publishing() {
    let workflow =
        fs::read_to_string(root().join(".github/workflows/ci.yml")).expect("read ci.yml");
    let job = &workflow[workflow.find("\n  release:\n").expect("release job")..];
    let steps: Vec<&str> = job.split("\n      - ").skip(1).collect();
    let position = |needle: &str| {
        steps
            .iter()
            .position(|step| step.contains(needle))
            .unwrap_or_else(|| panic!("release job has a step running `{needle}`"))
    };
    let collect = position("name: collect all target artifacts");
    let version = position("name: compute version + tag");
    let package = position("scripts/package-bazel-module.sh \"${{ steps.v.outputs.ver }}\" dist");
    let publish = position("softprops/action-gh-release");
    assert!(collect < package && version < package && package < publish);
    assert!(steps[package].contains("if: steps.tip.outputs.publish == 'true'"));
}

#[test]
fn every_merge_builds_the_packaged_module_under_bazel() {
    let workflow =
        fs::read_to_string(root().join(".github/workflows/ci.yml")).expect("read ci.yml");
    let start = workflow.find("\n  bazel:\n").expect("bazel job");
    let job = &workflow[start..];
    let job = &job[..job[1..].find("\n  # ").map_or(job.len(), |end| end + 1)];
    assert!(job.contains("    needs: [build-tool-bins]\n"), "{job}");
    assert!(job.contains("        bazel: [8.4.2, 9.3.1]\n"), "{job}");
    assert!(
        job.contains("          USE_BAZEL_VERSION: ${{ matrix.bazel }}\n"),
        "{job}"
    );
    assert!(
        job.contains(
            "          scripts/bazel-smoke.sh \"$PWD/krusty\" \"$RUNNER_TEMP/kotlin-stdlib.jar\"\n"
        ),
        "{job}"
    );
    let smoke = fs::read_to_string(root().join("scripts/bazel-smoke.sh")).expect("read smoke");
    assert!(smoke.contains(
        "for exec_groups in --incompatible_auto_exec_groups --noincompatible_auto_exec_groups; do\n"
    ));
    assert!(smoke
        .contains("  for compiler in toolchain --@krusty//:krusty_binary=//tools:krusty; do\n"));
    assert!(
        smoke.contains("    for name, use_worker in [(\"actions\", False), (\"worker\", True)]\n")
    );
}
