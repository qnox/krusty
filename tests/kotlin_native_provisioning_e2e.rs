//! `scripts/kotlin-native.sh` treats a cached distribution as present only when its common stdlib
//! KLIB carries metadata. CI restores `target/` through a cache that keeps directories but deletes
//! their files, so a directory skeleton must be discarded and provisioned again.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VERSION: &str = "9.9.9";

fn host() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        (os, arch) => panic!("no Kotlin/Native host for {os}-{arch}"),
    }
}

fn distribution_name() -> String {
    format!("kotlin-native-prebuilt-{}-{VERSION}", host())
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "krusty-kotlin-native-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create provisioning scratch directory");
    dir
}

/// Write the common stdlib payload the provisioning check requires under `root`.
fn write_stdlib(root: &Path) {
    let stdlib = root.join("klib/common/stdlib/default");
    fs::create_dir_all(stdlib.join("linkdata/package_kotlin")).expect("create stdlib linkdata");
    fs::write(stdlib.join("manifest"), "unique_name=stdlib\n").expect("write stdlib manifest");
    fs::write(stdlib.join("linkdata/package_kotlin/0_kotlin.knm"), [1u8])
        .expect("write stdlib fragment");
}

/// A release mirror holding one complete distribution archive for [`VERSION`].
fn mirror(dir: &Path) -> PathBuf {
    let staging = dir.join("staging");
    write_stdlib(&staging.join(distribution_name()));
    let releases = dir.join("releases");
    let tag = releases.join(format!("v{VERSION}"));
    fs::create_dir_all(&tag).expect("create mirror tag directory");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(tag.join(format!("{}.tar.gz", distribution_name())))
        .arg("-C")
        .arg(&staging)
        .arg(distribution_name())
        .status()
        .expect("run tar");
    assert!(status.success(), "tar archives the fixture distribution");
    releases
}

fn provision(cache: &Path, releases: &Path) -> Output {
    Command::new("bash")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/kotlin-native.sh"))
        .arg(VERSION)
        .arg(cache)
        .env(
            "KRUSTY_KOTLIN_NATIVE_RELEASES",
            format!("file://{}", releases.display()),
        )
        .output()
        .expect("run Kotlin/Native provisioning")
}

#[test]
fn a_directory_only_cached_stdlib_is_discarded_and_provisioned_again() {
    let dir = scratch("skeleton");
    let releases = mirror(&dir);
    let cache = dir.join("cache");
    let root = cache.join(VERSION).join(distribution_name());
    // What a restored `target/` holds after its files were cleaned: the directories alone.
    fs::create_dir_all(root.join("klib/common/stdlib/default/linkdata/package_kotlin"))
        .expect("create the directory-only cache");

    let output = provision(&cache, &releases);

    assert_eq!(
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("UTF-8 stdout"),
            String::from_utf8(output.stderr).expect("UTF-8 stderr"),
        ),
        (
            Some(0),
            format!("{}\n", root.display()),
            format!(
                "discarding incomplete kotlin-native {VERSION} cache at {}…\n\
                 downloading kotlin-native {VERSION} for {}…\n",
                root.display(),
                host()
            ),
        )
    );
    assert_eq!(
        fs::read(root.join("klib/common/stdlib/default/linkdata/package_kotlin/0_kotlin.knm"))
            .expect("the provisioned fragment"),
        [1u8]
    );
    fs::remove_dir_all(dir).expect("remove provisioning scratch directory");
}

#[test]
fn a_complete_cached_stdlib_is_reused_without_a_download() {
    let dir = scratch("complete");
    let cache = dir.join("cache");
    let root = cache.join(VERSION).join(distribution_name());
    write_stdlib(&root);

    // No mirror exists, so any download attempt fails the run.
    let output = provision(&cache, &dir.join("no-releases"));

    assert_eq!(
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("UTF-8 stdout"),
            String::from_utf8(output.stderr).expect("UTF-8 stderr"),
        ),
        (Some(0), format!("{}\n", root.display()), String::new())
    );
    fs::remove_dir_all(dir).expect("remove provisioning scratch directory");
}

#[test]
fn an_archive_without_stdlib_metadata_is_rejected() {
    let dir = scratch("empty-archive");
    let staging = dir.join("staging");
    fs::create_dir_all(
        staging
            .join(distribution_name())
            .join("klib/common/stdlib/default"),
    )
    .expect("create the metadata-free distribution");
    let releases = dir.join("releases");
    let tag = releases.join(format!("v{VERSION}"));
    fs::create_dir_all(&tag).expect("create mirror tag directory");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(tag.join(format!("{}.tar.gz", distribution_name())))
        .arg("-C")
        .arg(&staging)
        .arg(distribution_name())
        .status()
        .expect("run tar");
    assert!(status.success(), "tar archives the fixture distribution");
    let cache = dir.join("cache");

    let output = provision(&cache, &releases);

    assert_eq!(
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("UTF-8 stdout"),
            String::from_utf8(output.stderr).expect("UTF-8 stderr"),
        ),
        (
            Some(1),
            String::new(),
            format!(
                "downloading kotlin-native {VERSION} for {}…\n\
                 downloaded Kotlin/Native archive has no common stdlib KLIB metadata\n",
                host()
            ),
        )
    );
    assert!(!cache.join(VERSION).join(distribution_name()).exists());
    fs::remove_dir_all(dir).expect("remove provisioning scratch directory");
}
