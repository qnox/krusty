//! Failure evidence shared by conformance and product e2e tests.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FAILURE: AtomicU64 = AtomicU64::new(0);

pub fn retain(label: &str, report: &str, files: &[(&str, &[u8])]) -> io::Result<PathBuf> {
    let root = std::env::var_os("KRUSTY_TEST_FAILURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/test-failures"));
    retain_into(&root, label, report, files)
}

fn retain_into(
    root: &Path,
    label: &str,
    report: &str,
    files: &[(&str, &[u8])],
) -> io::Result<PathBuf> {
    let label: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let directory = root.join(format!(
        "{label}-{}-{}",
        std::process::id(),
        NEXT_FAILURE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory)?;
    let mut manifest = format!(
        "test: {}\nKotlin: {}\n{report}\n\nartifacts:\n",
        std::thread::current().name().unwrap_or("unknown"),
        std::env::var("KRUSTY_LANGUAGE_VERSION").unwrap_or_else(|_| "configured default".into())
    );
    for &(name, bytes) in files {
        let relative = Path::new(name);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "failure artifact must have a relative, non-traversing path",
            ));
        }
        let path = directory.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        manifest.push_str(&format!("{name}\t{} bytes\n", bytes.len()));
    }
    std::fs::write(directory.join("failure.txt"), manifest)?;
    Ok(directory)
}

#[test]
fn preserves_binary_loader_source_and_reason_in_distinct_failure_directories() {
    let root = std::env::temp_dir().join(format!("krusty-evidence-test-{}", std::process::id()));
    let files: &[(&str, &[u8])] = &[
        ("box.wasm", b"\0asm"),
        ("box.mjs", b"loader"),
        ("source.kt", b"whole source"),
    ];
    let first = retain_into(&root, "same/case", "exit signal: 11", files).unwrap();
    let second = retain_into(&root, "same/case", "timeout", files).unwrap();
    assert_ne!(first, second);
    for &(name, bytes) in files {
        assert_eq!(std::fs::read(first.join(name)).unwrap(), bytes);
    }
    let report = std::fs::read_to_string(first.join("failure.txt")).unwrap();
    assert!(report.contains("exit signal: 11"));
    assert!(report.contains("box.wasm\t4 bytes"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn refuses_artifact_paths_that_escape_the_failure_directory() {
    let root =
        std::env::temp_dir().join(format!("krusty-evidence-path-test-{}", std::process::id()));
    let error = retain_into(&root, "case", "failure", &[("../escaped", b"bad")]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    std::fs::remove_dir_all(root).unwrap();
}
