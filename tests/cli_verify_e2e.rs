//! `krusty verify-compile` compiles beside kotlinc's output and never into it. A layout in which
//! krusty's output or the record could overwrite the reference, or a jar output it cannot compare,
//! is refused before anything is written; a compilation without Kotlin sources is recorded as such,
//! and every run replaces the record an earlier run left.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::common;

struct Layout {
    root: PathBuf,
    reference: PathBuf,
    source: PathBuf,
}

impl Layout {
    fn new(name: &str) -> Layout {
        let root =
            std::env::temp_dir().join(format!("krusty-cli-verify-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let reference = root.join("kotlinc");
        std::fs::create_dir_all(reference.join("app")).expect("reference directory");
        std::fs::write(reference.join("app/Point.class"), b"kotlinc's bytes")
            .expect("sentinel class");
        std::fs::create_dir_all(reference.join("META-INF")).expect("META-INF");
        std::fs::write(
            reference.join("META-INF/main.kotlin_module"),
            b"kotlinc's module",
        )
        .expect("sentinel module");
        let source = root.join("src/Point.kt");
        std::fs::create_dir_all(source.parent().expect("parent")).expect("source directory");
        std::fs::write(&source, "package app\n\nclass Point(val x: Int)\n").expect("source");
        Layout {
            root,
            reference,
            source,
        }
    }

    /// Every file under the reference, with its bytes.
    fn reference_files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(directory: &Path, base: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(directory).expect("read the reference") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    walk(&path, base, files);
                } else {
                    let bytes = std::fs::read(&path).expect("read a reference file");
                    files.insert(
                        path.strip_prefix(base).expect("under base").to_path_buf(),
                        bytes,
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(&self.reference, &self.reference, &mut files);
        files
    }

    fn verify(&self, record: &Path, kotlinc: &[String]) -> (Option<i32>, String) {
        let output = Command::new(common::krusty_binary())
            .arg("verify-compile")
            .arg("--reference")
            .arg(&self.reference)
            .arg("--record")
            .arg(record)
            .args([
                "--compilation",
                ":compileKotlin",
                "--kotlinc-millis",
                "30",
                "--",
            ])
            .args(kotlinc)
            .output()
            .expect("run krusty verify-compile");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn kotlinc(&self, destination: &Path) -> Vec<String> {
        vec![
            "-classpath".to_string(),
            common::stdlib_jar().display().to_string(),
            "-d".to_string(),
            destination.display().to_string(),
            self.source.display().to_string(),
        ]
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn layouts_that_could_overwrite_kotlincs_output_are_refused_before_anything_is_written() {
    let layout = Layout::new("overlap");
    let before = layout.reference_files();
    let record = layout.root.join("records/compileKotlin.json");
    let overlap = |one: &str, one_path: &Path, other: &str, other_path: &Path| {
        format!(
            "error: {one} {} overlaps {other} {}; kotlinc's output, krusty's output and the record must be separate\n",
            one_path.display(),
            other_path.display()
        )
    };

    let mut cases = vec![
        (layout.reference.clone(), record.clone()),
        (layout.reference.join("krusty"), record.clone()),
        (layout.root.clone(), record.clone()),
        (layout.root.join("records/../kotlinc"), record.clone()),
    ];
    #[cfg(unix)]
    {
        let alias = layout.root.join("alias");
        std::os::unix::fs::symlink(&layout.reference, &alias).expect("link the reference");
        cases.push((alias.join("classes"), record.clone()));
        // A missing directory, its parent, then the link: only resolving in order finds the
        // reference.
        cases.push((layout.root.join("missing/../alias/classes"), record.clone()));
    }
    for (destination, record) in &cases {
        assert_eq!(
            layout.verify(record, &layout.kotlinc(destination)),
            (
                Some(2),
                overlap("-d", destination, "--reference", &layout.reference)
            ),
            "{}",
            destination.display()
        );
    }
    let inside = layout.reference.join("compileKotlin.json");
    assert_eq!(
        layout.verify(&inside, &layout.kotlinc(&layout.root.join("krusty"))),
        (
            Some(2),
            overlap("--record", &inside, "--reference", &layout.reference)
        )
    );
    let destination = layout.root.join("krusty");
    let in_output = destination.join("compileKotlin.json");
    assert_eq!(
        layout.verify(&in_output, &layout.kotlinc(&destination)),
        (Some(2), overlap("--record", &in_output, "-d", &destination))
    );
    let jar = layout.root.join("krusty.jar");
    assert_eq!(
        layout.verify(&record, &layout.kotlinc(&jar)),
        (
            Some(2),
            format!(
                "error: -d {} names a jar; verify-compile compares class directories\n",
                jar.display()
            )
        )
    );

    assert_eq!(layout.reference_files(), before);
    assert!(!record.exists());
    assert!(!destination.exists());
    assert!(!layout.reference.join("classes").exists());
    assert!(!layout.root.join("missing").exists());
    assert!(!jar.exists());
}

#[test]
fn each_run_replaces_the_record_with_its_own_outcome() {
    let layout = Layout::new("fresh");
    let record = layout.root.join("records/compileKotlin.json");
    let destination = layout.root.join("krusty");
    // verify-compile owns its output directory: an earlier run's class does not survive.
    std::fs::create_dir_all(destination.join("app")).expect("an earlier output");
    std::fs::write(destination.join("app/Removed.class"), b"earlier").expect("an earlier class");
    let (status, _) = layout.verify(&record, &layout.kotlinc(&destination));
    assert_eq!(status, Some(0));
    let compiled = std::fs::read_to_string(&record).expect("record");
    assert!(compiled.contains("\"status\": \"compiled\""), "{compiled}");
    assert!(destination.join("app/Point.class").is_file());
    assert!(!destination.join("app/Removed.class").exists());

    // The sources are gone: the record says so, rather than keeping the earlier result.
    let without_sources = vec![
        "-classpath".to_string(),
        common::stdlib_jar().display().to_string(),
        "-d".to_string(),
        destination.display().to_string(),
    ];
    assert_eq!(
        layout.verify(&record, &without_sources),
        (Some(0), String::new())
    );
    assert_eq!(
        std::fs::read_to_string(&record).expect("record"),
        "{\n  \"compilation\": \":compileKotlin\",\n  \"outcome\": {\n    \"status\": \"no-sources\"\n  }\n}\n"
    );

    // A compilation the build tool already knows krusty does not support, with no request to pass.
    let output = Command::new(common::krusty_binary())
        .arg("verify-compile")
        .arg("--reference")
        .arg(layout.root.join("kotlinc-dsl"))
        .arg("--record")
        .arg(&record)
        .args([
            "--compilation",
            ":compileKotlinDsl",
            "--unsupported",
            "kotlin-dsl compilations stay with kotlinc",
        ])
        .output()
        .expect("run krusty verify-compile");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&record).expect("record"),
        "{\n  \"compilation\": \":compileKotlinDsl\",\n  \"outcome\": {\n    \"status\": \"unsupported\",\n    \"reasons\": [\n      \"kotlin-dsl compilations stay with kotlinc\"\n    ]\n  }\n}\n"
    );
}

/// A request that compiles nothing names no output, so nothing beside the record is touched: not
/// the parser's default `krusty-out` in the working directory, which may be another build's.
#[test]
fn an_unsupported_record_touches_no_compiler_output() {
    let layout = Layout::new("record-only");
    let default_output = layout.root.join("krusty-out");
    std::fs::create_dir_all(&default_output).expect("an unrelated output");
    std::fs::write(default_output.join("Kept.class"), b"someone else's").expect("sentinel");
    let record = layout.root.join("records/unsupported.json");
    let output = Command::new(common::krusty_binary())
        .current_dir(&layout.root)
        .arg("verify-compile")
        .arg("--reference")
        .arg(&layout.reference)
        .arg("--record")
        .arg(&record)
        .args([
            "--compilation",
            ":compileKotlinDsl",
            "--unsupported",
            "reason",
        ])
        .output()
        .expect("run krusty verify-compile");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        std::fs::read(default_output.join("Kept.class")).expect("the unrelated output"),
        b"someone else's"
    );
    assert_eq!(
        std::fs::read_to_string(&record).expect("record"),
        "{\n  \"compilation\": \":compileKotlinDsl\",\n  \"outcome\": {\n    \"status\": \"unsupported\",\n    \"reasons\": [\n      \"reason\"\n    ]\n  }\n}\n"
    );

    // A request that compiles must name krusty's output itself.
    let (status, stderr) = layout.verify(
        &record,
        &[
            "-classpath".to_string(),
            common::stdlib_jar().display().to_string(),
            layout.source.display().to_string(),
        ],
    );
    assert_eq!(
        (status, stderr.lines().next().map(str::to_string)),
        (
            Some(2),
            Some("error: the kotlinc arguments name krusty's own output with -d".to_string())
        )
    );
    assert!(default_output.join("Kept.class").is_file());
}

/// A record verify-report would replace is refused before any report is removed.
#[test]
fn verify_report_refuses_a_record_among_its_reports_and_removes_nothing() {
    let layout = Layout::new("report-input");
    let out = layout.root.join("report");
    std::fs::create_dir_all(&out).expect("report directory");
    for name in [
        "krusty-verify.html",
        "krusty-verify.md",
        "krusty-verify-issue.md",
    ] {
        std::fs::write(out.join(name), name).expect("an earlier report");
    }
    std::os::unix::fs::symlink(out.join("krusty-verify.md"), layout.root.join("alias.json"))
        .expect("link");
    for record in [out.join("krusty-verify.md"), layout.root.join("alias.json")] {
        let output = Command::new(common::krusty_binary())
            .arg("verify-report")
            .arg("--out")
            .arg(&out)
            .arg(&record)
            .output()
            .expect("run krusty verify-report");
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "error: record {} is a report verify-report writes to --out {}\n",
                record.display(),
                out.display()
            )
        );
        for name in [
            "krusty-verify.html",
            "krusty-verify.md",
            "krusty-verify-issue.md",
        ] {
            assert_eq!(
                std::fs::read_to_string(out.join(name)).expect("the earlier report"),
                name
            );
        }
    }
}
