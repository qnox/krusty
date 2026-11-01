//! A compiled class directory's dependency fingerprint, over classes the real `krusty` binary emits:
//! a body-only edit leaves it unchanged, and every edit a dependent compilation observes moves it.
//! A class declaring an `inline` function contributes its bytes, so the inline function lives in
//! its own object here and the body edits elsewhere stay invisible.
//!
//! Skips (rather than fails) when the `krusty` binary is unavailable, as `hello_world` does.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty_build::dependency_view::{archive_fingerprint, directory_fingerprint};
use krusty_build::Digest;

fn krusty_binary() -> Option<PathBuf> {
    for variable in ["KRUSTY_BUILD_TEST_BINARY", "KRUSTY_BIN"] {
        if let Ok(path) = std::env::var(variable) {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    ["gate", "debug", "release", "coverage"]
        .iter()
        .map(|profile| workspace_root.join("target").join(profile).join("krusty"))
        .find(|candidate| candidate.is_file())
}

/// Compiles `source` as `library/Library.kt` into a fresh directory and fingerprints it.
fn fingerprint(krusty: &Path, scratch: &Path, tag: &str, source: &str) -> Digest {
    let directory = scratch.join(tag);
    let output = directory.join("classes");
    std::fs::create_dir_all(&directory).expect("create the scratch directory");
    let file = directory.join("Library.kt");
    std::fs::write(&file, source).expect("write the source");
    let result = Command::new(krusty)
        .arg(&file)
        .arg("-d")
        .arg(&output)
        .output()
        .expect("run krusty");
    assert!(
        result.status.success(),
        "krusty failed on {tag}:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    directory_fingerprint(&output).expect("fingerprint the classes")
}

const BASE: &str = "package library\n\
    const val LIMIT = 30\n\
    fun greet(name: String): String = \"Hello, \" + name\n\
    object Shouting { inline fun shout(name: String): String = name.uppercase() }\n\
    class Counter(var count: Int) { fun bump() { count += 1 } }\n";

#[test]
fn only_what_a_dependent_observes_moves_the_fingerprint() {
    let Some(krusty) = krusty_binary() else {
        eprintln!("skipping dependency_view: needs the krusty binary");
        return;
    };
    let scratch =
        std::env::temp_dir().join(format!("krusty-dependency-view-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let base = fingerprint(&krusty, &scratch, "base", BASE);

    let unchanged = [
        ("recompiled", BASE.to_string()),
        ("function-body", BASE.replace("\"Hello, \"", "\"Hi, \"")),
        ("member-body", BASE.replace("count += 1", "count += 2")),
    ];
    for (tag, source) in unchanged {
        assert_eq!(fingerprint(&krusty, &scratch, tag, &source), base, "{tag}");
    }

    let moved = [
        ("constant", BASE.replace("LIMIT = 30", "LIMIT = 99")),
        ("inline-body", BASE.replace("uppercase()", "lowercase()")),
        (
            "signature",
            BASE.replace("fun greet(name: String)", "fun greet(name: CharSequence)"),
        ),
        ("new-function", format!("{BASE}fun added() = 1\n")),
        (
            "visibility",
            BASE.replace("fun greet", "internal fun greet"),
        ),
    ];
    for (tag, source) in moved {
        assert_ne!(fingerprint(&krusty, &scratch, tag, &source), base, "{tag}");
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Compiles `source` into `<tag>/classes` and into `<tag>/library.jar`.
fn compile_both(krusty: &Path, scratch: &Path, tag: &str, source: &str) -> (PathBuf, PathBuf) {
    let directory = scratch.join(tag);
    std::fs::create_dir_all(&directory).expect("create the scratch directory");
    let file = directory.join("Library.kt");
    std::fs::write(&file, source).expect("write the source");
    let outputs = (directory.join("classes"), directory.join("library.jar"));
    for output in [&outputs.0, &outputs.1] {
        let result = Command::new(krusty)
            .arg(&file)
            .arg("-d")
            .arg(output)
            .output()
            .expect("run krusty");
        assert!(
            result.status.success(),
            "krusty failed on {tag}:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    outputs
}

/// A jar is read entry by entry through the same view as a class directory: a body-only edit
/// leaves a module's jar fingerprint unchanged, as it leaves its class directory's, and an edit a
/// dependent observes moves both.
#[test]
fn a_jar_is_read_through_the_dependency_view() {
    let Some(krusty) = krusty_binary() else {
        eprintln!("skipping dependency_view: needs the krusty binary");
        return;
    };
    let scratch =
        std::env::temp_dir().join(format!("krusty-dependency-jar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let both = |tag: &str, source: &str| {
        let (classes, jar) = compile_both(&krusty, &scratch, tag, source);
        (
            directory_fingerprint(&classes).expect("fingerprint the classes"),
            archive_fingerprint(&jar).expect("fingerprint the jar"),
        )
    };
    let base = both("base", BASE);
    assert_eq!(
        both("function-body", &BASE.replace("\"Hello, \"", "\"Hi, \"")),
        base
    );
    let inline = both("inline-body", &BASE.replace("uppercase()", "lowercase()"));
    assert_ne!(inline.0, base.0, "the class directory");
    assert_ne!(inline.1, base.1, "the jar");
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Replaces the one occurrence of `from` in the single class file under `classes` with `to`.
fn patch_class(classes: &Path, from: &[u8], to: &[u8]) {
    let mut files = Vec::new();
    let mut pending = vec![classes.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("list the classes") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "class")
            {
                files.push(path);
            }
        }
    }
    let mut patched = 0;
    for file in files {
        let mut bytes = std::fs::read(&file).expect("read a class");
        let positions: Vec<_> = bytes
            .windows(from.len())
            .enumerate()
            .filter(|(_, window)| *window == from)
            .map(|(position, _)| position)
            .collect();
        for position in positions {
            bytes[position..position + to.len()].copy_from_slice(to);
            patched += 1;
        }
        std::fs::write(&file, bytes).expect("write a class");
    }
    assert_eq!(patched, 1, "{from:02x?} occurs once");
}

/// A constant's NaN payload is part of what a dependent folds into its own bytecode, so two
/// declaration models that differ only there have different fingerprints.
#[test]
fn a_nan_payload_moves_the_fingerprint() {
    let Some(krusty) = krusty_binary() else {
        eprintln!("skipping dependency_view: needs the krusty binary");
        return;
    };
    let scratch =
        std::env::temp_dir().join(format!("krusty-dependency-nan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let source = "package library\nconst val SINGLE = 1.5f\nconst val DOUBLE = 2.5\n";
    // CONSTANT_Float 1.5f and CONSTANT_Double 2.5, as their constant-pool entries.
    let single = [4u8, 0x3f, 0xc0, 0, 0];
    let double = [6u8, 0x40, 0x04, 0, 0, 0, 0, 0, 0];
    let nan = |tag: &str, single_nan: [u8; 5], double_nan: [u8; 9]| {
        fingerprint(&krusty, &scratch, tag, source);
        let classes = scratch.join(tag).join("classes");
        patch_class(&classes, &single, &single_nan);
        patch_class(&classes, &double, &double_nan);
        directory_fingerprint(&classes).expect("fingerprint the classes")
    };
    let quiet = nan(
        "quiet",
        [4, 0x7f, 0xc0, 0, 0],
        [6, 0x7f, 0xf8, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        nan(
            "quiet-again",
            [4, 0x7f, 0xc0, 0, 0],
            [6, 0x7f, 0xf8, 0, 0, 0, 0, 0, 0]
        ),
        quiet
    );
    assert_ne!(
        nan(
            "single-payload",
            [4, 0x7f, 0xc0, 0, 1],
            [6, 0x7f, 0xf8, 0, 0, 0, 0, 0, 0]
        ),
        quiet,
        "the float's payload"
    );
    assert_ne!(
        nan(
            "double-payload",
            [4, 0x7f, 0xc0, 0, 0],
            [6, 0x7f, 0xf8, 0, 0, 0, 0, 0, 1]
        ),
        quiet,
        "the double's payload"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
