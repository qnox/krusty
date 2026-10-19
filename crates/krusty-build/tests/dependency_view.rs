//! A compiled class directory's dependency fingerprint, over classes the real `krusty` binary emits:
//! a body-only edit leaves it unchanged, and every edit a dependent compilation observes moves it.
//! A class declaring an `inline` function contributes its bytes, so the inline function lives in
//! its own object here and the body edits elsewhere stay invisible.
//!
//! Skips (rather than fails) when the `krusty` binary is unavailable, as `hello_world` does.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty_build::dependency_view::directory_fingerprint;
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
